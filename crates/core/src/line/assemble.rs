use docparse_config::FusionConfig;
use docparse_layout::Bbox;
use typed_builder::TypedBuilder;

use super::bidi::{detect_direction, order_items};
use super::metrics::LineMetrics;
use crate::{Baseline, LineError, TextItem, WritingDirection};

/// Maximum inline gap retained before a same-band sequence becomes a new fragment.
const MAX_HORIZONTAL_GAP: f64 = 15.0;

/// A conservative pre-assignment line fragment that exclusively owns its items.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct LineFragment {
    pub(crate) items: Vec<TextItem>,
    pub(crate) bbox: Bbox,
    pub(crate) baseline: Baseline,
    pub(crate) direction: WritingDirection,
    pub(crate) metrics: LineMetrics,
    pub(crate) rotation: f64,
}

impl LineFragment {
    /// Builds one canonical fragment from non-empty ordered or unordered text facts.
    pub(crate) fn from_items(
        mut items: Vec<TextItem>,
        page_width: f64,
    ) -> Result<Self, LineError> {
        let rotation = items.first().map_or(0.0, |item| item.rotation);
        let direction = detect_direction(&items, rotation);
        order_items(&mut items, direction);
        let metrics = LineMetrics::from_items(&items, page_width)?;
        Ok(Self::builder()
            .items(items)
            .bbox(metrics.bbox)
            .baseline(metrics.baseline)
            .direction(direction)
            .metrics(metrics)
            .rotation(rotation)
            .build())
    }
}

/// Boundary for conservative page-local line grouping implementations.
pub(crate) trait LineAssembler {
    /// Groups every input item into exactly one conservative line fragment.
    fn fragments(
        &self,
        items: Vec<TextItem>,
        config: &FusionConfig,
    ) -> Result<Vec<LineFragment>, LineError>;
}

/// Geometry-first line assembler that avoids speculative cross-column merges.
pub(crate) struct ConservativeLineAssembler;

impl LineAssembler for ConservativeLineAssembler {
    /// Forms cross-axis bands before flow-axis grouping so sub-point jitter cannot invert a line.
    fn fragments(
        &self,
        mut items: Vec<TextItem>,
        config: &FusionConfig,
    ) -> Result<Vec<LineFragment>, LineError> {
        items.sort_by(|left, right| {
            let left_vertical =
                detect_direction(std::slice::from_ref(left), left.rotation)
                    == WritingDirection::Vertical;
            let right_vertical =
                detect_direction(std::slice::from_ref(right), right.rotation)
                    == WritingDirection::Vertical;
            let left_cross = if left_vertical {
                left.bbox.left
            } else {
                left.bbox.top
            };
            let right_cross = if right_vertical {
                right.bbox.left
            } else {
                right.bbox.top
            };
            let left_flow = if left_vertical {
                left.bbox.top
            } else {
                left.bbox.left
            };
            let right_flow = if right_vertical {
                right.bbox.top
            } else {
                right.bbox.left
            };
            left_vertical
                .cmp(&right_vertical)
                .then_with(|| left.rotation.total_cmp(&right.rotation))
                .then_with(|| left_cross.total_cmp(&right_cross))
                .then_with(|| left_flow.total_cmp(&right_flow))
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        let page_width = items
            .iter()
            .map(|item| item.bbox.right)
            .fold(0.0_f64, f64::max)
            .max(1.0);

        // Banding is deliberately independent of x order: PDF glyph boxes on one baseline
        // commonly differ by fractions of a point, so sorting by exact top first can put a
        // right-hand item before its left-hand neighbor and make the later item look like a
        // large backwards jump.
        const CROSS_AXIS_SIZE_CAP: f64 = 24.0;
        let mut bands = Vec::<Vec<TextItem>>::new();
        for item in items {
            let item_vertical =
                detect_direction(std::slice::from_ref(&item), item.rotation)
                    == WritingDirection::Vertical;
            let merge = bands.last().is_some_and(|band| {
                let Some(first) = band.first() else {
                    return false;
                };
                let band_vertical = detect_direction(
                    std::slice::from_ref(first),
                    first.rotation,
                ) == WritingDirection::Vertical;
                if item_vertical != band_vertical
                    || (item.rotation - first.rotation).abs() > 2.0
                {
                    return false;
                }
                let item_cross_start = if item_vertical {
                    item.bbox.left
                } else {
                    item.bbox.top
                };
                let raw_item_cross_size = if item_vertical {
                    item.bbox.width()
                } else {
                    item.bbox.height()
                };
                let band_cross_start = band
                    .iter()
                    .map(|member| {
                        if item_vertical {
                            member.bbox.left
                        } else {
                            member.bbox.top
                        }
                    })
                    .fold(f64::INFINITY, f64::min);
                let band_cross_size = band
                    .iter()
                    .map(|member| {
                        if item_vertical {
                            member.bbox.width()
                        } else {
                            member.bbox.height()
                        }
                    })
                    .map(|size| size.clamp(1.0, CROSS_AXIS_SIZE_CAP))
                    .fold(1.0_f64, f64::max);
                let item_cross_size =
                    raw_item_cross_size.clamp(1.0, CROSS_AXIS_SIZE_CAP);
                // PDF font em boxes can be much taller than their visible row. Tighten
                // the band when only the incoming horizontal item has that anomaly.
                let inflated_height = !item_vertical
                    && raw_item_cross_size > CROSS_AXIS_SIZE_CAP
                    && raw_item_cross_size > band_cross_size * 2.0;
                let tolerance_factor = if inflated_height { 0.3 } else { 0.5 };
                (item_cross_start - band_cross_start).abs()
                    < band_cross_size.min(item_cross_size) * tolerance_factor
            });
            if merge {
                if let Some(band) = bands.last_mut() {
                    band.push(item);
                }
            } else {
                bands.push(vec![item]);
            }
        }

        let mut groups = Vec::<Vec<TextItem>>::new();
        for mut band in bands {
            let vertical = band.first().is_some_and(|first| {
                detect_direction(std::slice::from_ref(first), first.rotation)
                    == WritingDirection::Vertical
            });
            // LiteParse splits 90/270-degree groups when the flow-axis gap exceeds
            // three times the tallest item in that spatial rotation cluster.
            let vertical_gap_threshold = band
                .iter()
                .map(|item| item.bbox.height())
                .fold(0.0_f64, f64::max)
                * 3.0;
            band.sort_by(|left, right| {
                let ordering = if vertical {
                    left.bbox.top.total_cmp(&right.bbox.top)
                } else {
                    left.bbox.left.total_cmp(&right.bbox.left)
                };
                ordering.then_with(|| left.id.as_str().cmp(right.id.as_str()))
            });
            let mut band_groups = Vec::<Vec<TextItem>>::new();
            for item in band {
                let merge = band_groups
                    .last()
                    .and_then(|group| group.last())
                    .is_some_and(|previous| {
                        let within_vertical_cluster = !vertical
                            || item.bbox.top - previous.bbox.bottom
                                <= vertical_gap_threshold;
                        within_vertical_cluster
                            && Self::compatible(previous, &item, config)
                    });
                if merge {
                    if let Some(group) = band_groups.last_mut() {
                        group.push(item);
                    }
                } else {
                    band_groups.push(vec![item]);
                }
            }
            groups.extend(band_groups);
        }

        let mut fragments: Vec<_> = groups
            .into_iter()
            .map(|items| LineFragment::from_items(items, page_width))
            .collect::<Result<_, LineError>>()?;
        // Restore canonical page order after orientation-specific band construction.
        fragments.sort_by(|left, right| {
            left.rotation
                .total_cmp(&right.rotation)
                .then_with(|| left.bbox.top.total_cmp(&right.bbox.top))
                .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
                .then_with(|| {
                    left.items
                        .first()
                        .map(|item| item.id.as_str())
                        .cmp(&right.items.first().map(|item| item.id.as_str()))
                })
        });
        Ok(fragments)
    }
}

impl ConservativeLineAssembler {
    /// Returns whether two neighboring items are safe to merge before region assignment.
    fn compatible(
        left: &TextItem,
        right: &TextItem,
        config: &FusionConfig,
    ) -> bool {
        if (left.rotation - right.rotation).abs() > 2.0 {
            return false;
        }
        if detect_direction(std::slice::from_ref(left), left.rotation)
            == WritingDirection::Vertical
        {
            let horizontal_overlap = (left.bbox.right.min(right.bbox.right)
                - left.bbox.left.max(right.bbox.left))
            .max(0.0);
            return horizontal_overlap
                / left.bbox.width().min(right.bbox.width()).max(1.0)
                >= 0.5;
        }
        let vertical_overlap = (left.bbox.bottom.min(right.bbox.bottom)
            - left.bbox.top.max(right.bbox.top))
        .max(0.0);
        let overlap_ratio = vertical_overlap
            / left.bbox.height().min(right.bbox.height()).max(1.0);
        let font_size = left
            .style
            .as_ref()
            .and_then(|style| style.font_size)
            .unwrap_or_else(|| left.bbox.height());
        let right_font_size = right
            .style
            .as_ref()
            .and_then(|style| style.font_size)
            .unwrap_or_else(|| right.bbox.height());
        let font_compatible = (font_size - right_font_size).abs()
            <= config.estimated_font_size_tolerance_points.max(0.5);
        let gap = right.bbox.left - left.bbox.right;
        overlap_ratio >= 0.5
            && font_compatible
            && gap >= -font_size
            && gap <= MAX_HORIZONTAL_GAP
    }
}

#[cfg(test)]
mod tests {
    use docparse_config::FusionConfig;
    use docparse_layout::Bbox;

    use super::{ConservativeLineAssembler, LineAssembler};
    use crate::{
        TextItem, TextItemId, TextSource, TextStyle, WritingDirection,
    };

    /// Builds one native text item with a stable position and font size.
    fn item(
        index: u32,
        text: &str,
        bbox: [f64; 4],
        font_size: f64,
    ) -> TextItem {
        TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text(text.to_owned())
            .bbox(Bbox::try_from(bbox).expect("test bbox must be valid"))
            .source(TextSource::Native)
            .style(Some(
                TextStyle::builder().font_size(Some(font_size)).build(),
            ))
            .build()
    }

    /// Verifies nearby same-baseline fragments merge while distant columns remain separate.
    #[test]
    fn conservative_grouping_respects_large_column_gaps() {
        let items = vec![
            item(0, "left-a", [10.0, 10.0, 40.0, 20.0], 10.0),
            item(1, "left-b", [43.0, 10.0, 75.0, 20.0], 10.0),
            item(2, "right", [300.0, 10.0, 340.0, 20.0], 10.0),
        ];

        let fragments = ConservativeLineAssembler
            .fragments(items, &FusionConfig::default())
            .expect("grouping must succeed");

        assert_eq!(fragments.len(), 2);
        assert_eq!(
            fragments
                .first()
                .expect("first fragment must exist")
                .items
                .len(),
            2
        );
        assert_eq!(
            fragments
                .get(1)
                .expect("second fragment must exist")
                .items
                .len(),
            1
        );
    }

    /// Reproduces the sub-point y jitter that previously put a right-hand country first.
    #[test]
    fn horizontal_band_groups_subpoint_jitter_before_x_ordering() {
        let items = vec![
            item(
                0,
                "University of Illinois Urbana-Champaign,",
                [72.0, 136.992, 238.667, 145.142],
                8.9664,
            ),
            item(1, "USA", [241.958, 136.893, 260.814, 145.053], 8.9664),
        ];

        let fragments = ConservativeLineAssembler
            .fragments(items, &FusionConfig::default())
            .expect("same-band items must assemble");

        assert_eq!(fragments.len(), 1);
        assert_eq!(
            fragments
                .first()
                .expect("one same-band fragment must exist")
                .items
                .iter()
                .map(|item| item.raw_text.as_str())
                .collect::<Vec<_>>(),
            vec!["University of Illinois Urbana-Champaign,", "USA"]
        );
    }

    /// Verifies a modest column gutter does not become an inferred cross-column line.
    #[test]
    fn horizontal_band_preserves_twenty_point_column_gap() {
        let fragments = ConservativeLineAssembler
            .fragments(
                vec![
                    item(0, "left", [10.0, 10.0, 40.0, 20.0], 10.0),
                    item(1, "right", [60.0, 10.0, 90.0, 20.0], 10.0),
                ],
                &FusionConfig::default(),
            )
            .expect("same-band columns must assemble conservatively");

        assert_eq!(fragments.len(), 2);
    }

    /// Verifies an inflated em box cannot absorb a nearby line below it.
    #[test]
    fn horizontal_band_rejects_inflated_height_collision() {
        let fragments = ConservativeLineAssembler
            .fragments(
                vec![
                    item(0, "heading", [10.0, 435.6, 40.0, 450.5], 10.0),
                    item(1, "body", [43.0, 442.0, 75.0, 497.6], 10.0),
                ],
                &FusionConfig::default(),
            )
            .expect("inflated-height facts must assemble conservatively");

        assert_eq!(fragments.len(), 2);
    }

    /// Verifies LiteParse-style vertical clustering splits a large flow-axis gap.
    #[test]
    fn vertical_band_splits_gap_beyond_three_item_heights() {
        let mut upper = item(0, "upper", [10.0, 10.0, 30.0, 20.0], 10.0);
        upper.rotation = 270.0;
        let mut lower = item(1, "lower", [10.0, 80.0, 30.0, 90.0], 10.0);
        lower.rotation = 270.0;

        let fragments = ConservativeLineAssembler
            .fragments(vec![upper, lower], &FusionConfig::default())
            .expect("vertical facts must assemble");

        assert_eq!(fragments.len(), 2);
    }

    /// Verifies line changes, rotations, and input permutations preserve unique ownership.
    #[test]
    fn grouping_preserves_every_text_item_once() {
        let mut rotated = item(3, "vertical", [400.0, 10.0, 410.0, 70.0], 10.0);
        rotated.rotation = 90.0;
        let inputs = vec![
            item(2, "second", [10.0, 30.0, 50.0, 40.0], 10.0),
            rotated,
            item(0, "first", [10.0, 10.0, 40.0, 20.0], 10.0),
            item(1, "line", [43.0, 10.0, 65.0, 20.0], 10.0),
        ];

        let fragments = ConservativeLineAssembler
            .fragments(inputs, &FusionConfig::default())
            .expect("grouping must succeed");
        let ids: Vec<_> = fragments
            .iter()
            .flat_map(|fragment| {
                fragment.items.iter().map(|item| item.id.as_str())
            })
            .collect();

        assert_eq!(ids.len(), 4);
        assert_eq!(
            fragments
                .iter()
                .find(
                    |fragment| fragment.direction == WritingDirection::Vertical
                )
                .map(|fragment| fragment.items.len()),
            Some(1)
        );
    }
}
