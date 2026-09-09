use std::collections::{BTreeMap, BTreeSet};

use docparse_layout::{Bbox, Point};

use super::LineFragment;
use crate::{Baseline, LineError, TableRule, TextItem, TextItemId};

/// Temporary inline geometry; final lines always recover the original source facts.
#[derive(Default)]
pub(super) struct InlineInput {
    pub items: Vec<TextItem>,
    pub(super) originals: BTreeMap<TextItemId, Vec<TextItem>>,
}

impl TextItem {
    /// Identifies single glyphs whose ink hangs below their origin instead of resting on a text baseline.
    fn has_hanging_baseline(&self) -> bool {
        self.style.as_ref().is_some_and(|style| {
            let size = style.font_size.unwrap_or(0.0);
            size > 0.0
                && self.raw_text.trim().chars().count() == 1
                && self.baseline.is_some_and(|baseline| {
                    self.bbox.bottom - baseline.start.y > size * 0.5
                        && baseline.start.y <= self.bbox.center().y
                })
                && self
                    .rotation
                    .rem_euclid(360.0)
                    .min(360.0 - self.rotation.rem_euclid(360.0))
                    <= 2.0
        })
    }
}

impl InlineInput {
    /// Uses local typography and fraction bars to form atoms before ordinary baseline grouping.
    #[allow(
        clippy::indexing_slicing,
        reason = "word indices are collected from the current immutable item vector before replacement"
    )]
    pub fn prepare(
        mut items: Vec<TextItem>,
        rules: &[TableRule],
        confirmed_formula: bool,
    ) -> Result<Self, LineError> {
        let mut originals = BTreeMap::new();
        // Compute every attachment from unchanged neighbors. A tall symbol must not
        // move another symbol into a neighboring equation through a chain of guesses.
        let baselines: Vec<_> = items
            .iter()
            .map(|item| {
                if !item.has_hanging_baseline() {
                    return None;
                }
                let size = item.style.as_ref()?.font_size?;
                let mut candidates: Vec<_> = items
                    .iter()
                    .filter(|neighbor| !neighbor.has_hanging_baseline())
                    .filter_map(|neighbor| {
                        let neighbor_size =
                            neighbor.style.as_ref()?.font_size?;
                        let baseline = neighbor.baseline?.start.y;
                        let overlap =
                            item.bbox.bottom.min(neighbor.bbox.bottom)
                                - item.bbox.top.max(neighbor.bbox.top);
                        let gap = (item.bbox.left - neighbor.bbox.right)
                            .max(neighbor.bbox.left - item.bbox.right)
                            .max(0.0);
                        ((0.9..=1.1).contains(&(neighbor_size / size))
                            && overlap
                                >= neighbor
                                    .bbox
                                    .height()
                                    .min(item.bbox.height())
                                    * 0.5
                            && gap <= size * 3.0
                            && (item.rotation - neighbor.rotation).abs() <= 2.0)
                            .then_some((
                                gap + (item.bbox.center().y
                                    - neighbor.bbox.center().y)
                                    .abs(),
                                baseline,
                            ))
                    })
                    .collect();
                candidates.sort_by(|a, b| {
                    a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1))
                });
                let &(score, baseline) = candidates.first()?;
                // Competing rows with essentially equal geometric support are not
                // evidence for either baseline; keep the source's original grouping.
                if candidates.iter().skip(1).any(
                    |&(other_score, other_baseline)| {
                        other_score - score <= size * 0.05
                            && (other_baseline - baseline).abs() > size * 0.2
                    },
                ) {
                    None
                } else {
                    Some(baseline)
                }
            })
            .collect();
        for (item, baseline) in items.iter_mut().zip(baselines) {
            if let Some(y) = baseline {
                originals.insert(item.id.clone(), vec![item.clone()]);
                item.baseline = Some(Baseline {
                    start: Point::new(item.bbox.left, y),
                    end: Point::new(item.bbox.right, y),
                });
            }
        }
        let aligned_glyphs = originals.len();
        let mut input = Self { items, originals };
        input.group_operator_limits()?;
        let Self {
            mut items,
            mut originals,
        } = input;
        let mut fractions = 0;

        let mut bars: Vec<_> = rules
            .iter()
            .filter_map(|rule| match *rule {
                TableRule::Horizontal { y, left, right } if right > left => {
                    Some((y, left, right))
                }
                _ => None,
            })
            .collect();
        // Inner fractions become indivisible atoms before a wider enclosing fraction.
        bars.sort_by(|a, b| {
            (a.2 - a.1)
                .total_cmp(&(b.2 - b.1))
                .then_with(|| a.0.total_cmp(&b.0))
        });
        for (y, left, right) in bars {
            let mut above = Vec::new();
            let mut below = Vec::new();
            let mut size = 0.0_f64;
            for (index, item) in items.iter().enumerate() {
                let Some(font) =
                    item.style.as_ref().and_then(|style| style.font_size)
                else {
                    continue;
                };
                let b = item.bbox;
                let angle = item.rotation.rem_euclid(360.0);
                if angle.min(360.0 - angle) > 2.0
                    || b.left < left - 0.5
                    || b.right > right + 0.5
                {
                    continue;
                }
                if b.bottom <= y + 0.05 && y - b.bottom <= font * 0.9 {
                    above.push(index);
                    size = size.max(font);
                } else if b.top >= y - 0.05 && b.top - y <= font * 0.9 {
                    below.push(index);
                    size = size.max(font);
                }
            }
            if above.is_empty() || below.is_empty() {
                continue;
            }
            let mut members: BTreeSet<_> =
                above.iter().chain(&below).copied().collect();
            // A fraction is inline only when a larger neighboring run establishes its
            // mathematical axis. Ordinary table rules, underlines, and stacked prose
            // have no such witness and cannot collapse two rows through this path.
            let anchor = items
                .iter()
                .enumerate()
                .filter(|(index, _)| !members.contains(index))
                .filter_map(|(_, item)| {
                    let font = item.style.as_ref()?.font_size?;
                    let baseline = item.baseline?.start.y;
                    let gap = (left - item.bbox.right)
                        .max(item.bbox.left - right)
                        .max(0.0);
                    // Display fractions can use the full body font. Reuse a measured
                    // axis there instead of rounding a guessed baseline ahead of the lhs.
                    ((size <= font * 0.85
                        || (confirmed_formula && size <= font * 1.05))
                        && gap <= font * 2.0
                        && (baseline - font * 0.25 - y).abs() <= font * 0.2)
                        .then_some((gap, baseline, font))
                })
                .min_by(|a, b| a.0.total_cmp(&b.0));
            // Model-confirmed formula regions can contain standalone fractions.
            // Outside those regions, keep requiring an independent typographic axis.
            let anchor = anchor.or_else(|| {
                confirmed_formula.then_some((0.0, y + size * 0.25, size))
            });
            let Some((_, baseline, font)) = anchor else {
                continue;
            };
            // Resolve scripts independently on each side before sealing the fraction.
            // Sorting raw glyphs by x would insert an upper limit into a split i=1.
            let mut ordered = Vec::with_capacity(members.len());
            for side in [above, below] {
                let fragments = side
                    .into_iter()
                    .map(|index| {
                        LineFragment::from_items(
                            vec![items[index].clone()],
                            right.max(1.0),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut fragments =
                    LineFragment::attach_scripts(fragments, right.max(1.0))?;
                let direction = super::bidi::detect_direction(
                    fragments.iter().flat_map(|fragment| &fragment.items),
                    0.0,
                );
                fragments.sort_by(|a, b| {
                    if direction == crate::WritingDirection::RightToLeft {
                        b.bbox.right.total_cmp(&a.bbox.right)
                    } else {
                        a.bbox.left.total_cmp(&b.bbox.left)
                    }
                });
                ordered.extend(
                    fragments.into_iter().flat_map(|fragment| fragment.items),
                );
            }
            if confirmed_formula {
                // Large fraction delimiters can be vector paths with no text item.
                // A compact raised group just beyond the bar belongs to the complete
                // fraction, not its numerator or the following body operand.
                let top = ordered
                    .iter()
                    .map(|item| item.bbox.top)
                    .fold(f64::INFINITY, f64::min);
                let scripts = items
                    .iter()
                    .enumerate()
                    .filter(|(index, item)| {
                        !members.contains(index)
                            && item
                                .style
                                .as_ref()
                                .and_then(|style| style.font_size)
                                .is_some_and(|size| size <= font * 0.85)
                    })
                    .map(|(_, item)| {
                        LineFragment::from_items(
                            vec![item.clone()],
                            right.max(1.0),
                        )
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut powers: Vec<_> =
                    LineFragment::attach_scripts(scripts, right.max(1.0))?
                        .into_iter()
                        .filter(|script| {
                            let gap = script.bbox.left - right;
                            (0.45..=0.85)
                                .contains(&(script.metrics.font_size / font))
                                && script.rotation.rem_euclid(360.0).min(
                                    360.0 - script.rotation.rem_euclid(360.0),
                                ) <= 2.0
                                && gap >= -font * 0.25
                                && gap <= font * 1.25
                                && script.bbox.top >= top - font * 0.75
                                && script.bbox.bottom <= y
                                && script.baseline.start.y
                                    < baseline - font * 0.5
                                && script.bbox.width() <= font * 2.0
                                // A real intervening base or delimiter owns its own
                                // power; the wider gap is only for missing vector delimiters.
                                && !items.iter().enumerate().any(|(index,item)| {
                                    !members.contains(&index)
                                        && item.bbox.left >= right - 0.5
                                        && item.bbox.left < script.bbox.left
                                        && item.bbox.top < y
                                        && item.bbox.bottom > y
                                })
                                && script
                                    .items
                                    .iter()
                                    .flat_map(|item| item.raw_text.chars())
                                    .filter(|c| !c.is_whitespace())
                                    .take(13)
                                    .count()
                                    <= 12
                        })
                        .collect();
                // Multiple competing raised groups are not reliable ownership evidence.
                if powers.len() == 1
                    && let Some(power) = powers.pop()
                {
                    for item in power.items {
                        if let Some(index) =
                            items.iter().position(|source| source.id == item.id)
                        {
                            members.insert(index);
                            ordered.push(item);
                        }
                    }
                }
            }
            let Some(mut atom) = ordered.first().cloned() else {
                continue;
            };
            let mut source = Vec::new();
            let mut bounds = atom.bbox;
            atom.raw_text.clear();
            for item in &ordered {
                bounds = Bbox::try_from([
                    bounds.left.min(item.bbox.left),
                    bounds.top.min(item.bbox.top),
                    bounds.right.max(item.bbox.right),
                    bounds.bottom.max(item.bbox.bottom),
                ])?;
                atom.raw_text.push_str(&item.raw_text);
                source.extend(
                    originals
                        .remove(&item.id)
                        .unwrap_or_else(|| vec![item.clone()]),
                );
            }
            atom.bbox = bounds;
            atom.baseline = Some(Baseline {
                start: Point::new(bounds.left, baseline),
                end: Point::new(bounds.right, baseline),
            });
            if let Some(style) = &mut atom.style {
                style.font_size = Some(font);
            }
            originals.insert(atom.id.clone(), source);
            fractions += 1;
            items = items
                .into_iter()
                .enumerate()
                .filter(|(index, _)| !members.contains(index))
                .map(|(_, item)| item)
                .collect();
            items.push(atom);
        }
        if fractions > 0 || aligned_glyphs > 0 {
            tracing::debug!(
                "prepared {} inline fractions and aligned {} hanging glyphs for line assembly",
                fractions,
                aligned_glyphs
            );
        }
        Ok(Self { items, originals })
    }

    /// Keeps detached, centered upper/lower limits with a baseline-aligned hanging operator.
    #[allow(
        clippy::indexing_slicing,
        reason = "indices refer to the unchanged item vector until all owners are selected"
    )]
    fn group_operator_limits(&mut self) -> Result<(), LineError> {
        // Only glyphs with independently established body baselines can anchor a
        // display operator. Requiring both sides avoids absorbing isolated notes.
        let operators: Vec<_> = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| self.originals.contains_key(&item.id))
            .map(|(index, _)| index)
            .collect();
        if operators.is_empty() {
            return Ok(());
        }
        let owners: Vec<_> = self
            .items
            .iter()
            .map(|item| {
                if self.originals.contains_key(&item.id) {
                    return None;
                }
                let size = item.style.as_ref()?.font_size?;
                let mut candidates: Vec<_> = operators
                    .iter()
                    .filter_map(|&index| {
                        let operator = &self.items[index];
                        let font = operator.style.as_ref()?.font_size?;
                        let above = operator.bbox.top - item.bbox.bottom;
                        let below = item.bbox.top - operator.bbox.bottom;
                        let overlap = operator.bbox.right.min(item.bbox.right)
                            - operator.bbox.left.max(item.bbox.left);
                        if !(0.45..=0.85).contains(&(size / font))
                            || item.baseline.is_none()
                            || (item.rotation - operator.rotation).abs() > 2.0
                            || item.bbox.height() > font
                            || overlap < item.bbox.width() * 0.5
                            || !(0.0..=font * 0.5).contains(&above.max(below))
                        {
                            return None;
                        }
                        Some((
                            index,
                            above >= 0.0,
                            (item.bbox.center().x - operator.bbox.center().x)
                                .abs()
                                / font,
                        ))
                    })
                    .collect();
                candidates.sort_by(|a, b| a.2.total_cmp(&b.2));
                let &(index, above, score) = candidates.first()?;
                // Choose ownership from original geometry, rejecting competing operators.
                (!candidates
                    .get(1)
                    .is_some_and(|candidate| candidate.2 - score <= 0.05))
                .then_some((index, above))
            })
            .collect();
        let mut consumed = BTreeSet::new();
        for operator in operators {
            let mut above = Vec::new();
            let mut below = Vec::new();
            for (index, owner) in owners.iter().enumerate() {
                match owner {
                    Some((parent, true)) if *parent == operator => {
                        above.push(index)
                    }
                    Some((parent, false)) if *parent == operator => {
                        below.push(index)
                    }
                    _ => {}
                }
            }
            if above.is_empty() || below.is_empty() {
                continue;
            }
            let font = self.items[operator]
                .style
                .as_ref()
                .and_then(|style| style.font_size)
                .unwrap_or(0.0);
            // Limit bands stay compact and centered, even when PDF objects split r=1.
            if [&above, &below].iter().any(|side| {
                let left = side
                    .iter()
                    .map(|&index| self.items[index].bbox.left)
                    .fold(f64::INFINITY, f64::min);
                let right = side
                    .iter()
                    .map(|&index| self.items[index].bbox.right)
                    .fold(f64::NEG_INFINITY, f64::max);
                right - left > font * 3.0
                    || ((left + right) * 0.5
                        - self.items[operator].bbox.center().x)
                        .abs()
                        > font * 0.35
                    || side
                        .iter()
                        .flat_map(|&index| self.items[index].raw_text.chars())
                        .filter(|ch| !ch.is_whitespace())
                        .take(13)
                        .count()
                        > 12
            }) {
                continue;
            }
            above.sort_by(|&a, &b| {
                self.items[a]
                    .bbox
                    .left
                    .total_cmp(&self.items[b].bbox.left)
                    .then_with(|| self.items[a].id.cmp(&self.items[b].id))
            });
            below.sort_by(|&a, &b| {
                self.items[a]
                    .bbox
                    .left
                    .total_cmp(&self.items[b].bbox.left)
                    .then_with(|| self.items[a].id.cmp(&self.items[b].id))
            });
            let mut atom = self.items[operator].clone();
            let mut source = self
                .originals
                .remove(&atom.id)
                .unwrap_or_else(|| vec![atom.clone()]);
            // Emit the operator first, then complete lower and upper bands, so even
            // a lower limit wider than its operator cannot move in front of it.
            for index in below.into_iter().chain(above) {
                let item = &self.items[index];
                atom.bbox = Bbox::try_from([
                    atom.bbox.left.min(item.bbox.left),
                    atom.bbox.top.min(item.bbox.top),
                    atom.bbox.right.max(item.bbox.right),
                    atom.bbox.bottom.max(item.bbox.bottom),
                ])?;
                atom.raw_text.push_str(&item.raw_text);
                source.push(item.clone());
                consumed.insert(index);
            }
            self.originals.insert(atom.id.clone(), source);
            self.items[operator] = atom;
        }
        self.items = std::mem::take(&mut self.items)
            .into_iter()
            .enumerate()
            .filter(|(index, _)| !consumed.contains(index))
            .map(|(_, item)| item)
            .collect();
        Ok(())
    }

    /// Expands ordered atoms without re-sorting their numerator/denominator or changing raw facts.
    pub fn restore(mut self, fragments: &mut [LineFragment]) {
        for fragment in fragments {
            fragment.items = std::mem::take(&mut fragment.items)
                .into_iter()
                .flat_map(|item| {
                    self.originals
                        .remove(&item.id)
                        .unwrap_or_else(|| vec![item])
                })
                .collect();
        }
    }
}
