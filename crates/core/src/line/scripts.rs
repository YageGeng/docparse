use super::LineFragment;
use crate::{LineError, WritingDirection};

impl LineFragment {
    /// Scores a short raised/lowered run against a nearby upright parent using original metrics.
    fn script_parent_score(&self, parent: &Self) -> Option<f64> {
        let size = parent.metrics.font_size;
        let ratio = self.metrics.font_size / size;
        // A compact index such as Theta,i-j may exceed four source characters.
        // Require operator evidence for the larger bound so ordinary small-font
        // words and notes do not gain the same attachment allowance.
        let compound = self
            .items
            .iter()
            .any(|item| item.raw_text.contains(['+', '-', '−', '=', ',']));
        let maximum_characters = if compound { 12 } else { 4 };
        let upright = |fragment: &Self| {
            let rotation = fragment.rotation.rem_euclid(360.0);
            rotation.min(360.0 - rotation) <= 2.0
                && fragment.direction != WritingDirection::Vertical
        };
        // Typography is required: bbox height alone also describes unrelated notes and
        // rotated text. Bound run length/width so a small paragraph cannot become a script.
        if !upright(self)
            || !upright(parent)
            || self.metrics.font_size_estimated
            || parent.metrics.font_size_estimated
            || !size.is_finite()
            || size <= 0.0
            || !(0.45..=0.85).contains(&ratio)
            || !(1..=maximum_characters).contains(
                &self
                    .items
                    .iter()
                    .flat_map(|item| item.raw_text.chars())
                    .filter(|character| !character.is_whitespace())
                    .take(maximum_characters + 1)
                    .count(),
            )
            || self.bbox.width() > size * if compound { 3.0 } else { 2.0 }
            // Tight ink for a digit can be taller than a lowercase base such as x.
            // The known parent font, rather than that glyph's ink height, bounds a script.
            || self.bbox.height() > size
        {
            return None;
        }
        // A whole line can geometrically overlap every script and tie with its
        // real intermediate parent (for example the 1 under tau above q). Score
        // actual source runs instead of inventing a base from that union box.
        if parent.items.len() > 1 {
            return parent
                .items
                .iter()
                .filter_map(|item| {
                    let mut glyph = Self::from_items(
                        vec![item.clone()],
                        parent.bbox.right.max(1.0),
                    )
                    .ok()?;
                    // Tight lowercase ink may barely overlap a valid superscript.
                    // Retain the established row band while limiting horizontal
                    // reach to the actual base run; source geometry stays untouched.
                    glyph.bbox.top = parent.bbox.top;
                    glyph.bbox.bottom = parent.bbox.bottom;
                    self.script_parent_score(&glyph)
                })
                .min_by(f64::total_cmp);
        }
        // Ordinary scripts follow their base in reading direction. A following word
        // or delimiter must not tie with the preceding base and orphan the index.
        if (parent.direction == WritingDirection::LeftToRight
            && self.bbox.center().x < parent.bbox.left)
            || (parent.direction == WritingDirection::RightToLeft
                && self.bbox.center().x > parent.bbox.right)
        {
            return None;
        }
        let overlap = self.bbox.bottom.min(parent.bbox.bottom)
            - self.bbox.top.max(parent.bbox.top);
        let gap = (self.bbox.left - parent.bbox.right)
            .max(parent.bbox.left - self.bbox.right)
            .max(0.0);
        // Prefer copied text baselines when available. Glyph descenders can move a
        // bbox bottom even when small text shares the body's actual baseline.
        let baseline = |fragment: &Self| {
            fragment
                .items
                .iter()
                .filter_map(|item| item.baseline)
                .map(|baseline| (baseline.start.y + baseline.end.y) * 0.5)
                .reduce(f64::min)
                .unwrap_or(fragment.baseline.start.y)
        };
        let baseline_shift = (baseline(self) - baseline(parent)).abs();
        let center_shift =
            (self.bbox.center().y - parent.bbox.center().y).abs();
        // Require a shifted baseline and an actual shared vertical band. A nearby next
        // row or same-baseline font change must not join the parent via this exception.
        // page_to_viewport uses an integer device grid at 1000x scale. Allow two
        // coordinate rounding units at the boundary instead of orphaning a valid index.
        let baseline_tolerance = 0.002;
        // Measured origins distinguish shallow TeX subscripts from same-baseline
        // font changes even when tight word boxes barely move their ink centers.
        let minimum_shift =
            if self.items.iter().all(|item| item.baseline.is_some())
                && parent.items.iter().all(|item| item.baseline.is_some())
            {
                0.1
            } else {
                0.15
            };
        if overlap < self.bbox.height() * 0.2
            || gap > size * 0.25
            || baseline_shift + baseline_tolerance < size * minimum_shift
            || baseline_shift > size * 0.9 + baseline_tolerance
            || center_shift < size * minimum_shift
        {
            return None;
        }
        Some((center_shift + gap) / size)
    }

    /// Selects a typographic parent only when it clearly outranks every competing source run.
    pub(super) fn script_parent(&self, fragments: &[Self]) -> Option<usize> {
        let mut best: Option<(usize, f64)> = None;
        let mut runner_up = f64::INFINITY;
        for (index, parent) in fragments.iter().enumerate() {
            // The strict font-size ratio also excludes the fragment itself.
            let Some(score) = self.script_parent_score(parent) else {
                continue;
            };
            if best.is_none_or(|(_, current)| score < current) {
                runner_up = best.map_or(f64::INFINITY, |(_, current)| current);
                best = Some((index, score));
            } else {
                runner_up = runner_up.min(score);
            }
        }
        best.filter(|(_, score)| runner_up - score > 0.05)
            .map(|(index, _)| index)
    }

    /// Joins adjacent pieces of one index only when their combined geometry fits a parent.
    fn join_script_pieces(
        mut fragments: Vec<Self>,
        page_width: f64,
    ) -> Result<Vec<Self>, LineError> {
        // PDF text objects may split one index such as f+1 into adjacent pieces.
        // Join only short, same-font continuations that still fit an existing larger
        // parent. This does not assemble ordinary body lines before column assignment.
        loop {
            let pair =
                fragments.iter().enumerate().find_map(|(index, seed)| {
                    fragments
                        .iter()
                        .filter(|parent| {
                            seed.script_parent_score(parent).is_some()
                        })
                        .find_map(|parent| {
                            fragments.iter().enumerate().find_map(
                                |(next_index, next)| {
                                    if next_index == index {
                                        return None;
                                    }
                                    let size = seed.metrics.font_size;
                                    let gap = if parent.direction
                                        == WritingDirection::RightToLeft
                                    {
                                        seed.bbox.left - next.bbox.right
                                    } else {
                                        next.bbox.left - seed.bbox.right
                                    };
                                    if (size - next.metrics.font_size).abs()
                                        > size * 0.05
                                        || (seed.baseline.start.y
                                            - next.baseline.start.y)
                                            .abs()
                                            > size * 0.1
                                        || gap < -size * 0.1
                                        || gap > size * 0.5
                                        || seed
                                            .items
                                            .iter()
                                            .chain(&next.items)
                                            .flat_map(|item| {
                                                item.raw_text.chars()
                                            })
                                            .filter(|c| !c.is_whitespace())
                                            .take(13)
                                            .count()
                                            > 12
                                    {
                                        return None;
                                    }
                                    let items = seed
                                        .items
                                        .iter()
                                        .chain(&next.items)
                                        .cloned()
                                        .collect();
                                    Self::from_items(items, page_width)
                                        .ok()
                                        .filter(|run| {
                                            run.script_parent_score(parent)
                                                .is_some()
                                        })
                                        .map(|_| {
                                            (
                                                index.min(next_index),
                                                index.max(next_index),
                                            )
                                        })
                                },
                            )
                        })
                });
            let Some((left, right)) = pair else {
                break;
            };
            let other = fragments.remove(right);
            if let Some(fragment) = fragments.get_mut(left) {
                let mut items = std::mem::take(&mut fragment.items);
                items.extend(other.items);
                *fragment = Self::from_items(items, page_width)?;
            }
        }
        Ok(fragments)
    }

    /// Moves scripts, including nested indices, through original typographic parent relations.
    #[allow(
        clippy::indexing_slicing,
        reason = "owner and child indices refer to the unchanged fragment vector; windows always contain two items"
    )]
    pub(crate) fn attach_scripts(
        fragments: Vec<Self>,
        page_width: f64,
    ) -> Result<Vec<Self>, LineError> {
        let fragments = Self::join_script_pieces(fragments, page_width)?;
        // A same-baseline script band can contain indices of different bases (for
        // example the two indices in w1.w2). Reconsider its original items separately
        // instead of attaching a wide synthetic run or leaving all its indices orphaned.
        let split: Vec<_> = fragments
            .iter()
            .map(|fragment| {
                fragment.items.len() > 1
                    && !fragment.metrics.font_size_estimated
                    && (!fragments.iter().any(|parent| {
                        fragment.script_parent_score(parent).is_some()
                    }) || fragment.items.windows(2).any(|pair| {
                        pair[1].bbox.left - pair[0].bbox.right
                            > fragment.metrics.font_size * 0.5
                    }))
                    && fragments.iter().any(|parent| {
                        !parent.metrics.font_size_estimated
                            && (0.45..=0.85).contains(
                                &(fragment.metrics.font_size
                                    / parent.metrics.font_size),
                            )
                            && fragment.bbox.bottom.min(parent.bbox.bottom)
                                > fragment.bbox.top.max(parent.bbox.top)
                            && fragment.items.iter().any(|item| {
                                // Leave the originals untouched; split a band only when an
                                // individual item can actually join a parent.
                                Self::from_items(vec![item.clone()], page_width)
                                    .is_ok_and(|single| {
                                        single
                                            .script_parent_score(parent)
                                            .is_some()
                                    })
                            })
                    })
            })
            .collect();
        let mut candidates = Vec::with_capacity(fragments.len());
        for (fragment, split) in fragments.into_iter().zip(split) {
            if split {
                for item in fragment.items {
                    candidates.push(Self::from_items(vec![item], page_width)?);
                }
            } else {
                candidates.push(fragment);
            }
        }
        // Splitting a mixed band must not leave a genuine multi-part limit such
        // as i=1 exposed to interleaving with its neighboring upper limit.
        let fragments = Self::join_script_pieces(candidates, page_width)?;
        // Select every owner from the original geometry. Do not let a newly attached
        // script enlarge the search band and drag an adjacent row into the same line.
        let owners: Vec<_> = fragments
            .iter()
            // Formula membership and final attachment must use the same ambiguity guard.
            .map(|script| script.script_parent(&fragments))
            .collect();
        let mut order: Vec<_> = (0..fragments.len()).collect();
        order.sort_by(|&a, &b| {
            fragments[a]
                .metrics
                .font_size
                .total_cmp(&fragments[b].metrics.font_size)
        });
        let mut children: Vec<Vec<Self>> =
            (0..fragments.len()).map(|_| Vec::new()).collect();
        let mut slots: Vec<_> = fragments.into_iter().map(Some).collect();
        let mut result = Vec::new();
        // Every parent has a strictly larger font. Build child atoms first, keeping
        // a split index such as i=1 together when it meets a sibling upper index.
        for index in order {
            let Some(mut fragment) =
                slots.get_mut(index).and_then(Option::take)
            else {
                continue;
            };
            let nested = std::mem::take(&mut children[index]);
            if !nested.is_empty() {
                let mut atoms: Vec<_> = std::mem::take(&mut fragment.items)
                    .into_iter()
                    .map(|item| (item.bbox, vec![item]))
                    .collect();
                atoms.extend(
                    nested.into_iter().map(|child| (child.bbox, child.items)),
                );
                atoms.sort_by(|a, b| {
                    if fragment.direction == WritingDirection::RightToLeft {
                        b.0.right.total_cmp(&a.0.right)
                    } else {
                        a.0.left.total_cmp(&b.0.left)
                    }
                });
                let items =
                    atoms.into_iter().flat_map(|(_, items)| items).collect();
                let mut rebuilt = Self::from_ordered_items(items, page_width)?;
                rebuilt.baseline = fragment.baseline;
                rebuilt.metrics.baseline = fragment.baseline;
                rebuilt.metrics.font_size = fragment.metrics.font_size;
                rebuilt.metrics.font_size_estimated =
                    fragment.metrics.font_size_estimated;
                fragment = rebuilt;
            }
            if let Some(parent) = owners[index] {
                children[parent].push(fragment);
            } else {
                result.push(fragment);
            }
        }
        result.sort_by(|a, b| {
            a.reading_order_y()
                .total_cmp(&b.reading_order_y())
                .then_with(|| a.bbox.left.total_cmp(&b.bbox.left))
        });
        Ok(result)
    }
}
