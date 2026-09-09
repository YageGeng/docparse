use std::collections::BTreeMap;

use docparse_config::FusionConfig;
use docparse_layout::{Bbox, LayoutDetection, LayoutLabel};

use super::inline::InlineInput;
use super::{ConservativeLineAssembler, LineAssembler, LineFragment};
use crate::{LineError, TableRule, TextItem};

/// Validated model evidence that bounds formula-specific ordering without owning text.
#[derive(Debug, Clone, Copy)]
pub(crate) struct FormulaRegion {
    pub bbox: Bbox,
    pub inline: bool,
}

impl TryFrom<&LayoutDetection> for FormulaRegion {
    type Error = ();

    /// Rejects non-formula labels and invalid detection geometry before selecting source members.
    fn try_from(detection: &LayoutDetection) -> Result<Self, Self::Error> {
        if !detection.confidence.is_finite()
            || !(0.0..=1.0).contains(&detection.confidence)
            || LayoutLabel::try_from(detection.class_id).is_err()
        {
            return Err(());
        }
        let inline = match detection.label {
            LayoutLabel::InlineFormula => true,
            LayoutLabel::DisplayFormula => false,
            _ => return Err(()),
        };
        let b = detection.bbox;
        let bbox = Bbox::try_from([b.left, b.top, b.right, b.bottom])
            .map_err(|_error| ())?;
        Ok(Self { bbox, inline })
    }
}

impl TextItem {
    /// Assigns a source run to its smallest well-covering formula, leaving crossing prose intact.
    pub(crate) fn formula_region(
        &self,
        regions: &[FormulaRegion],
    ) -> Option<usize> {
        let single_glyph = self.raw_text.trim().chars().take(2).count() == 1;
        regions
            .iter()
            .enumerate()
            .filter(|(_, region)| {
                let b = region.bbox;
                let area = (b.right.min(self.bbox.right)
                    - b.left.max(self.bbox.left))
                .max(0.0)
                    * (b.bottom.min(self.bbox.bottom)
                        - b.top.max(self.bbox.top))
                    .max(0.0);
                // Model boxes can clip the loose ink bounds of small scripts.
                // Admit only centered glyphs with bounded edge overflow; a wide
                // prose run crossing the formula still keeps its original owner.
                // Detection error is in viewport coordinates, not script font
                // size: the same two-point clipping must not split nested levels.
                let padding = 2.0;
                let center = self.bbox.center();
                let coverage = area / self.bbox.area().max(f64::EPSILON);
                coverage >= 0.8
                    || (center.x >= b.left
                        && center.x <= b.right
                        && center.y >= b.top
                        && center.y <= b.bottom
                        // A centered individual glyph is indivisible; a majority
                        // overlap retains a clipped superscript without pulling in
                        // a multi-word prose run that crosses the same boundary.
                        && ((single_glyph && coverage >= 0.5)
                            || (self.bbox.left >= b.left - padding
                                && self.bbox.right <= b.right + padding
                                && self.bbox.top >= b.top - padding
                                && self.bbox.bottom <= b.bottom + padding)))
            })
            .min_by(|(_, a), (_, b)| {
                a.bbox
                    .area()
                    .total_cmp(&b.bbox.area())
                    .then_with(|| b.inline.cmp(&a.inline))
                    .then_with(|| a.bbox.top.total_cmp(&b.bbox.top))
                    .then_with(|| a.bbox.left.total_cmp(&b.bbox.left))
            })
            .map(|(index, _)| index)
    }
}

impl LineFragment {
    /// Groups original source fragments by formula, retaining clipped scripts with an unambiguous parent.
    #[allow(
        clippy::indexing_slicing,
        reason = "owner and traversal indices are generated from the unchanged fragment vector"
    )]
    pub(crate) fn formula_groups(
        fragments: Vec<Self>,
        formulas: &[FormulaRegion],
    ) -> BTreeMap<Option<usize>, Vec<Self>> {
        let mut owners: Vec<_> = fragments
            .iter()
            .map(|fragment| {
                fragment
                    .items
                    .first()
                    .and_then(|item| item.formula_region(formulas))
            })
            .collect();
        if owners.iter().any(Option::is_some)
            && owners.iter().any(Option::is_none)
        {
            let mut order: Vec<_> = (0..fragments.len()).collect();
            // Parents have strictly larger fonts. Resolve them first so nested
            // clipped indices inherit the same scope without changing any geometry.
            order.sort_by(|&a, &b| {
                fragments[b]
                    .metrics
                    .font_size
                    .total_cmp(&fragments[a].metrics.font_size)
            });
            for index in order {
                if owners[index].is_none() {
                    owners[index] = fragments[index]
                        .script_parent(&fragments)
                        .and_then(|parent| owners[parent]);
                }
            }
        }
        let mut groups = BTreeMap::<_, Vec<_>>::new();
        for (fragment, owner) in fragments.into_iter().zip(owners) {
            groups.entry(owner).or_default().push(fragment);
        }
        groups
    }
}

impl ConservativeLineAssembler {
    /// Orders detected formula rows in isolation and inserts intact atoms into surrounding prose.
    pub(crate) fn fragments_with_formulas(
        &self,
        items: Vec<TextItem>,
        config: &FusionConfig,
        rules: &[TableRule],
        formulas: &[FormulaRegion],
    ) -> Result<Vec<super::LineFragment>, LineError> {
        if formulas.is_empty() {
            return self.fragments_with_rules(items, config, rules);
        }
        // Resolve boundary-crossing script ownership before hiding its parent
        // inside a formula atom; the outside fallback cannot reopen that atom.
        let page_width = items
            .iter()
            .map(|item| item.bbox.right)
            .fold(1.0_f64, f64::max);
        let fragments = items
            .into_iter()
            .map(|item| LineFragment::from_items(vec![item], page_width))
            .collect::<Result<Vec<_>, _>>()?;
        let mut groups: BTreeMap<_, Vec<_>> =
            LineFragment::formula_groups(fragments, formulas)
                .into_iter()
                .map(|(owner, fragments)| {
                    (
                        owner,
                        fragments
                            .into_iter()
                            .flat_map(|fragment| fragment.items)
                            .collect(),
                    )
                })
                .collect();
        let mut input = InlineInput {
            items: groups.remove(&None).unwrap_or_default(),
            ..InlineInput::default()
        };
        // Freeze external baseline evidence before adding atoms, preventing one
        // formula's temporary geometry from moving the next formula onto a new row.
        let anchors: Vec<_> = input
            .items
            .iter()
            .filter_map(|item| {
                Some((
                    item.bbox,
                    item.style.as_ref()?.font_size?,
                    item.baseline?,
                    item.rotation,
                ))
            })
            .collect();
        for (owner, items) in groups {
            let Some(region) = owner.and_then(|index| formulas.get(index))
            else {
                continue;
            };
            let mut prepared = InlineInput::prepare(items, rules, true)?;
            let mut fragments =
                self.fragments(std::mem::take(&mut prepared.items), config)?;
            prepared.restore(&mut fragments);
            // A display detection may cover several equation rows. Preserve the
            // assembled rows instead of flattening the whole detection into one line.
            for fragment in fragments {
                let Some(mut atom) = fragment.items.first().cloned() else {
                    continue;
                };
                atom.raw_text = fragment
                    .items
                    .iter()
                    .map(|item| item.raw_text.as_str())
                    .collect();
                atom.bbox = fragment.bbox;
                atom.baseline = Some(fragment.baseline);
                if let Some(style) = &mut atom.style {
                    style.font_size = Some(fragment.metrics.font_size);
                }
                // Borrow a horizontal prose baseline only for upright formulas;
                // rotated rows retain their established orientation-aware baseline.
                let angle = atom.rotation.rem_euclid(360.0);
                if region.inline && angle.min(360.0 - angle) <= 2.0 {
                    let anchor = anchors
                        .iter()
                        .filter_map(|&(bbox, size, baseline, rotation)| {
                            let overlap = bbox.bottom.min(atom.bbox.bottom)
                                - bbox.top.max(atom.bbox.top);
                            let gap = (bbox.left - atom.bbox.right)
                                .max(atom.bbox.left - bbox.right)
                                .max(0.0);
                            let baseline_gap = (baseline.start.y
                                - fragment.baseline.start.y)
                                .abs();
                            (size.is_finite()
                                && size > 0.0
                                // A clipped script or a neighboring text row cannot
                                // become the baseline witness for the whole formula.
                                && size >= fragment.metrics.font_size * 0.85
                                && baseline_gap <= size * 0.5
                                && overlap
                                    >= bbox.height().min(atom.bbox.height())
                                        * 0.5
                                && gap <= size * 3.0
                                && (rotation - atom.rotation).abs() <= 2.0)
                                .then_some((gap + baseline_gap, size, baseline))
                        })
                        .min_by(|a, b| {
                            a.0.total_cmp(&b.0).then_with(|| {
                                a.2.start.y.total_cmp(&b.2.start.y)
                            })
                        });
                    if let Some((_, size, baseline)) = anchor {
                        // Only the temporary inline atom adopts the prose baseline;
                        // restoration retains every measured formula glyph unchanged.
                        atom.baseline = Some(crate::Baseline {
                            start: docparse_layout::Point::new(
                                atom.bbox.left,
                                baseline.start.y,
                            ),
                            end: docparse_layout::Point::new(
                                atom.bbox.right,
                                baseline.start.y,
                            ),
                        });
                        let style = atom.style.get_or_insert_with(|| {
                            crate::TextStyle::builder().build()
                        });
                        style.font_size = Some(size);
                    }
                }
                input.originals.insert(atom.id.clone(), fragment.items);
                input.items.push(atom);
            }
        }
        let mut fragments = self.fragments_with_rules(
            std::mem::take(&mut input.items),
            config,
            rules,
        )?;
        input.restore(&mut fragments);
        Ok(fragments)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real display fractions keep their right-hand powers after the fraction and before later operands.
    #[test]
    fn survey_fraction_powers_follow_their_base() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/line/survey-fraction-powers.json"
        ))
        .expect("real formulas");
        // A neighboring variable's own power must not be stolen by the fraction.
        let neighboring_power = serde_json::json!({
            "bbox": [10.0, 25.0, 60.0, 58.0],
            "expected": "=abc2",
            "rules": [[44.0, 30.0, 40.0]],
            "items": [
                [0,"=",[15.0,39.0,23.0,49.0],12.0,47.0],
                [1,"a",[31.0,33.0,38.0,41.0],12.0,40.0],
                [2,"b",[31.0,46.0,38.0,54.0],12.0,53.0],
                [3,"c",[43.0,39.0,49.0,49.0],12.0,47.0],
                [4,"2",[49.0,34.0,53.0,41.0],8.0,40.0]
            ]
        });
        for formula in fixture
            .get("formulas")
            .expect("formulas")
            .as_array()
            .expect("formulas")
            .iter()
            .chain(std::iter::once(&neighboring_power))
        {
            let facts: Vec<(u32, String, [f64; 4], f64, f64)> =
                serde_json::from_value(
                    formula.get("items").expect("items").clone(),
                )
                .expect("glyphs");
            let bbox: [f64; 4] = serde_json::from_value(
                formula.get("bbox").expect("bbox").clone(),
            )
            .expect("region");
            let bars: Vec<[f64; 3]> = serde_json::from_value(
                formula.get("rules").expect("rules").clone(),
            )
            .expect("bars");
            let rules: Vec<_> = bars
                .into_iter()
                .map(|[y, left, right]| crate::TableRule::Horizontal {
                    y,
                    left,
                    right,
                })
                .collect();
            let items: Vec<_> = facts
                .into_iter()
                .map(|(id, text, b, size, y)| {
                    TextItem::builder()
                        .id(crate::TextItemId::native(4, id))
                        .raw_text(text)
                        .bbox(Bbox::try_from(b).expect("glyph"))
                        .baseline(Some(crate::Baseline {
                            start: docparse_layout::Point::new(b[0], y),
                            end: docparse_layout::Point::new(b[2], y),
                        }))
                        .style(Some(
                            crate::TextStyle::builder()
                                .font_size(Some(size))
                                .build(),
                        ))
                        .source(crate::TextSource::Native)
                        .build()
                })
                .collect();
            for reverse in [false, true] {
                let mut input = items.clone();
                if reverse {
                    input.reverse();
                }
                let lines = ConservativeLineAssembler
                    .fragments_with_formulas(
                        input,
                        &FusionConfig::default(),
                        &rules,
                        &[FormulaRegion {
                            bbox: Bbox::try_from(bbox).expect("region"),
                            inline: false,
                        }],
                    )
                    .expect("assemble");
                let text: String = lines
                    .iter()
                    .flat_map(|line| &line.items)
                    .flat_map(|item| item.raw_text.chars())
                    .filter(|c| !c.is_whitespace())
                    .collect();
                assert_eq!(
                    text,
                    formula
                        .get("expected")
                        .expect("expected")
                        .as_str()
                        .expect("expected order")
                );
                // Preserve the existing conservative split across wide spacing;
                // this regression pins source order, not a new line-merging policy.
            }
        }
    }

    /// A measured script clipped by a real formula box cannot move its equation after the following prose.
    #[test]
    fn clipped_formula_bounds_preserve_the_original_body_row() {
        for source in [
            include_str!(
                "../../tests/fixtures/line/clipped-inline-formula.json"
            ),
            include_str!(
                "../../tests/fixtures/line/clipped-nested-formula.json"
            ),
            include_str!(
                "../../tests/fixtures/line/clipped-proof-formula.json"
            ),
        ] {
            let fixture: serde_json::Value =
                serde_json::from_str(source).expect("real inline row");
            let facts: Vec<(u32, String, [f64; 4], f64, f64)> =
                serde_json::from_value(
                    fixture.get("items").expect("items").clone(),
                )
                .expect("glyph facts");
            let regions: Vec<[f64; 4]> = serde_json::from_value(
                fixture.get("regions").expect("regions").clone(),
            )
            .expect("regions");
            let regions: Vec<_> = regions
                .into_iter()
                .map(|bounds| FormulaRegion {
                    bbox: Bbox::try_from(bounds).expect("formula bounds"),
                    inline: true,
                })
                .collect();
            let items: Vec<_> = facts
                .into_iter()
                .map(|(id, text, bounds, size, y)| {
                    TextItem::builder()
                        .id(crate::TextItemId::native(3, id))
                        .raw_text(text)
                        .bbox(Bbox::try_from(bounds).expect("glyph bounds"))
                        .baseline(Some(crate::Baseline {
                            start: docparse_layout::Point::new(bounds[0], y),
                            end: docparse_layout::Point::new(bounds[2], y),
                        }))
                        .style(Some(
                            crate::TextStyle::builder()
                                .font_size(Some(size))
                                .build(),
                        ))
                        .source(crate::TextSource::Native)
                        .build()
                })
                .collect();
            for reverse in [false, true] {
                let mut input = items.clone();
                if reverse {
                    input.reverse();
                }
                let lines = ConservativeLineAssembler
                    .fragments_with_formulas(
                        input,
                        &FusionConfig::default(),
                        &[],
                        &regions,
                    )
                    .expect("scoped formula");
                let text: Vec<_> = lines
                    .iter()
                    .map(|line| crate::Line::derive_text(&line.items))
                    .collect();
                assert_eq!(
                    text,
                    vec![
                        fixture
                            .get("expected")
                            .and_then(serde_json::Value::as_str)
                            .expect("source reading order")
                    ]
                );
            }
        }
    }
}
