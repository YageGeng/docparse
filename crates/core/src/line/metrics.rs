use docparse_layout::Bbox;
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::{Baseline, LineError, TextItem};

/// Coarse horizontal anchor used by paragraph and ordering heuristics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum LineAnchor {
    Left,
    Center,
    Right,
}

/// Deterministic aggregate geometry and typography for one line fragment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub(crate) struct LineMetrics {
    pub(crate) font_size: f64,
    #[builder(default)]
    pub(crate) font_size_estimated: bool,
    pub(crate) bold_ratio: f64,
    pub(crate) italic_ratio: f64,
    pub(crate) bbox: Bbox,
    pub(crate) baseline: Baseline,
    pub(crate) anchor: LineAnchor,
    pub(crate) indent: f64,
}

impl LineMetrics {
    /// Aggregates character-weighted style and unioned geometry from non-empty items.
    pub(crate) fn from_items(
        items: &[TextItem],
        page_width: f64,
    ) -> Result<Self, LineError> {
        let Some(first) = items.first() else {
            return Err(LineError::EmptyFragment);
        };
        let mut bbox = first.bbox;
        let mut character_count = 0_usize;
        let mut font_size_sum = 0.0;
        let mut font_size_characters = 0_usize;
        let mut bold_characters = 0_usize;
        let mut italic_characters = 0_usize;
        for item in items {
            bbox = Bbox::try_from([
                bbox.left.min(item.bbox.left),
                bbox.top.min(item.bbox.top),
                bbox.right.max(item.bbox.right),
                bbox.bottom.max(item.bbox.bottom),
            ])?;
            let count = item.raw_text.chars().count().max(1);
            character_count += count;
            if let Some(style) = &item.style {
                if let Some(font_size) = style.font_size {
                    font_size_sum += font_size * count as f64;
                    font_size_characters += count;
                }
                if style.bold {
                    bold_characters += count;
                }
                if style.italic {
                    italic_characters += count;
                }
            }
        }
        let font_size_estimated = font_size_characters == 0;
        let font_size = if font_size_estimated {
            bbox.height()
        } else {
            font_size_sum / font_size_characters as f64
        };
        let center_offset = (bbox.center().x - page_width / 2.0).abs();
        let anchor = if center_offset <= page_width * 0.20 {
            LineAnchor::Center
        } else if bbox.center().x < page_width / 2.0 {
            LineAnchor::Left
        } else {
            LineAnchor::Right
        };
        let axes = super::TextAxes::from(first.rotation);
        let baseline = if axes.is_oblique() {
            match (first.baseline, items.last().and_then(|item| item.baseline))
            {
                (Some(first), Some(last)) => Baseline {
                    start: first.start,
                    end: last.end,
                },
                _ => {
                    // Estimate only when measured anchors are unavailable, using the text
                    // direction through the box center rather than a horizontal bottom edge.
                    let unit =
                        axes.unproject(docparse_layout::Point::new(1.0, 0.0));
                    let half_length = 0.5
                        * (bbox.width() / unit.x.abs())
                            .min(bbox.height() / unit.y.abs());
                    let center = bbox.center();
                    Baseline {
                        start: docparse_layout::Point::new(
                            center.x - unit.x * half_length,
                            center.y - unit.y * half_length,
                        ),
                        end: docparse_layout::Point::new(
                            center.x + unit.x * half_length,
                            center.y + unit.y * half_length,
                        ),
                    }
                }
            }
        } else {
            // Use the dominant font's measured baseline. A superscript can sort first
            // in x order or extend the line bbox without moving the body baseline.
            let measured = items
                .iter()
                .filter_map(|item| {
                    item.baseline.map(|baseline| {
                        (
                            item.style
                                .as_ref()
                                .and_then(|style| style.font_size)
                                .unwrap_or(0.0),
                            baseline,
                        )
                    })
                })
                .max_by(|(left, _), (right, _)| left.total_cmp(right));
            let y = if super::bidi::detect_direction(items, first.rotation)
                == crate::WritingDirection::Vertical
            {
                bbox.bottom
            } else {
                measured.map_or(bbox.bottom, |(_, baseline)| baseline.start.y)
            };
            Baseline {
                start: docparse_layout::Point::new(bbox.left, y),
                end: docparse_layout::Point::new(bbox.right, y),
            }
        };
        Ok(Self::builder()
            .font_size(font_size)
            .font_size_estimated(font_size_estimated)
            .bold_ratio(bold_characters as f64 / character_count.max(1) as f64)
            .italic_ratio(
                italic_characters as f64 / character_count.max(1) as f64,
            )
            .bbox(bbox)
            .baseline(baseline)
            .anchor(anchor)
            .indent(bbox.left)
            .build())
    }
}

#[cfg(test)]
mod tests {
    use docparse_layout::Bbox;

    use super::{LineAnchor, LineMetrics};
    use crate::{TextItem, TextItemId, TextSource, TextStyle};

    /// Builds one styled item for metric aggregation.
    fn item(index: u32, left: f64, font_size: f64, bold: bool) -> TextItem {
        TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text("text".to_owned())
            .bbox(
                Bbox::try_from([left, 10.0, left + 20.0, 20.0])
                    .expect("test bbox must be valid"),
            )
            .source(TextSource::Native)
            .style(Some(
                TextStyle::builder()
                    .font_size(Some(font_size))
                    .bold(bold)
                    .build(),
            ))
            .build()
    }

    /// Verifies character-weighted typography and line anchors are deterministic.
    #[test]
    fn metrics_aggregate_font_and_style() {
        let items = vec![item(0, 10.0, 10.0, false), item(1, 35.0, 14.0, true)];

        let metrics = LineMetrics::from_items(&items, 100.0)
            .expect("line metrics must build");

        assert!((metrics.font_size - 12.0).abs() <= f64::EPSILON);
        assert!((metrics.bold_ratio - 0.5).abs() <= f64::EPSILON);
        assert_eq!(metrics.anchor, LineAnchor::Center);
        assert_eq!(
            metrics.bbox,
            Bbox::try_from([10.0, 10.0, 55.0, 20.0]).expect("valid bbox")
        );
    }
}
