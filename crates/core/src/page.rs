use std::collections::BTreeMap;
use std::sync::Arc;

use docparse_config::{OcrPolicy, ValidatedConfig};
use docparse_layout::{Bbox, LayoutDetection, LayoutLabel, Point};
use typed_builder::TypedBuilder;

use crate::fusion::assign::{AssignmentEngine, AssignmentResult};
use crate::fusion::fallback::xy_cut;
use crate::fusion::order::order_blocks;
use crate::semantic::{FormulaMatcher, SemanticAssembler};
use crate::{
    Baseline, DocumentContext, ExtractedPage, OcrResult, PageAnalysisError,
    PageResult, PageWarning, ResultValidator, TextItem, TextItemId, TextSource,
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

/// Pure page-local analyzer that shares only frozen configuration and context.
#[derive(Debug, Clone)]
pub(crate) struct PageAnalyzer {
    config: Arc<ValidatedConfig>,
}

impl PageAnalyzer {
    /// Creates an analyzer from immutable validated configuration.
    pub(crate) const fn new(config: Arc<ValidatedConfig>) -> Self {
        Self { config }
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
        let native_area: f64 = extracted
            .text_items
            .iter()
            .filter(|item| item.watermark.is_none())
            .map(|item| item.bbox.area())
            .filter(|area| area.is_finite() && *area > 0.0)
            .sum();
        let native_text_coverage =
            (native_area / page_bbox.area()).clamp(0.0, 1.0);
        let mut missing_regions = Vec::new();
        if extracted
            .text_items
            .iter()
            .all(|item| item.watermark.is_some())
        {
            missing_regions.push(extracted.content_bounds.unwrap_or(page_bbox));
        } else {
            for detection in &detections {
                if detection.label == LayoutLabel::InlineFormula
                    || !Self::valid_page_bbox(detection.bbox, page_bbox)
                {
                    continue;
                }
                let has_native_text = extracted.text_items.iter().any(|item| {
                    item.watermark.is_none()
                        && Self::intersection_area(item.bbox, detection.bbox)
                            / item.bbox.area().max(f64::EPSILON)
                            >= self.config.fusion().center_minimum_line_coverage
                });
                if !has_native_text {
                    missing_regions.push(detection.bbox);
                }
            }
        }
        missing_regions.sort_by(|left, right| {
            left.top
                .total_cmp(&right.top)
                .then_with(|| left.left.total_cmp(&right.left))
                .then_with(|| left.bottom.total_cmp(&right.bottom))
                .then_with(|| left.right.total_cmp(&right.right))
        });
        missing_regions.dedup();
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
        mut ocr: OcrCompletion,
    ) -> Result<PageResult, PageAnalysisError> {
        let PageAnalysisDraft {
            extracted,
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
        let (watermark_items, mut text_items): (Vec<_>, Vec<_>) = extracted
            .text_items
            .into_iter()
            .partition(|item| item.watermark.is_some());
        let assembler = SemanticAssembler::new(
            extracted.page_number,
            page_bbox,
            self.config.fusion().clone(),
        );
        let watermarks = assembler.watermark_blocks(
            watermark_items,
            extracted.watermark_annotations,
            &extracted.watermark_evidence,
        )?;
        // OCR sees the original raster and may rediscover the same overlay. Deduplicate
        // complete watermark lines before any OCR text becomes eligible for body assignment.
        if let OcrCompletion::Succeeded(result) = &mut ocr {
            result.items.retain(|fact| {
                !watermarks.iter().any(|watermark| {
                    !watermark.text.is_empty()
                        && Self::equivalent_text(&watermark.text, &fact.text)
                        && Self::intersection_area(watermark.bbox, fact.bbox)
                            / watermark
                                .bbox
                                .area()
                                .min(fact.bbox.area())
                                .max(f64::EPSILON)
                            >= 0.8
                })
            });
        }
        self.merge_ocr(
            extracted.page_number,
            page_bbox,
            &mut text_items,
            &mut warnings,
            ocr,
            &missing_regions,
        );

        let AssignmentResult {
            model_seeds,
            residual,
            inline_formulas,
            diagnostics: assignment_diagnostics,
        } = AssignmentEngine::new(
            extracted.page_number,
            page_bbox,
            self.config.fusion().clone(),
        )
        .assign(text_items, detections)?;
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
            warnings.extend(output.warnings);
        }
        let fallback = assembler.fallback_blocks(residual, &fallback_tree)?;
        blocks.extend(fallback.blocks);
        warnings.extend(fallback.warnings);
        let formula_blocks = FormulaMatcher::new(extracted.page_number)
            .attach(&mut blocks, inline_formulas);
        blocks.extend(formula_blocks);
        let mut ordered = order_blocks(blocks)?;
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
            .warnings(warnings)
            .diagnostics(diagnostics)
            .build();
        let actual_native_ids: BTreeMap<_, usize> = page
            .iter_text_items()
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

    /// Validates, indexes, and deduplicates optional OCR facts against native text.
    fn merge_ocr(
        &self,
        page_number: u32,
        page_bbox: Bbox,
        text_items: &mut Vec<TextItem>,
        warnings: &mut Vec<PageWarning>,
        completion: OcrCompletion,
        missing_regions: &[Bbox],
    ) {
        if self.config.ocr().policy == OcrPolicy::Disabled {
            return;
        }
        if missing_regions.is_empty() {
            return;
        }
        let result = match completion {
            OcrCompletion::Succeeded(result) => result,
            OcrCompletion::Unavailable | OcrCompletion::NotRequested => {
                warnings.push(PageWarning {
                    code: "OcrUnavailable".to_owned(),
                    stage: "ocr".to_owned(),
                    message: "OCR was requested for missing regions but no engine was available"
                        .to_owned(),
                });
                return;
            }
            OcrCompletion::Failed(message) => {
                warnings.push(PageWarning {
                    code: "OcrFailed".to_owned(),
                    stage: "ocr".to_owned(),
                    message,
                });
                return;
            }
        };
        for (source_index, fact) in result.items.into_iter().enumerate() {
            let Ok(source_index) = u32::try_from(source_index) else {
                warnings.push(PageWarning {
                    code: "InvalidOcrResult".to_owned(),
                    stage: "ocr".to_owned(),
                    message: "OCR result index exceeds u32".to_owned(),
                });
                continue;
            };
            let polygon_valid = fact.polygon.as_ref().is_none_or(|polygon| {
                polygon.area().is_finite()
                    && polygon.area() > 0.0
                    && Self::valid_page_bbox(polygon.bbox(), page_bbox)
            });
            if fact.text.trim().is_empty()
                || !fact.confidence.is_finite()
                || !(0.0..=1.0).contains(&fact.confidence)
                || !Self::valid_page_bbox(fact.bbox, page_bbox)
                || !polygon_valid
            {
                warnings.push(PageWarning {
                    code: "InvalidOcrResult".to_owned(),
                    stage: "ocr".to_owned(),
                    message: format!("ignored OCR result {source_index}"),
                });
                continue;
            }
            let trimmed_text = fact.text.trim();
            let duplicate = text_items.iter().any(|native| {
                native.source == TextSource::Native
                    && Self::equivalent_text(&native.raw_text, trimmed_text)
                    && Self::intersection_area(native.bbox, fact.bbox)
                        / native
                            .bbox
                            .area()
                            .min(fact.bbox.area())
                            .max(f64::EPSILON)
                        >= 0.8
            });
            if duplicate {
                continue;
            }
            text_items.push(
                TextItem::builder()
                    .id(TextItemId::ocr(page_number, source_index))
                    .raw_text(fact.text)
                    .raw_bbox(Some(fact.bbox))
                    .bbox(fact.bbox)
                    .baseline(Some(Baseline {
                        start: Point::new(fact.bbox.left, fact.bbox.bottom),
                        end: Point::new(fact.bbox.right, fact.bbox.bottom),
                    }))
                    .source(TextSource::Ocr)
                    .confidence(Some(fact.confidence))
                    .extraction_order(source_index)
                    .build(),
            );
        }
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

    /// Returns axis-aligned overlap area for OCR and missing-region decisions.
    fn intersection_area(left: Bbox, right: Bbox) -> f64 {
        let width =
            (left.right.min(right.right) - left.left.max(right.left)).max(0.0);
        let height =
            (left.bottom.min(right.bottom) - left.top.max(right.top)).max(0.0);
        width * height
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
}
