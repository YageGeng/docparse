//! PP-StructureV3 layout postprocessing.

use std::collections::HashMap;

use super::{LayoutBlock, LayoutBox, LayoutError, LayoutLabel, LayoutPage};

/// Postprocessing options.
#[derive(Debug, Clone, Copy)]
pub struct PostprocessOptions {
    /// Minimum confidence retained in output.
    pub threshold: f32,
    /// IoU threshold used to suppress duplicate boxes with the same label.
    pub nms_iou_threshold: f32,
    /// IoU threshold used to merge high-overlap boxes with different labels.
    pub cross_label_iou_threshold: f32,
}

impl Default for PostprocessOptions {
    fn default() -> Self {
        Self {
            threshold: 0.2,
            nms_iou_threshold: 0.5,
            cross_label_iou_threshold: 0.9,
        }
    }
}

/// Decodes Paddle-style fetch rows into typed layout detections.
pub fn postprocess_fetch_rows(
    values: &[f32],
    columns: usize,
    image_width: u32,
    image_height: u32,
    options: PostprocessOptions,
) -> Result<LayoutPage, LayoutError> {
    if columns < 6 {
        return Err(LayoutError::Postprocess(
            "fetch rows must contain at least 6 columns".to_owned(),
        ));
    }
    if !values.len().is_multiple_of(columns) {
        return Err(LayoutError::Postprocess(
            "fetch row value count must be divisible by columns".to_owned(),
        ));
    }
    validate_unit_threshold("threshold", options.threshold)?;
    validate_unit_threshold("nms_iou_threshold", options.nms_iou_threshold)?;
    validate_unit_threshold(
        "cross_label_iou_threshold",
        options.cross_label_iou_threshold,
    )?;

    let mut blocks = Vec::new();
    for row in values.chunks_exact(columns) {
        let class_value = row.first().copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing class id".to_owned())
        })?;
        let score = row.get(1).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing score".to_owned())
        })?;
        if score < options.threshold {
            continue;
        }
        let class_id = if class_value.is_sign_negative() {
            0
        } else {
            class_value as i64
        };
        let order = if columns >= 7 {
            row.get(6).copied().map(|value| value as i64)
        } else {
            None
        };
        let x_min = row.get(2).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing x_min".to_owned())
        })?;
        let y_min = row.get(3).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing y_min".to_owned())
        })?;
        let x_max = row.get(4).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing x_max".to_owned())
        })?;
        let y_max = row.get(5).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing y_max".to_owned())
        })?;
        blocks.push(LayoutBlock {
            label: LayoutLabel::from_class_id(class_id),
            score,
            bbox: LayoutBox::from_xyxy_clamped(
                x_min,
                y_min,
                x_max,
                y_max,
                image_width,
                image_height,
            ),
            order,
        });
    }

    // Normalize nested labels before NMS so duplicate ReferenceContent/Text
    // proposals inside references collapse into one semantic block.
    relabel_text_inside_references(&mut blocks, options.nms_iou_threshold);

    // Resolve duplicate boxes in one score-ordered pass to avoid repeated sorts
    // while still applying separate same-label and cross-label overlap rules.
    let mut blocks = suppress_duplicate_blocks(
        blocks,
        options.nms_iou_threshold,
        options.cross_label_iou_threshold,
    );
    blocks.sort_by(|left, right| {
        left.order
            .unwrap_or(i64::MAX)
            .cmp(&right.order.unwrap_or(i64::MAX))
    });

    Ok(LayoutPage {
        width: image_width,
        height: image_height,
        blocks,
    })
}

/// Validates that a postprocess threshold is finite and inside the unit interval.
fn validate_unit_threshold(name: &str, value: f32) -> Result<(), LayoutError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(LayoutError::Postprocess(format!(
            "{name} must be between 0 and 1"
        )))
    }
}

/// Converts Text blocks covered by Reference regions into ReferenceContent.
fn relabel_text_inside_references(
    blocks: &mut [LayoutBlock],
    overlap_threshold: f32,
) {
    let references: Vec<LayoutBox> = blocks
        .iter()
        .filter(|block| block.label == LayoutLabel::Reference)
        .map(|block| block.bbox)
        .collect();

    for block in blocks {
        if block.label == LayoutLabel::Text
            && references.iter().any(|reference| {
                bbox_overlap_score(*reference, block.bbox) > overlap_threshold
            })
        {
            block.label = LayoutLabel::ReferenceContent;
        }
    }
}

/// Suppresses duplicate blocks after one confidence sort.
fn suppress_duplicate_blocks(
    mut blocks: Vec<LayoutBlock>,
    nms_iou_threshold: f32,
    cross_label_iou_threshold: f32,
) -> Vec<LayoutBlock> {
    blocks.sort_by(|left, right| right.score.total_cmp(&left.score));
    let mut kept: Vec<LayoutBlock> = Vec::with_capacity(blocks.len());
    let mut kept_by_label: HashMap<LayoutLabel, Vec<usize>> = HashMap::new();

    for block in blocks {
        let should_suppress =
            has_cross_label_duplicate(&kept, &block, cross_label_iou_threshold)
                || has_same_label_duplicate(
                    &kept,
                    &kept_by_label,
                    &block,
                    nms_iou_threshold,
                );
        if !should_suppress {
            let kept_index = kept.len();
            kept_by_label
                .entry(block.label)
                .or_default()
                .push(kept_index);
            kept.push(block);
        }
    }

    kept
}

/// Checks whether a different-label kept block already describes the same region.
fn has_cross_label_duplicate(
    kept: &[LayoutBlock],
    block: &LayoutBlock,
    overlap_threshold: f32,
) -> bool {
    kept.iter().any(|kept_block| {
        kept_block.label != block.label
            && bbox_iou(kept_block.bbox, block.bbox) > overlap_threshold
    })
}

/// Calculates standard intersection-over-union for conflicting-label boxes.
fn bbox_iou(left: LayoutBox, right: LayoutBox) -> f32 {
    let metrics = bbox_overlap_metrics(left, right);

    if metrics.union <= 0.0 {
        0.0
    } else {
        metrics.intersection / metrics.union
    }
}

/// Checks only same-label kept boxes by using the label index bucket.
fn has_same_label_duplicate(
    kept: &[LayoutBlock],
    kept_by_label: &HashMap<LayoutLabel, Vec<usize>>,
    block: &LayoutBlock,
    overlap_threshold: f32,
) -> bool {
    kept_by_label.get(&block.label).is_some_and(|indices| {
        indices
            .iter()
            .filter_map(|index| kept.get(*index))
            .any(|kept_block| {
                bbox_overlap_score(kept_block.bbox, block.bbox)
                    > overlap_threshold
            })
    })
}

/// Calculates the strongest duplicate-overlap score for two xywh boxes.
fn bbox_overlap_score(left: LayoutBox, right: LayoutBox) -> f32 {
    let metrics = bbox_overlap_metrics(left, right);
    let contained_overlap = if metrics.min_area <= 0.0 {
        0.0
    } else {
        metrics.intersection / metrics.min_area
    };
    let iou = if metrics.union <= 0.0 {
        0.0
    } else {
        metrics.intersection / metrics.union
    };

    // Use the stronger of IoU and smaller-box coverage so near-contained
    // duplicate layout boxes are merged even when their standard IoU is modest.
    iou.max(contained_overlap)
}

struct BboxOverlapMetrics {
    intersection: f32,
    min_area: f32,
    union: f32,
}

/// Calculates shared overlap primitives for xywh layout boxes.
fn bbox_overlap_metrics(
    left: LayoutBox,
    right: LayoutBox,
) -> BboxOverlapMetrics {
    let left_x2 = left.x + left.width;
    let left_y2 = left.y + left.height;
    let right_x2 = right.x + right.width;
    let right_y2 = right.y + right.height;

    let intersection_width = left_x2.min(right_x2) - left.x.max(right.x);
    let intersection_height = left_y2.min(right_y2) - left.y.max(right.y);
    let intersection =
        intersection_width.max(0.0) * intersection_height.max(0.0);
    let left_area = left.width.max(0.0) * left.height.max(0.0);
    let right_area = right.width.max(0.0) * right.height.max(0.0);
    let min_area = left_area.min(right_area);
    let union = left_area + right_area - intersection;

    BboxOverlapMetrics {
        intersection,
        min_area,
        union,
    }
}

#[cfg(test)]
mod tests {
    use super::{LayoutBlock, LayoutBox, LayoutLabel, LayoutPage};

    use super::{PostprocessOptions, postprocess_fetch_rows};

    /// Returns a block by index with a readable failure message.
    fn block_at(page: &LayoutPage, index: usize) -> &LayoutBlock {
        page.blocks
            .get(index)
            .expect("expected block should be present")
    }

    /// Asserts two scores are equal for the exact fixture values used in tests.
    fn assert_score_eq(actual: f32, expected: f32) {
        assert!((actual - expected).abs() <= f32::EPSILON);
    }

    #[test]
    fn fetch_postprocess_filters_threshold_and_orders_results() {
        let values = vec![
            22.0, 0.95, 1.0, 2.0, 10.0, 20.0, 2.0, 8.0, 0.20, 0.0, 0.0, 5.0,
            5.0, 1.0, 14.0, 0.90, -1.0, -2.0, 30.0, 40.0, 1.0,
        ];

        let page = postprocess_fetch_rows(
            &values,
            7,
            25,
            30,
            PostprocessOptions {
                threshold: 0.5,
                ..PostprocessOptions::default()
            },
        )
        .expect("postprocess should succeed");

        assert_eq!(page.blocks.len(), 2);
        let first = page.blocks.first().expect("first block should exist");
        let second = page.blocks.get(1).expect("second block should exist");
        assert_eq!(first.label, LayoutLabel::Image);
        assert_eq!(
            first.bbox,
            LayoutBox {
                x: 0.0,
                y: 0.0,
                width: 25.0,
                height: 30.0
            }
        );
        assert_eq!(second.label, LayoutLabel::Text);
    }

    #[test]
    fn fetch_postprocess_applies_nms_only_within_the_same_label() {
        let values = vec![
            22.0, 0.90, 0.0, 0.0, 100.0, 100.0, 2.0, 22.0, 0.80, 10.0, 10.0,
            90.0, 90.0, 1.0, 21.0, 0.70, 30.0, 10.0, 110.0, 90.0, 0.0, 22.0,
            0.60, 120.0, 120.0, 180.0, 180.0, 3.0,
        ];

        let page = postprocess_fetch_rows(
            &values,
            7,
            200,
            200,
            PostprocessOptions {
                threshold: 0.5,
                nms_iou_threshold: 0.5,
                ..PostprocessOptions::default()
            },
        )
        .expect("postprocess should succeed");

        assert_eq!(page.blocks.len(), 3);
        assert_eq!(block_at(&page, 0).label, LayoutLabel::Table);
        assert_eq!(block_at(&page, 1).label, LayoutLabel::Text);
        assert_score_eq(block_at(&page, 1).score, 0.90);
        assert_eq!(block_at(&page, 2).label, LayoutLabel::Text);
        assert_score_eq(block_at(&page, 2).score, 0.60);
    }

    #[test]
    fn fetch_postprocess_suppresses_same_label_contained_boxes() {
        let values = vec![
            1.0, 0.90, 100.0, 100.0, 950.0, 600.0, 0.0, 1.0, 0.60, 120.0,
            110.0, 485.0, 590.0, 1.0,
        ];

        let page = postprocess_fetch_rows(
            &values,
            7,
            1000,
            700,
            PostprocessOptions {
                threshold: 0.4,
                nms_iou_threshold: 0.5,
                ..PostprocessOptions::default()
            },
        )
        .expect("postprocess should succeed");

        assert_eq!(page.blocks.len(), 1);
        assert_eq!(block_at(&page, 0).label, LayoutLabel::Algorithm);
        assert_score_eq(block_at(&page, 0).score, 0.90);
    }

    #[test]
    fn fetch_postprocess_relabels_text_inside_reference_as_reference_content() {
        let values = vec![
            18.0, 0.93, 100.0, 100.0, 900.0, 900.0, 0.0, 19.0, 0.86, 150.0,
            150.0, 850.0, 450.0, 1.0, 22.0, 0.43, 150.0, 150.0, 850.0, 450.0,
            1.0,
        ];

        let page = postprocess_fetch_rows(
            &values,
            7,
            1000,
            1000,
            PostprocessOptions {
                threshold: 0.4,
                nms_iou_threshold: 0.5,
                ..PostprocessOptions::default()
            },
        )
        .expect("postprocess should succeed");

        assert_eq!(page.blocks.len(), 2);
        assert_eq!(block_at(&page, 0).label, LayoutLabel::Reference);
        assert_eq!(block_at(&page, 1).label, LayoutLabel::ReferenceContent);
        assert_score_eq(block_at(&page, 1).score, 0.86);
    }

    #[test]
    fn fetch_postprocess_merges_high_overlap_cross_label_boxes_by_score() {
        let values = vec![
            17.0, 0.46, 100.0, 100.0, 450.0, 130.0, 8.0, 1.0, 0.33, 100.0,
            100.0, 450.0, 130.0, 8.0,
        ];

        let page = postprocess_fetch_rows(
            &values,
            7,
            600,
            300,
            PostprocessOptions {
                threshold: 0.3,
                nms_iou_threshold: 0.5,
                cross_label_iou_threshold: 0.9,
            },
        )
        .expect("postprocess should succeed");

        assert_eq!(page.blocks.len(), 1);
        assert_eq!(block_at(&page, 0).label, LayoutLabel::ParagraphTitle);
        assert_score_eq(block_at(&page, 0).score, 0.46);
    }

    #[test]
    fn default_options_use_requested_layout_and_nms_thresholds() {
        let options = PostprocessOptions::default();

        assert_score_eq(options.threshold, 0.2);
        assert_score_eq(options.nms_iou_threshold, 0.5);
        assert_score_eq(options.cross_label_iou_threshold, 0.9);
    }

    #[test]
    fn postprocess_rejects_invalid_threshold_options() {
        let values = vec![22.0, 0.95, 1.0, 2.0, 10.0, 20.0];

        let error = postprocess_fetch_rows(
            &values,
            6,
            25,
            30,
            PostprocessOptions {
                threshold: f32::NAN,
                ..PostprocessOptions::default()
            },
        )
        .expect_err("NaN confidence threshold should be rejected");

        assert!(
            error
                .to_string()
                .contains("threshold must be between 0 and 1")
        );
    }
}
