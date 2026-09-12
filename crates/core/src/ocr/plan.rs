use docparse_config::{OcrPolicy, ValidatedConfig};
use docparse_layout::{Bbox, LayoutDetection, LayoutLabel};

use super::index::TextIndex;
use crate::TextItem;

/// Page-local OCR work and coverage diagnostics, independent of semantic block construction.
pub(crate) struct OcrPlan {
    pub(crate) regions: Vec<Bbox>,
    pub(crate) native_text_coverage: f64,
}

impl OcrPlan {
    /// Classifies native mapping quality once, then compares only cached geometry against detections.
    pub(crate) fn new(
        items: &[TextItem],
        detections: &[LayoutDetection],
        page: Bbox,
        config: &ValidatedConfig,
    ) -> Self {
        let search = config.ocr().policy == OcrPolicy::MissingRegions;
        let mut healthy = Vec::new();
        let mut regions = Vec::new();
        let mut area = 0.0;
        for item in items.iter().filter(|item| item.watermark.is_none()) {
            if item.needs_ocr() {
                if search {
                    regions.push(item.bbox);
                }
            } else {
                let item_area = item.bbox.area();
                if item_area.is_finite() && item_area > 0.0 {
                    area += item_area;
                }
                if search {
                    healthy.push(item);
                }
            }
        }
        let native_text_coverage = (area / page.area()).clamp(0.0, 1.0);
        match config.ocr().policy {
            // Coverage remains observable when disabled, but no region search or character count is needed.
            OcrPolicy::Disabled => {
                return Self {
                    regions,
                    native_text_coverage,
                };
            }
            OcrPolicy::Always => {
                return Self {
                    regions: vec![page],
                    native_text_coverage,
                };
            }
            OcrPolicy::MissingRegions => {}
        }
        let characters: usize = healthy
            .iter()
            .map(|item| item.raw_text.chars().count())
            .sum();
        if characters < 20 || (characters < 2000 && native_text_coverage < 0.15)
        {
            return Self {
                regions: vec![page],
                native_text_coverage,
            };
        }
        let healthy: TextIndex<'_> = healthy.into_iter().collect();
        for detection in detections {
            let bbox = detection.bbox;
            if matches!(
                detection.label,
                LayoutLabel::InlineFormula | LayoutLabel::Reference
            ) || Bbox::try_from([
                bbox.left,
                bbox.top,
                bbox.right,
                bbox.bottom,
            ])
            .is_err()
                || !page.contains_bbox(bbox)
            {
                continue;
            }
            // Images need OCR regardless of their native captions, so they need no native-coverage scan.
            let image = matches!(
                detection.label,
                LayoutLabel::Image
                    | LayoutLabel::Chart
                    | LayoutLabel::HeaderImage
                    | LayoutLabel::FooterImage
            );
            if image
                || !healthy.overlapping(bbox).any(|item| {
                    item.bbox.intersection_area(bbox)
                        / item.bbox.area().max(f64::EPSILON)
                        >= config.fusion().center_minimum_line_coverage
                })
            {
                regions.push(bbox);
            }
        }
        regions.sort_by(|a, b| {
            a.top
                .total_cmp(&b.top)
                .then_with(|| a.left.total_cmp(&b.left))
                .then_with(|| a.bottom.total_cmp(&b.bottom))
                .then_with(|| a.right.total_cmp(&b.right))
        });
        regions.dedup();
        Self {
            regions,
            native_text_coverage,
        }
    }
}
