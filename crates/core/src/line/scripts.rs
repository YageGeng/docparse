use super::LineFragment;
use crate::{LineError, WritingDirection};

impl LineFragment {
    /// Scores a short raised/lowered run against a nearby upright parent using original metrics.
    fn script_parent_score(&self, parent: &Self) -> Option<f64> {
        let size = parent.metrics.font_size;
        let ratio = self.metrics.font_size / size;
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
            || !(1..=4).contains(
                &self
                    .items
                    .iter()
                    .flat_map(|item| item.raw_text.chars())
                    .filter(|character| !character.is_whitespace())
                    .take(5)
                    .count(),
            )
            || self.bbox.width() > size * 2.0
            || self.bbox.height() > parent.bbox.height()
        {
            return None;
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
        if overlap < self.bbox.height() * 0.2
            || gap > size * 0.25
            || baseline_shift + baseline_tolerance < size * 0.15
            || baseline_shift > size * 0.9 + baseline_tolerance
            || center_shift < size * 0.15
        {
            return None;
        }
        Some((center_shift + gap) / size)
    }

    /// Moves scripts, including nested indices, through original typographic parent relations.
    pub(crate) fn attach_scripts(
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
                                            .take(5)
                                            .count()
                                            > 4
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
        // A same-baseline script band can contain indices of different bases (for
        // example the two indices in w1.w2). Reconsider its original items separately
        // instead of attaching a wide synthetic run or leaving all its indices orphaned.
        let split: Vec<_> = fragments
            .iter()
            .map(|fragment| {
                fragment.items.len() > 1
                    && !fragment.metrics.font_size_estimated
                    && !fragments.iter().any(|parent| {
                        fragment.script_parent_score(parent).is_some()
                    })
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
        let fragments = candidates;
        // Select every owner from the original geometry. Do not let a newly attached
        // script enlarge the search band and drag an adjacent row into the same line.
        let owners: Vec<_> = fragments
            .iter()
            .enumerate()
            .map(|(index, script)| {
                let mut best: Option<(usize, f64)> = None;
                let mut runner_up = f64::INFINITY;
                for (parent_index, parent) in fragments.iter().enumerate() {
                    if index == parent_index {
                        continue;
                    }
                    let Some(score) = script.script_parent_score(parent) else {
                        continue;
                    };
                    if best.is_none_or(|(_, current)| score < current) {
                        runner_up =
                            best.map_or(f64::INFINITY, |(_, current)| current);
                        best = Some((parent_index, score));
                    } else {
                        runner_up = runner_up.min(score);
                    }
                }
                best.filter(|(_, score)| runner_up - score > 0.05)
                    .map(|(index, _)| index)
            })
            .collect();
        let mut changed = vec![false; fragments.len()];
        let mut slots: Vec<_> = fragments.into_iter().map(Some).collect();
        for (index, owner) in owners.iter().enumerate() {
            let Some(mut owner) = *owner else {
                continue;
            };
            // Every edge goes to a strictly larger font, so the original parent graph
            // is acyclic. Follow it for nested indices without using any expanded bbox.
            while let Some(next) = owners.get(owner).copied().flatten() {
                owner = next;
            }
            let Some(script) = slots.get_mut(index).and_then(Option::take)
            else {
                continue;
            };
            if let Some(parent) = slots.get_mut(owner).and_then(Option::as_mut)
            {
                parent.items.extend(script.items);
                if let Some(changed) = changed.get_mut(owner) {
                    *changed = true;
                }
            }
        }
        slots
            .into_iter()
            .zip(changed)
            .filter_map(|(slot, changed)| {
                slot.map(|mut fragment| {
                    if changed {
                        let mut rebuilt = Self::from_items(
                            std::mem::take(&mut fragment.items),
                            page_width,
                        )?;
                        // Script extents change the bbox, not the body's baseline or font.
                        rebuilt.baseline = fragment.baseline;
                        rebuilt.metrics.baseline = fragment.baseline;
                        rebuilt.metrics.font_size = fragment.metrics.font_size;
                        rebuilt.metrics.font_size_estimated =
                            fragment.metrics.font_size_estimated;
                        Ok(rebuilt)
                    } else {
                        Ok(fragment)
                    }
                })
            })
            .collect()
    }
}
