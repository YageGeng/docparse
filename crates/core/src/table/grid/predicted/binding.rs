//! Unique source-word assignment over approximate model positions.
use super::*;

/// One selected logical cell for every immutable source word.
pub(super) struct WordAssignment(pub Vec<usize>);

impl TryFrom<(&TableGeometry<'_>, &Table, &[Bbox])> for WordAssignment {
    type Error = String;

    /// Scores model positions and calibrated support while refusing ambiguous or indivisible cross-column runs.
    fn try_from(
        (geometry, table, regions): (&TableGeometry<'_>, &Table, &[Bbox]),
    ) -> Result<Self, Self::Error> {
        let mut owners = Vec::with_capacity(geometry.spans.len());
        for word in geometry.spans {
            let b = word.span.bbox;
            // A fraction's denominator and superscripts inherit the established parent line,
            // even when their individual ink crosses a learned row divider.
            let x = (b.left + b.right) * 0.5;
            let y = (word.baseline - geometry.font_size * 0.3)
                .clamp(geometry.bounds.top, geometry.bounds.bottom - 1e-6);
            let mut candidates = table
                .cells
                .iter()
                .enumerate()
                .filter_map(|(index, cell)| {
                    let c = cell.bbox?;
                    let dx = (b.left - c.right).max(c.left - b.right).max(0.0);
                    let dy = (y - c.bottom).max(c.top - y).max(0.0);
                    let region = regions.get(index)?;
                    let supported = x >= region.left
                        && x < region.right
                        && y >= region.top
                        && y < region.bottom;
                    let region_dx =
                        (x - region.right).max(region.left - x).max(0.0);
                    let region_dy =
                        (y - region.bottom).max(region.top - y).max(0.0);
                    if !supported
                        && (dx > geometry.font_size || dy > geometry.font_size)
                        && (region_dx > geometry.font_size
                            || region_dy > geometry.font_size)
                    {
                        return None;
                    }
                    let horizontal =
                        (b.right.min(c.right) - b.left.max(c.left)).max(0.0)
                            / b.width();
                    let vertical = ((y + geometry.font_size * 0.5)
                        .min(c.bottom)
                        - (y - geometry.font_size * 0.5).max(c.top))
                    .max(0.0)
                        / geometry.font_size;
                    let score = 0.5 * horizontal * vertical
                        + if supported { 1.0 } else { 0.0 }
                        - 0.01
                            * ((x - (c.left + c.right) * 0.5).abs()
                                / region.width()
                                + (y - (c.top + c.bottom) * 0.5).abs()
                                    / region.height());
                    Some((index, score))
                })
                .collect::<Vec<_>>();
            candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
            let owner = candidates.first().map(|c| c.0).ok_or_else(|| {
                format!(
                    "no nearby model cell owns {} bytes {:?}",
                    word.item.id.as_str(),
                    word.span.byte_range
                )
            })?;
            let selected =
                table.cells.get(owner).ok_or("missing predicted cell")?;
            let support =
                regions.get(owner).ok_or("missing predicted support")?;
            let raw = selected.bbox.ok_or("model cell lacks position")?;
            let raw_coverage = (b.right.min(raw.right) - b.left.max(raw.left))
                .max(0.0)
                / b.width();
            let support_coverage = (b.right.min(support.right)
                - b.left.max(support.left))
            .max(0.0)
                / b.width();
            if !selected.is_header
                && selected.column_span == 1
                && raw_coverage < 0.6
                && support_coverage < 0.6
            {
                return Err(format!(
                    "ambiguous model column for {} bytes {:?}: model coverage {:.3}, calibrated coverage {:.3}",
                    word.item.id.as_str(),
                    word.span.byte_range,
                    raw_coverage,
                    support_coverage
                ));
            }
            if !selected.is_header
                && selected.column_span == 1
                && b.width() > support.width() * 1.5
            {
                return Err(format!(
                    "indivisible source run {} bytes {:?} crosses multiple predicted columns",
                    word.item.id.as_str(),
                    word.span.byte_range
                ));
            }
            owners.push(owner);
        }
        Ok(Self(owners))
    }
}

impl WordAssignment {
    /// Keeps contiguous native phrases together without joining across real column whitespace.
    #[allow(
        clippy::indexing_slicing,
        reason = "source rows and assignments share validated word indices"
    )]
    pub(super) fn align_phrases(
        &mut self,
        geometry: &TableGeometry<'_>,
        table: &Table,
    ) -> Result<(), String> {
        // Keep a contiguous native phrase together across noisy model boundaries. Real table
        // columns remain separated by their measured horizontal whitespace, independent of model labels.
        for row in &geometry.rows {
            let mut groups: Vec<Vec<usize>> = Vec::new();
            let mut right = f64::NEG_INFINITY;
            let mut previous_columns = None;
            for &index in &row.spans {
                let b = geometry.spans[index].span.bbox;
                let cell = table
                    .cells
                    .get(self.0[index])
                    .ok_or("missing phrase owner")?;
                let columns = (cell.column, cell.column_span);
                // Close numeric columns can share a baseline and a small gap; phrase voting
                // may repair row drift, but must not move words across established columns.
                if b.left - right > geometry.font_size * 0.6
                    || previous_columns != Some(columns)
                {
                    groups.push(Vec::new());
                }
                groups
                    .last_mut()
                    .ok_or("missing native phrase")?
                    .push(index);
                right = right.max(b.right);
                previous_columns = Some(columns);
            }
            for indices in groups {
                let mut votes: BTreeMap<usize, f64> = BTreeMap::new();
                for &index in &indices {
                    *votes.entry(self.0[index]).or_default() += geometry.spans
                        [index]
                        .span
                        .bbox
                        .width()
                        .min(geometry.font_size * 2.0);
                }
                let Some((&winner, _)) =
                    votes.iter().max_by(|a, b| a.1.total_cmp(b.1))
                else {
                    continue;
                };
                for index in indices {
                    self.0[index] = winner;
                }
            }
        }
        Ok(())
    }

    /// Measures final ink only after phrase ownership is stable.
    pub(super) fn measure(
        &self,
        geometry: &TableGeometry<'_>,
        cell_count: usize,
    ) -> Result<Vec<Option<Bbox>>, String> {
        let mut ink: Vec<Option<Bbox>> = vec![None; cell_count];
        for (word, &owner) in geometry.spans.iter().zip(&self.0) {
            let b = word.span.bbox;
            let owned = ink.get_mut(owner).ok_or("invalid line owner")?;
            *owned = Some(match *owned {
                Some(c) => Bbox::try_from([
                    c.left.min(b.left),
                    c.top.min(b.top),
                    c.right.max(b.right),
                    c.bottom.max(b.bottom),
                ])
                .map_err(|e| e.to_string())?,
                None => b,
            });
        }
        Ok(ink)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TableTextSpan, TextItem, TextItemId, TextSource, TextStyle};

    /// Phrase voting must not turn adjacent numeric columns into a single cell merely because their gap is small.
    #[test]
    fn close_columns_keep_distinct_source_owners() {
        let items = [10.0, 20.0]
            .into_iter()
            .enumerate()
            .map(|(index, left)| {
                TextItem::builder()
                    .id(TextItemId::native(1, index as u32))
                    .raw_text("12".to_owned())
                    .bbox(
                        Bbox::try_from([left, 10.0, left + 9.0, 20.0])
                            .expect("word bounds"),
                    )
                    .source(TextSource::Native)
                    .style(Some(
                        TextStyle::builder().font_size(Some(10.0)).build(),
                    ))
                    .build()
            })
            .collect::<Vec<_>>();
        let spans = items
            .iter()
            .map(|item| {
                LocatedSpan::builder()
                    .span(TableTextSpan {
                        text_item_id: item.id.clone(),
                        byte_range: 0..2,
                        bbox: item.bbox,
                    })
                    .item(item)
                    .baseline(20.0)
                    .build()
            })
            .collect::<Vec<_>>();
        let geometry = TableGeometry::new(
            Bbox::try_from([0.0, 0.0, 40.0, 30.0]).expect("table bounds"),
            &spans,
        );
        let table = Table::builder()
            .row_count(1)
            .column_count(2)
            .cells(
                (0..2)
                    .map(|column| {
                        TableCell::builder().row(0).column(column).build()
                    })
                    .collect(),
            )
            .source(TableStructureSource::ExternalTsr)
            .build();
        let mut assignment = WordAssignment(vec![0, 1]);
        assignment
            .align_phrases(&geometry, &table)
            .expect("native phrases");
        assert_eq!(assignment.0, [0, 1]);
    }
}
