//! Bounded table preparation and model submission share resource admission across pages.
use std::time::Duration;
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use docparse_common::timing::{TimingStage, Timings};
use docparse_config::FusionConfig;
use docparse_layout::{
    AffineTransform, Bbox, LayoutLabel, PageImage, PageImageInput,
    PageTransform, Point,
};
use futures_util::{StreamExt, stream};
use typed_builder::TypedBuilder;

use super::ParseRuntimeError;
use crate::line::FormulaRegion;
use crate::page::PageTableDraft;
use crate::table::TableAssembler;
use crate::{
    Block, Evidence, PageWarning, TableEvidence, TableMode, TableOptions,
    TableStructureEngine, TableStructureError, TsrRequestReason, TsrTableInput,
    TsrTableRequest,
};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Immutable page inputs shared by every table request on that page.
///
/// A blocking hop cannot borrow from the draft, so each request clones this context: the arcs
/// share the page evidence while the image and transform stay cheap copies.
#[derive(Clone, TypedBuilder)]
struct TableContext {
    config: Arc<FusionConfig>,
    evidence: Arc<TableEvidence>,
    formulas: Arc<[FormulaRegion]>,
    image: PageImage,
    transform: PageTransform,
    timings: Timings,
    page: u32,
    engine: Arc<dyn TableStructureEngine>,
}

/// Shares table policy and the provider across all pages of a parse.
#[derive(TypedBuilder)]
pub(crate) struct TableRuntime {
    options: TableOptions,
    #[builder(default)]
    engine: Option<Arc<dyn TableStructureEngine>>,
    admission: docparse_common::ResourceBudget,
}

impl TableRuntime {
    /// Shares validated policy and global pre-crop admission across every page using a parser.
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "browser trait objects stay in one Worker; native bounds require Send and Sync"
    )]
    pub(crate) fn shared(
        options: TableOptions,
        engine: Option<Arc<dyn TableStructureEngine>>,
        admission: docparse_common::ResourceBudget,
    ) -> Result<Arc<Self>, TableStructureError> {
        options.validate(engine.is_some())?;
        // Built-in engines share their own budget with direct model callers; overrides use the parser budget.
        let admission = engine
            .as_ref()
            .and_then(|engine| engine.admission())
            .unwrap_or(admission);
        Ok(Arc::new(
            Self::builder()
                .options(options)
                .engine(engine)
                .admission(admission)
                .build(),
        ))
    }

    /// Replenishes a bounded window of same-page tables while preserving result ownership and failures.
    ///
    /// The draft is taken and returned by value so this page can own its evidence across the
    /// blocking hops below: every synchronous section runs on the blocking pool, which keeps
    /// grid solving and crop copying off the async executor threads.
    pub(crate) async fn resolve(
        &self,
        draft: PageTableDraft,
        image: &PageImage,
        transform: &PageTransform,
        config: &FusionConfig,
        timings: &Timings,
    ) -> Result<PageTableDraft, ParseRuntimeError> {
        if self.options.mode == TableMode::RulesOnly {
            let config = config.clone();
            let timings = timings.clone();
            let restored = docparse_common::run_cpu(move || {
                let mut draft = draft;
                draft.reconstruct_local(&config, &timings);
                draft
            })
            .await
            .map_err(|error| ParseRuntimeError::Task(error.to_string()))?;
            return Ok(restored);
        }
        let Some(engine) = &self.engine else {
            return Ok(draft);
        };
        let config = Arc::new(config.clone());
        let image = image.clone();
        let transform = transform.clone();
        let timings = timings.clone();
        let engine = Arc::clone(engine);
        let (mut draft, context) = docparse_common::run_cpu(move || {
            let context = TableContext::builder()
                .config(config)
                .evidence(Arc::new(draft.extracted.table_evidence.clone()))
                .formulas(Arc::from(draft.formula_regions.as_slice()))
                .image(image)
                .transform(transform)
                .timings(timings)
                .page(draft.extracted.page_number)
                .engine(engine)
                .build();
            (draft, context)
        })
        .await
        .map_err(|error| ParseRuntimeError::Task(error.to_string()))?;
        // Bound scheduled futures as well as cropped inputs while retaining model-sized overlap.
        let count = draft.blocks.len();
        let mut completed: Vec<Option<Block>> =
            (0..count).map(|_| None).collect();
        let requests = stream::iter(
            std::mem::take(&mut draft.blocks)
                .into_iter()
                .enumerate()
                .map(|(index, block)| {
                    let context = context.clone();
                    async move {
                        if block.label == LayoutLabel::Table {
                            self.resolve_block(block, &context).await.map(
                                |(block, warnings)| (index, block, warnings),
                            )
                        } else {
                            Ok((index, block, Vec::new()))
                        }
                    }
                }),
        )
        .buffer_unordered(self.admission.capacity());
        tokio::pin!(requests);
        while let Some(result) = requests.next().await {
            let (index, block, warnings) = result?;
            *completed.get_mut(index).expect("original block index") =
                Some(block);
            draft.warnings.extend(warnings);
        }
        draft.blocks = completed
            .into_iter()
            .map(|block| block.expect("completed block"))
            .collect();
        // Completion order must not change deterministic page diagnostics.
        draft.warnings.sort_by(|left, right| {
            (&left.stage, &left.code, &left.message).cmp(&(
                &right.stage,
                &right.code,
                &right.message,
            ))
        });
        Ok(draft)
    }

    /// Resolves one block while preserving native text across timed external recognition.
    ///
    /// Rules and successful filling move the block onto CPU workers; cropping captures only geometry.
    async fn resolve_block(
        &self,
        mut staged: Block,
        context: &TableContext,
    ) -> Result<(Block, Vec<PageWarning>), ParseRuntimeError> {
        let reason = if self.options.mode == TableMode::Fallback {
            let (restored, probe) = docparse_common::run_cpu({
                let context = context.clone();
                move || {
                    let _timer = context
                        .timings
                        .for_page(context.page)
                        .start(TimingStage::TableRules);
                    let assembler = TableAssembler::new(
                        context.config.as_ref(),
                        context.evidence.as_ref(),
                        context.formulas.as_ref(),
                    );
                    let outcome = assembler.reconstruct(&mut staged);
                    (staged, outcome)
                }
            })
            .await
            .map_err(|error| ParseRuntimeError::Task(error.to_string()))?;
            staged = restored;
            match probe {
                Ok(()) => {
                    return Ok((staged, Vec::new()));
                }
                Err(message) => {
                    tracing::debug!(
                        "local table {} requires external structure: {}",
                        staged.id.as_str(),
                        message
                    );
                    TsrRequestReason::RulesFailed { message }
                }
            }
        } else {
            TsrRequestReason::TsrOnly
        };
        // One deadline covers admission, CPU preparation, and model work; the source block stays local.
        let external = context
            .timings
            .for_page(context.page)
            .start(TimingStage::TableExternal);
        let recognized = crate::wasm_compat::timeout(
            Duration::from_millis(self.options.timeout_ms),
            context.recognize(&staged, reason, &self.admission),
        )
        .await;
        drop(external);
        let (staged, result) = match recognized {
            Ok(Ok((request, input))) => {
                context.fill(staged, request, input).await?
            }
            Ok(Err(ParseRuntimeError::Table(error))) => (staged, Err(error)),
            Ok(Err(error)) => return Err(error),
            Err(_elapsed) => (
                staged,
                Err(TableStructureError::Timeout {
                    timeout_ms: self.options.timeout_ms,
                }),
            ),
        };
        let warnings = if let Err(error) = result {
            tracing::warn!(
                "external table {} on page {} failed with {}: {}",
                staged.id.as_str(),
                context.page,
                error.code(),
                error
            );
            vec![
                PageWarning {
                    code: error.code().to_owned(),
                    stage: "table".to_owned(),
                    message: format!("table {}: {}", staged.id.as_str(), error),
                },
                PageWarning {
                    code: "TableStructureUnavailable".to_owned(),
                    stage: "table".to_owned(),
                    message: format!(
                        "table {} retains source lines: {}",
                        staged.id.as_str(),
                        error
                    ),
                },
            ]
        } else {
            Vec::new()
        };
        Ok((staged, warnings))
    }
}

impl TableContext {
    /// Prepares an owned crop and obtains topology while leaving source text available for immediate timeout recovery.
    async fn recognize(
        &self,
        block: &Block,
        reason: TsrRequestReason,
        admission: &docparse_common::ResourceBudget,
    ) -> Result<(TsrTableRequest, TsrTableInput), ParseRuntimeError> {
        let queued = self
            .timings
            .for_page(self.page)
            .start(TimingStage::TsrQueue);
        let resources = admission.reserve().await.map_err(|error| {
            TableStructureError::Engine {
                message: error.to_string(),
            }
        })?;
        drop(queued);
        resources.scope(async {
            let crop = TableCrop::from((block, self, reason));
            let mut request = docparse_common::run_cpu(move || TsrTableRequest::try_from(crop))
                .await.map_err(|error| ParseRuntimeError::Task(error.to_string()))??;
            request.timings = self.timings.for_page(self.page);
            tracing::info!("requesting table structure {} from {} for page {} block {}", request.request_id, self.engine.name(), self.page, block.id.as_str());
            let input = self.engine.recognize(request.clone()).await?;
            Ok((request, input))
        }).await
    }

    /// Applies successful topology on the CPU pool and preserves the original block if validation fails.
    async fn fill(
        &self,
        mut block: Block,
        request: TsrTableRequest,
        input: TsrTableInput,
    ) -> Result<(Block, Result<(), TableStructureError>), ParseRuntimeError>
    {
        let context = self.clone();
        docparse_common::run_cpu(move || {
            let _fill = context
                .timings
                .for_page(context.page)
                .start(TimingStage::TableFill);
            let assembler = TableAssembler::new(
                &context.config,
                &context.evidence,
                &context.formulas,
            );
            let outcome = assembler.reconstruct_external(
                &mut block,
                &request,
                input,
                context.engine.geometry_policy(),
            );
            if outcome.is_ok() {
                let details = BTreeMap::from([
                    ("request_id".to_owned(), request.request_id.clone()),
                    ("engine".to_owned(), context.engine.name().to_owned()),
                    (
                        "reason".to_owned(),
                        match request.reason {
                            TsrRequestReason::RulesFailed { message } => {
                                message
                            }
                            TsrRequestReason::TsrOnly => "tsr_only".to_owned(),
                        },
                    ),
                ]);
                block.evidence.push(
                    Evidence::builder()
                        .kind("external_table_structure".to_owned())
                        .details(details)
                        .build(),
                );
                tracing::info!(
                    "completed table structure {} for page {} block {}",
                    request.request_id,
                    context.page,
                    block.id.as_str()
                );
            }
            (block, outcome)
        })
        .await
        .map_err(|error| ParseRuntimeError::Task(error.to_string()))
    }
}

/// Small owned crop metadata crosses CPU boundaries without moving or copying the block's text graph.
#[derive(TypedBuilder)]
struct TableCrop {
    page: u32,
    block_id: crate::BlockId,
    regions: Vec<Bbox>,
    image: PageImage,
    transform: PageTransform,
    reason: TsrRequestReason,
}

impl From<(&Block, &TableContext, TsrRequestReason)> for TableCrop {
    /// Copies only geometry and cheap shared handles so cancellation can return the caller's original block.
    fn from(
        (block, context, reason): (&Block, &TableContext, TsrRequestReason),
    ) -> Self {
        Self::builder()
            .page(context.page)
            .block_id(block.id.clone())
            .regions(
                std::iter::once(block.bbox)
                    .chain(
                        block
                            .source_regions()
                            .filter(|region| {
                                region.label.as_ref().is_none_or(|label| {
                                    *label == LayoutLabel::Table
                                })
                            })
                            .map(|region| region.bbox),
                    )
                    .collect(),
            )
            .image(context.image.clone())
            .transform(context.transform.clone())
            .reason(reason)
            .build()
    }
}

impl TryFrom<TableCrop> for TsrTableRequest {
    type Error = TableStructureError;

    /// Rounds in pixel space and derives an exact transform for the resulting owned crop.
    #[allow(
        clippy::cast_sign_loss,
        reason = "pixel coordinates are clamped to nonnegative image dimensions before conversion"
    )]
    fn try_from(input: TableCrop) -> Result<Self, Self::Error> {
        let invalid = |reason: &str| TableStructureError::InvalidInput {
            reason: reason.to_owned(),
        };
        if input.transform.render_size()
            != (input.image.width(), input.image.height())
        {
            return Err(invalid("table image and transform sizes disagree"));
        }
        let mut regions = input.regions.into_iter();
        let mut region = regions
            .next()
            .ok_or_else(|| invalid("missing table region"))?;
        for original in regions {
            region = Bbox::try_from([
                region.left.min(original.left),
                region.top.min(original.top),
                region.right.max(original.right),
                region.bottom.max(original.bottom),
            ])
            .map_err(|error| {
                invalid(&format!("invalid source table region: {error}"))
            })?;
        }
        let (width, height) = input.transform.viewport_size();
        let region = Bbox::try_from([
            region.left.max(0.0),
            region.top.max(0.0),
            region.right.min(width),
            region.bottom.min(height),
        ])
        .map_err(|error| {
            invalid(&format!("table is outside the page: {error}"))
        })?;
        let start = input
            .transform
            .viewport_to_rendered(Point::new(region.left, region.top));
        let end = input
            .transform
            .viewport_to_rendered(Point::new(region.right, region.bottom));
        let left =
            start.x.floor().clamp(0.0, f64::from(input.image.width())) as u32;
        let top =
            start.y.floor().clamp(0.0, f64::from(input.image.height())) as u32;
        let right =
            end.x.ceil().clamp(0.0, f64::from(input.image.width())) as u32;
        let bottom =
            end.y.ceil().clamp(0.0, f64::from(input.image.height())) as u32;
        let crop_width = right
            .checked_sub(left)
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("empty crop width"))?;
        let crop_height = bottom
            .checked_sub(top)
            .filter(|n| *n > 0)
            .ok_or_else(|| invalid("empty crop height"))?;
        let stride = usize::try_from(input.image.width())
            .ok()
            .and_then(|w| w.checked_mul(3))
            .ok_or_else(|| invalid("image stride overflow"))?;
        let row_bytes = usize::try_from(crop_width)
            .ok()
            .and_then(|w| w.checked_mul(3))
            .ok_or_else(|| invalid("crop stride overflow"))?;
        let capacity = row_bytes
            .checked_mul(crop_height as usize)
            .ok_or_else(|| invalid("crop size overflow"))?;
        let mut pixels = Vec::with_capacity(capacity);
        for y in top..bottom {
            let offset = (y as usize)
                .checked_mul(stride)
                .and_then(|p| p.checked_add(left as usize * 3))
                .ok_or_else(|| invalid("crop offset overflow"))?;
            let end = offset
                .checked_add(row_bytes)
                .ok_or_else(|| invalid("crop offset overflow"))?;
            pixels.extend_from_slice(
                input
                    .image
                    .data()
                    .get(offset..end)
                    .ok_or_else(|| invalid("crop exceeds image buffer"))?,
            );
        }
        let origin = input
            .transform
            .rendered_to_viewport(Point::new(f64::from(left), f64::from(top)));
        let corner = input.transform.rendered_to_viewport(Point::new(
            f64::from(right),
            f64::from(bottom),
        ));
        let crop_bbox =
            Bbox::try_from([origin.x, origin.y, corner.x, corner.y]).map_err(
                |error| invalid(&format!("invalid crop transform: {error}")),
            )?;
        let image = PageImage::try_from(
            PageImageInput::builder()
                .width(crop_width)
                .height(crop_height)
                .pixel_format(input.image.pixel_format())
                .data(Arc::from(pixels))
                .build(),
        )
        .map_err(|error| invalid(&format!("invalid crop image: {error}")))?;
        Ok(TsrTableRequest::builder()
            .request_id(format!(
                "tsr-{}",
                NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed)
            ))
            .page_number(input.page)
            .block_id(input.block_id)
            .crop_bbox(crop_bbox)
            .image(Arc::new(image))
            .crop_to_viewport(
                AffineTransform::builder()
                    .a(width / f64::from(input.image.width()))
                    .b(0.0)
                    .c(0.0)
                    .d(height / f64::from(input.image.height()))
                    .e(origin.x)
                    .f(origin.y)
                    .build(),
            )
            .reason(input.reason)
            .build())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BlockId, LabelSource, SourceRegionEvidence};
    use docparse_layout::{
        GeometrySource, PageRotation, PageTransformInput, PixelFormat,
    };

    /// Provides an external endpoint that must never be reached after crop preparation expires.
    struct DeadlineEngine;
    impl TableStructureEngine for DeadlineEngine {
        /// Identifies the isolated deadline regression.
        fn name(&self) -> &str {
            "deadline-test"
        }
        /// Returning an ordinary provider error distinguishes accidental inference from a preparation timeout.
        fn recognize(
            &self,
            _request: TsrTableRequest,
        ) -> crate::WasmBoxedFuture<
            '_,
            Result<TsrTableInput, TableStructureError>,
        > {
            Box::pin(async {
                Err(TableStructureError::Engine {
                    message: "unexpected inference".into(),
                })
            })
        }
    }

    /// CPU preparation shares the external deadline, while canceled cleanup continues retaining its slot.
    #[tokio::test]
    #[ignore = "saturates the process-wide CPU pool; run this regression in isolation"]
    async fn cpu_wait_is_within_table_deadline() {
        let budget = docparse_common::ResourceBudget::new(1);
        let engine = Arc::new(DeadlineEngine) as Arc<dyn TableStructureEngine>;
        let runtime = TableRuntime::shared(
            TableOptions::builder()
                .mode(TableMode::TsrOnly)
                .timeout_ms(30)
                .build(),
            Some(Arc::clone(&engine)),
            budget.clone(),
        )
        .expect("runtime");
        let image = PageImage::try_from(
            PageImageInput::builder()
                .width(10)
                .height(10)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(vec![255; 300]))
                .build(),
        )
        .expect("image");
        let transform = PageTransform::try_from(
            PageTransformInput::builder()
                .page_to_viewport(AffineTransform::identity())
                .viewport_width(10.0)
                .viewport_height(10.0)
                .render_width(10)
                .render_height(10)
                .model_width(800)
                .model_height(800)
                .rotation(PageRotation::Degrees0)
                .build(),
        )
        .expect("transform");
        let context = TableContext::builder()
            .config(Arc::new(FusionConfig::default()))
            .evidence(Arc::new(TableEvidence::default()))
            .formulas(Arc::from([]))
            .image(image)
            .transform(transform)
            .timings(Timings::default())
            .page(1)
            .engine(engine)
            .build();
        let block = Block::builder()
            .id(BlockId::model(1, 0, 0))
            .label(LayoutLabel::Table)
            .label_source(LabelSource::Model)
            .text("source text".into())
            .bbox(Bbox::try_from([0.0, 0.0, 10.0, 10.0]).expect("bbox"))
            .final_order(0)
            .lines(Vec::new())
            .build();
        let gate =
            Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let (entered, mut started) = tokio::sync::mpsc::unbounded_channel();
        let mut blockers = tokio::task::JoinSet::new();
        let count = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .saturating_sub(1)
            .max(1);
        for _ in 0..count {
            let gate = Arc::clone(&gate);
            let entered = entered.clone();
            blockers.spawn(docparse_common::run_cpu(move || {
                entered.send(()).expect("CPU entered");
                let mut released = gate.0.lock().expect("gate");
                while !*released {
                    released = gate.1.wait(released).expect("release");
                }
            }));
        }
        let occupied = tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..count {
                started.recv().await.expect("CPU occupied");
            }
        })
        .await;
        if occupied.is_err() {
            // Setup failures must also release workers before reporting a changed pool size or startup failure.
            *gate.0.lock().expect("gate") = true;
            gate.1.notify_all();
            while blockers.join_next().await.is_some() {}
        }
        occupied.expect("CPU pool fully occupied");
        let result = tokio::time::timeout(
            Duration::from_millis(300),
            runtime.resolve_block(block, &context),
        )
        .await;
        let held =
            tokio::time::timeout(Duration::from_millis(5), budget.reserve())
                .await
                .is_err();
        // Always release real workers before checking the result, including on the failing baseline.
        *gate.0.lock().expect("gate") = true;
        gate.1.notify_all();
        while let Some(result) = blockers.join_next().await {
            result.expect("join").expect("CPU operation");
        }
        let _released =
            tokio::time::timeout(Duration::from_secs(1), budget.reserve())
                .await
                .expect("cleanup finishes")
                .expect("open budget");
        let (block, warnings) = result
            .expect("table deadline must cover CPU admission")
            .expect("source fallback");
        assert_eq!(block.text, "source text");
        assert!(
            warnings
                .iter()
                .any(|warning| warning.code == "TableExternalTimeout")
        );
        assert!(held, "real cleanup returned its resource slot too early");
    }

    /// Cropping uses canonical coordinates once, independent of the PDF page's rotation or origin.
    #[test]
    fn crop_uses_actual_pixels_and_original_table_extent() {
        let mut pixels = Vec::new();
        for y in 0..100u8 {
            for x in 0..200u8 {
                pixels.extend_from_slice(&[x, y, 0]);
            }
        }
        let image = PageImage::try_from(
            PageImageInput::builder()
                .width(200)
                .height(100)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(pixels))
                .build(),
        )
        .expect("image");
        let region = Bbox::try_from([10.1, 5.2, 89.9, 40.1]).expect("region");
        let block = Block::builder()
            .id(BlockId::model(7, 0, 0))
            .label(LayoutLabel::Table)
            .label_source(LabelSource::Model)
            .text(String::new())
            .bbox(Bbox::try_from([20.0, 10.0, 80.0, 30.0]).expect("content"))
            .final_order(0)
            .lines(Vec::new())
            .source_regions(vec![
                SourceRegionEvidence::builder()
                    .label(LayoutLabel::Table)
                    .bbox(region)
                    .geometry_source(GeometrySource::DerivedFromBbox)
                    .build(),
            ])
            .build();
        for (rotation, coefficients) in [
            (PageRotation::Degrees0, [1.0, 0.0, 0.0, -1.0, -30.0, 70.0]),
            (PageRotation::Degrees90, [0.0, 1.0, 1.0, 0.0, -20.0, -30.0]),
            (
                PageRotation::Degrees180,
                [-1.0, 0.0, 0.0, 1.0, 130.0, -20.0],
            ),
            (
                PageRotation::Degrees270,
                [0.0, -1.0, -1.0, 0.0, 70.0, 130.0],
            ),
        ] {
            let [a, b, c, d, e, f] = coefficients;
            let transform = PageTransform::try_from(
                PageTransformInput::builder()
                    .page_to_viewport(
                        AffineTransform::builder()
                            .a(a)
                            .b(b)
                            .c(c)
                            .d(d)
                            .e(e)
                            .f(f)
                            .build(),
                    )
                    .viewport_width(100.0)
                    .viewport_height(50.0)
                    .render_width(200)
                    .render_height(100)
                    .model_width(800)
                    .model_height(800)
                    .rotation(rotation)
                    .build(),
            )
            .expect("transform");
            let request = TsrTableRequest::try_from(
                TableCrop::builder()
                    .page(7)
                    .block_id(block.id.clone())
                    .regions(vec![block.bbox, region])
                    .image(image.clone())
                    .transform(transform.clone())
                    .reason(TsrRequestReason::TsrOnly)
                    .build(),
            )
            .expect("crop");
            assert_eq!(request.page_number, 7);
            assert_eq!(
                (request.image.width(), request.image.height()),
                (160, 71)
            );
            assert_eq!(request.image.data().get(..3), Some(&[20, 10, 0][..]));
            assert_eq!(
                request.crop_bbox,
                Bbox::try_from([10.0, 5.0, 90.0, 40.5]).expect("rounded crop")
            );
            assert_eq!(
                request
                    .crop_to_viewport
                    .transform_point(Point::new(0.0, 0.0)),
                Point::new(10.0, 5.0)
            );
            assert_eq!(
                request
                    .crop_to_viewport
                    .transform_point(Point::new(160.0, 71.0)),
                Point::new(90.0, 40.5)
            );
        }
    }
}
