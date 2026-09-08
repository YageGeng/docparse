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

/// One source item with a temporary box aligned to its orientation group's reference direction.
struct AlignedItem {
    item: TextItem,
    bbox: Bbox,
    vertical: bool,
}

impl TryFrom<(TextItem, super::TextAxes)> for AlignedItem {
    type Error = LineError;

    /// Keeps page geometry intact while projecting into the group's shared reference axes.
    fn try_from(
        (item, axes): (TextItem, super::TextAxes),
    ) -> Result<Self, Self::Error> {
        let vertical =
            detect_direction(std::slice::from_ref(&item), item.rotation)
                == WritingDirection::Vertical;
        let bbox = if axes.is_oblique() {
            let projected = axes.project_bbox(item.bbox)?;
            let anchor = axes.project(
                item.baseline
                    .map_or(item.bbox.center(), |baseline| baseline.start),
            );
            let height = item
                .style
                .as_ref()
                .and_then(|style| style.font_height.or(style.font_size))
                .filter(|height| height.is_finite() && *height > 0.0)
                .unwrap_or(projected.height());
            let flow = item
                .baseline
                .map(|baseline| {
                    let start = axes.project(baseline.start).x;
                    let end = axes.project(baseline.end).x;
                    (start.min(end), start.max(end))
                })
                .filter(|(start, end)| end > start);
            let (left, right) =
                flow.unwrap_or((projected.left, projected.right));
            // A slanted run's page AABB grows with its length. Its measured baseline and
            // font height describe the cross-line band without that artificial inflation.
            Bbox::try_from([left, anchor.y, right, anchor.y + height])?
        } else if vertical {
            Bbox::try_from([
                item.bbox.top,
                item.bbox.left,
                item.bbox.bottom,
                item.bbox.right,
            ])?
        } else {
            item.bbox
        };
        Ok(Self {
            item,
            bbox,
            vertical,
        })
    }
}

/// Geometry-first line assembler that avoids speculative cross-column merges.
pub(crate) struct ConservativeLineAssembler;

impl LineAssembler for ConservativeLineAssembler {
    /// Forms cross-axis bands before flow-axis grouping, including non-cardinal text.
    fn fragments(
        &self,
        items: Vec<TextItem>,
        config: &FusionConfig,
    ) -> Result<Vec<LineFragment>, LineError> {
        let page_width = items
            .iter()
            .map(|item| item.bbox.right)
            .fold(0.0_f64, f64::max)
            .max(1.0);
        // Select orientation groups before comparing positions. Every group uses its
        // smallest source angle as a deterministic reference, so translating the page
        // adds the same offset to all projected coordinates. Never compare coordinates
        // computed with separate per-item angles, even when those angles are close.
        let mut items = items;
        items.sort_by(|left, right| {
            left.rotation
                .total_cmp(&right.rotation)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
        let mut orientations = Vec::<Vec<TextItem>>::new();
        for item in items {
            let same_orientation = orientations
                .last()
                .and_then(|group| group.first())
                .is_some_and(|first| {
                    (item.rotation - first.rotation).abs() <= 2.0
                        && super::TextAxes::from(item.rotation).is_oblique()
                            == super::TextAxes::from(first.rotation)
                                .is_oblique()
                });
            if same_orientation {
                if let Some(group) = orientations.last_mut() {
                    group.push(item);
                }
            } else {
                orientations.push(vec![item]);
            }
        }
        let mut groups = Vec::<Vec<AlignedItem>>::new();
        for orientation in orientations {
            let Some(first) = orientation.first() else {
                continue;
            };
            let axes = super::TextAxes::from(first.rotation);
            let mut items = orientation
                .into_iter()
                .map(|item| AlignedItem::try_from((item, axes)))
                .collect::<Result<Vec<_>, _>>()?;
            // Geometry orders members inside an orientation group. Sorting by each
            // member's exact angle first would interleave otherwise parallel lines.
            items.sort_by(|left, right| {
                left.bbox
                    .top
                    .total_cmp(&right.bbox.top)
                    .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
                    .then_with(|| {
                        left.item.id.as_str().cmp(right.item.id.as_str())
                    })
            });

            // Cross-axis bands are independent of flow order so sub-point glyph jitter cannot
            // make a later source item appear to jump backwards across its own line.
            const CROSS_AXIS_SIZE_CAP: f64 = 24.0;
            let mut bands = Vec::<Vec<AlignedItem>>::new();
            for item in items {
                let merge = bands.last().is_some_and(|band| {
                    let Some(first) = band.first() else {
                        return false;
                    };
                    if item.vertical != first.vertical
                        || (item.item.rotation - first.item.rotation).abs()
                            > 2.0
                    {
                        return false;
                    }
                    let raw_item_cross_size = item.bbox.height();
                    let band_cross_start = band
                        .iter()
                        .map(|member| member.bbox.top)
                        .fold(f64::INFINITY, f64::min);
                    let band_cross_size = band
                        .iter()
                        .map(|member| member.bbox.height())
                        .map(|size| size.clamp(1.0, CROSS_AXIS_SIZE_CAP))
                        .fold(1.0_f64, f64::max);
                    let item_cross_size =
                        raw_item_cross_size.clamp(1.0, CROSS_AXIS_SIZE_CAP);
                    let inflated_height = !item.vertical
                        && raw_item_cross_size > CROSS_AXIS_SIZE_CAP
                        && raw_item_cross_size > band_cross_size * 2.0;
                    let tolerance_factor =
                        if inflated_height { 0.3 } else { 0.5 };
                    (item.bbox.top - band_cross_start).abs()
                        < band_cross_size.min(item_cross_size)
                            * tolerance_factor
                });
                if merge {
                    if let Some(band) = bands.last_mut() {
                        band.push(item);
                    }
                } else {
                    bands.push(vec![item]);
                }
            }

            for mut band in bands {
                let vertical = band.first().is_some_and(|first| first.vertical);
                // Preserve the existing cardinal vertical-text gap policy in its aligned frame.
                let vertical_gap_threshold = band
                    .iter()
                    .map(|item| item.bbox.width())
                    .fold(0.0_f64, f64::max)
                    * 3.0;
                band.sort_by(|left, right| {
                    left.bbox.left.total_cmp(&right.bbox.left).then_with(|| {
                        left.item.id.as_str().cmp(right.item.id.as_str())
                    })
                });
                let mut band_groups = Vec::<Vec<AlignedItem>>::new();
                for item in band {
                    let merge = band_groups
                        .last()
                        .and_then(|group| group.last())
                        .is_some_and(|previous| {
                            let within_vertical_cluster = !vertical
                                || item.bbox.left - previous.bbox.right
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
        }

        let mut fragments: Vec<_> = groups
            .into_iter()
            .map(|items| {
                LineFragment::from_items(
                    items.into_iter().map(|aligned| aligned.item).collect(),
                    page_width,
                )
            })
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
    /// Applies overlap, font, and gap policies to boxes in the same reference frame.
    fn compatible(
        left: &AlignedItem,
        right: &AlignedItem,
        config: &FusionConfig,
    ) -> bool {
        if (left.item.rotation - right.item.rotation).abs() > 2.0 {
            return false;
        }
        let overlap = (left.bbox.bottom.min(right.bbox.bottom)
            - left.bbox.top.max(right.bbox.top))
        .max(0.0);
        let overlap_ratio =
            overlap / left.bbox.height().min(right.bbox.height()).max(1.0);
        if left.vertical {
            return overlap_ratio >= 0.5;
        }
        let font_size = left
            .item
            .style
            .as_ref()
            // Oblique PDF objects may use Tf=1 with a large text matrix. Their
            // overlap allowance must use the displayed cross-line height.
            .and_then(|style| {
                if super::TextAxes::from(left.item.rotation).is_oblique() {
                    style.font_height.or(style.font_size)
                } else {
                    style.font_size
                }
            })
            .unwrap_or_else(|| left.bbox.height());
        let right_font_size = right
            .item
            .style
            .as_ref()
            .and_then(|style| {
                if super::TextAxes::from(right.item.rotation).is_oblique() {
                    style.font_height.or(style.font_size)
                } else {
                    style.font_size
                }
            })
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

    /// Groups oblique spans by their baseline instead of page top, preserving parallel lines.
    #[test]
    fn scaled_oblique_glyphs_use_physical_font_height() {
        let mut items = Vec::new();
        for (index, x) in [20.0_f64, 40.0].into_iter().enumerate() {
            let mut glyph = item(
                index as u32,
                if index == 0 { "A" } else { "B" },
                [x - 8.0, 100.0 - x - 8.0, x + 38.0, 100.0 - x + 38.0],
                1.0,
            );
            glyph.rotation = 315.0;
            glyph.style.as_mut().expect("style").font_height = Some(48.0);
            glyph.baseline = Some(crate::Baseline {
                start: docparse_layout::Point::new(x, 100.0 - x),
                end: docparse_layout::Point::new(x + 30.0, 70.0 - x),
            });
            items.push(glyph);
        }
        let fragments = ConservativeLineAssembler
            .fragments(items, &FusionConfig::default())
            .expect("scaled text");
        assert_eq!(
            fragments.len(),
            1,
            "glyph overlap is relative to displayed size, not raw Tf=1"
        );
        assert_eq!(
            fragments
                .first()
                .expect("line")
                .items
                .iter()
                .map(|item| item.raw_text.as_str())
                .collect::<String>(),
            "AB"
        );
    }

    /// Groups oblique spans by their baseline instead of page top, preserving parallel lines.
    #[test]
    fn oblique_spans_use_local_text_coordinates() {
        for rotation in [45, 135, 225, 315] {
            let items = [
                (0, "SLANTED ", (10.0, 110.0), (40.0, 80.0)),
                (1, "TEXT", (45.0, 75.0), (75.0, 45.0)),
                (2, "OTHER", (30.0, 130.0), (60.0, 100.0)),
            ]
            .into_iter()
            .rev()
            .map(|(index, text, (x1, y1), (x2, y2))| {
                let (x1, y1, x2, y2): (f64, f64, f64, f64) = match rotation {
                    45 => (x1, 140.0 - y1, x2, 140.0 - y2),
                    135 => (y1, x1, y2, x2),
                    225 => (140.0 - x1, y1, 140.0 - x2, y2),
                    _ => (x1, y1, x2, y2),
                };
                let mut span = item(
                    index,
                    text,
                    [
                        x1.min(x2) - 3.0,
                        y1.min(y2) - 3.0,
                        x1.max(x2) + 3.0,
                        y1.max(y2) + 3.0,
                    ],
                    10.0,
                );
                span.rotation = f64::from(rotation);
                span.baseline = Some(crate::Baseline {
                    start: docparse_layout::Point::new(x1, y1),
                    end: docparse_layout::Point::new(x2, y2),
                });
                span
            })
            .collect();
            let fragments = ConservativeLineAssembler
                .fragments(items, &FusionConfig::default())
                .expect("valid oblique source spans");
            assert_eq!(fragments.len(), 2);
            let line = fragments
                .iter()
                .find(|fragment| fragment.items.len() == 2)
                .expect("the two collinear spans must share a line");
            assert_eq!(
                line.items
                    .iter()
                    .map(|item| item.raw_text.as_str())
                    .collect::<String>(),
                "SLANTED TEXT"
            );
            assert_eq!(line.rotation.to_bits(), f64::from(rotation).to_bits());
            let delta_x = line.baseline.end.x - line.baseline.start.x;
            let delta_y = line.baseline.end.y - line.baseline.start.y;
            let actual = delta_y.atan2(delta_x).to_degrees().rem_euclid(360.0);
            assert!((actual - f64::from(rotation)).abs() < 1e-6);
        }
    }

    /// Moving slightly differing spans together cannot change their line membership.
    #[test]
    fn oblique_grouping_is_translation_invariant() {
        for (shift_x, shift_y) in
            [(0.0, 0.0), (500.0, 500.0), (500.0, 0.0), (0.0, 500.0)]
        {
            let mut items = Vec::new();
            for (index, rotation, position) in
                [(0, 45.0_f64, 10.0), (1, 46.0_f64, 35.0)]
            {
                let start = docparse_layout::Point::new(
                    position + shift_x,
                    position + shift_y,
                );
                let (sine, cosine) = rotation.to_radians().sin_cos();
                let end = docparse_layout::Point::new(
                    start.x + cosine * 28.0,
                    start.y + sine * 28.0,
                );
                let mut span = item(
                    index,
                    "word",
                    [start.x - 3.0, start.y - 3.0, end.x + 3.0, end.y + 3.0],
                    10.0,
                );
                span.rotation = rotation;
                span.baseline = Some(crate::Baseline { start, end });
                items.push(span);
            }
            for input in [items.clone(), items.into_iter().rev().collect()] {
                let groups = ConservativeLineAssembler
                    .fragments(input, &FusionConfig::default())
                    .expect("valid spans");
                assert_eq!(
                    groups.len(),
                    1,
                    "translation by ({shift_x}, {shift_y}) changed grouping"
                );
                assert_eq!(groups.first().expect("one line").items.len(), 2);
            }
        }
    }

    /// Angle jitter must not place another parallel line between adjacent source spans.
    #[test]
    fn oblique_grouping_keeps_parallel_bands_with_angle_jitter() {
        let mut items = Vec::new();
        for (index, rotation, position, offset) in [
            (0, 45.0_f64, 10.0, 0.0),
            (1, 46.0, 35.0, 0.0),
            (2, 45.0, 10.0, 40.0),
            (3, 46.0, 35.0, 40.0),
        ] {
            let start = docparse_layout::Point::new(
                position + 500.0,
                position + 500.0 + offset,
            );
            let (sine, cosine) = rotation.to_radians().sin_cos();
            let end = docparse_layout::Point::new(
                start.x + cosine * 28.0,
                start.y + sine * 28.0,
            );
            let mut span = item(
                index,
                "word",
                [start.x - 3.0, start.y - 3.0, end.x + 3.0, end.y + 3.0],
                10.0,
            );
            span.rotation = rotation;
            span.baseline = Some(crate::Baseline { start, end });
            items.push(span);
        }
        let groups = ConservativeLineAssembler
            .fragments(items, &FusionConfig::default())
            .expect("parallel lines");
        assert_eq!(groups.len(), 2);
        let ids: Vec<_> = groups
            .iter()
            .map(|group| {
                group
                    .items
                    .iter()
                    .map(|item| item.id.as_str())
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(ids, [vec!["p1:t0", "p1:t1"], vec!["p1:t2", "p1:t3"]]);
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
