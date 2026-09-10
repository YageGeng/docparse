//! One parse's bounded async table stage over already-owned layout blocks.
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use docparse_config::FusionConfig;
use docparse_layout::timing::{TimingStage, Timings};
use docparse_layout::{
    AffineTransform, Bbox, LayoutLabel, PageImage, PageImageInput,
    PageTransform, Point,
};
use tokio::sync::Semaphore;
use typed_builder::TypedBuilder;

use crate::page::PageTableDraft;
use crate::{
    Block, Evidence, PageWarning, TableMode, TableOptions,
    TableStructureEngine, TableStructureError, TsrRequestReason, TsrTableInput,
    TsrTableRequest,
};

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Shared once per parse, so parallel pages obey the same external concurrency budget.
pub(crate) struct TableRuntime {
    options: TableOptions,
    engine: Option<Arc<dyn TableStructureEngine>>,
    permits: Semaphore,
}

impl TableRuntime {
    /// Validates the optional engine and creates a bounded per-parse budget.
    #[allow(
        clippy::arc_with_non_send_sync,
        reason = "browser trait objects stay in one Worker; native bounds require Send and Sync"
    )]
    pub(crate) fn shared(
        options: TableOptions,
        engine: Option<Arc<dyn TableStructureEngine>>,
    ) -> Result<Arc<Self>, TableStructureError> {
        options.validate(engine.is_some())?;
        Ok(Arc::new(Self {
            permits: Semaphore::new(options.max_in_flight),
            options,
            engine,
        }))
    }

    /// Resolves only layout table blocks, preserving source ownership on every failed attempt.
    pub(crate) async fn resolve(
        &self,
        draft: &mut PageTableDraft,
        image: &PageImage,
        transform: &PageTransform,
        config: &FusionConfig,
        timings: &Timings,
    ) {
        if self.options.mode == TableMode::RulesOnly {
            draft.reconstruct_local(config, timings);
            return;
        }
        let Some(engine) = &self.engine else {
            return;
        };
        let page = draft.extracted.page_number;
        let assembler = crate::table::TableAssembler::new(
            config,
            &draft.extracted.table_evidence,
            &draft.formula_regions,
        );
        for block in draft
            .blocks
            .iter_mut()
            .filter(|b| b.label == LayoutLabel::Table)
        {
            let reason = if self.options.mode == TableMode::Fallback {
                let _timer =
                    timings.for_page(page).start(TimingStage::TableRules);
                match assembler.reconstruct(block) {
                    Ok(()) => continue,
                    Err(message) => {
                        tracing::debug!(
                            "local table {} requires external structure: {}",
                            block.id.as_str(),
                            message
                        );
                        TsrRequestReason::RulesFailed { message }
                    }
                }
            } else {
                TsrRequestReason::ExternalOnly
            };
            let request = TsrTableRequest::try_from(
                TableCrop::builder()
                    .page(page)
                    .block(block)
                    .image(image)
                    .transform(transform)
                    .reason(reason.clone())
                    .build(),
            );
            let result = match request {
                Ok(request) => {
                    tracing::info!(
                        "requesting table structure {} from {} for page {} block {}",
                        request.request_id,
                        engine.name(),
                        page,
                        block.id.as_str()
                    );
                    let result = self
                        .recognize(Arc::clone(engine), request.clone(), timings)
                        .await;
                    match result {
                        Ok(input) => {
                            let _fill = timings
                                .for_page(page)
                                .start(TimingStage::TableFill);
                            assembler
                                .reconstruct_external(block, &request, input)
                                .map(|()| {
                                    let details = std::collections::BTreeMap::from([
                                        ("request_id".to_owned(), request.request_id.clone()),
                                        ("engine".to_owned(), engine.name().to_owned()),
                                        ("reason".to_owned(), match reason {
                                            TsrRequestReason::RulesFailed { message } => message,
                                            TsrRequestReason::ExternalOnly => "external_only".to_owned(),
                                        }),
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
                                        page,
                                        block.id.as_str()
                                    );
                                })
                        }
                        Err(error) => Err(error),
                    }
                }
                Err(error) => Err(error),
            };
            if let Err(error) = result {
                tracing::warn!(
                    "external table {} on page {} failed with {}: {}",
                    block.id.as_str(),
                    page,
                    error.code(),
                    error
                );
                draft.warnings.push(PageWarning {
                    code: error.code().to_owned(),
                    stage: "table".to_owned(),
                    message: format!("table {}: {}", block.id.as_str(), error),
                });
                draft.warnings.push(PageWarning {
                    code: "TableStructureUnavailable".to_owned(),
                    stage: "table".to_owned(),
                    message: format!(
                        "table {} retains source lines: {}",
                        block.id.as_str(),
                        error
                    ),
                });
            }
        }
    }

    /// Includes queue wait in the deadline and releases the permit and provider future on cancellation.
    async fn recognize(
        &self,
        engine: Arc<dyn TableStructureEngine>,
        request: TsrTableRequest,
        timings: &Timings,
    ) -> Result<TsrTableInput, TableStructureError> {
        let _timer = timings
            .for_page(request.page_number)
            .start(TimingStage::TableExternal);
        crate::wasm_compat::timeout(
            Duration::from_millis(self.options.timeout_ms),
            async {
                let _permit =
                    self.permits.acquire().await.map_err(|error| {
                        TableStructureError::Engine {
                            message: format!("table scheduler closed: {error}"),
                        }
                    })?;
                engine.recognize(request).await
            },
        )
        .await
        .map_err(|_elapsed| TableStructureError::Timeout {
            timeout_ms: self.options.timeout_ms,
        })?
    }
}

/// Borrowed, already-validated page facts used to create one independently owned crop.
#[derive(TypedBuilder)]
struct TableCrop<'a> {
    page: u32,
    block: &'a Block,
    image: &'a PageImage,
    transform: &'a PageTransform,
    reason: TsrRequestReason,
}

impl TryFrom<TableCrop<'_>> for TsrTableRequest {
    type Error = TableStructureError;

    /// Rounds in pixel space and derives an exact transform for the resulting owned crop.
    #[allow(
        clippy::cast_sign_loss,
        reason = "pixel coordinates are clamped to nonnegative image dimensions before conversion"
    )]
    fn try_from(input: TableCrop<'_>) -> Result<Self, Self::Error> {
        let invalid = |reason: &str| TableStructureError::InvalidInput {
            reason: reason.to_owned(),
        };
        if input.transform.render_size()
            != (input.image.width(), input.image.height())
        {
            return Err(invalid("table image and transform sizes disagree"));
        }
        let mut region = input.block.bbox;
        for original in input.block.source_regions().filter(|r| {
            r.label.as_ref().is_none_or(|l| *l == LayoutLabel::Table)
        }) {
            region = Bbox::try_from([
                region.left.min(original.bbox.left),
                region.top.min(original.bbox.top),
                region.right.max(original.bbox.right),
                region.bottom.max(original.bbox.bottom),
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
            .block_id(input.block.id.clone())
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
                    .block(&block)
                    .image(&image)
                    .transform(&transform)
                    .reason(TsrRequestReason::ExternalOnly)
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
