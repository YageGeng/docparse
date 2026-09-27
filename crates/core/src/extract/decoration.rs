// SPDX-License-Identifier: Apache-2.0
// Geometric underline and strikeout correlation adapted from pdf-inspector and LiteParse.

use ::pdfium::{PathObject, SegmentKind};
use typed_builder::TypedBuilder;

use crate::TextItem;

/// Maximum stroke thickness in viewport points for a line to qualify as an underline or strikeout rule.
const MAX_RULE_THICKNESS: f64 = 2.0;

/// Minimum fraction of a text item's horizontal extent that a rule must cover.
const MIN_X_OVERLAP: f64 = 0.60;

/// Same-span rules repeating at this many vertical levels are table rulings, not semantic underlines.
const MIN_REPEATED_RULE_LEVELS: usize = 3;

/// Vertical tolerance for clustering rules on the same vertical level.
const RULE_Y_DEDUP_EPS: f64 = 2.0;

/// Horizontal span similarity required when clustering repeated table rulings.
const RULE_SPAN_OVERLAP_RATIO: f64 = 0.80;

/// Strikeout middle-band bounds relative to item height.
const STRIKE_BAND_TOP_RATIO: f64 = 0.20;
const STRIKE_BAND_BOT_RATIO: f64 = 0.65;

/// Maximum overhang in em-units for a strikeout rule beyond the text it decorates.
const STRIKE_OWNER_PAD_EM: f64 = 1.0;

/// A horizontal rule candidate in canonical viewport coordinates.
#[derive(Debug, Clone, Copy, TypedBuilder)]
pub(crate) struct HorizontalRule {
    pub(crate) x1: f64,
    pub(crate) x2: f64,
    pub(crate) y: f64,
}

impl HorizontalRule {
    /// Extracts horizontal rules from PDF vector path objects.
    pub(crate) fn from_paths(paths: &[PathObject]) -> Vec<Self> {
        let mut rules = Vec::new();

        for path in paths {
            // Horizontal stroked line segments
            if path.is_stroked
                && f64::from(path.stroke_width) <= MAX_RULE_THICKNESS
            {
                let mut previous = None;
                for segment in &path.segments {
                    let current = (f64::from(segment.x), f64::from(segment.y));
                    if segment.kind == SegmentKind::MoveTo {
                        previous = Some(current);
                    } else if segment.kind == SegmentKind::LineTo
                        && let Some(prev) = previous
                    {
                        let dy = (prev.1 - current.1).abs();
                        let dx = (prev.0 - current.0).abs();
                        if dy <= MAX_RULE_THICKNESS && dx > 1.0 {
                            rules.push(
                                Self::builder()
                                    .x1(prev.0.min(current.0))
                                    .x2(prev.0.max(current.0))
                                    .y((prev.1 + current.1) * 0.5)
                                    .build(),
                            );
                        }
                        previous = Some(current);
                    }
                }
            }

            // Filled path objects: partition into MoveTo sub-paths to separate disjoint underlines
            if path.is_filled {
                let mut min_x = f64::INFINITY;
                let mut max_x = f64::NEG_INFINITY;
                let mut min_y = f64::INFINITY;
                let mut max_y = f64::NEG_INFINITY;
                let mut point_count = 0_usize;

                let evaluate_subpath =
                    |min_x: f64,
                     max_x: f64,
                     min_y: f64,
                     max_y: f64,
                     count: usize,
                     rules: &mut Vec<Self>| {
                        if count >= 2 {
                            let width = max_x - min_x;
                            let height = max_y - min_y;
                            if height > 0.0
                                && height <= MAX_RULE_THICKNESS
                                && width > height
                                && width > 1.0
                            {
                                rules.push(
                                    Self::builder()
                                        .x1(min_x)
                                        .x2(max_x)
                                        .y((min_y + max_y) * 0.5)
                                        .build(),
                                );
                            }
                        }
                    };

                for segment in &path.segments {
                    let x = f64::from(segment.x);
                    let y = f64::from(segment.y);
                    if segment.kind == SegmentKind::MoveTo && point_count > 0 {
                        evaluate_subpath(
                            min_x,
                            max_x,
                            min_y,
                            max_y,
                            point_count,
                            &mut rules,
                        );
                        min_x = x;
                        max_x = x;
                        min_y = y;
                        max_y = y;
                        point_count = 1;
                    } else {
                        min_x = min_x.min(x);
                        max_x = max_x.max(x);
                        min_y = min_y.min(y);
                        max_y = max_y.max(y);
                        point_count += 1;
                    }
                }
                if point_count > 0 {
                    evaluate_subpath(
                        min_x,
                        max_x,
                        min_y,
                        max_y,
                        point_count,
                        &mut rules,
                    );
                } else {
                    let b = &path.bbox;
                    let width = f64::from(b.right - b.left);
                    let height = f64::from(b.bottom - b.top);
                    if height > 0.0
                        && height <= MAX_RULE_THICKNESS
                        && width > height
                        && width > 1.0
                    {
                        rules.push(
                            Self::builder()
                                .x1(f64::from(b.left))
                                .x2(f64::from(b.right))
                                .y(f64::from(b.top + b.bottom) * 0.5)
                                .build(),
                        );
                    }
                }
            }
        }

        rules
    }

    /// Returns the horizontal width of this rule.
    pub(crate) fn width(&self) -> f64 {
        self.x2 - self.x1
    }

    /// Checks if this rule horizontally covers at least the required fraction of an item's width.
    pub(crate) fn covers_item_horizontally(&self, item: &TextItem) -> bool {
        let ix1 = item.bbox.left;
        let ix2 = item.bbox.right;
        let item_width = (ix2 - ix1).max(f64::EPSILON);
        let overlap = (self.x2.min(ix2) - self.x1.max(ix1)).max(0.0);
        overlap / item_width >= MIN_X_OVERLAP
    }

    /// Checks if two horizontal rules share a similar horizontal span.
    pub(crate) fn has_similar_span(&self, other: &Self) -> bool {
        let overlap = (self.x2.min(other.x2) - self.x1.max(other.x1)).max(0.0);
        let min_w = self.width().min(other.width());
        if min_w <= 0.0 {
            return false;
        }
        overlap / min_w >= RULE_SPAN_OVERLAP_RATIO
    }

    /// Checks whether this rule is part of a table grid enclosing cell text from above and below.
    fn is_table_grid_rule(
        &self,
        all_rules: &[Self],
        items: &[TextItem],
    ) -> bool {
        let mut y_levels = [self.y, 0.0, 0.0];
        let mut count = 1_usize;
        for other in all_rules {
            if (self.y - other.y).abs() < f64::EPSILON
                && (self.x1 - other.x1).abs() < f64::EPSILON
            {
                continue;
            }
            let already_seen = y_levels.get(..count).is_some_and(|slice| {
                slice
                    .iter()
                    .any(|&y| (y - other.y).abs() <= RULE_Y_DEDUP_EPS)
            });
            if self.has_similar_span(other)
                && !already_seen
                && let Some(slot) = y_levels.get_mut(count)
            {
                *slot = other.y;
                count += 1;
                if count >= MIN_REPEATED_RULE_LEVELS {
                    break;
                }
            }
        }
        if count < MIN_REPEATED_RULE_LEVELS {
            return false;
        }

        // Check if any covered item has an enclosing rule ABOVE it as well (table cell grid).
        // Underlined prose paragraphs only have rules below each line, never above.
        items.iter().any(|item| {
            if !self.covers_item_horizontally(item)
                || item.bbox.bottom > self.y + 5.0
                || item.bbox.top < self.y - 40.0
            {
                return false;
            }
            // Check if there is an upper ruling sitting above this item
            all_rules.iter().any(|upper| {
                upper.y <= item.bbox.top + 2.0
                    && upper.y >= item.bbox.top - 20.0
                    && upper.covers_item_horizontally(item)
            })
        })
    }

    /// Checks whether this rule serves as an underline for a character's horizontal center, baseline, and bottom.
    pub(crate) fn is_underline_for_char(
        &self,
        mid_x: f64,
        baseline_y: f64,
        bottom: f64,
        font_size: f64,
    ) -> bool {
        if self.x1 > mid_x || mid_x > self.x2 {
            return false;
        }
        let dy = self.y - baseline_y;
        let below_bbox = self.y - bottom;
        dy >= -1.5
            && dy <= (font_size * 0.72).max(6.0)
            && below_bbox <= (font_size * 0.50).max(6.0)
    }

    /// Checks whether this rule serves as a strikeout across a character's horizontal center and vertical box.
    pub(crate) fn is_strikeout_for_char(
        &self,
        mid_x: f64,
        top: f64,
        bottom: f64,
    ) -> bool {
        if self.x1 > mid_x || mid_x > self.x2 {
            return false;
        }
        let h = (bottom - top).max(1.0);
        let band_top = top + h * STRIKE_BAND_TOP_RATIO;
        let band_bot = top + h * STRIKE_BAND_BOT_RATIO;
        self.y >= band_top && self.y <= band_bot
    }
}

/// Associates vector strokes and rules with text items to identify underlines and strikeouts.
pub(crate) struct DecorationCorrelator<'a> {
    items: &'a mut [TextItem],
    rules: &'a [HorizontalRule],
}

impl<'a> DecorationCorrelator<'a> {
    /// Builds a correlator from extracted text items and candidate horizontal rules.
    pub(crate) fn new(
        items: &'a mut [TextItem],
        rules: &'a [HorizontalRule],
    ) -> Self {
        Self { items, rules }
    }

    /// Correlates rules with text items, setting underline and strikeout flags in place.
    pub(crate) fn correlate(mut self) {
        if self.items.is_empty() || self.rules.is_empty() {
            return;
        }

        let valid_rules = self.filter_candidate_rules();
        self.apply_strikeouts(&valid_rules);
        self.apply_underlines(&valid_rules);
    }

    /// Filters candidate rules, removing table ruling grids and math fraction bars.
    fn filter_candidate_rules(&self) -> Vec<HorizontalRule> {
        self.rules
            .iter()
            .copied()
            .filter(|rule| {
                !rule.is_table_grid_rule(self.rules, self.items)
                    && !self.is_fraction_rule(rule)
            })
            .collect()
    }

    /// Checks if a short horizontal rule functions as a fraction bar between adjacent text items.
    fn is_fraction_rule(&self, rule: &HorizontalRule) -> bool {
        if rule.width() > 60.0 {
            return false;
        }
        let mut has_numerator = false;
        let mut has_denominator = false;
        for item in self.items.iter() {
            if item.bbox.bottom < rule.y - 25.0 || item.bbox.top > rule.y + 25.0
            {
                continue;
            }
            let font_size = item.effective_font_size();
            let dy_num = rule.y - item.bbox.bottom;
            if dy_num >= -1.0
                && dy_num <= font_size * 0.35
                && rule.covers_item_horizontally(item)
            {
                has_numerator = true;
            }
            let dy_den = item.bbox.top - rule.y;
            if dy_den >= -1.0
                && dy_den <= font_size * 0.35
                && rule.covers_item_horizontally(item)
            {
                has_denominator = true;
            }
            if has_numerator && has_denominator {
                return true;
            }
        }
        false
    }

    /// Detects strikeout lines crossing through the vertical middle band of glyph runs.
    fn apply_strikeouts(&mut self, rules: &[HorizontalRule]) {
        for rule in rules {
            let mut struck_indices = Vec::new();
            let mut min_x = f64::INFINITY;
            let mut max_x = f64::NEG_INFINITY;
            let mut font_size_sum = 0.0;

            for (idx, item) in self.items.iter().enumerate() {
                if item.raw_text.trim().is_empty()
                    || item.rotation.abs() > 2.0
                    || item.is_strikeout()
                    || rule.y < item.bbox.top
                    || rule.y > item.bbox.bottom
                {
                    continue;
                }
                let h = item.bbox.height();
                if h <= 0.0 {
                    continue;
                }
                let band_top = item.bbox.top + h * STRIKE_BAND_TOP_RATIO;
                let band_bot = item.bbox.top + h * STRIKE_BAND_BOT_RATIO;
                if rule.y >= band_top
                    && rule.y <= band_bot
                    && rule.covers_item_horizontally(item)
                {
                    struck_indices.push(idx);
                    min_x = min_x.min(item.bbox.left);
                    max_x = max_x.max(item.bbox.right);
                    font_size_sum += item.effective_font_size();
                }
            }

            if struck_indices.is_empty() {
                continue;
            }

            let avg_font_size = font_size_sum / struck_indices.len() as f64;
            let pad = (avg_font_size * STRIKE_OWNER_PAD_EM).max(4.0);

            if rule.x1 >= min_x - pad && rule.x2 <= max_x + pad {
                for idx in struck_indices {
                    if let Some(item) = self.items.get_mut(idx) {
                        item.set_strikeout(true);
                    }
                }
            }
        }
    }

    /// Detects underline rules sitting directly below the text baseline.
    fn apply_underlines(&mut self, rules: &[HorizontalRule]) {
        for rule in rules {
            for item in self.items.iter_mut() {
                if item.raw_text.trim().is_empty()
                    || item.rotation.abs() > 2.0
                    || item.is_strikeout()
                    || item.is_underline()
                    || rule.y < item.bbox.top
                    || rule.y > item.bbox.bottom + 20.0
                {
                    continue;
                }
                let font_size = item.effective_font_size();
                let baseline_y = item
                    .baseline
                    .map(|b| (b.start.y + b.end.y) * 0.5)
                    .unwrap_or_else(|| {
                        item.bbox.bottom - item.bbox.height() * 0.20
                    });

                let dy = rule.y - baseline_y;
                let below_bbox = rule.y - item.bbox.bottom;
                if dy >= -1.5
                    && dy <= (font_size * 0.65).max(6.0)
                    && below_bbox <= (font_size * 0.50).max(6.0)
                    && rule.covers_item_horizontally(item)
                {
                    item.set_underline(true);
                }
            }
        }
    }
}

/// Correlates horizontal rules with text items to assign underline and strikeout styles.
pub(crate) fn assign_decorations(
    items: &mut [TextItem],
    rules: &[HorizontalRule],
) {
    DecorationCorrelator::new(items, rules).correlate();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TextItemId, TextStyle};
    use ::pdfium::{PathSegment, RectF};
    use docparse_layout::Bbox;

    fn make_test_item(
        text: &str,
        x1: f64,
        y1: f64,
        x2: f64,
        y2: f64,
    ) -> TextItem {
        let bbox = Bbox::try_from([x1, y1, x2, y2]).expect("valid test bbox");
        TextItem::builder()
            .id(TextItemId::native(1, 0))
            .raw_text(text.to_string())
            .bbox(bbox)
            .source(crate::TextSource::Native)
            .style(Some(TextStyle::builder().font_size(Some(y2 - y1)).build()))
            .build()
    }

    fn make_horizontal_stroke(
        x1: f32,
        y1: f32,
        x2: f32,
        y2: f32,
        width: f32,
    ) -> PathObject {
        PathObject {
            bbox: RectF {
                left: x1.min(x2),
                top: y1.min(y2),
                right: x1.max(x2),
                bottom: y1.max(y2),
            },
            stroke_color: None,
            fill_color: None,
            stroke_width: width,
            is_stroked: true,
            is_filled: false,
            segments: vec![
                PathSegment {
                    kind: SegmentKind::MoveTo,
                    x: x1,
                    y: y1,
                    close: false,
                },
                PathSegment {
                    kind: SegmentKind::LineTo,
                    x: x2,
                    y: y2,
                    close: false,
                },
            ],
        }
    }

    #[test]
    fn strikeout_is_detected_on_midline_stroke() {
        let mut items =
            vec![make_test_item("Struck text", 10.0, 10.0, 70.0, 20.0)];
        let stroke = make_horizontal_stroke(8.0, 15.0, 72.0, 15.0, 1.0);
        let rules = HorizontalRule::from_paths(&[stroke]);
        assign_decorations(&mut items, &rules);
        let first = items.first().expect("first item exists");
        assert!(first.is_strikeout());
        assert!(!first.is_underline());
    }

    #[test]
    fn underline_is_detected_below_baseline() {
        let mut items =
            vec![make_test_item("Underlined", 10.0, 10.0, 70.0, 20.0)];
        let stroke = make_horizontal_stroke(9.0, 21.0, 71.0, 21.0, 1.0);
        let rules = HorizontalRule::from_paths(&[stroke]);
        assign_decorations(&mut items, &rules);
        let first = items.first().expect("first item exists");
        assert!(first.is_underline());
        assert!(!first.is_strikeout());
    }

    #[test]
    fn table_rulings_are_not_mistaken_for_underlines() {
        let mut items =
            vec![make_test_item("Cell text", 10.0, 10.0, 70.0, 20.0)];
        let strokes = vec![
            make_horizontal_stroke(5.0, 9.0, 100.0, 9.0, 1.0),
            make_horizontal_stroke(5.0, 21.0, 100.0, 21.0, 1.0),
            make_horizontal_stroke(5.0, 33.0, 100.0, 33.0, 1.0),
        ];
        let rules = HorizontalRule::from_paths(&strokes);
        assign_decorations(&mut items, &rules);
        let first = items.first().expect("first item exists");
        assert!(!first.is_underline());
    }

    #[test]
    fn filled_subpath_underlines_are_detected() {
        let mut items = vec![
            make_test_item("Word1", 10.0, 10.0, 50.0, 20.0),
            make_test_item("Word2", 60.0, 10.0, 100.0, 20.0),
        ];
        let path = PathObject {
            bbox: RectF {
                left: 10.0,
                top: 21.0,
                right: 100.0,
                bottom: 21.6,
            },
            stroke_color: None,
            fill_color: None,
            stroke_width: 0.0,
            is_stroked: false,
            is_filled: true,
            segments: vec![
                PathSegment {
                    kind: SegmentKind::MoveTo,
                    x: 10.0,
                    y: 21.6,
                    close: false,
                },
                PathSegment {
                    kind: SegmentKind::LineTo,
                    x: 50.0,
                    y: 21.6,
                    close: false,
                },
                PathSegment {
                    kind: SegmentKind::LineTo,
                    x: 50.0,
                    y: 21.0,
                    close: false,
                },
                PathSegment {
                    kind: SegmentKind::LineTo,
                    x: 10.0,
                    y: 21.0,
                    close: false,
                },
                PathSegment {
                    kind: SegmentKind::MoveTo,
                    x: 60.0,
                    y: 21.6,
                    close: false,
                },
                PathSegment {
                    kind: SegmentKind::LineTo,
                    x: 100.0,
                    y: 21.6,
                    close: false,
                },
                PathSegment {
                    kind: SegmentKind::LineTo,
                    x: 100.0,
                    y: 21.0,
                    close: false,
                },
                PathSegment {
                    kind: SegmentKind::LineTo,
                    x: 60.0,
                    y: 21.0,
                    close: false,
                },
            ],
        };
        let rules = HorizontalRule::from_paths(&[path]);
        assign_decorations(&mut items, &rules);
        assert!(items.first().is_some_and(|item| item.is_underline()));
        assert!(items.get(1).is_some_and(|item| item.is_underline()));
    }

    #[test]
    fn test_dense_underline_extraction() {
        let pdf_path = std::path::Path::new(
            "../../benchmark/ParseBench/data/docs/text/text_dense__underline.pdf",
        );
        if !pdf_path.exists() {
            return;
        }
        let library = ::pdfium::Library::init();
        let doc = library
            .load_document(
                pdf_path
                    .to_str()
                    .expect("valid unicode path for test document"),
                None,
            )
            .expect("document loads");
        let page = doc.page(0).expect("page loads");
        let view_box = page.view_box().expect("view box exists");
        let text_page = page.text().expect("text page loads");
        let mut evidence = crate::TableEvidence::default();
        let items = crate::extract::text::extract_page_text_items(
            &page,
            &text_page,
            &view_box,
            1,
            &mut evidence,
            None,
        )
        .expect("extract page text items succeeds");

        // Verify key phrases in dense CJK text are extracted with underline style
        let expected_underlines = [
            "開催地としての魅力や集客力を高める",
            "会場運営者",
            "回遊性の向上",
            "自主興行の開催",
            "付加価値の創出",
            "誘客",
            "産業界",
            "行政",
            "周辺施設",
            "観光エリア",
            "コラボイベント",
            "ネーミングライツ",
            "法人シート契約",
            "会場運営の支援",
        ];

        for &phrase in &expected_underlines {
            let found = items.iter().any(|item| {
                item.is_underline() && item.raw_text.contains(phrase)
            });
            assert!(found, "Expected underlined item containing '{phrase}'");
        }
    }
}
