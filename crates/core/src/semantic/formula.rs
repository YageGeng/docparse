use std::collections::BTreeMap;

use docparse_layout::{Bbox, LayoutDetection};

use crate::{
    Block, BlockId, InlineContentStatus, InlineSpan, LabelSource,
    ModelRegionId, SourceRegionEvidence, TextItemRange,
};

/// Non-owning formula matcher for one canonical page.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FormulaMatcher {
    page_number: u32,
}

impl FormulaMatcher {
    /// Creates a matcher whose independent formula IDs use one page number.
    pub(crate) const fn new(page_number: u32) -> Self {
        Self { page_number }
    }

    /// Attaches formula evidence to its best line and returns unmatched empty blocks.
    pub(crate) fn attach(
        &self,
        blocks: &mut [Block],
        mut formulas: Vec<LayoutDetection>,
    ) -> Vec<Block> {
        formulas.sort_by(|left, right| {
            left.bbox
                .top
                .total_cmp(&right.bbox.top)
                .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
                .then_with(|| {
                    left.source_detection_index
                        .cmp(&right.source_detection_index)
                })
        });
        let mut unmatched = Vec::new();
        for formula in formulas {
            let Some((block_index, line_index)) =
                Self::best_line(blocks, &formula)
            else {
                unmatched.push(self.independent_block(formula));
                continue;
            };
            let Some(span) = blocks
                .get(block_index)
                .and_then(|block| block.lines.get(line_index))
                .map(|line| Self::span(line, &formula))
            else {
                unmatched.push(self.independent_block(formula));
                continue;
            };
            if let Some(line) = blocks
                .get_mut(block_index)
                .and_then(|block| block.lines.get_mut(line_index))
            {
                line.inline_spans.push(span);
                line.inline_spans.sort_by(|left, right| {
                    left.bbox
                        .left
                        .total_cmp(&right.bbox.left)
                        .then_with(|| left.bbox.top.total_cmp(&right.bbox.top))
                        .then_with(|| {
                            left.text_item_range
                                .start
                                .cmp(&right.text_item_range.start)
                        })
                });
            } else {
                unmatched.push(self.independent_block(formula));
            }
        }
        unmatched
    }

    /// Selects the strongest geometrically compatible line with stable index ties.
    fn best_line(
        blocks: &[Block],
        formula: &LayoutDetection,
    ) -> Option<(usize, usize)> {
        let mut best: Option<(usize, usize, f64, f64)> = None;
        for (block_index, block) in blocks.iter().enumerate() {
            for (line_index, line) in block.lines.iter().enumerate() {
                let vertical_overlap =
                    Self::vertical_overlap(line.bbox, formula.bbox);
                let vertical_ratio = vertical_overlap
                    / line
                        .bbox
                        .height()
                        .min(formula.bbox.height())
                        .max(f64::EPSILON);
                let horizontal_margin =
                    line.bbox.height().max(formula.bbox.height());
                let within_x = formula.bbox.right
                    >= line.bbox.left - horizontal_margin
                    && formula.bbox.left <= line.bbox.right + horizontal_margin;
                let baseline_y =
                    line.baseline.map_or(line.bbox.bottom, |baseline| {
                        (baseline.start.y + baseline.end.y) / 2.0
                    });
                let baseline_distance =
                    (formula.bbox.center().y - baseline_y).abs();
                let maximum_distance = horizontal_margin * 1.5;
                if vertical_ratio <= 0.0
                    || !within_x
                    || baseline_distance > maximum_distance
                {
                    continue;
                }
                let score = vertical_ratio
                    + (1.0
                        - baseline_distance
                            / maximum_distance.max(f64::EPSILON));
                let candidate =
                    (block_index, line_index, score, baseline_distance);
                let replace = best.as_ref().is_none_or(|current| {
                    candidate.2.total_cmp(&current.2).is_gt()
                        || (candidate.2.total_cmp(&current.2).is_eq()
                            && (candidate.3.total_cmp(&current.3).is_lt()
                                || (candidate.3.total_cmp(&current.3).is_eq()
                                    && (candidate.0, candidate.1)
                                        < (current.0, current.1))))
                });
                if replace {
                    best = Some(candidate);
                }
            }
        }
        best.map(|(block_index, line_index, _, _)| (block_index, line_index))
    }

    /// Builds one span and derives completeness strictly from overlapping text facts.
    fn span(line: &crate::Line, formula: &LayoutDetection) -> InlineSpan {
        let overlaps: Vec<_> = line
            .text_items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let area = Self::intersection_area(item.bbox, formula.bbox);
                (area > 0.0).then_some((index, item, area))
            })
            .collect();
        let overlap_area: f64 = overlaps.iter().map(|(_, _, area)| area).sum();
        let coverage = (overlap_area / formula.bbox.area()).clamp(0.0, 1.0);
        let (range, extracted_text, content_status) =
            if let (Some(first), Some(last)) =
                (overlaps.first(), overlaps.last())
            {
                let text = overlaps
                    .iter()
                    .map(|(_, item, _)| item.raw_text.as_str())
                    .collect::<String>();
                let status = if coverage >= 0.8 {
                    InlineContentStatus::Complete
                } else {
                    InlineContentStatus::Partial
                };
                (
                    TextItemRange::new(first.0, last.0.saturating_add(1)),
                    Some(text),
                    status,
                )
            } else {
                let insertion = line
                    .text_items
                    .iter()
                    .take_while(|item| {
                        item.bbox.center().x <= formula.bbox.center().x
                    })
                    .count();
                (
                    TextItemRange::new(insertion, insertion),
                    None,
                    InlineContentStatus::Missing,
                )
            };
        InlineSpan::builder()
            .label(formula.label.clone())
            .confidence(Some(formula.confidence))
            .bbox(formula.bbox)
            .polygon(formula.polygon.clone())
            .text_item_range(range)
            .extracted_text(extracted_text)
            .content_status(content_status)
            .build()
    }

    /// Preserves an unmatched formula as an empty model-backed block without copying text.
    fn independent_block(&self, formula: LayoutDetection) -> Block {
        let region_id = ModelRegionId::detected(
            self.page_number,
            formula.source_detection_index,
        );
        let source_region = SourceRegionEvidence::builder()
            .model_region_id(Some(region_id.clone()))
            .bbox(formula.bbox)
            .polygon(formula.polygon.clone())
            .geometry_source(formula.geometry_source)
            .confidence(Some(formula.confidence))
            .model_order(Some(formula.model_order))
            .build();
        Block::builder()
            .id(BlockId::model(
                self.page_number,
                formula.source_detection_index,
                0,
            ))
            .label(formula.label)
            .text(String::new())
            .raw_label(Some(formula.raw_label))
            .label_source(LabelSource::Model)
            .confidence(Some(formula.confidence))
            .bbox(formula.bbox)
            .polygon(formula.polygon)
            .source_region(Some(source_region))
            .model_region_id(Some(region_id))
            .model_order(Some(formula.model_order))
            .final_order(0)
            .evidence(Vec::new())
            .semantic_hints(BTreeMap::new())
            .lines(Vec::new())
            .build()
    }

    /// Returns the positive vertical overlap between two canonical boxes.
    fn vertical_overlap(left: Bbox, right: Bbox) -> f64 {
        (left.bottom.min(right.bottom) - left.top.max(right.top)).max(0.0)
    }

    /// Returns axis-aligned intersection area without changing source geometry.
    fn intersection_area(left: Bbox, right: Bbox) -> f64 {
        let width =
            (left.right.min(right.right) - left.left.max(right.left)).max(0.0);
        let height =
            (left.bottom.min(right.bottom) - left.top.max(right.top)).max(0.0);
        width * height
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use docparse_layout::{
        Bbox, GeometrySource, LayoutDetection, LayoutLabel, Point,
    };

    use super::FormulaMatcher;
    use crate::{
        Baseline, Block, BlockId, InlineContentStatus, LabelSource, Line,
        LineId, TextItem, TextItemId, TextSource, WritingDirection,
    };

    /// Builds one validated box for formula matching tests.
    fn bbox(value: [f64; 4]) -> Bbox {
        Bbox::try_from(value).expect("test bbox must be valid")
    }

    /// Builds one inline-formula detection with a stable source index.
    fn formula(index: u32, bounds: [f64; 4]) -> LayoutDetection {
        LayoutDetection::builder()
            .source_detection_index(index)
            .raw_label("inline_formula".to_owned())
            .class_id(15)
            .label(LayoutLabel::InlineFormula)
            .confidence(0.9)
            .bbox(bbox(bounds))
            .polygon(None)
            .geometry_source(GeometrySource::DerivedFromBbox)
            .model_order(i64::from(index))
            .metadata(BTreeMap::new())
            .build()
    }

    /// Builds one two-item line wrapped in a model text block.
    fn block() -> Block {
        let block_id = BlockId::model(1, 0, 0);
        let items = vec![
            TextItem::builder()
                .id(TextItemId::native(1, 0))
                .raw_text("left".to_owned())
                .bbox(bbox([10.0, 10.0, 40.0, 20.0]))
                .source(TextSource::Native)
                .build(),
            TextItem::builder()
                .id(TextItemId::native(1, 1))
                .raw_text("x+y".to_owned())
                .bbox(bbox([45.0, 10.0, 70.0, 20.0]))
                .source(TextSource::Native)
                .build(),
        ];
        let line = Line::builder()
            .id(LineId::new(&block_id, 0))
            .text("leftx+y".to_owned())
            .bbox(bbox([10.0, 10.0, 70.0, 20.0]))
            .baseline(Some(Baseline {
                start: Point::new(10.0, 20.0),
                end: Point::new(70.0, 20.0),
            }))
            .direction(WritingDirection::LeftToRight)
            .text_items(items)
            .build();
        Block::builder()
            .id(block_id)
            .label(LayoutLabel::Text)
            .text("leftx+y".to_owned())
            .label_source(LabelSource::Model)
            .bbox(bbox([10.0, 10.0, 70.0, 20.0]))
            .final_order(0)
            .lines(vec![line])
            .build()
    }

    /// Verifies one formula attaches to the best line and exact item range.
    #[test]
    fn formula_attaches_without_owning_text() {
        let mut blocks = vec![block()];
        let unmatched = FormulaMatcher::new(1)
            .attach(&mut blocks, vec![formula(2, [45.0, 10.0, 70.0, 20.0])]);

        assert!(unmatched.is_empty());
        let line = blocks
            .first()
            .and_then(|block| block.lines.first())
            .expect("test line must remain present");
        assert_eq!(line.text_items.len(), 2);
        assert_eq!(line.inline_spans.len(), 1);
        let span = line.inline_spans.first().expect("one span must exist");
        assert_eq!(span.text_item_range.start, 1);
        assert_eq!(span.text_item_range.end, 2);
        assert_eq!(span.extracted_text.as_deref(), Some("x+y"));
        assert_eq!(span.content_status, InlineContentStatus::Complete);
    }

    /// Verifies multiple formula spans are sorted by X rather than input order.
    #[test]
    fn attached_spans_are_sorted_by_position() {
        let mut blocks = vec![block()];
        FormulaMatcher::new(1).attach(
            &mut blocks,
            vec![
                formula(2, [45.0, 10.0, 70.0, 20.0]),
                formula(1, [10.0, 10.0, 40.0, 20.0]),
            ],
        );
        let spans = &blocks
            .first()
            .and_then(|block| block.lines.first())
            .expect("test line must exist")
            .inline_spans;

        assert_eq!(spans.len(), 2);
        let first = spans.first().expect("first span must exist");
        let second = spans.get(1).expect("second span must exist");
        assert!(first.bbox.left < second.bbox.left);
    }

    /// Verifies partial overlap and missing source text have distinct statuses.
    #[test]
    fn content_status_distinguishes_partial_and_missing() {
        let mut blocks = vec![block()];
        let unmatched = FormulaMatcher::new(1).attach(
            &mut blocks,
            vec![
                formula(1, [35.0, 10.0, 55.0, 20.0]),
                formula(2, [72.0, 10.0, 78.0, 20.0]),
            ],
        );
        let spans = &blocks
            .first()
            .and_then(|block| block.lines.first())
            .expect("test line must exist")
            .inline_spans;

        let partial = spans.first().expect("partial span must exist");
        let missing = spans.get(1).expect("missing span must exist");
        assert_eq!(partial.content_status, InlineContentStatus::Partial);
        assert_eq!(missing.content_status, InlineContentStatus::Missing);
        assert!(missing.extracted_text.is_none());
        assert!(unmatched.is_empty());
    }

    /// Verifies an unmatched formula becomes an empty independent formula block.
    #[test]
    fn unmatched_formula_becomes_independent_block() {
        let mut blocks = vec![block()];
        let unmatched = FormulaMatcher::new(1)
            .attach(&mut blocks, vec![formula(9, [10.0, 60.0, 90.0, 80.0])]);

        assert_eq!(unmatched.len(), 1);
        let block = unmatched.first().expect("one formula block must exist");
        assert_eq!(block.id.as_str(), "p1:b:m9:s0");
        assert_eq!(block.label, LayoutLabel::InlineFormula);
        assert!(block.lines.is_empty());
    }
}
