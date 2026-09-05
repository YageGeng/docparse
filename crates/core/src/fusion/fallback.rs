// SPDX-License-Identifier: Apache-2.0
// XY-cut concepts are derived from LiteParse revision
// b2e76ec5b0c1cb4eb11d67296e916792f4fb5858 and adapted to DocParse regions.

use docparse_layout::Bbox;

use crate::RegionPath;
use crate::line::LineFragment;

const MAX_DEPTH: usize = 8;

/// Axis used by one recursive whitespace cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CutAxis {
    Horizontal,
    Vertical,
}

/// Leaf ownership or recursively ordered children for one region.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RegionKind {
    Leaf {
        fragment_indices: Vec<usize>,
    },
    Split {
        axis: CutAxis,
        children: Vec<RegionTree>,
    },
}

/// Stable XY-cut region tree with deterministic path and geometry.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RegionTree {
    pub(crate) path: RegionPath,
    pub(crate) bbox: Bbox,
    pub(crate) kind: RegionKind,
}

impl RegionTree {
    /// Visits leaves in deterministic pre-order without exposing mutable tree state.
    pub(crate) fn visit_leaves(
        &self,
        visitor: &mut impl FnMut(&RegionPath, &[usize]),
    ) {
        match &self.kind {
            RegionKind::Leaf { fragment_indices } => {
                visitor(&self.path, fragment_indices)
            }
            RegionKind::Split { children, .. } => {
                for child in children {
                    child.visit_leaves(visitor);
                }
            }
        }
    }
}

/// Recursively partitions residual line fragments without crossing model obstacles.
pub(crate) fn xy_cut(
    fragments: &[LineFragment],
    obstacles: &[Bbox],
    page_bbox: Bbox,
) -> RegionTree {
    let mut indices: Vec<_> = (0..fragments.len()).collect();
    sort_indices(&mut indices, fragments);
    build_region(
        fragments,
        obstacles,
        indices,
        page_bbox,
        RegionPath::root(),
        0,
    )
}

/// Builds one region and recurses only when a significant unobstructed gap exists.
fn build_region(
    fragments: &[LineFragment],
    obstacles: &[Bbox],
    mut indices: Vec<usize>,
    bbox: Bbox,
    path: RegionPath,
    depth: usize,
) -> RegionTree {
    sort_indices(&mut indices, fragments);
    if indices.len() <= 1 || depth >= MAX_DEPTH {
        return RegionTree {
            path,
            bbox,
            kind: RegionKind::Leaf {
                fragment_indices: indices,
            },
        };
    }

    if let Some((upper, lower)) = banner_partition(&indices, fragments, bbox) {
        let children = split_children(
            fragments,
            obstacles,
            [upper, lower],
            bbox,
            &path,
            CutAxis::Horizontal,
            depth,
        );
        return RegionTree {
            path,
            bbox,
            kind: RegionKind::Split {
                axis: CutAxis::Horizontal,
                children,
            },
        };
    }

    if let Some((left, right)) =
        vertical_partition(&indices, fragments, obstacles, bbox)
    {
        let children = split_children(
            fragments,
            obstacles,
            [left, right],
            bbox,
            &path,
            CutAxis::Vertical,
            depth,
        );
        return RegionTree {
            path,
            bbox,
            kind: RegionKind::Split {
                axis: CutAxis::Vertical,
                children,
            },
        };
    }

    if let Some((upper, lower)) = horizontal_partition(&indices, fragments) {
        let children = split_children(
            fragments,
            obstacles,
            [upper, lower],
            bbox,
            &path,
            CutAxis::Horizontal,
            depth,
        );
        return RegionTree {
            path,
            bbox,
            kind: RegionKind::Split {
                axis: CutAxis::Horizontal,
                children,
            },
        };
    }

    RegionTree {
        path,
        bbox,
        kind: RegionKind::Leaf {
            fragment_indices: indices,
        },
    }
}

/// Builds two spatially sorted child trees with axis-specific stable paths.
fn split_children(
    fragments: &[LineFragment],
    obstacles: &[Bbox],
    partitions: [Vec<usize>; 2],
    parent_bbox: Bbox,
    parent_path: &RegionPath,
    axis: CutAxis,
    depth: usize,
) -> Vec<RegionTree> {
    partitions
        .into_iter()
        .enumerate()
        .map(|(ordinal, indices)| {
            let path = match axis {
                CutAxis::Horizontal => {
                    parent_path.horizontal_child(ordinal as u32)
                }
                CutAxis::Vertical => parent_path.vertical_child(ordinal as u32),
            };
            // Partitions contain valid source indices; retaining the parent is a safe invariant fallback.
            let bbox =
                union_indices(&indices, fragments).unwrap_or(parent_bbox);
            build_region(fragments, obstacles, indices, bbox, path, depth + 1)
        })
        .collect()
}

/// Peels one or more wide top lines when a clear gap precedes remaining content.
fn banner_partition(
    indices: &[usize],
    fragments: &[LineFragment],
    bbox: Bbox,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let mut by_top = indices.to_vec();
    by_top.sort_by(|left, right| {
        match (fragments.get(*left), fragments.get(*right)) {
            (Some(left), Some(right)) => {
                left.bbox.top.total_cmp(&right.bbox.top)
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.cmp(right),
        }
    });
    let first = *by_top.first()?;
    let first_fragment = fragments.get(first)?;
    if first_fragment.bbox.width() < bbox.width() * 0.65 {
        return None;
    }
    let banner_bottom = first_fragment.bbox.bottom;
    let next_top = by_top
        .iter()
        .skip(1)
        .filter_map(|index| {
            fragments.get(*index).map(|fragment| fragment.bbox.top)
        })
        .fold(f64::INFINITY, f64::min);
    if !next_top.is_finite()
        || next_top - banner_bottom <= median_height(indices, fragments) * 3.0
    {
        return None;
    }
    let (upper, lower): (Vec<_>, Vec<_>) =
        indices.iter().copied().partition(|index| {
            fragments.get(*index).is_some_and(|fragment| {
                fragment.bbox.bottom <= banner_bottom + 1.0e-9
            })
        });
    (!upper.is_empty() && !lower.is_empty()).then_some((upper, lower))
}

/// Finds the largest positive vertical gutter with content on both sides.
fn vertical_partition(
    indices: &[usize],
    fragments: &[LineFragment],
    obstacles: &[Bbox],
    region: Bbox,
) -> Option<(Vec<usize>, Vec<usize>)> {
    let mut by_center = indices.to_vec();
    by_center.sort_by(|left, right| {
        match (fragments.get(*left), fragments.get(*right)) {
            (Some(left), Some(right)) => {
                left.bbox.center().x.total_cmp(&right.bbox.center().x)
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.cmp(right),
        }
    });
    let mut best: Option<(f64, Vec<usize>, Vec<usize>)> = None;
    for split in 1..by_center.len() {
        let (left_slice, right_slice) = by_center.split_at(split);
        if left_slice.len() < 2 || right_slice.len() < 2 {
            continue;
        }
        let left_edge = left_slice
            .iter()
            .filter_map(|index| {
                fragments.get(*index).map(|fragment| fragment.bbox.right)
            })
            .fold(f64::NEG_INFINITY, f64::max);
        let right_edge = right_slice
            .iter()
            .filter_map(|index| {
                fragments.get(*index).map(|fragment| fragment.bbox.left)
            })
            .fold(f64::INFINITY, f64::min);
        let gap = right_edge - left_edge;
        let minimum_gap = (median_height(indices, fragments) * 2.0)
            .max(region.width() * 0.03);
        let obstacle_crosses = obstacles.iter().any(|obstacle| {
            obstacle.left < right_edge
                && obstacle.right > left_edge
                && obstacle.top < region.bottom
                && obstacle.bottom > region.top
        });
        if gap < minimum_gap || obstacle_crosses {
            continue;
        }
        if best.as_ref().is_none_or(|(best_gap, _, _)| gap > *best_gap) {
            best = Some((gap, left_slice.to_vec(), right_slice.to_vec()));
        }
    }
    best.map(|(_, left, right)| (left, right))
}

/// Finds a horizontal band gap large enough not to split ordinary line spacing.
fn horizontal_partition(
    indices: &[usize],
    fragments: &[LineFragment],
) -> Option<(Vec<usize>, Vec<usize>)> {
    let mut by_center = indices.to_vec();
    by_center.sort_by(|left, right| {
        match (fragments.get(*left), fragments.get(*right)) {
            (Some(left), Some(right)) => {
                left.bbox.center().y.total_cmp(&right.bbox.center().y)
            }
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.cmp(right),
        }
    });
    let threshold = median_height(indices, fragments) * 3.0;
    let mut best: Option<(f64, Vec<usize>, Vec<usize>)> = None;
    for split in 1..by_center.len() {
        let (upper_slice, lower_slice) = by_center.split_at(split);
        let upper_edge = upper_slice
            .iter()
            .filter_map(|index| {
                fragments.get(*index).map(|fragment| fragment.bbox.bottom)
            })
            .fold(f64::NEG_INFINITY, f64::max);
        let lower_edge = lower_slice
            .iter()
            .filter_map(|index| {
                fragments.get(*index).map(|fragment| fragment.bbox.top)
            })
            .fold(f64::INFINITY, f64::min);
        let gap = lower_edge - upper_edge;
        if gap >= threshold
            && best.as_ref().is_none_or(|(best_gap, _, _)| gap > *best_gap)
        {
            best = Some((gap, upper_slice.to_vec(), lower_slice.to_vec()));
        }
    }
    best.map(|(_, upper, lower)| (upper, lower))
}

/// Returns median fragment height for scale-aware gap thresholds.
fn median_height(indices: &[usize], fragments: &[LineFragment]) -> f64 {
    let mut heights: Vec<_> = indices
        .iter()
        .filter_map(|index| {
            fragments.get(*index).map(|fragment| fragment.bbox.height())
        })
        .filter(|height| height.is_finite() && *height > 0.0)
        .collect();
    heights.sort_by(f64::total_cmp);
    let middle = heights.len() / 2;
    match heights.as_slice() {
        [] => 1.0,
        values if values.len().is_multiple_of(2) => {
            let lower =
                values.get(middle.saturating_sub(1)).copied().unwrap_or(1.0);
            let upper = values.get(middle).copied().unwrap_or(lower);
            (lower + upper) / 2.0
        }
        values => values.get(middle).copied().unwrap_or(1.0),
    }
}

/// Returns the union bbox of a valid non-empty fragment index set.
fn union_indices(
    indices: &[usize],
    fragments: &[LineFragment],
) -> Option<Bbox> {
    let first = indices.first().and_then(|index| fragments.get(*index))?;
    indices.iter().skip(1).try_fold(first.bbox, |bbox, index| {
        let other = fragments.get(*index)?.bbox;
        Bbox::try_from([
            bbox.left.min(other.left),
            bbox.top.min(other.top),
            bbox.right.max(other.right),
            bbox.bottom.max(other.bottom),
        ])
        .ok()
    })
}

/// Sorts fragment indices by geometry and stable first-item identity.
fn sort_indices(indices: &mut [usize], fragments: &[LineFragment]) {
    indices.sort_by(|left, right| {
        match (fragments.get(*left), fragments.get(*right)) {
            (Some(left_fragment), Some(right_fragment)) => left_fragment
                .bbox
                .top
                .total_cmp(&right_fragment.bbox.top)
                .then_with(|| {
                    left_fragment.bbox.left.total_cmp(&right_fragment.bbox.left)
                })
                .then_with(|| {
                    left_fragment
                        .items
                        .first()
                        .map(|item| item.id.as_str())
                        .cmp(
                            &right_fragment
                                .items
                                .first()
                                .map(|item| item.id.as_str()),
                        )
                }),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.cmp(right),
        }
    });
}

#[cfg(test)]
mod tests {
    use docparse_layout::Bbox;

    use super::{CutAxis, RegionKind, RegionTree, xy_cut};
    use crate::line::LineFragment;
    use crate::line::metrics::LineMetrics;
    use crate::{
        Baseline, RegionPath, TextItem, TextItemId, TextSource,
        WritingDirection,
    };

    /// Builds one single-item line fragment for XY-cut tests.
    fn fragment(index: u32, bbox: [f64; 4]) -> LineFragment {
        let bbox = Bbox::try_from(bbox).expect("test bbox must be valid");
        let item = TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text(format!("item-{index}"))
            .bbox(bbox)
            .source(TextSource::Native)
            .build();
        let baseline = Baseline {
            start: docparse_layout::Point::new(bbox.left, bbox.bottom),
            end: docparse_layout::Point::new(bbox.right, bbox.bottom),
        };
        let metrics = LineMetrics::builder()
            .font_size(bbox.height())
            .bold_ratio(0.0)
            .italic_ratio(0.0)
            .bbox(bbox)
            .baseline(baseline)
            .anchor(crate::line::LineAnchor::Left)
            .indent(bbox.left)
            .build();
        LineFragment::builder()
            .items(vec![item])
            .bbox(bbox)
            .baseline(baseline)
            .direction(WritingDirection::LeftToRight)
            .metrics(metrics)
            .rotation(0.0)
            .build()
    }

    /// Returns every leaf path paired with its stable text item ID.
    fn leaf_assignments(
        tree: &RegionTree,
        fragments: &[LineFragment],
    ) -> Vec<(String, String)> {
        let mut assignments = Vec::new();
        tree.visit_leaves(&mut |path, indices| {
            for index in indices {
                let id = fragments
                    .get(*index)
                    .and_then(|fragment| fragment.items.first())
                    .expect("leaf indices must reference a text item")
                    .id
                    .as_str()
                    .to_owned();
                assignments.push((id, path.as_str().to_owned()));
            }
        });
        assignments.sort();
        assignments
    }

    /// Verifies tight single-column prose remains one leaf.
    #[test]
    fn single_column_returns_one_leaf() {
        let fragments: Vec<_> = (0..6)
            .map(|row| {
                fragment(
                    row,
                    [
                        50.0,
                        50.0 + f64::from(row) * 14.0,
                        450.0,
                        60.0 + f64::from(row) * 14.0,
                    ],
                )
            })
            .collect();

        let tree = xy_cut(
            &fragments,
            &[],
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("valid page"),
        );

        assert!(matches!(tree.kind, RegionKind::Leaf { .. }));
        assert_eq!(tree.path, RegionPath::root());
    }

    /// Verifies a clear two-column gutter creates left and right stable paths.
    #[test]
    fn two_columns_split_vertically() {
        let mut fragments = Vec::new();
        for row in 0..4 {
            let top = 100.0 + f64::from(row) * 14.0;
            fragments.push(fragment(row * 2, [50.0, top, 270.0, top + 10.0]));
            fragments
                .push(fragment(row * 2 + 1, [340.0, top, 560.0, top + 10.0]));
        }

        let tree = xy_cut(
            &fragments,
            &[],
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("valid page"),
        );

        assert!(matches!(
            tree.kind,
            RegionKind::Split {
                axis: CutAxis::Vertical,
                ..
            }
        ));
        let paths: Vec<_> = leaf_assignments(&tree, &fragments)
            .into_iter()
            .map(|(_, path)| path)
            .collect();
        assert!(paths.iter().any(|path| path == "r.v0"));
        assert!(paths.iter().any(|path| path == "r.v1"));
    }

    /// Verifies a full-width title is peeled before the body splits into columns.
    #[test]
    fn banner_precedes_two_column_body() {
        let mut fragments = vec![fragment(0, [80.0, 40.0, 532.0, 60.0])];
        for row in 0..4 {
            let top = 180.0 + f64::from(row) * 14.0;
            fragments
                .push(fragment(row * 2 + 1, [50.0, top, 270.0, top + 10.0]));
            fragments
                .push(fragment(row * 2 + 2, [340.0, top, 560.0, top + 10.0]));
        }

        let tree = xy_cut(
            &fragments,
            &[],
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("valid page"),
        );
        let assignments = leaf_assignments(&tree, &fragments);

        assert!(matches!(
            tree.kind,
            RegionKind::Split {
                axis: CutAxis::Horizontal,
                ..
            }
        ));
        assert_eq!(
            assignments
                .iter()
                .find(|(id, _)| id == "p1:t0")
                .map(|(_, path)| path.as_str()),
            Some("r.h0")
        );
        assert!(
            assignments
                .iter()
                .any(|(_, path)| path.starts_with("r.h1.v"))
        );
    }

    /// Verifies obstacles prevent a vertical cut from crossing model geometry.
    #[test]
    fn obstacle_blocks_crossing_gutter_cut() {
        let fragments = vec![
            fragment(0, [50.0, 100.0, 270.0, 110.0]),
            fragment(1, [340.0, 100.0, 560.0, 110.0]),
            fragment(2, [50.0, 130.0, 270.0, 140.0]),
            fragment(3, [340.0, 130.0, 560.0, 140.0]),
        ];
        let obstacle = Bbox::try_from([250.0, 80.0, 360.0, 180.0])
            .expect("valid obstacle");

        let tree = xy_cut(
            &fragments,
            &[obstacle],
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("valid page"),
        );

        assert!(matches!(tree.kind, RegionKind::Leaf { .. }));
    }

    /// Verifies input permutation cannot change item-to-path assignments.
    #[test]
    fn paths_are_stable_under_input_permutation() {
        let forward = vec![
            fragment(0, [50.0, 100.0, 270.0, 110.0]),
            fragment(1, [340.0, 100.0, 560.0, 110.0]),
            fragment(2, [50.0, 130.0, 270.0, 140.0]),
            fragment(3, [340.0, 130.0, 560.0, 140.0]),
        ];
        let mut reverse = forward.clone();
        reverse.reverse();
        let page =
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("valid page");

        let forward_tree = xy_cut(&forward, &[], page);
        let reverse_tree = xy_cut(&reverse, &[], page);

        assert_eq!(
            leaf_assignments(&forward_tree, &forward),
            leaf_assignments(&reverse_tree, &reverse)
        );
    }
}
