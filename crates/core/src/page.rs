use std::collections::BTreeMap;
use std::sync::Arc;

use docparse_config::ValidatedConfig;
use docparse_layout::{Bbox, LayoutDetection, LayoutLabel};
use typed_builder::TypedBuilder;

use crate::fusion::assign::{AssignmentEngine, AssignmentResult};
use crate::fusion::fallback::xy_cut;
use crate::fusion::order::order_blocks;
use crate::semantic::{FormulaMatcher, SemanticAssembler};
use crate::{
    DocumentContext, ExtractedPage, OcrResult, PageAnalysisError, PageResult,
    PageWarning, ResultValidator, TextItem, TextSource,
};

/// Outcome supplied after the optional external OCR stage.
#[derive(Debug)]
pub(crate) enum OcrCompletion {
    NotRequested,
    Unavailable,
    Succeeded(OcrResult),
    Failed(String),
}

/// Reusable immutable facts produced before the optional OCR call.
#[derive(Debug, Clone, TypedBuilder)]
pub(crate) struct PageAnalysisDraft {
    pub(crate) extracted: ExtractedPage,
    pub(crate) detections: Vec<LayoutDetection>,
    pub(crate) context: Arc<DocumentContext>,
    pub(crate) missing_regions: Vec<Bbox>,
    pub(crate) native_text_coverage: f64,
}

/// Stable block ownership and retained native evidence while table structures are resolved.
#[derive(TypedBuilder)]
pub(crate) struct PageTableDraft {
    pub(crate) blocks: Vec<crate::Block>,
    pub(crate) warnings: Vec<PageWarning>,
    pub(crate) extracted: ExtractedPage,
    pub(crate) formula_regions: Vec<crate::line::FormulaRegion>,
    context: Arc<DocumentContext>,
    expected_native_ids: BTreeMap<String, usize>,
    references: Vec<crate::Block>,
    watermarks: Vec<crate::Block>,
    inline_formulas: Vec<LayoutDetection>,
    assignment_diagnostics: Vec<String>,
    native_text_coverage: f64,
    replaced_native_text: Vec<TextItem>,
}

impl PageTableDraft {
    /// Preserves the existing synchronous local-only behavior for ordinary and non-rendered pages.
    pub(crate) fn reconstruct_local(
        &mut self,
        config: &docparse_config::FusionConfig,
        timings: &docparse_common::timing::Timings,
    ) {
        let _timer = self
            .blocks
            .iter()
            .any(|b| b.label == LayoutLabel::Table)
            .then(|| {
                timings
                    .for_page(self.extracted.page_number)
                    .start(docparse_common::timing::TimingStage::TableStructure)
            });
        let assembler = crate::table::TableAssembler::new(
            config,
            &self.extracted.table_evidence,
            &self.formula_regions,
        );
        for block in self
            .blocks
            .iter_mut()
            .filter(|block| block.label == LayoutLabel::Table)
        {
            if let Err(reason) = assembler.reconstruct(block) {
                tracing::warn!(
                    "table structure unavailable for {}: {}",
                    block.id.as_str(),
                    reason
                );
                self.warnings.push(PageWarning {
                    code: "TableStructureUnavailable".to_owned(),
                    stage: "table".to_owned(),
                    message: format!(
                        "table {} retains source lines: {}",
                        block.id.as_str(),
                        reason
                    ),
                });
            }
        }
    }
}

/// Pure page-local analyzer that shares only frozen configuration and context.
#[derive(Debug, Clone)]
pub(crate) struct PageAnalyzer {
    config: Arc<ValidatedConfig>,
    timings: docparse_common::timing::Timings,
}

impl PageAnalyzer {
    /// Creates an analyzer from immutable validated configuration.
    pub(crate) fn new(config: Arc<ValidatedConfig>) -> Self {
        Self {
            config,
            timings: docparse_common::timing::Timings::default(),
        }
    }

    /// Shares the per-parse timing sink without making observations part of page data.
    pub(crate) fn with_timings(
        mut self,
        timings: docparse_common::timing::Timings,
    ) -> Self {
        self.timings = timings;
        self
    }

    /// Prepares stable native/layout facts and suggested OCR missing regions.
    pub(crate) fn prepare(
        &self,
        extracted: ExtractedPage,
        detections: Vec<LayoutDetection>,
        context: Arc<DocumentContext>,
    ) -> Result<PageAnalysisDraft, PageAnalysisError> {
        let standalone_page_matches = context
            .metadata
            .get("standalone_page_number")
            .and_then(|value| value.parse::<u32>().ok())
            == Some(extracted.page_number);
        if extracted.page_number == 0
            || (extracted.page_number > context.page_count
                && !standalone_page_matches)
            || !extracted.width.is_finite()
            || extracted.width <= 0.0
            || !extracted.height.is_finite()
            || extracted.height <= 0.0
            || !matches!(extracted.rotation, 0 | 90 | 180 | 270)
        {
            return Err(PageAnalysisError::InvalidInput {
                reason:
                    "page number, dimensions, rotation, and context must agree"
                        .to_owned(),
            });
        }
        let page_bbox =
            Bbox::try_from([0.0, 0.0, extracted.width, extracted.height])
                .map_err(|error| PageAnalysisError::InvalidInput {
                    reason: error.to_string(),
                })?;
        // OCR planning owns mapping classification and region selection; page assembly consumes its facts.
        let crate::ocr::plan::OcrPlan {
            regions: missing_regions,
            native_text_coverage,
        } = crate::ocr::plan::OcrPlan::new(
            &extracted.text_items,
            &detections,
            page_bbox,
            &self.config,
        );
        tracing::debug!(
            "prepared page {} with {} native items, {} detections, and {} OCR regions",
            extracted.page_number,
            extracted.text_items.len(),
            detections.len(),
            missing_regions.len()
        );
        Ok(PageAnalysisDraft::builder()
            .extracted(extracted)
            .detections(detections)
            .context(context)
            .missing_regions(missing_regions)
            .native_text_coverage(native_text_coverage)
            .build())
    }

    /// Finishes one page through deduplication, fusion, semantics, formula, and order.
    pub(crate) fn finish(
        &self,
        draft: PageAnalysisDraft,
        ocr: OcrCompletion,
    ) -> Result<PageResult, PageAnalysisError> {
        let mut table_draft = self.compose(draft, ocr)?;
        table_draft.reconstruct_local(self.config.fusion(), &self.timings);
        self.complete(table_draft)
    }

    /// Completes canonical text ownership while retaining all evidence needed by an async table stage.
    pub(crate) fn compose(
        &self,
        draft: PageAnalysisDraft,
        mut ocr: OcrCompletion,
    ) -> Result<PageTableDraft, PageAnalysisError> {
        let PageAnalysisDraft {
            mut extracted,
            detections,
            context,
            missing_regions,
            native_text_coverage,
        } = draft;
        let page_bbox =
            Bbox::try_from([0.0, 0.0, extracted.width, extracted.height])
                .map_err(|error| PageAnalysisError::InvalidInput {
                    reason: error.to_string(),
                })?;
        let expected_native_ids: BTreeMap<_, usize> = extracted
            .text_items
            .iter()
            .filter(|item| item.source == TextSource::Native)
            .fold(BTreeMap::new(), |mut counts, item| {
                *counts.entry(item.id.as_str().to_owned()).or_default() += 1;
                counts
            });
        let mut warnings = Vec::new();
        // Remove watermarks before OCR ownership, model assignment, fallback XY-cut, and
        // formula attachment. Their original facts remain owned exactly once by detached blocks.
        let (watermark_items, mut text_items): (Vec<_>, Vec<_>) =
            std::mem::take(&mut extracted.text_items)
                .into_iter()
                .partition(|item| item.watermark.is_some());
        // Supply formula scopes before text grouping; InlineSpan annotation still
        // happens afterward, using the resulting canonical item order and ranges.
        let formula_regions: Vec<_> = detections
            .iter()
            .filter(|detection| {
                Self::valid_page_bbox(detection.bbox, page_bbox)
                    && detection.polygon.as_ref().is_none_or(|polygon| {
                        Self::valid_page_bbox(polygon.bbox(), page_bbox)
                    })
            })
            .filter_map(|detection| {
                crate::line::FormulaRegion::try_from(detection).ok()
            })
            .collect();
        let assembler = SemanticAssembler::new(
            extracted.page_number,
            page_bbox,
            self.config.fusion().clone(),
        )
        .with_evidence(&extracted.table_evidence.rules, &formula_regions);
        let watermarks = assembler.watermark_blocks(
            watermark_items,
            std::mem::take(&mut extracted.watermark_annotations),
            &extracted.watermark_evidence,
        )?;
        // OCR sees the original raster and may rediscover the same overlay. Deduplicate
        // complete watermark lines before any OCR text becomes eligible for body assignment.
        if let OcrCompletion::Succeeded(result) = &mut ocr {
            result.items.retain(|fact| {
                !watermarks.iter().any(|watermark| {
                    !watermark.text.is_empty()
                        && Self::equivalent_text(&watermark.text, &fact.text)
                        && watermark.bbox.intersection_area(fact.bbox)
                            / watermark
                                .bbox
                                .area()
                                .min(fact.bbox.area())
                                .max(f64::EPSILON)
                            >= 0.8
                })
            });
        }
        let replaced_native_text = crate::ocr::merge::OcrMerger::builder()
            .config(self.config.ocr())
            .page_number(extracted.page_number)
            .page_bbox(page_bbox)
            .missing_regions(&missing_regions)
            .build()
            .merge(&mut text_items, &mut warnings, ocr);

        let figure_matches =
            crate::figure::FigureCatalog::new(&extracted.embedded_images)
                .match_detections(&detections, page_bbox);
        let AssignmentResult {
            mut model_seeds,
            reference_seeds,
            residual,
            inline_formulas,
            diagnostics: assignment_diagnostics,
        } = AssignmentEngine::new(
            extracted.page_number,
            page_bbox,
            self.config.fusion().clone(),
        )
        .assign(text_items, detections)?;
        for seed in &mut model_seeds {
            if let Some(matched) =
                figure_matches.get(&seed.source_detection_index)
            {
                seed.embedded_image_index = Some(matched.image_index);
                seed.figure_bounds = Some(matched.bounds);
            }
        }
        let references: Vec<_> = reference_seeds
            .into_iter()
            .map(|seed| assembler.reference_block(seed))
            .collect();
        // Cut spatial regions before grouping residual spans into lines. Otherwise a
        // narrow column gutter can be mistaken for an inline gap and disappear in a
        // page-wide merged line. Each leaf reassembles only its own original facts.
        let obstacles: Vec<_> =
            model_seeds.iter().map(|seed| seed.bbox).collect();
        let fallback_tree = xy_cut(&residual, &obstacles, page_bbox);
        let mut blocks = Vec::new();
        for seed in model_seeds {
            let output = assembler.model_blocks(seed)?;
            blocks.extend(output.blocks);
            // Empty duplicate candidates may disappear during normalization. Report
            // missing content from the final owners, while retaining other diagnostics.
            warnings.extend(
                output
                    .warnings
                    .into_iter()
                    .filter(|warning| warning.code != "EmptyModelRegion"),
            );
        }
        let fallback = assembler.fallback_blocks(residual, &fallback_tree)?;
        blocks.extend(fallback.blocks);
        warnings.extend(fallback.warnings);
        let mut blocks = assembler.normalize_blocks(blocks)?;
        // Expand after merging so a larger image rectangle cannot swallow a neighbor.
        crate::figure::FigureCatalog::place(&mut blocks);
        Ok(PageTableDraft::builder()
            .blocks(blocks)
            .warnings(warnings)
            .extracted(extracted)
            .formula_regions(formula_regions)
            .context(context)
            .expected_native_ids(expected_native_ids)
            .replaced_native_text(replaced_native_text)
            .references(references)
            .watermarks(watermarks)
            .inline_formulas(inline_formulas)
            .assignment_diagnostics(assignment_diagnostics)
            .native_text_coverage(native_text_coverage)
            .build())
    }

    /// Finalizes ordering, formulas, and conservation only after every table attempt has completed.
    pub(crate) fn complete(
        &self,
        draft: PageTableDraft,
    ) -> Result<PageResult, PageAnalysisError> {
        let PageTableDraft {
            mut blocks,
            mut warnings,
            extracted,
            context,
            expected_native_ids,
            replaced_native_text,
            references,
            watermarks,
            inline_formulas,
            assignment_diagnostics,
            native_text_coverage,
            ..
        } = draft;
        warnings.extend(
            blocks
                .iter()
                .filter(|block| {
                    block.lines.is_empty()
                        && matches!(
                            crate::label_policy::LabelPolicy::from(
                                &block.label
                            ),
                            crate::label_policy::LabelPolicy::FlowText
                                | crate::label_policy::LabelPolicy::Title
                        )
                })
                .map(|block| PageWarning {
                    code: "EmptyModelRegion".to_owned(),
                    stage: "semantic".to_owned(),
                    message: format!(
                        "content layout {} contains no extracted text",
                        block.id.as_str()
                    ),
                }),
        );
        let formula_blocks = FormulaMatcher::new(extracted.page_number)
            .attach(&mut blocks, inline_formulas);
        // Unmatched inline detections have no text ownership. Keep a diagnostic rather
        // than introducing another empty content layout that can overlap nearby prose.
        warnings.extend(formula_blocks.into_iter().map(|block| PageWarning {
            code: "UnmatchedInlineFormula".to_owned(),
            stage: "formula".to_owned(),
            message: format!(
                "inline formula region {} has no corresponding content line",
                block.id.as_str()
            ),
        }));
        let mut ordered = order_blocks(blocks)?;
        // Label already ordered content without letting reference envelopes influence
        // ownership, grouping, or reading order. Raw model labels remain in provenance.
        for block in &mut ordered.blocks {
            if block.label == LayoutLabel::Text
                && references.iter().any(|reference| {
                    block.bbox.intersection_area(reference.bbox)
                        >= block.bbox.area() * 0.5
                })
            {
                block.label = LayoutLabel::ReferenceContent;
                block.label_source = crate::LabelSource::Heuristic;
                block.text =
                    crate::Block::derive_text(&block.label, &block.lines);
            }
        }
        if !references.is_empty() {
            tracing::debug!(
                "kept {} empty reference annotations outside body layout on page {}",
                references.len(),
                extracted.page_number
            );
        }
        for mut reference in references {
            reference.final_order = ordered.blocks.len() as u32;
            ordered.blocks.push(reference);
        }
        if !watermarks.is_empty() {
            tracing::debug!(
                "detached {} watermark blocks from body layout on page {}",
                watermarks.len(),
                extracted.page_number
            );
        }
        // final_order is the serialization position; detached watermarks never enter the body graph.
        for mut block in watermarks {
            block.final_order = ordered.blocks.len() as u32;
            ordered.blocks.push(block);
        }

        let mut diagnostics = BTreeMap::new();
        for (index, message) in assignment_diagnostics.into_iter().enumerate() {
            diagnostics.insert(format!("assignment.{index}"), message);
        }
        for (index, edge) in ordered.removed_edges.iter().enumerate() {
            diagnostics.insert(
                format!("order.removed_edge.{index}"),
                format!(
                    "{}>{} source={:?} weight={} reason={}",
                    edge.from.as_str(),
                    edge.to.as_str(),
                    edge.source,
                    edge.preservation_weight,
                    edge.reason
                ),
            );
        }
        diagnostics.insert(
            "native_text_coverage".to_owned(),
            native_text_coverage.to_string(),
        );
        if let Some(model_revision) = &context.model_revision {
            diagnostics
                .insert("model_revision".to_owned(), model_revision.clone());
        }
        // Preserve partial intersections instead of expanding their union across unrelated
        // content. One page warning summarizes the pairs; diagnostics retain their identities.
        let mut overlap_count = 0;
        for (index, block) in ordered
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| !block.is_detached())
        {
            for other in ordered
                .blocks
                .iter()
                .skip(index + 1)
                .filter(|block| !block.is_detached())
            {
                let width = block.bbox.right.min(other.bbox.right)
                    - block.bbox.left.max(other.bbox.left);
                let height = block.bbox.bottom.min(other.bbox.bottom)
                    - block.bbox.top.max(other.bbox.top);
                if width > 1e-6 && height > 1e-6 {
                    diagnostics.insert(
                        format!("content.overlap.{overlap_count}"),
                        format!(
                            "{} overlaps {} with IoU {:.6}",
                            block.id.as_str(),
                            other.id.as_str(),
                            block.bbox.iou(other.bbox)
                        ),
                    );
                    overlap_count += 1;
                }
            }
        }
        if overlap_count > 0 {
            tracing::warn!(
                "page {} retained {} partially overlapping content layout pairs; see content.overlap diagnostics",
                extracted.page_number,
                overlap_count
            );
            warnings.push(PageWarning {
                code: "ContentLayoutOverlap".to_owned(),
                stage: "semantic".to_owned(),
                message: format!("retained {overlap_count} partially overlapping content layout pairs; see content.overlap diagnostics"),
            });
        }
        warnings.sort_by(|left, right| {
            left.stage
                .cmp(&right.stage)
                .then_with(|| left.code.cmp(&right.code))
                .then_with(|| left.message.cmp(&right.message))
        });
        let page = PageResult::builder()
            .page_number(extracted.page_number)
            .width(extracted.width)
            .height(extracted.height)
            .rotation(extracted.rotation)
            .blocks(ordered.blocks)
            .replaced_native_text(replaced_native_text)
            .warnings(warnings)
            .diagnostics(diagnostics)
            .build();
        let actual_native_ids: BTreeMap<_, usize> = page
            .iter_text_items()
            .chain(page.replaced_native_text.iter())
            .filter(|item| item.source == TextSource::Native)
            .fold(BTreeMap::new(), |mut counts, item| {
                *counts.entry(item.id.as_str().to_owned()).or_default() += 1;
                counts
            });
        if actual_native_ids != expected_native_ids {
            return Err(PageAnalysisError::NativeTextConservation {
                page_number: page.page_number,
            });
        }
        ResultValidator::validate_page(&page)?;
        tracing::debug!(
            "completed page {} with {} blocks and {} warnings",
            page.page_number,
            page.blocks.len(),
            page.warnings.len()
        );
        Ok(page)
    }

    /// Returns whether a candidate box is finite, positive, and contained by the page.
    fn valid_page_bbox(candidate: Bbox, page: Bbox) -> bool {
        [
            candidate.left,
            candidate.top,
            candidate.right,
            candidate.bottom,
        ]
        .iter()
        .all(|value| value.is_finite())
            && candidate.right > candidate.left
            && candidate.bottom > candidate.top
            && candidate.left >= page.left
            && candidate.top >= page.top
            && candidate.right <= page.right
            && candidate.bottom <= page.bottom
    }

    /// Compares text after stable case and whitespace normalization for native deduplication.
    fn equivalent_text(left: &str, right: &str) -> bool {
        let normalize = |value: &str| {
            value.split_whitespace().collect::<String>().to_lowercase()
        };
        normalize(left) == normalize(right)
    }
}

#[cfg(test)]
mod tests {
    /// Disabled OCR keeps coverage diagnostics but performs no missing-region work.
    #[test]
    fn disabled_ocr_has_no_missing_region_plan() {
        let page = extracted();
        let draft = PageAnalyzer::new(config())
            .prepare(
                page.clone(),
                vec![detection(0, [0.0, 80.0, 100.0, 100.0])],
                context(&page),
            )
            .expect("prepare");
        assert!(draft.missing_regions.is_empty());
        assert!(draft.native_text_coverage > 0.0);
    }

    /// Sparse native text cannot hide a scan when layout misses its image region.
    #[test]
    fn sparse_native_page_requests_full_page_ocr() {
        let page = extracted();
        // This policy test must explicitly enable OCR; disabled planning deliberately does no region work.
        let mut raw = RawConfig::default();
        raw.ocr.policy = docparse_config::OcrPolicy::MissingRegions;
        let draft = PageAnalyzer::new(Arc::new(
            ValidatedConfig::try_from(raw).expect("config"),
        ))
        .prepare(page.clone(), vec![], context(&page))
        .expect("prepare");
        assert_eq!(
            draft.missing_regions,
            [Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("page")]
        );
    }

    /// Sideways OCR lines must follow their cross-line axis rather than noisy page-top coordinates.
    #[test]
    fn sideways_ocr_paragraph_keeps_line_order() {
        let mut page = extracted();
        page.text_items.clear();
        let mut raw = RawConfig::default();
        raw.ocr.policy = docparse_config::OcrPolicy::Always;
        let analyzer = PageAnalyzer::new(Arc::new(
            ValidatedConfig::try_from(raw).expect("config"),
        ));
        let draft = analyzer
            .prepare(
                page.clone(),
                vec![detection(0, [0.0, 0.0, 100.0, 100.0])],
                context(&page),
            )
            .expect("prepare");
        let items = [0, 1, 2]
            .into_iter()
            .map(|index| {
                let x = 10.0 + f64::from(index) * 15.0;
                let y = 10.0 - f64::from(index);
                let polygon = docparse_layout::Polygon::try_from(vec![
                    docparse_layout::Point::new(x, 70.0),
                    docparse_layout::Point::new(x, y),
                    docparse_layout::Point::new(x + 8.0, y),
                    docparse_layout::Point::new(x + 8.0, 70.0),
                ])
                .expect("rotated polygon");
                crate::OcrTextItem::builder()
                    .text(format!("Line {}", index + 1))
                    .bbox(polygon.bbox())
                    .polygon(Some(polygon))
                    .confidence(0.95)
                    .build()
            })
            .collect();
        let result = analyzer
            .finish(
                draft,
                OcrCompletion::Succeeded(
                    crate::OcrResult::builder().items(items).build(),
                ),
            )
            .expect("finish");
        assert_eq!(
            result
                .iter_text_items()
                .map(|item| item.raw_text.as_str())
                .collect::<Vec<_>>(),
            ["Line 1", "Line 2", "Line 3"]
        );
        assert!(
            result
                .iter_lines()
                .all(|line| (line.rotation - 270.0).abs() < 1e-6)
        );
    }

    /// Only a successful, confident OCR replacement removes unusable native text from reading order.
    #[test]
    fn ocr_replacement_archives_native_and_failure_preserves_it() {
        let mut page = extracted();
        let original = page.text_items.first_mut().expect("native item");
        original.raw_text = "\u{fffd}\u{fffd}".into();
        let original = original.clone();
        let mut raw = RawConfig::default();
        raw.ocr.policy = docparse_config::OcrPolicy::MissingRegions;
        let analyzer = PageAnalyzer::new(Arc::new(
            ValidatedConfig::try_from(raw).expect("config"),
        ));
        let draft = analyzer
            .prepare(page.clone(), vec![], context(&page))
            .expect("prepare");
        assert!(
            draft
                .missing_regions
                .iter()
                .any(|region| region.contains_bbox(original.bbox))
        );
        let failed = analyzer
            .finish(
                draft.clone(),
                OcrCompletion::Failed("test engine failure".into()),
            )
            .expect("failure result");
        assert!(failed.replaced_native_text.is_empty());
        assert!(
            failed
                .iter_text_items()
                .any(|item| item.raw_text == original.raw_text)
        );
        let result = crate::OcrResult::builder()
            .items(vec![
                crate::OcrTextItem::builder()
                    .text("recovered".into())
                    .bbox(original.bbox)
                    .confidence(0.95)
                    .build(),
            ])
            .build();
        let recovered = analyzer
            .finish(draft, OcrCompletion::Succeeded(result))
            .expect("replacement result");
        assert_eq!(recovered.replaced_native_text, [original]);
        assert!(
            recovered
                .iter_text_items()
                .any(|item| item.raw_text == "recovered"
                    && item.source == TextSource::Ocr)
        );
        assert!(!recovered.iter_text_items().any(TextItem::needs_ocr));
        let json = serde_json::to_string(&recovered).expect("serialize");
        assert_eq!(
            serde_json::from_str::<crate::PageResult>(&json)
                .expect("deserialize"),
            recovered
        );
    }

    /// Healthy native text wins over overlapping OCR even when recognition differs by a character.
    #[test]
    fn ocr_enrichment_preserves_native_and_adjacent_ocr_lines() {
        let mut raw = docparse_config::RawConfig::default();
        raw.ocr.policy = docparse_config::OcrPolicy::MissingRegions;
        let analyzer = super::PageAnalyzer::new(std::sync::Arc::new(
            docparse_config::ValidatedConfig::try_from(raw).expect("config"),
        ));
        let page = docparse_layout::Bbox::try_from([0.0, 0.0, 100.0, 100.0])
            .expect("page");
        let mut items = vec![
            crate::TextItem::builder()
                .id(crate::TextItemId::native(1, 0))
                .raw_text("native text".into())
                .bbox(
                    docparse_layout::Bbox::try_from([10.0, 10.0, 60.0, 20.0])
                        .expect("box"),
                )
                .source(crate::TextSource::Native)
                .build(),
        ];
        let native = items.first().expect("native").clone();
        let result = crate::OcrResult::builder()
            .items(
                [
                    ("native texf", [10.0, 10.0, 60.0, 20.0]),
                    ("first new", [10.0, 30.0, 60.0, 40.0]),
                    ("second new", [10.0, 39.0, 60.0, 49.0]),
                ]
                .into_iter()
                .map(|(text, bounds)| {
                    crate::OcrTextItem::builder()
                        .text(text.into())
                        .bbox(
                            docparse_layout::Bbox::try_from(bounds)
                                .expect("box"),
                        )
                        .confidence(0.95)
                        .build()
                })
                .collect(),
            )
            .build();
        crate::ocr::merge::OcrMerger::builder()
            .config(analyzer.config.ocr())
            .page_number(1)
            .page_bbox(page)
            .missing_regions(&[page])
            .build()
            .merge(
                &mut items,
                &mut Vec::new(),
                super::OcrCompletion::Succeeded(result),
            );
        assert_eq!(items.len(), 3);
        assert_eq!(items.first(), Some(&native));
        assert_eq!(
            items
                .iter()
                .filter(|item| item.source == crate::TextSource::Ocr)
                .map(|item| item.raw_text.as_str())
                .collect::<Vec<_>>(),
            ["first new", "second new"]
        );
    }

    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use docparse_config::{RawConfig, ValidatedConfig};
    use docparse_layout::{Bbox, GeometrySource, LayoutDetection, LayoutLabel};

    use super::{OcrCompletion, PageAnalyzer};
    use crate::{
        DocumentContextBuilder, ExtractedPage, PageProbe, TextItem, TextItemId,
        TextSource,
    };

    /// Builds validated configuration without reading model artifacts.
    fn config() -> Arc<ValidatedConfig> {
        let mut raw = RawConfig::default();
        raw.layout.model_path = PathBuf::from("/tmp/docparse-test-model.onnx");
        raw.layout.model_config_path =
            PathBuf::from("/tmp/docparse-test-model.yml");
        raw.layout.model_manifest_path =
            PathBuf::from("/tmp/docparse-test-model.json");
        Arc::new(
            ValidatedConfig::try_from(raw).expect("test config must validate"),
        )
    }

    /// Builds one extracted page containing three stable native facts.
    fn extracted() -> ExtractedPage {
        let items = [
            (0, "left", [10.0, 10.0, 40.0, 20.0]),
            (1, "right", [60.0, 10.0, 90.0, 20.0]),
            (2, "lower", [10.0, 60.0, 50.0, 70.0]),
        ]
        .into_iter()
        .map(|(index, text, bounds)| {
            TextItem::builder()
                .id(TextItemId::native(1, index))
                .raw_text(text.to_owned())
                .bbox(Bbox::try_from(bounds).expect("test bbox must be valid"))
                .source(TextSource::Native)
                .extraction_order(index)
                .build()
        })
        .collect();
        ExtractedPage::builder()
            .page_number(1)
            .width(100.0)
            .height(100.0)
            .rotation(0)
            .text_items(items)
            .build()
    }

    /// Repeated narrow gutters must survive fallback line assembly.
    #[test]
    fn narrow_columns_remain_separate_without_layout_detections() {
        let mut page = extracted();
        page.width = 612.0;
        page.height = 792.0;
        page.text_items = (0..6)
            .flat_map(|row| {
                [false, true].map(move |right| {
                    let (left, end) =
                        if right { (312.0, 564.0) } else { (48.0, 300.0) };
                    TextItem::builder()
                        .id(TextItemId::native(1, row * 2 + u32::from(right)))
                        .raw_text(format!(
                            "{} row {row}",
                            if right { "RIGHT" } else { "LEFT" }
                        ))
                        .bbox(
                            Bbox::try_from([
                                left,
                                50.0 + f64::from(row) * 12.0,
                                end,
                                59.0 + f64::from(row) * 12.0,
                            ])
                            .expect("column line"),
                        )
                        .source(TextSource::Native)
                        .style(Some(
                            crate::TextStyle::builder()
                                .font_size(Some(10.0))
                                .build(),
                        ))
                        .extraction_order(row * 2 + u32::from(right))
                        .build()
                })
            })
            .collect();
        let analyzer = PageAnalyzer::new(config());
        let draft = analyzer
            .prepare(page.clone(), Vec::new(), context(&page))
            .expect("prepare");
        let result = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("fallback analysis");
        assert!(
            result
                .blocks
                .iter()
                .all(|block| block.bbox.right <= 300.0
                    || block.bbox.left >= 312.0),
            "fallback must not bridge a 12-point column gutter"
        );
        assert_eq!(result.iter_text_items().count(), 12);
        assert_eq!(result.blocks.len(), 2);
        assert!(
            result
                .blocks
                .first()
                .expect("left column")
                .text
                .starts_with("LEFT")
        );
        assert!(
            result
                .blocks
                .last()
                .expect("right column")
                .text
                .starts_with("RIGHT")
        );
        // The same aligned spans with a normal word-sized gap must remain inline.
        for item in page
            .text_items
            .iter_mut()
            .filter(|item| item.extraction_order % 2 == 1)
        {
            item.bbox.left -= 8.0;
            item.bbox.right -= 8.0;
        }
        let draft = analyzer
            .prepare(page.clone(), Vec::new(), context(&page))
            .expect("inline preparation");
        let inline = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("inline fallback");
        assert_eq!(
            inline.blocks.len(),
            1,
            "ordinary inline gaps are not column gutters"
        );
        assert_eq!(inline.blocks.first().expect("paragraph").lines.len(), 6);
    }

    /// Builds one valid text detection with caller-selected source geometry.
    fn detection(index: u32, bounds: [f64; 4]) -> LayoutDetection {
        LayoutDetection::builder()
            .source_detection_index(index)
            .raw_label("text".to_owned())
            .class_id(22)
            .label(LayoutLabel::Text)
            .confidence(0.9)
            .bbox(
                Bbox::try_from(bounds)
                    .expect("test detection bbox must be valid"),
            )
            .polygon(None)
            .geometry_source(GeometrySource::DerivedFromBbox)
            .model_order(i64::from(index))
            .metadata(BTreeMap::new())
            .build()
    }

    /// Freezes one-page document context for isolated page analysis.
    fn context(page: &ExtractedPage) -> Arc<crate::DocumentContext> {
        let mut builder = DocumentContextBuilder::new(1);
        builder
            .push_page(PageProbe::from(page))
            .expect("test probe must be valid");
        builder.build().expect("test context must build")
    }

    /// Inline formula detections must constrain script ownership before ordinary prose grouping.
    #[test]
    fn inline_formula_region_keeps_its_script_out_of_neighboring_prose() {
        let mut page = extracted();
        page.text_items = [
            (0, "before ", [0.0, 10.0, 20.0, 20.0], 13.0, 18.0),
            (1, "x", [20.0, 10.0, 30.0, 20.0], 10.0, 18.0),
            (2, "2", [20.0, 16.0, 24.0, 23.0], 7.0, 22.0),
            (3, " after", [40.0, 10.0, 70.0, 20.0], 13.0, 18.0),
        ]
        .into_iter()
        .map(|(index, text, bounds, size, y)| {
            TextItem::builder()
                .id(TextItemId::native(1, index))
                .raw_text(text.to_owned())
                .bbox(Bbox::try_from(bounds).expect("source"))
                .baseline(Some(crate::Baseline {
                    start: docparse_layout::Point::new(bounds[0], y),
                    end: docparse_layout::Point::new(bounds[2], y),
                }))
                .style(Some(
                    crate::TextStyle::builder().font_size(Some(size)).build(),
                ))
                .source(TextSource::Native)
                .build()
        })
        .collect();
        let mut formula = detection(1, [20.0, 9.0, 30.0, 24.0]);
        formula.label = LayoutLabel::InlineFormula;
        let analyzer = PageAnalyzer::new(config());
        for reverse in [false, true] {
            let mut source = page.clone();
            if reverse {
                source.text_items.reverse();
            }
            let draft = analyzer
                .prepare(
                    source,
                    vec![detection(0, [0.0, 5.0, 90.0, 30.0]), formula.clone()],
                    context(&page),
                )
                .expect("prepare");
            let result = analyzer
                .finish(draft, OcrCompletion::NotRequested)
                .expect("formula page");
            assert!(
                result
                    .blocks
                    .iter()
                    .any(|block| block.text == "before x2 after"),
                "{:?}",
                result
                    .blocks
                    .iter()
                    .map(|block| &block.text)
                    .collect::<Vec<_>>()
            );
            let span = result
                .iter_lines()
                .flat_map(|line| &line.inline_spans)
                .next()
                .expect("formula span");
            assert_eq!(span.extracted_text.as_deref(), Some("x2"));
            assert_eq!(result.iter_text_items().count(), 4);
        }
    }

    /// Clipping a nested index must not move it past the formula's following operands.
    #[test]
    fn clipped_formula_scope_preserves_nested_script_ownership() {
        let mut page = extracted();
        page.text_items = [
            (0, "q", [20.0, 50.0, 26.0, 60.0], 10.0, 58.0),
            (1, "τ", [26.0, 56.0, 31.0, 63.0], 7.0, 62.0),
            (2, "1", [31.0, 61.0, 34.0, 66.0], 5.0, 65.0),
            (3, "=", [40.0, 50.0, 46.0, 60.0], 10.0, 58.0),
            (4, "x", [52.0, 50.0, 58.0, 60.0], 10.0, 58.0),
        ]
        .into_iter()
        .map(|(index, text, bounds, size, y)| {
            TextItem::builder()
                .id(TextItemId::native(1, index))
                .raw_text(text.to_owned())
                .bbox(Bbox::try_from(bounds).expect("source"))
                .baseline(Some(crate::Baseline {
                    start: docparse_layout::Point::new(bounds[0], y),
                    end: docparse_layout::Point::new(bounds[2], y),
                }))
                .style(Some(
                    crate::TextStyle::builder().font_size(Some(size)).build(),
                ))
                .source(TextSource::Native)
                .build()
        })
        .collect();
        let analyzer = PageAnalyzer::new(config());
        for label in [LayoutLabel::InlineFormula, LayoutLabel::DisplayFormula] {
            for bottom in [67.0, 63.0, 58.0] {
                for reverse in [false, true] {
                    let mut source = page.clone();
                    if reverse {
                        source.text_items.reverse();
                    }
                    let mut formula = detection(1, [19.0, 49.0, 60.0, bottom]);
                    formula.class_id = if label == LayoutLabel::InlineFormula {
                        15
                    } else {
                        5
                    };
                    formula.label = label.clone();
                    // Inline math belongs to prose; standalone display math owns its row.
                    let mut detections = vec![formula];
                    if label == LayoutLabel::InlineFormula {
                        detections.push(detection(0, [10.0, 30.0, 90.0, 90.0]));
                    }
                    let draft = analyzer
                        .prepare(source, detections, context(&page))
                        .expect("prepare");
                    let result = analyzer
                        .finish(draft, OcrCompletion::NotRequested)
                        .expect("formula page");
                    assert_eq!(
                        result
                            .iter_lines()
                            .map(|line| line.text.as_str())
                            .collect::<Vec<_>>(),
                        ["qτ1=x"],
                        "{label:?}, bottom {bottom}, reverse {reverse}",
                    );
                    assert_eq!(
                        result.iter_text_items().count(),
                        page.text_items.len()
                    );
                    for original in &page.text_items {
                        let mut actual = result
                            .iter_text_items()
                            .find(|item| item.id == original.id)
                            .expect("original glyph")
                            .clone();
                        // The parser may assign order, but it must not change the source facts.
                        actual.final_order = original.final_order;
                        assert_eq!(&actual, original);
                    }
                }
            }
        }
    }

    /// Confirmed display formulas can order a standalone fraction without an external prose anchor.
    #[test]
    fn display_formula_region_orders_an_unanchored_fraction() {
        let mut page = extracted();
        page.text_items = [(0, "a", 10.0), (1, "b", 24.0)]
            .into_iter()
            .map(|(index, text, top)| {
                TextItem::builder()
                    .id(TextItemId::native(1, index))
                    .raw_text(text.to_owned())
                    .bbox(
                        Bbox::try_from([20.0, top, 26.0, top + 7.0])
                            .expect("fraction"),
                    )
                    .baseline(Some(crate::Baseline {
                        start: docparse_layout::Point::new(20.0, top + 6.0),
                        end: docparse_layout::Point::new(26.0, top + 6.0),
                    }))
                    .style(Some(
                        crate::TextStyle::builder()
                            .font_size(Some(8.0))
                            .build(),
                    ))
                    .source(TextSource::Native)
                    .build()
            })
            .collect();
        page.table_evidence
            .rules
            .push(crate::TableRule::Horizontal {
                y: 20.0,
                left: 19.0,
                right: 27.0,
            });
        let mut formula = detection(0, [18.0, 8.0, 28.0, 33.0]);
        formula.label = LayoutLabel::DisplayFormula;
        let analyzer = PageAnalyzer::new(config());
        let draft = analyzer
            .prepare(page.clone(), vec![formula], context(&page))
            .expect("prepare");
        let result = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("fraction page");
        let block = result
            .blocks
            .iter()
            .find(|block| block.label == LayoutLabel::DisplayFormula)
            .expect("display formula");
        assert_eq!(block.text, "ab");
        assert_eq!(block.lines.len(), 1);
        assert_eq!(result.iter_text_items().count(), 2);
    }

    /// Reference envelopes must not steal partially covered text from bibliography entries.
    #[test]
    fn reference_is_visual_only_and_children_own_all_text() {
        let page = extracted();
        let mut parent = detection(0, [0.0, 0.0, 100.0, 100.0]);
        parent.label = LayoutLabel::Reference;
        parent.raw_label = "reference".to_owned();
        parent.class_id = 18;
        parent.confidence = 0.99;
        let bounds = parent.bbox;
        let mut children = vec![
            detection(1, [5.0, 5.0, 85.0, 25.0]),
            detection(2, [5.0, 55.0, 55.0, 75.0]),
        ];
        for child in &mut children {
            child.label = LayoutLabel::ReferenceContent;
            child.raw_label = "reference_content".to_owned();
            child.class_id = 19;
        }
        let analyzer = PageAnalyzer::new(config());
        let draft = analyzer
            .prepare(
                page.clone(),
                [vec![parent], children].concat(),
                context(&page),
            )
            .expect("prepare bibliography");
        let result = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("bibliography");
        let reference = result
            .blocks
            .iter()
            .find(|block| block.label == LayoutLabel::Reference)
            .expect("visual reference");
        assert!(reference.text.is_empty() && reference.lines.is_empty());
        assert_eq!(reference.bbox, bounds);
        let entries: Vec<_> = result
            .blocks
            .iter()
            .filter(|block| block.label == LayoutLabel::ReferenceContent)
            .collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries
                .iter()
                .flat_map(|block| &block.lines)
                .flat_map(|line| &line.text_items)
                .count(),
            3
        );
        assert!(
            entries.iter().any(|block| block.text.contains("right")),
            "The partially covered trailing text must remain in its entry"
        );
        assert!(
            entries
                .iter()
                .all(|block| block.final_order < reference.final_order)
        );
    }

    /// Missing child detections must preserve bibliography text without filling the visual envelope.
    #[test]
    fn reference_without_children_keeps_fallback_content_separate() {
        let page = extracted();
        let mut parent = detection(0, [0.0, 0.0, 100.0, 100.0]);
        parent.label = LayoutLabel::Reference;
        parent.raw_label = "reference".to_owned();
        parent.class_id = 18;
        let analyzer = PageAnalyzer::new(config());
        let draft = analyzer
            .prepare(page.clone(), vec![parent], context(&page))
            .expect("prepare");
        let result = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("fallback bibliography");
        assert_eq!(result.iter_text_items().count(), 3);
        assert!(
            result
                .blocks
                .iter()
                .filter(|block| !block.lines.is_empty())
                .all(|block| block.label == LayoutLabel::ReferenceContent)
        );
        let reference = result
            .blocks
            .iter()
            .find(|block| block.label == LayoutLabel::Reference)
            .expect("reference");
        assert!(reference.text.is_empty() && reference.lines.is_empty());
    }

    /// A visual reference may change semantic labels but never content merge geometry.
    #[test]
    fn reference_cannot_change_content_layout_geometry() {
        let mut page = extracted();
        page.text_items.truncate(2);
        for (item, bounds) in page
            .text_items
            .iter_mut()
            .zip([[10.0, 10.0, 60.0, 20.0], [10.0, 24.0, 60.0, 34.0]])
        {
            item.bbox = Bbox::try_from(bounds).expect("adjacent text");
        }
        let body = vec![
            detection(0, [5.0, 5.0, 65.0, 22.0]),
            detection(1, [5.0, 23.0, 65.0, 40.0]),
        ];
        let analyzer = PageAnalyzer::new(config());
        let analyze = |detections| {
            let draft = analyzer
                .prepare(page.clone(), detections, context(&page))
                .expect("prepare");
            analyzer
                .finish(draft, OcrCompletion::NotRequested)
                .expect("layout")
        };
        let before = analyze(body.clone());
        let mut reference = detection(10, [0.0, 0.0, 100.0, 100.0]);
        reference.label = LayoutLabel::Reference;
        reference.raw_label = "reference".to_owned();
        reference.class_id = 18;
        let after = analyze([body, vec![reference]].concat());
        let bounds = |page: &crate::PageResult| {
            page.blocks
                .iter()
                .filter(|block| !block.is_detached())
                .map(|block| block.bbox)
                .collect::<Vec<_>>()
        };
        assert_eq!(bounds(&before), bounds(&after));
        assert_eq!(after.iter_text_items().count(), 2);
    }

    /// Equivalent model hypotheses must not leave a second empty overlapping content box.
    #[test]
    fn content_layout_collapses_duplicate_model_regions() {
        let page = extracted();
        let analyzer = PageAnalyzer::new(config());
        let draft = analyzer
            .prepare(
                page.clone(),
                vec![
                    detection(0, [5.0, 5.0, 45.0, 25.0]),
                    detection(1, [6.0, 6.0, 44.0, 24.0]),
                ],
                context(&page),
            )
            .expect("prepare duplicate regions");
        let result = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("normalize duplicates");
        assert_eq!(
            result
                .blocks
                .iter()
                .filter(|block| block.bbox.top < 25.0 && block.bbox.left < 45.0)
                .count(),
            1
        );
        assert_eq!(result.iter_text_items().count(), 3);
        let merged = result
            .blocks
            .iter()
            .find(|block| block.bbox.top < 25.0 && block.bbox.left < 45.0)
            .expect("merged candidate");
        assert_eq!(
            merged.source_regions.len(),
            2,
            "Both model hypotheses must remain inspectable"
        );
        assert!(
            !result
                .warnings
                .iter()
                .any(|warning| warning.code == "EmptyModelRegion"),
            "Absorbed duplicate candidates must not leave stale empty-content warnings"
        );
    }

    /// Partial crossings stay separate and report diagnostics without swallowing a third block.
    #[test]
    fn content_layout_preserves_partial_crossings_with_warning() {
        let mut page = extracted();
        page.text_items = [
            (0, "horizontal", [5.0, 5.0, 35.0, 10.0]),
            (1, "vertical", [5.0, 5.0, 10.0, 35.0]),
            (2, "inside union", [25.0, 25.0, 30.0, 30.0]),
        ]
        .into_iter()
        .map(|(index, text, bounds)| {
            TextItem::builder()
                .id(TextItemId::native(1, index))
                .raw_text(text.to_owned())
                .bbox(Bbox::try_from(bounds).expect("content geometry"))
                .source(TextSource::Native)
                .extraction_order(index)
                .build()
        })
        .collect();
        let analyzer = PageAnalyzer::new(config());
        let draft = analyzer
            .prepare(
                page.clone(),
                vec![
                    detection(0, [4.0, 4.0, 36.0, 11.0]),
                    detection(1, [4.0, 4.0, 11.0, 36.0]),
                    detection(2, [24.0, 24.0, 31.0, 31.0]),
                ],
                context(&page),
            )
            .expect("prepare crossing layouts");
        let result = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("partial overlaps are non-fatal");
        assert_eq!(result.blocks.len(), 3);
        assert_eq!(result.iter_text_items().count(), 3);
        assert!(
            result
                .blocks
                .iter()
                .all(|block| block.source_regions.is_empty())
        );
        assert_eq!(
            result
                .warnings
                .iter()
                .filter(|warning| warning.code == "ContentLayoutOverlap")
                .count(),
            1
        );
        assert_eq!(
            result
                .diagnostics
                .keys()
                .filter(|key| key.starts_with("content.overlap."))
                .count(),
            1
        );
    }

    /// Close but independently detected paragraphs must not lose their established boundaries.
    #[test]
    fn content_layout_preserves_separate_model_paragraphs() {
        let mut page = extracted();
        page.text_items.truncate(2);
        for (item, (text, bounds)) in page.text_items.iter_mut().zip([
            ("Previous paragraph.", [10.0, 10.0, 60.0, 20.0]),
            ("• New paragraph.", [10.0, 24.0, 60.0, 34.0]),
        ]) {
            item.raw_text = text.to_owned();
            item.bbox = Bbox::try_from(bounds).expect("paragraph bounds");
        }
        let analyzer = PageAnalyzer::new(config());
        let draft = analyzer
            .prepare(
                page.clone(),
                vec![
                    detection(0, [5.0, 5.0, 65.0, 22.0]),
                    detection(1, [5.0, 23.0, 65.0, 40.0]),
                ],
                context(&page),
            )
            .expect("prepare paragraphs");
        let result = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("paragraph layouts");
        assert_eq!(
            result.blocks.len(),
            2,
            "A gap alone must not erase two non-overlapping model paragraphs"
        );
    }

    /// A script joins its parent line while the next paragraph keeps its independent layout.
    #[test]
    fn content_layout_attaches_script_without_merging_next_line() {
        let mut page = extracted();
        page.text_items = [
            (0, "body", [10.0, 10.0, 60.0, 20.0], 10.0),
            (1, "2", [32.0, 16.0, 36.0, 23.0], 7.0),
            (2, "continuation", [10.0, 24.0, 60.0, 34.0], 10.0),
        ]
        .into_iter()
        .map(|(index, text, bounds, size)| {
            TextItem::builder()
                .id(TextItemId::native(1, index))
                .raw_text(text.to_owned())
                .bbox(Bbox::try_from(bounds).expect("math bounds"))
                .source(TextSource::Native)
                .style(Some(
                    crate::TextStyle::builder().font_size(Some(size)).build(),
                ))
                .extraction_order(index)
                .build()
        })
        .collect();
        let analyzer = PageAnalyzer::new(config());
        let analyze = |page: ExtractedPage| {
            let draft = analyzer
                .prepare(
                    page.clone(),
                    vec![detection(0, [10.0, 10.0, 60.0, 16.0])],
                    context(&page),
                )
                .expect("prepare math");
            analyzer
                .finish(draft, OcrCompletion::NotRequested)
                .expect("math layout")
        };
        let forward = analyze(page.clone());
        page.text_items.reverse();
        let reverse = analyze(page);
        assert_eq!(
            forward.blocks.len(),
            2,
            "Only the script should join its parent"
        );
        assert_eq!(forward.iter_text_items().count(), 3);
        assert_eq!(forward.blocks, reverse.blocks);
        assert_eq!(
            forward.blocks.first().expect("merged block").bbox,
            Bbox::try_from([10.0, 10.0, 60.0, 23.0])
                .expect("parent and script bounds")
        );
        let parent = forward.blocks.first().expect("parent");
        assert_eq!(parent.lines.len(), 1);
        assert_eq!(
            parent
                .lines
                .first()
                .expect("parent line")
                .text_items
                .iter()
                .map(|item| item.id.clone())
                .collect::<Vec<_>>(),
            vec![TextItemId::native(1, 0), TextItemId::native(1, 1)]
        );
        assert!(
            !forward
                .warnings
                .iter()
                .any(|warning| warning.code == "ContentLayoutOverlap")
        );
    }

    /// Confirmed watermarks cannot affect the ordinary model, fallback, or order results.
    #[test]
    fn confirmed_watermark_cannot_change_body_fusion() {
        let original = extracted();
        let mut marked = original.clone();
        marked.text_items.push(
            TextItem::builder()
                .id(TextItemId::native(1, 3))
                .raw_text("DRAFT".to_owned())
                .bbox(
                    Bbox::try_from([0.0, 0.0, 100.0, 100.0])
                        .expect("watermark bounds"),
                )
                .source(TextSource::Native)
                .watermark(Some(crate::WatermarkSource::PdfMarkedContent))
                .extraction_order(3)
                .build(),
        );
        let analyzer = PageAnalyzer::new(config());
        let analyze = |page: ExtractedPage| {
            let draft = analyzer
                .prepare(
                    page.clone(),
                    vec![detection(0, [5.0, 5.0, 50.0, 30.0])],
                    context(&page),
                )
                .expect("prepare");
            analyzer
                .finish(draft, OcrCompletion::NotRequested)
                .expect("finish")
        };
        let before = analyze(original);
        let after = analyze(marked);
        let body: Vec<_> = after
            .blocks
            .iter()
            .filter(|block| block.label != LayoutLabel::Watermark)
            .cloned()
            .collect();
        assert_eq!(
            before.blocks, body,
            "watermark must not affect model ownership, XY-cut, or body order"
        );
        let watermark = after.blocks.last().expect("independent watermark");
        assert_eq!(watermark.label, LayoutLabel::Watermark);
        assert_eq!(watermark.text, "DRAFT");
        assert_eq!(after.iter_text_items().count(), 4);
    }

    /// Watermark annotation rectangles cannot capture ordinary text beneath them.
    #[test]
    fn watermark_annotation_does_not_claim_overlapping_body_text() {
        let mut page = extracted();
        page.watermark_annotations.push(
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("annotation box"),
        );
        let analyzer = PageAnalyzer::new(config());
        let draft = analyzer
            .prepare(page.clone(), Vec::new(), context(&page))
            .expect("prepare");
        let result = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("finish");
        let annotation = result.blocks.last().expect("detached annotation");
        assert_eq!(annotation.label, LayoutLabel::Watermark);
        assert_eq!(annotation.label_source, crate::LabelSource::Pdf);
        assert!(annotation.text.is_empty() && annotation.lines.is_empty());
        assert_eq!(result.iter_text_items().count(), 3);
    }

    /// OCR duplicates of a known watermark cannot reenter body ownership.
    #[test]
    fn ocr_cannot_restore_a_detached_watermark_to_body_flow() {
        let mut page = extracted();
        let bounds =
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("watermark box");
        page.text_items.push(
            TextItem::builder()
                .id(TextItemId::native(1, 3))
                .raw_text("DRAFT".to_owned())
                .bbox(bounds)
                .source(TextSource::Native)
                .watermark(Some(crate::WatermarkSource::PdfMarkedContent))
                .extraction_order(3)
                .build(),
        );
        let mut raw = RawConfig::default();
        raw.ocr.policy = docparse_config::OcrPolicy::MissingRegions;
        let analyzer = PageAnalyzer::new(Arc::new(
            ValidatedConfig::try_from(raw).expect("OCR config"),
        ));
        let draft = analyzer
            .prepare(
                page.clone(),
                vec![detection(0, [0.0, 80.0, 100.0, 100.0])],
                context(&page),
            )
            .expect("prepare");
        let ocr = crate::OcrResult::builder()
            .items(vec![
                crate::OcrTextItem::builder()
                    .text("DRAFT".to_owned())
                    .bbox(bounds)
                    .confidence(0.9)
                    .build(),
            ])
            .build();
        let result = analyzer
            .finish(draft, OcrCompletion::Succeeded(ocr))
            .expect("finish");
        assert_eq!(
            result
                .iter_text_items()
                .filter(|item| item.raw_text == "DRAFT")
                .count(),
            1
        );
        assert!(
            result
                .blocks
                .iter()
                .filter(|block| block.label != LayoutLabel::Watermark)
                .all(|block| !block.text.contains("DRAFT"))
        );
    }

    /// Verifies full and partial model coverage preserve every native text ID once.
    #[test]
    fn page_pipeline_preserves_native_text_under_partial_coverage() {
        let page = extracted();
        let draft = PageAnalyzer::new(config())
            .prepare(
                page.clone(),
                vec![detection(0, [5.0, 5.0, 50.0, 30.0])],
                context(&page),
            )
            .expect("page preparation must succeed");
        let result = PageAnalyzer::new(config())
            .finish(draft, OcrCompletion::NotRequested)
            .expect("page fusion must succeed");
        let mut ids: Vec<_> = result
            .iter_text_items()
            .map(|item| item.id.as_str())
            .collect();
        ids.sort_unstable();

        assert_eq!(ids, vec!["p1:t0", "p1:t1", "p1:t2"]);
        assert!(
            result
                .blocks
                .iter()
                .any(|block| block.model_region_id.is_some())
        );
        assert!(
            result
                .blocks
                .iter()
                .any(|block| block.model_region_id.is_none())
        );
        // The right-hand item shares a visual row with covered text but remains
        // fallback-owned because its own geometry lies outside the model region.
        let owner_region = |item_id: &str| {
            result.blocks.iter().find_map(|block| {
                block
                    .lines
                    .iter()
                    .flat_map(|line| &line.text_items)
                    .any(|item| item.id.as_str() == item_id)
                    .then_some(block.model_region_id.as_ref())
            })
        };
        assert!(owner_region("p1:t0").is_some_and(|region| region.is_some()));
        assert!(owner_region("p1:t1").is_some_and(|region| region.is_none()));
        assert!(owner_region("p1:t2").is_some_and(|region| region.is_none()));
    }

    /// Verifies input permutations cannot alter canonical serialized page output.
    #[test]
    fn page_pipeline_is_permutation_deterministic() {
        let forward_page = extracted();
        let mut reverse_page = forward_page.clone();
        reverse_page.text_items.reverse();
        let forward_detections = vec![
            detection(0, [5.0, 5.0, 50.0, 30.0]),
            detection(1, [5.0, 50.0, 55.0, 80.0]),
        ];
        let mut reverse_detections = forward_detections.clone();
        reverse_detections.reverse();
        let analyzer = PageAnalyzer::new(config());
        let forward = analyzer
            .finish(
                analyzer
                    .prepare(
                        forward_page.clone(),
                        forward_detections,
                        context(&forward_page),
                    )
                    .expect("forward preparation must succeed"),
                OcrCompletion::NotRequested,
            )
            .expect("forward analysis must succeed");
        let reverse = analyzer
            .finish(
                analyzer
                    .prepare(
                        reverse_page.clone(),
                        reverse_detections,
                        context(&reverse_page),
                    )
                    .expect("reverse preparation must succeed"),
                OcrCompletion::NotRequested,
            )
            .expect("reverse analysis must succeed");

        assert_eq!(
            serde_json::to_vec(&forward).expect("forward page must serialize"),
            serde_json::to_vec(&reverse).expect("reverse page must serialize")
        );
    }

    /// Verifies a page with no model detections degrades to pure geometry fallback.
    #[test]
    fn empty_layout_uses_fallback_without_losing_text() {
        let page = extracted();
        let analyzer = PageAnalyzer::new(config());
        let result = analyzer
            .finish(
                analyzer
                    .prepare(page.clone(), Vec::new(), context(&page))
                    .expect("fallback preparation must succeed"),
                OcrCompletion::NotRequested,
            )
            .expect("fallback analysis must succeed");

        assert_eq!(result.iter_text_items().count(), 3);
        assert!(
            result
                .blocks
                .iter()
                .all(|block| block.model_region_id.is_none())
        );
    }

    /// Runs merger and semantic assembly with deterministic image-space OCR facts.
    fn ocr_regression_page(
        native: Vec<TextItem>,
        facts: Vec<crate::OcrTextItem>,
    ) -> crate::PageResult {
        let mut page = extracted();
        page.text_items = native;
        let mut raw = RawConfig::default();
        raw.ocr.policy = docparse_config::OcrPolicy::Always;
        let analyzer = PageAnalyzer::new(Arc::new(
            ValidatedConfig::try_from(raw).expect("config"),
        ));
        let draft = analyzer
            .prepare(
                page.clone(),
                vec![detection(0, [0.0, 0.0, 100.0, 100.0])],
                context(&page),
            )
            .expect("prepare");
        analyzer
            .finish(
                draft,
                OcrCompletion::Succeeded(
                    crate::OcrResult::builder().items(facts).build(),
                ),
            )
            .expect("finish")
    }

    /// Constructs an original PDF span before visual recovery.
    fn ocr_regression_native(text: &str, bounds: [f64; 4]) -> TextItem {
        TextItem::builder()
            .id(TextItemId::native(1, 0))
            .raw_text(text.into())
            .bbox(Bbox::try_from(bounds).expect("bbox"))
            .source(TextSource::Native)
            .build()
    }

    /// Constructs a confident OCR box with optional upside-down reading corners.
    fn ocr_regression_fact(
        text: &str,
        bounds: [f64; 4],
        upside_down: bool,
    ) -> crate::OcrTextItem {
        let [left, top, right, bottom] = bounds;
        let mut corners = vec![
            docparse_layout::Point::new(left, top),
            docparse_layout::Point::new(right, top),
            docparse_layout::Point::new(right, bottom),
            docparse_layout::Point::new(left, bottom),
        ];
        if upside_down {
            corners.rotate_left(2);
        }
        crate::OcrTextItem::builder()
            .text(text.into())
            .bbox(Bbox::try_from(bounds).expect("bbox"))
            .polygon(Some(
                docparse_layout::Polygon::try_from(corners).expect("polygon"),
            ))
            .confidence(0.99)
            .build()
    }

    /// A native label must not suppress the scanned value sharing its detector line.
    #[test]
    fn ocr_regression_partial_native_overlap_retains_scanned_value() {
        let result = ocr_regression_page(
            vec![ocr_regression_native("Total:", [10.0, 10.0, 30.0, 20.0])],
            vec![ocr_regression_fact(
                "Total: 100",
                [10.0, 10.0, 90.0, 20.0],
                false,
            )],
        );
        let texts: Vec<_> = result
            .iter_text_items()
            .map(|item| item.raw_text.as_str())
            .collect();
        assert_eq!(texts.concat(), "Total: 100");
        assert_eq!(
            result
                .iter_text_items()
                .filter(|item| item.source == TextSource::Native)
                .count(),
            1
        );
    }

    /// Multiple confident boxes covering a bad native line must replace it collectively.
    #[test]
    fn ocr_regression_split_ocr_replaces_one_bad_native_span() {
        let result = ocr_regression_page(
            vec![ocr_regression_native(
                "\u{fffd}\u{fffd}\u{fffd}\u{fffd}",
                [10.0, 10.0, 90.0, 20.0],
            )],
            vec![
                ocr_regression_fact("Hello", [10.0, 10.0, 47.0, 20.0], false),
                ocr_regression_fact("world", [50.0, 10.0, 90.0, 20.0], false),
            ],
        );
        assert_eq!(
            result.replaced_native_text.len(),
            1,
            "canonical text still contains {:?}",
            result
                .iter_text_items()
                .map(|item| item.raw_text.as_str())
                .collect::<Vec<_>>()
        );
    }

    /// Upside-down English words follow the reversed physical inline axis.
    #[test]
    fn ocr_regression_upside_down_words_keep_reading_order() {
        let result = ocr_regression_page(
            vec![],
            vec![
                ocr_regression_fact("world", [10.0, 10.0, 45.0, 20.0], true),
                ocr_regression_fact("Hello", [50.0, 10.0, 85.0, 20.0], true),
            ],
        );
        let text = result
            .iter_lines()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text, "Hello world");
    }

    /// A visible word gap after punctuation still separates recognized English words.
    #[test]
    fn ocr_regression_ocr_word_spacing_after_punctuation() {
        let result = ocr_regression_page(
            vec![],
            vec![
                ocr_regression_fact("Hello,", [10.0, 10.0, 45.0, 20.0], false),
                ocr_regression_fact("world", [50.0, 10.0, 85.0, 20.0], false),
            ],
        );
        let text = result
            .iter_lines()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(text, "Hello, world");
    }
    /// Estimated OCR sizes must retain the tolerant paragraph policy despite having numeric hints.
    #[test]
    fn ocr_regression_ocr_font_metric_retains_estimation() {
        let result = ocr_regression_page(
            vec![],
            vec![
                ocr_regression_fact(
                    "first line",
                    [10.0, 10.0, 85.0, 20.0],
                    false,
                ),
                ocr_regression_fact(
                    "second line",
                    [10.0, 24.0, 85.0, 35.0],
                    false,
                ),
            ],
        );
        let fragments: Vec<_> = result
            .iter_lines()
            .map(|line| {
                crate::line::LineFragment::from_items(
                    line.text_items.clone(),
                    100.0,
                )
                .expect("fragment")
            })
            .collect();
        assert_eq!(fragments.len(), 2);
        assert!(fragments.iter().flat_map(|line| &line.items).all(|item| {
            item.style
                .as_ref()
                .is_some_and(|style| style.font_size_estimated)
        }));
        let decision = crate::semantic::ParagraphSplitter::new(
            docparse_config::FusionConfig::default(),
        )
        .between(
            fragments.first().expect("first"),
            fragments.last().expect("last"),
        );
        assert_eq!(
            decision,
            crate::semantic::ParagraphDecision::Continue,
            "estimated flags: {:?}",
            fragments
                .iter()
                .map(|line| line.metrics.font_size_estimated)
                .collect::<Vec<_>>()
        );
    }

    /// Internal native spans divide OCR into independently owned fragments in reading order.
    #[test]
    fn ocr_regression_internal_native_keeps_source_ids_and_geometry() {
        let native = ocr_regression_native("USD", [40.0, 10.0, 60.0, 20.0]);
        let result = ocr_regression_page(
            vec![native.clone()],
            vec![ocr_regression_fact(
                "10 USD 20",
                [10.0, 10.0, 90.0, 20.0],
                false,
            )],
        );
        let items: Vec<_> = result.iter_text_items().collect();
        assert_eq!(
            items
                .iter()
                .map(|item| item.raw_text.as_str())
                .collect::<String>(),
            "10 USD 20"
        );
        assert_eq!(
            items
                .iter()
                .map(|item| item.id.as_str())
                .collect::<Vec<_>>(),
            ["p1:o0", "p1:t0", "p1:o0:s1"]
        );
        assert!(items.first().expect("prefix").bbox.right <= native.bbox.left);
        assert!(items.last().expect("suffix").bbox.left >= native.bbox.right);
        assert_eq!(items.get(1).expect("native").bbox, native.bbox);
    }

    /// Native suffixes, half turns, explicit spaces and multi-byte prefixes retain exact canonical text.
    #[test]
    fn ocr_regression_partial_overlap_respects_boundaries() {
        for (native_text, bounds, recognized, upside_down, expected) in [
            ("USD", [65.0, 10.0, 90.0, 20.0], "100 USD", false, "100 USD"),
            (
                "Total:",
                [70.0, 10.0, 90.0, 20.0],
                "Total: 100",
                true,
                "Total: 100",
            ),
            (
                "Total: ",
                [10.0, 10.0, 30.0, 20.0],
                "Total: 100",
                false,
                "Total: 100",
            ),
            (
                "金额：",
                [10.0, 10.0, 30.0, 20.0],
                "金额： 100",
                false,
                "金额： 100",
            ),
        ] {
            let mut native = ocr_regression_native(native_text, bounds);
            if upside_down {
                native.rotation = 180.0;
            }
            let result = ocr_regression_page(
                vec![native],
                vec![ocr_regression_fact(
                    recognized,
                    [10.0, 10.0, 90.0, 20.0],
                    upside_down,
                )],
            );
            assert_eq!(
                result
                    .iter_lines()
                    .map(|line| line.text.as_str())
                    .collect::<String>(),
                expected
            );
            assert_eq!(
                result
                    .iter_text_items()
                    .filter(|item| item.source == TextSource::Native)
                    .count(),
                1
            );
        }
    }

    /// Duplicate or weak boxes must not manufacture enough evidence to remove the original PDF facts.
    #[test]
    fn ocr_regression_incomplete_replacement_preserves_native() {
        let native =
            ocr_regression_native("\u{fffd}\u{fffd}", [10.0, 10.0, 90.0, 20.0]);
        let repeated =
            ocr_regression_fact("Hello", [10.0, 10.0, 45.0, 20.0], false);
        let mut weak =
            ocr_regression_fact("Hello world", [10.0, 10.0, 90.0, 20.0], false);
        weak.confidence = 0.79;
        for facts in [vec![repeated.clone(), repeated], vec![weak]] {
            let result = ocr_regression_page(vec![native.clone()], facts);
            assert!(result.replaced_native_text.is_empty());
            assert_eq!(
                result
                    .iter_text_items()
                    .map(|item| item.raw_text.as_str())
                    .collect::<Vec<_>>(),
                [native.raw_text.as_str()]
            );
        }
    }

    /// All matching PDF glyph spans jointly remove only their duplicate OCR prefix.
    #[test]
    fn ocr_regression_fragmented_native_prefix_is_preserved_once() {
        let native =
            [("To", 10.0, 17.0), ("tal", 17.0, 26.0), (":", 26.0, 30.0)]
                .into_iter()
                .enumerate()
                .map(|(index, (text, left, right))| {
                    let mut item =
                        ocr_regression_native(text, [left, 10.0, right, 20.0]);
                    item.id = TextItemId::native(1, index as u32);
                    item
                })
                .collect();
        let result = ocr_regression_page(
            native,
            vec![ocr_regression_fact(
                "Total: 100",
                [10.0, 10.0, 90.0, 20.0],
                false,
            )],
        );
        assert_eq!(
            result
                .iter_lines()
                .map(|line| line.text.as_str())
                .collect::<String>(),
            "Total: 100"
        );
        assert_eq!(
            result
                .iter_text_items()
                .filter(|item| item.source == TextSource::Native)
                .count(),
            3
        );
    }

    /// Detection padding must not put a matched native label ahead of the OCR words on its own row.
    #[test]
    fn ocr_regression_native_anchor_calibrates_padded_fragments() {
        let mut native = ocr_regression_native("USD", [40.0, 30.0, 60.0, 50.0]);
        native.style = Some(
            crate::TextStyle::builder()
                .font_size(Some(20.0))
                .font_height(Some(20.0))
                .build(),
        );
        native.baseline = Some(crate::Baseline {
            start: docparse_layout::Point::new(40.0, 45.0),
            end: docparse_layout::Point::new(60.0, 45.0),
        });
        let result = ocr_regression_page(
            vec![native],
            vec![ocr_regression_fact(
                "Pay USD 20",
                [10.0, 25.0, 90.0, 50.0],
                false,
            )],
        );
        assert_eq!(
            result
                .iter_lines()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["Pay USD 20"]
        );
        assert!(
            result
                .iter_text_items()
                .filter(|item| item.source == TextSource::Ocr)
                .all(|item| item
                    .style
                    .as_ref()
                    .is_some_and(|style| style.font_size_estimated))
        );
    }
    /// Measures native classification and missing-region planning in optimized builds, including input ownership costs.
    #[test]
    #[ignore = "release-mode microbenchmark; run ocr_perf_ with --ignored --nocapture --test-threads=1"]
    fn ocr_perf_page_preparation() {
        let mut page = extracted();
        page.width = 2000.0;
        page.height = 2000.0;
        page.text_items = (0..600).map(|index| TextItem::builder()
            .id(TextItemId::native(1, index)).raw_text("A native document line with figures and character mapping checks.".into())
            .bbox(Bbox::try_from([10.0 + f64::from(index % 20) * 80.0, 10.0 + f64::from(index / 20) * 18.0,
                85.0 + f64::from(index % 20) * 80.0, 20.0 + f64::from(index / 20) * 18.0]).expect("box"))
            .source(TextSource::Native).build()).collect();
        let regions: Vec<_> = (0..60)
            .map(|index| {
                detection(
                    index,
                    [
                        10.0,
                        900.0 + f64::from(index) * 16.0,
                        1600.0,
                        912.0 + f64::from(index) * 16.0,
                    ],
                )
            })
            .collect();
        let context = context(&page);
        for policy in [
            docparse_config::OcrPolicy::Disabled,
            docparse_config::OcrPolicy::MissingRegions,
        ] {
            let mut raw = RawConfig::default();
            raw.ocr.policy = policy;
            let analyzer = PageAnalyzer::new(Arc::new(
                ValidatedConfig::try_from(raw).expect("config"),
            ));
            let mut samples = Vec::new();
            for _ in 0..7 {
                let start = std::time::Instant::now();
                for _ in 0..20 {
                    std::hint::black_box(
                        analyzer
                            .prepare(
                                page.clone(),
                                regions.clone(),
                                Arc::clone(&context),
                            )
                            .expect("prepare"),
                    );
                }
                samples
                    .push(start.elapsed().as_secs_f64() * 1_000_000.0 / 20.0);
            }
            samples.sort_by(f64::total_cmp);
            println!(
                "ocr_perf preparation {policy:?}: {:.3} us/page",
                samples.get(3).expect("median")
            );
        }
    }
}
