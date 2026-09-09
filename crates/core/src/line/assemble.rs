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
    /// Orders upright fragments by the body baseline so raised scripts cannot jump ahead.
    pub(crate) fn reading_order_y(&self) -> f64 {
        if self.direction != WritingDirection::Vertical
            && !super::TextAxes::from(self.rotation).is_oblique()
            && self.items.iter().any(|item| item.baseline.is_some())
        {
            self.baseline.start.y
        } else {
            // An estimated bbox bottom can put a small overlapping glyph before prose.
            self.bbox.top
        }
    }

    /// Builds one canonical fragment from non-empty ordered or unordered text facts.
    pub(crate) fn from_items(
        mut items: Vec<TextItem>,
        page_width: f64,
    ) -> Result<Self, LineError> {
        let rotation = items.first().map_or(0.0, |item| item.rotation);
        let direction = detect_direction(&items, rotation);
        order_items(&mut items, direction);
        Self::from_ordered_items(items, page_width)
    }

    /// Builds metrics around an established inline order without flattening compound atoms again.
    pub(crate) fn from_ordered_items(
        items: Vec<TextItem>,
        page_width: f64,
    ) -> Result<Self, LineError> {
        let rotation = items.first().map_or(0.0, |item| item.rotation);
        let direction = detect_direction(&items, rotation);
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
                if !item.vertical && !axes.is_oblique() {
                    // Group body text on its baseline before inserting scripts. Using
                    // bbox tops mixes raised glyphs with prose and splits the row each
                    // time the font changes; its fragments then sort ahead of the prose.
                    let size = item
                        .item
                        .style
                        .as_ref()
                        .and_then(|style| style.font_size)
                        .filter(|size| size.is_finite() && *size > 0.0)
                        .unwrap_or_else(|| item.bbox.height());
                    let baseline = item
                        .item
                        .baseline
                        .map_or(item.bbox.bottom, |baseline| baseline.start.y);
                    let band_index = bands.iter().position(|band| {
                        let Some(first) = band.first() else {
                            return false;
                        };
                        let first_size = first
                            .item
                            .style
                            .as_ref()
                            .and_then(|style| style.font_size)
                            .filter(|size| size.is_finite() && *size > 0.0)
                            .unwrap_or_else(|| first.bbox.height());
                        let first_baseline = first
                            .item
                            .baseline
                            .map_or(first.bbox.bottom, |baseline| {
                                baseline.start.y
                            });
                        (size - first_size).abs()
                            <= config
                                .estimated_font_size_tolerance_points
                                .max(0.5)
                            && (baseline - first_baseline).abs()
                                <= size
                                    .min(first_size)
                                    .clamp(1.0, CROSS_AXIS_SIZE_CAP)
                                    * 0.2
                    });
                    if let Some(band) =
                        band_index.and_then(|index| bands.get_mut(index))
                    {
                        band.push(item);
                    } else {
                        bands.push(vec![item]);
                    }
                    continue;
                }
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
        fragments = LineFragment::attach_scripts(fragments, page_width)?;
        fragments = Self::join_script_gaps(fragments, page_width, config)?;
        // Restore canonical page order after orientation-specific band construction.
        fragments.sort_by(|left, right| {
            left.rotation
                .total_cmp(&right.rotation)
                .then_with(|| {
                    left.reading_order_y().total_cmp(&right.reading_order_y())
                })
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
    /// Shares optional vector evidence with line assembly after text ownership is known.
    pub(crate) fn fragments_with_rules(
        &self,
        items: Vec<TextItem>,
        config: &FusionConfig,
        rules: &[crate::TableRule],
    ) -> Result<Vec<LineFragment>, LineError> {
        let mut input =
            super::inline::InlineInput::prepare(items, rules, false)?;
        let mut fragments =
            self.fragments(std::mem::take(&mut input.items), config)?;
        input.restore(&mut fragments);
        Ok(fragments)
    }

    /// Rejoins one body baseline when attached scripts fill an apparent inline gap.
    fn join_script_gaps(
        mut fragments: Vec<LineFragment>,
        page_width: f64,
        config: &FusionConfig,
    ) -> Result<Vec<LineFragment>, LineError> {
        loop {
            let pair =
                fragments.iter().enumerate().find_map(|(index, left)| {
                    let angle = left.rotation.rem_euclid(360.0);
                    if angle.min(360.0 - angle) > 2.0
                        || left.direction == WritingDirection::Vertical
                    {
                        return None;
                    }
                    fragments
                        .iter()
                        .enumerate()
                        .skip(index + 1)
                        .find(|(_, right)| {
                            let size = left
                                .metrics
                                .font_size
                                .min(right.metrics.font_size);
                            let gap = (left.bbox.left - right.bbox.right)
                                .max(right.bbox.left - left.bbox.right);
                            // Retained body baselines/font sizes establish one physical row.
                            // Script-inflated bbox tops and bottoms are not used to merge rows.
                            left.direction == right.direction
                                && (left.rotation - right.rotation).abs() <= 2.0
                                && (left.metrics.font_size
                                    - right.metrics.font_size)
                                    .abs()
                                    <= config
                                        .estimated_font_size_tolerance_points
                                        .max(0.5)
                                && (left.baseline.start.y
                                    - right.baseline.start.y)
                                    .abs()
                                    <= size.clamp(1.0, 24.0) * 0.2
                                && gap >= -size
                                && gap <= MAX_HORIZONTAL_GAP
                        })
                        .map(|(right, _)| (index, right))
                });
            let Some((left, right)) = pair else {
                break;
            };
            let other = fragments.remove(right);
            if let Some(fragment) = fragments.get_mut(left) {
                let baseline = fragment.baseline;
                let font_size = fragment.metrics.font_size;
                let estimated = fragment.metrics.font_size_estimated;
                let first = std::mem::take(&mut fragment.items);
                let first_precedes = if fragment.direction
                    == crate::WritingDirection::RightToLeft
                {
                    fragment.bbox.right >= other.bbox.right
                } else {
                    fragment.bbox.left <= other.bbox.left
                };
                let items = if first_precedes {
                    first.into_iter().chain(other.items).collect()
                } else {
                    other.items.into_iter().chain(first).collect()
                };
                // Both fragments already own ordered script groups. Re-sorting the
                // expanded members would interleave a sum's upper and lower limits.
                *fragment =
                    LineFragment::from_ordered_items(items, page_width)?;
                // Extend the segment across the joined row while retaining its body height.
                fragment.baseline.start.y = baseline.start.y;
                fragment.baseline.end.y = baseline.end.y;
                fragment.metrics.baseline = fragment.baseline;
                fragment.metrics.font_size = font_size;
                fragment.metrics.font_size_estimated = estimated;
            }
        }
        Ok(fragments)
    }

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

    /// Raised and lowered small glyphs join a parent without moving its baseline or the next row.
    #[test]
    fn scripts_join_parent_line_and_preserve_body_baseline() {
        for bounds in [[32.0, 7.0, 36.0, 14.0], [32.0, 16.0, 36.0, 23.0]] {
            let inputs = vec![
                item(0, "body", [10.0, 10.0, 60.0, 20.0], 10.0),
                item(1, "2", bounds, 7.0),
                item(2, "next", [10.0, 24.0, 60.0, 34.0], 10.0),
            ];
            let forward = ConservativeLineAssembler
                .fragments(inputs.clone(), &FusionConfig::default())
                .expect("lines");
            let reverse = ConservativeLineAssembler
                .fragments(
                    inputs.into_iter().rev().collect(),
                    &FusionConfig::default(),
                )
                .expect("reverse lines");
            assert_eq!(forward, reverse);
            assert_eq!(forward.len(), 2);
            let parent = forward.first().expect("parent line");
            assert_eq!(
                parent
                    .items
                    .iter()
                    .map(|item| item.id.clone())
                    .collect::<Vec<_>>(),
                vec![TextItemId::native(1, 0), TextItemId::native(1, 1)]
            );
            assert!((parent.baseline.start.y - 20.0).abs() < 1e-6);
            assert!((parent.metrics.font_size - 10.0).abs() < 1e-6);
        }
    }

    /// Small type alone does not establish script ownership across baselines, columns or notes.
    #[test]
    fn small_non_script_text_remains_separate() {
        for (text, bounds, font_size) in [
            ("2", [32.0, 13.0, 36.0, 20.0], 7.0),
            ("2", [73.0, 16.0, 77.0, 23.0], 7.0),
            ("footnote", [32.0, 16.0, 58.0, 23.0], 7.0),
            ("2", [32.0, 16.0, 36.0, 23.0], 10.0),
        ] {
            let lines = ConservativeLineAssembler
                .fragments(
                    vec![
                        item(0, "body", [10.0, 10.0, 60.0, 20.0], 10.0),
                        item(1, text, bounds, font_size),
                    ],
                    &FusionConfig::default(),
                )
                .expect("lines");
            assert_eq!(
                lines.len(),
                2,
                "independent small text: {text} {bounds:?}"
            );
        }
    }

    /// Reconstructs two real mixed-math rows without moving their scripts ahead of prose.
    #[test]
    fn mixed_math_rows_preserve_inline_script_order() {
        // The excerpt uses an unrotated 792-point page. Baselines were mapped from
        // its PDF text matrices; bboxes and source text are unchanged copied facts.
        let rows: Vec<(u32, String, [f64; 4], f64, f64)> =
            serde_json::from_str(include_str!(
                "../../tests/fixtures/line/math-scripts.json"
            ))
            .expect("math source facts");
        for (scale, dx, dy) in [(1.0, 0.0, 0.0), (1.0, 200.0, 50.0)] {
            let items: Vec<_> = rows
                .iter()
                .map(|(index, text, bounds, size, baseline)| {
                    let [left, top, right, bottom] = *bounds;
                    let mut item = item(
                        *index,
                        text,
                        [
                            left * scale + dx,
                            top * scale + dy,
                            right * scale + dx,
                            bottom * scale + dy,
                        ],
                        size * scale,
                    );
                    item.baseline = Some(crate::Baseline {
                        start: docparse_layout::Point::new(
                            item.bbox.left,
                            baseline * scale + dy,
                        ),
                        end: docparse_layout::Point::new(
                            item.bbox.right,
                            baseline * scale + dy,
                        ),
                    });
                    item
                })
                .collect();
            let forward = ConservativeLineAssembler
                .fragments(items.clone(), &FusionConfig::default())
                .expect("math lines");
            let reverse = ConservativeLineAssembler
                .fragments(
                    items.into_iter().rev().collect(),
                    &FusionConfig::default(),
                )
                .expect("reverse math lines");
            assert_eq!(
                forward, reverse,
                "source iteration order must not change physical rows"
            );
            assert_eq!(
                forward
                    .iter()
                    .map(|line| crate::Line::derive_text(&line.items))
                    .collect::<Vec<_>>(),
                vec![
                    "Lemma 2. If q = δτ∗(r, q0τ) and no prefix of r is in L(τ ) i.e. ∄w1 ∈ Σ∗, w2 ∈ Σ∗such that w1.w2 =",
                    "r and δτ∗(w1, q0τ) ∈ Fτ then dmatch(t, q,Λ) ⇐⇒ dmatch(r.t, q0τ,Λ).",
                ]
            );
            assert_eq!(forward.iter().flat_map(|line| &line.items).count(), 81);
        }
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

    /// Measured equal baselines override misleading glyph bottoms when font sizes differ.
    #[test]
    fn measured_baselines_prevent_false_script_attachment() {
        let mut items = vec![
            item(0, "body", [10.0, 10.0, 60.0, 20.0], 10.0),
            item(1, "2", [32.0, 16.0, 36.0, 23.0], 7.0),
        ];
        for item in &mut items {
            item.baseline = Some(crate::Baseline {
                start: docparse_layout::Point::new(item.bbox.left, 20.0),
                end: docparse_layout::Point::new(item.bbox.right, 20.0),
            });
        }
        let lines = ConservativeLineAssembler
            .fragments(items, &FusionConfig::default())
            .expect("lines");
        assert_eq!(
            lines.len(),
            2,
            "a measured baseline has priority over the bbox bottom"
        );
    }

    /// Nested indices follow original parent relations without absorbing the following row.
    #[test]
    fn nested_scripts_follow_original_typographic_parent_chain() {
        let specs = [
            (0, "q", [10.0, 10.0, 15.0, 20.0], 10.0, 18.0),
            (1, "τ", [15.0, 6.0, 19.0, 12.0], 7.0, 10.5),
            (2, "f", [18.5, 9.0, 22.0, 13.0], 5.0, 11.8),
            (3, "0", [14.9, 18.0, 19.0, 24.0], 7.0, 22.0),
            (4, "next", [10.0, 30.0, 50.0, 40.0], 10.0, 38.0),
        ];
        let fragments = specs
            .into_iter()
            .map(|(index, text, bounds, size, y)| {
                let mut source = item(index, text, bounds, size);
                source.baseline = Some(crate::Baseline {
                    start: docparse_layout::Point::new(source.bbox.left, y),
                    end: docparse_layout::Point::new(source.bbox.right, y),
                });
                super::LineFragment::from_items(vec![source], 100.0)
                    .expect("source fragment")
            })
            .collect();
        let output = super::LineFragment::attach_scripts(fragments, 100.0)
            .expect("nested attachment");
        assert_eq!(
            output.len(),
            2,
            "a nested index must not become an independent line"
        );
        let parent = output.first().expect("parent");
        assert_eq!(crate::Line::derive_text(&parent.items), "q0τf");
        assert!((parent.baseline.start.y - 18.0).abs() < 1e-6);
        assert_eq!(output.last().expect("following row").items.len(), 1);
    }

    /// Nested scripts fill an apparent inline gap without joining the following body row.
    #[test]
    fn script_extents_bridge_fragments_on_the_same_body_baseline() {
        let specs = [
            (0, "q", [10.0, 10.0, 15.0, 20.0], 10.0, 18.0),
            (1, "τ", [15.0, 6.0, 19.0, 12.0], 7.0, 10.5),
            (2, "f+1", [18.5, 9.0, 28.5, 13.0], 5.0, 11.8),
            (3, "0", [14.9, 18.0, 19.0, 24.0], 7.0, 22.0),
            (4, ",Λ)", [32.0, 10.0, 48.0, 20.0], 10.0, 18.0),
            (5, "next", [10.0, 30.0, 50.0, 40.0], 10.0, 38.0),
        ];
        let sources = specs
            .into_iter()
            .map(|(index, text, bounds, size, y)| {
                let mut source = item(index, text, bounds, size);
                source.baseline = Some(crate::Baseline {
                    start: docparse_layout::Point::new(source.bbox.left, y),
                    end: docparse_layout::Point::new(source.bbox.right, y),
                });
                source
            })
            .collect();
        let lines = ConservativeLineAssembler
            .fragments(sources, &FusionConfig::default())
            .expect("physical lines");
        assert!(
            (lines.first().expect("first row").baseline.end.x - 48.0).abs()
                < 1e-6
        );
        assert_eq!(
            lines
                .iter()
                .map(|line| crate::Line::derive_text(&line.items))
                .collect::<Vec<_>>(),
            vec!["q0τf+1,Λ)", "next"]
        );
    }

    /// A nearby font-size difference cannot split an unrelated run into separate words.
    #[test]
    fn unrelated_small_font_run_is_not_split_into_words() {
        let parent = super::LineFragment::from_items(
            vec![item(0, "body", [0.0, 10.0, 20.0, 20.0], 10.0)],
            200.0,
        )
        .expect("parent");
        let note = super::LineFragment::from_items(
            vec![
                item(1, "small ", [100.0, 10.0, 115.0, 17.0], 7.0),
                item(2, "note", [116.0, 10.0, 130.0, 17.0], 7.0),
            ],
            200.0,
        )
        .expect("independent note");
        let output =
            super::LineFragment::attach_scripts(vec![parent, note], 200.0)
                .expect("attachment");
        assert_eq!(output.len(), 2);
        assert_eq!(output.last().expect("note").items.len(), 2);
    }

    /// Unknown typography and equally plausible parents must leave script ownership unresolved.
    #[test]
    fn scripts_require_known_typography_and_an_unambiguous_parent() {
        let base = item(0, "body", [10.0, 10.0, 60.0, 20.0], 10.0);
        let script = item(1, "2", [32.0, 16.0, 36.0, 23.0], 7.0);
        let mut unknown = script.clone();
        unknown.style = None;
        let lines = ConservativeLineAssembler
            .fragments(vec![base, unknown], &FusionConfig::default())
            .expect("unknown font");
        assert_eq!(lines.len(), 2);
        let items = vec![
            item(0, "left", [10.0, 10.0, 60.0, 20.0], 10.0),
            script,
            item(2, "right", [11.0, 10.0, 61.0, 20.0], 10.0),
        ];
        let fragments = items
            .into_iter()
            .map(|item| {
                super::LineFragment::from_items(vec![item], 100.0)
                    .expect("fragment")
            })
            .collect();
        let result = super::LineFragment::attach_scripts(fragments, 100.0)
            .expect("attachment");
        assert_eq!(
            result.len(),
            3,
            "equal candidates must not pick an arbitrary owner"
        );
    }

    /// A split nested index is reconstructed before per-region text ownership is decided.
    #[test]
    fn split_nested_script_run_stays_with_its_base_before_assignment() {
        let rows: Vec<(u32, String, [f64; 4], f64, f64)> =
            serde_json::from_str(include_str!(
                "../../tests/fixtures/line/nested-math-scripts.json"
            ))
            .expect("source formula");
        let fragments = rows
            .into_iter()
            .map(|(index, text, bounds, size, y)| {
                let mut source = item(index, &text, bounds, size);
                source.baseline = Some(crate::Baseline {
                    start: docparse_layout::Point::new(source.bbox.left, y),
                    end: docparse_layout::Point::new(source.bbox.right, y),
                });
                super::LineFragment::from_items(vec![source], 612.0)
                    .expect("source fragment")
            })
            .collect();
        let output = super::LineFragment::attach_scripts(fragments, 612.0)
            .expect("script attachment");
        let q = output
            .iter()
            .find(|fragment| {
                fragment
                    .items
                    .iter()
                    .any(|source| source.id == TextItemId::native(1, 8))
            })
            .expect("q owner");
        assert_eq!(crate::Line::derive_text(&q.items), "q0τf+1");
        assert!(
            !q.items
                .iter()
                .any(|source| source.id == TextItemId::native(1, 13)),
            "following punctuation keeps independent ownership"
        );
        assert_eq!(
            output.iter().flat_map(|fragment| &fragment.items).count(),
            17
        );
    }

    /// Device-grid rounding cannot turn an otherwise valid baseline shift into an orphan.
    #[test]
    fn rounded_pdfium_origins_do_not_orphan_subscripts() {
        let specs = [
            (
                0,
                "w",
                [336.16, 125.375, 343.283, 134.222],
                9.962599754333496,
                132.28900146484375,
            ),
            (
                1,
                "2",
                [343.292, 128.978, 347.26, 135.136],
                6.973800182342529,
                133.7830047607422,
            ),
            (
                2,
                "or",
                [351.083, 125.425, 359.96, 134.222],
                9.962599754333496,
                132.28900146484375,
            ),
        ];
        let fragments = specs
            .into_iter()
            .map(|(index, text, bounds, size, y)| {
                let mut source = item(index, text, bounds, size);
                source.baseline = Some(crate::Baseline {
                    start: docparse_layout::Point::new(source.bbox.left, y),
                    end: docparse_layout::Point::new(source.bbox.right, y),
                });
                super::LineFragment::from_items(vec![source], 612.0)
                    .expect("source")
            })
            .collect();
        let output = super::LineFragment::attach_scripts(fragments, 612.0)
            .expect("rounded baseline attachment");
        assert_eq!(output.len(), 2);
        assert_eq!(
            crate::Line::derive_text(&output.first().expect("base").items),
            "w2"
        );
    }

    /// A broad body fragment must not compete with the actual intermediate parent of a nested index.
    #[test]
    fn nested_index_uses_its_glyph_parent_instead_of_the_whole_body_box() {
        let items = [
            (
                0,
                "dmatch(",
                [220.043, 120.218, 255.223, 130.171],
                9.9626,
                127.690,
            ),
            (
                1,
                "r.t,",
                [255.233, 120.776, 268.563, 129.623],
                9.9626,
                127.690,
            ),
            (
                2,
                "q",
                [270.227, 120.776, 274.730, 129.623],
                9.9626,
                127.690,
            ),
            (
                3,
                "τ",
                [275.049, 118.422, 279.108, 124.615],
                6.9738,
                123.262,
            ),
            (
                4,
                "1",
                [278.723, 120.826, 282.110, 125.224],
                4.9813,
                124.258,
            ),
            (
                5,
                "0",
                [274.691, 125.541, 278.659, 131.699],
                6.9738,
                130.346,
            ),
            (
                6,
                ",Λ)",
                [283.109, 120.218, 302.926, 130.171],
                9.9626,
                127.690,
            ),
        ]
        .into_iter()
        .map(|(index, text, bounds, size, y)| {
            let mut source = item(index, text, bounds, size);
            source.baseline = Some(crate::Baseline {
                start: docparse_layout::Point::new(bounds[0], y),
                end: docparse_layout::Point::new(bounds[2], y),
            });
            source
        })
        .collect::<Vec<_>>();
        for reverse in [false, true] {
            let mut input = items.clone();
            if reverse {
                input.reverse();
            }
            let lines = ConservativeLineAssembler
                .fragments(input, &FusionConfig::default())
                .expect("nested index");
            assert_eq!(
                lines
                    .iter()
                    .map(|line| crate::Line::derive_text(&line.items))
                    .collect::<Vec<_>>(),
                ["dmatch(r.t,q0τ1,Λ)"]
            );
        }
    }

    /// A script cannot move a right-hand fragment ahead of its left-hand baseline peer.
    #[test]
    fn scripts_do_not_reorder_fragments_sharing_a_body_baseline() {
        let specs = [
            (0, "Proof.", [0.0, 10.0, 12.0, 20.0], 10.0, 18.0),
            (1, "(a) q", [40.0, 10.0, 65.0, 20.0], 10.0, 18.0),
            (2, "2", [65.0, 7.0, 69.0, 14.0], 7.0, 12.0),
            (3, "Next", [0.0, 30.0, 40.0, 40.0], 10.0, 38.0),
        ];
        let sources = specs
            .into_iter()
            .map(|(index, text, bounds, size, y)| {
                let mut source = item(index, text, bounds, size);
                source.baseline = Some(crate::Baseline {
                    start: docparse_layout::Point::new(source.bbox.left, y),
                    end: docparse_layout::Point::new(source.bbox.right, y),
                });
                source
            })
            .collect();
        let lines = ConservativeLineAssembler
            .fragments(sources, &FusionConfig::default())
            .expect("row order");
        assert_eq!(
            lines
                .iter()
                .map(|line| crate::Line::derive_text(&line.items))
                .collect::<Vec<_>>(),
            vec!["Proof.", "(a) q2", "Next"]
        );
    }

    /// An ordinary postfix index belongs to the preceding base, not following punctuation.
    #[test]
    fn postfix_script_does_not_choose_following_text() {
        let items = vec![
            item(0, "q", [10.0, 10.0, 16.0, 20.0], 10.0),
            item(1, "2", [16.3, 7.0, 20.3, 14.0], 7.0),
            item(2, ")", [21.0, 10.0, 25.0, 20.0], 10.0),
        ];
        let fragments = items
            .into_iter()
            .map(|source| {
                super::LineFragment::from_items(vec![source], 100.0)
                    .expect("source")
            })
            .collect();
        let output = super::LineFragment::attach_scripts(fragments, 100.0)
            .expect("postfix attachment");
        assert_eq!(output.len(), 2);
        assert_eq!(
            crate::Line::derive_text(&output.first().expect("base").items),
            "q2"
        );
        assert_eq!(
            crate::Line::derive_text(
                &output.last().expect("following text").items
            ),
            ")"
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
    /// Stacked fractions and hanging operators stay in their own equation without changing source glyphs.
    #[test]
    fn stacked_math_preserves_source_characters_and_equation_order() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/line/stacked-math.json"
        ))
        .expect("copied math facts");
        let facts: Vec<(u32, String, [f64; 4], f64, f64)> =
            serde_json::from_value(
                fixture.get("items").expect("fixture items").clone(),
            )
            .expect("source words");
        let items: Vec<_> = facts
            .iter()
            .map(|(index, text, bounds, size, y)| {
                let mut source = item(*index, text, *bounds, *size);
                source.baseline = Some(crate::Baseline {
                    start: docparse_layout::Point::new(bounds[0], *y),
                    end: docparse_layout::Point::new(bounds[2], *y),
                });
                source
            })
            .collect();
        let bars: Vec<[f64; 3]> = serde_json::from_value(
            fixture.get("rules").expect("fixture rules").clone(),
        )
        .expect("fraction bars");
        let rules: Vec<_> = bars
            .into_iter()
            .map(|[y, left, right]| crate::TableRule::Horizontal {
                y,
                left,
                right,
            })
            .collect();
        let expected = [
            "x−µσ· γ + β, µ = 1dPi=1 dxi, σ =q1dPi=1d(xi − µ))2",
            "xRMS(x)· γ, RMS(x) = q1dPi=1 dxi2",
            "LayerNorm(α · x + Sublayer(x))",
        ];
        for reverse in [false, true] {
            let mut input = items.clone();
            if reverse {
                input.reverse();
            }
            let result = ConservativeLineAssembler
                .fragments_with_rules(input, &FusionConfig::default(), &rules)
                .expect("equations");
            assert_eq!(
                result
                    .iter()
                    .map(|line| crate::Line::derive_text(&line.items))
                    .collect::<Vec<_>>(),
                expected
            );
            let mut actual: Vec<_> =
                result.into_iter().flat_map(|line| line.items).collect();
            actual.sort_by(|a, b| a.id.cmp(&b.id));
            let mut source = items.clone();
            source.sort_by(|a, b| a.id.cmp(&b.id));
            assert_eq!(actual, source, "ordering must not mutate source facts");
        }
    }
    /// Tight word boxes retain shallow, measured subscripts instead of moving all indices to the line end.
    #[test]
    fn tight_math_ink_preserves_shallow_subscript_ownership() {
        let specs = [
            ("x", [10.0, 6.45, 14.7, 10.0], 8.0, 10.0),
            ("1", [15.3, 7.15, 17.6, 11.12], 6.0, 11.12),
            ("+", [21.0, 5.18, 26.6, 10.82], 8.0, 10.0),
            ("x", [30.0, 6.45, 34.7, 10.0], 8.0, 10.0),
            ("2", [35.3, 7.15, 38.1, 11.12], 6.0, 11.12),
        ];
        let inputs = specs
            .into_iter()
            .enumerate()
            .map(|(index, (text, bounds, size, y))| {
                let mut source = item(index as u32, text, bounds, size);
                source.baseline = Some(crate::Baseline {
                    start: docparse_layout::Point::new(bounds[0], y),
                    end: docparse_layout::Point::new(bounds[2], y),
                });
                source
            })
            .collect();
        let lines = ConservativeLineAssembler
            .fragments(inputs, &FusionConfig::default())
            .expect("tight math line");
        assert_eq!(
            lines
                .iter()
                .map(|line| crate::Line::derive_text(&line.items))
                .collect::<Vec<_>>(),
            ["x1+x2"]
        );
    }

    /// Detached limits copied from real display equations stay beside their operator in either source order.
    #[test]
    fn display_operator_limits_stay_before_the_summand() {
        let cases: Vec<serde_json::Value> = serde_json::from_str(include_str!(
            "../../tests/fixtures/line/display-operator-limits.json"
        ))
        .expect("real display equations");
        for case in cases {
            let facts: Vec<(u32, String, [f64; 4], f64, f64)> =
                serde_json::from_value(
                    case.get("items").expect("items").clone(),
                )
                .expect("source glyphs");
            let sources: Vec<_> = facts
                .into_iter()
                .map(|(index, text, bounds, size, y)| {
                    let mut source = item(index, &text, bounds, size);
                    source.baseline = Some(crate::Baseline {
                        start: docparse_layout::Point::new(bounds[0], y),
                        end: docparse_layout::Point::new(bounds[2], y),
                    });
                    source
                })
                .collect();
            for reverse in [false, true] {
                let mut input = sources.clone();
                if reverse {
                    input.reverse();
                }
                let lines = ConservativeLineAssembler
                    .fragments_with_rules(input, &FusionConfig::default(), &[])
                    .expect("display equation");
                let texts: Vec<_> = lines
                    .iter()
                    .map(|line| crate::Line::derive_text(&line.items))
                    .collect();
                assert!(
                    texts.iter().any(|text| text.contains(
                        case.get("expected_group")
                            .and_then(serde_json::Value::as_str)
                            .expect("limit order")
                    )),
                    "{}: {texts:?}",
                    case.get("source").expect("source PDF")
                );
                let mut actual: Vec<_> =
                    lines.into_iter().flat_map(|line| line.items).collect();
                actual.sort_by(|a, b| a.id.cmp(&b.id));
                let mut expected = sources.clone();
                expected.sort_by(|a, b| a.id.cmp(&b.id));
                assert_eq!(
                    actual, expected,
                    "source facts must survive grouping"
                );
            }
        }
    }

    /// An isolated note or equally plausible operators must not fabricate a limit group.
    #[test]
    fn detached_limits_require_both_sides_and_unique_ownership() {
        let sources: Vec<_> = [
            (0, "+", [10.0, 10.0, 18.0, 20.0], 10.0, 18.0),
            (1, "X", [20.0, 8.0, 34.0, 22.0], 9.5, 8.0),
            (2, "N", [24.0, 1.0, 30.0, 7.0], 7.0, 6.0),
            (3, "i=1", [20.2, 24.0, 34.0, 30.2], 7.0, 29.0),
        ]
        .into_iter()
        .map(|(index, text, bounds, size, y)| {
            let mut source = item(index, text, bounds, size);
            source.baseline = Some(crate::Baseline {
                start: docparse_layout::Point::new(bounds[0], y),
                end: docparse_layout::Point::new(bounds[2], y),
            });
            source
        })
        .collect();
        for ambiguous in [false, true] {
            let mut input = sources.clone();
            if ambiguous {
                let mut duplicate = input.get(1).expect("operator").clone();
                duplicate.id = TextItemId::native(1, 4);
                input.push(duplicate);
            } else {
                input.pop();
            }
            let lines = ConservativeLineAssembler
                .fragments_with_rules(input, &FusionConfig::default(), &[])
                .expect("conservative limits");
            let upper = lines
                .iter()
                .find(|line| line.items.iter().any(|item| item.raw_text == "N"))
                .expect("unclaimed upper note");
            assert_eq!(crate::Line::derive_text(&upper.items), "N");
        }
    }

    /// An operator-bearing multi-piece index remains attached as a whole before following body text.
    #[test]
    fn compound_math_index_stays_before_following_body() {
        let specs = [
            ("R", [10.0, 10.0, 16.0, 18.0], 8.0, 18.0),
            ("Θ", [16.5, 13.2, 21.0, 18.5], 6.0, 19.3),
            (",", [22.0, 17.0, 23.0, 19.5], 6.0, 19.3),
            ("i", [24.0, 13.2, 26.0, 19.5], 6.0, 19.3),
            ("−", [27.0, 16.0, 31.0, 16.3], 6.0, 19.3),
            ("j", [32.0, 13.2, 35.0, 19.5], 6.0, 19.3),
            ("x", [36.0, 12.4, 41.0, 18.0], 8.0, 18.0),
        ];
        let inputs = specs
            .into_iter()
            .enumerate()
            .map(|(index, (text, bounds, size, y))| {
                let mut source = item(index as u32, text, bounds, size);
                source.baseline = Some(crate::Baseline {
                    start: docparse_layout::Point::new(bounds[0], y),
                    end: docparse_layout::Point::new(bounds[2], y),
                });
                source
            })
            .collect();
        let lines = ConservativeLineAssembler
            .fragments(inputs, &FusionConfig::default())
            .expect("compound script line");
        assert_eq!(
            lines
                .iter()
                .map(|line| crate::Line::derive_text(&line.items))
                .collect::<Vec<_>>(),
            ["RΘ,i−jx"]
        );
    }
    /// Equally plausible neighboring rows cannot move a hanging glyph into either equation.
    #[test]
    fn ambiguous_hanging_glyph_keeps_its_original_line() {
        let specs = [
            ("A", [0.0, 5.0, 8.0, 15.0], 13.0),
            ("Q", [10.0, 8.0, 15.0, 24.0], 8.0),
            ("B", [17.0, 17.0, 25.0, 27.0], 25.0),
        ];
        for reverse in [false, true] {
            let mut inputs: Vec<_> = specs
                .iter()
                .enumerate()
                .map(|(index, (text, bounds, y))| {
                    let mut source = item(index as u32, text, *bounds, 10.0);
                    source.baseline = Some(crate::Baseline {
                        start: docparse_layout::Point::new(bounds[0], *y),
                        end: docparse_layout::Point::new(bounds[2], *y),
                    });
                    source
                })
                .collect();
            if reverse {
                inputs.reverse();
            }
            let lines = ConservativeLineAssembler
                .fragments_with_rules(inputs, &FusionConfig::default(), &[])
                .expect("ambiguous glyph");
            assert_eq!(
                lines
                    .iter()
                    .map(|line| crate::Line::derive_text(&line.items))
                    .collect::<Vec<_>>(),
                ["Q", "A", "B"]
            );
        }
    }

    /// A horizontal rule alone cannot collapse ordinary stacked text into an inline fraction.
    #[test]
    fn separated_text_rows_do_not_become_fractions() {
        let mut inputs = vec![
            item(0, "first", [10.0, 5.0, 30.0, 12.0], 8.0),
            item(1, "second", [10.0, 18.0, 30.0, 25.0], 8.0),
        ];
        for source in &mut inputs {
            source.baseline = Some(crate::Baseline {
                start: docparse_layout::Point::new(
                    source.bbox.left,
                    source.bbox.bottom,
                ),
                end: docparse_layout::Point::new(
                    source.bbox.right,
                    source.bbox.bottom,
                ),
            });
        }
        let lines = ConservativeLineAssembler
            .fragments_with_rules(
                inputs,
                &FusionConfig::default(),
                &[crate::TableRule::Horizontal {
                    y: 15.0,
                    left: 10.0,
                    right: 30.0,
                }],
            )
            .expect("separate rows");
        assert_eq!(
            lines
                .iter()
                .map(|line| crate::Line::derive_text(&line.items))
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
    }
    /// Joining already ordered fragments preserves right-to-left flow across a physical line.
    #[test]
    fn joining_fragments_preserves_right_to_left_order() {
        let fragments = [
            item(0, "עולם", [10.0, 10.0, 30.0, 20.0], 10.0),
            item(1, "שלום", [40.0, 10.0, 60.0, 20.0], 10.0),
        ]
        .into_iter()
        .map(|item| {
            super::LineFragment::from_items(vec![item], 100.0)
                .expect("RTL fragment")
        })
        .collect();
        let lines = ConservativeLineAssembler::join_script_gaps(
            fragments,
            100.0,
            &FusionConfig::default(),
        )
        .expect("joined RTL line");
        assert_eq!(
            lines
                .iter()
                .map(|line| crate::Line::derive_text(&line.items))
                .collect::<Vec<_>>(),
            ["שלוםעולם"]
        );
    }
}
