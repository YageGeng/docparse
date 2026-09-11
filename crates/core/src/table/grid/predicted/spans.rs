//! Source separators refine predicted spans before publication.
use super::*;

impl TableGeometry<'_> {
    /// Splits a learned rowspan only where a visible separator crosses its column band.
    #[allow(
        clippy::indexing_slicing,
        reason = "indices are drawn from the bounded model grid"
    )]
    pub(super) fn split_ruled_spans(
        &self,
        table: &mut Table,
        regions: &mut Vec<Bbox>,
        rules: &[TableRule],
    ) -> Result<(), String> {
        let mut row_cuts = vec![None; table.row_count + 1];
        for (cell, region) in table.cells.iter().zip(regions.iter()) {
            row_cuts[cell.row] = Some(region.top);
            row_cuts[cell.row + cell.row_span] = Some(region.bottom);
        }
        let mut refined = Vec::new();
        let mut refined_regions = Vec::new();
        for (cell, region) in table.cells.iter().zip(regions.iter()) {
            let mut start = cell.row;
            for end in cell.row + 1..=cell.row + cell.row_span {
                let last = end == cell.row + cell.row_span;
                let separator = row_cuts[end].is_some_and(|y| {
                    self.coverage(rules, true, y, region.left, region.right)
                        >= 0.9
                });
                if !last && !separator {
                    continue;
                }
                let top = row_cuts[start].unwrap_or(region.top);
                let bottom = row_cuts[end].unwrap_or(region.bottom);
                let band =
                    Bbox::try_from([region.left, top, region.right, bottom])
                        .map_err(|e| e.to_string())?;
                let mut part = cell.clone();
                part.row = start;
                part.row_span = end - start;
                if start != cell.row || !last {
                    let b = part.bbox.ok_or("model cell lacks geometry")?;
                    part.bbox = Some(
                        Bbox::try_from([
                            b.left,
                            b.top.max(top),
                            b.right,
                            b.bottom.min(bottom),
                        ])
                        .unwrap_or(band),
                    );
                }
                refined.push(part);
                refined_regions.push(band);
                start = end;
            }
        }
        table.cells = refined;
        *regions = refined_regions;
        CellGrid::try_from(table.clone())?;
        Ok(())
    }

    /// Merges empty neighbors only across an observed partial separator omitted in their column.
    #[allow(
        clippy::indexing_slicing,
        reason = "alive flags, regions, and owners share the validated cell indices"
    )]
    pub(super) fn merge_ruled_blanks(
        &self,
        table: &mut Table,
        owners: &mut [usize],
        regions: &mut Vec<Bbox>,
        rules: &[TableRule],
    ) -> Result<(), String> {
        // A partial PDF separator ending before a blank model slot is positive evidence of
        // a spanning cell. Merge only empty neighbors across that omitted separator.
        let header = table
            .cells
            .iter()
            .filter(|c| c.is_header)
            .map(|c| c.row + c.row_span)
            .max()
            .unwrap_or(1);
        let mut alive = vec![true; table.cells.len()];
        for _ in 0..table.row_count {
            let mut changed = false;
            for first in 0..table.cells.len() {
                if !alive[first] || table.cells[first].row < header {
                    continue;
                }
                for second in first + 1..table.cells.len() {
                    if !alive[second] {
                        continue;
                    }
                    let a = &table.cells[first];
                    let b = &table.cells[second];
                    if a.column != b.column
                        || a.column_span != b.column_span
                        || a.bbox.is_some() == b.bbox.is_some()
                    {
                        continue;
                    }
                    let boundary = if a.row + a.row_span == b.row {
                        regions[first].bottom
                    } else if b.row + b.row_span == a.row {
                        regions[second].bottom
                    } else {
                        continue;
                    };
                    let left = regions[first].left;
                    let right = regions[first].right;
                    if !rules.iter().any(|rule|matches!(*rule,TableRule::Horizontal {y,left:l,right:r} if (y-boundary).abs()<=self.rule_tolerance() && r-l>=self.bounds.width()*0.25 && (r.min(right)-l.max(left)).max(0.0)/(right-left)<0.1)) {continue;}
                    let (target, removed) = if a.bbox.is_some() {
                        (first, second)
                    } else {
                        (second, first)
                    };
                    let start = a.row.min(b.row);
                    let span = a.row_span + b.row_span;
                    table.cells[target].row = start;
                    table.cells[target].row_span = span;
                    regions[target] = Bbox::try_from([
                        left,
                        regions[first].top.min(regions[second].top),
                        right,
                        regions[first].bottom.max(regions[second].bottom),
                    ])
                    .map_err(|e| e.to_string())?;
                    alive[removed] = false;
                    changed = true;
                    break;
                }
            }
            if !changed {
                break;
            }
        }
        let mut remap = vec![0; table.cells.len()];
        let mut kept = Vec::new();
        let mut kept_regions = Vec::new();
        for (index, cell) in
            std::mem::take(&mut table.cells).into_iter().enumerate()
        {
            if alive[index] {
                remap[index] = kept.len();
                kept.push(cell);
                kept_regions.push(regions[index]);
            }
        }
        table.cells = kept;
        *regions = kept_regions;
        for owner in owners.iter_mut() {
            *owner = remap[*owner];
        }
        Ok(())
    }
}

impl TableGeometry<'_> {
    /// Extends a model-supported category label over empty positions inside one fully ruled band.
    #[allow(
        clippy::indexing_slicing,
        reason = "band selections and source owners refer to validated model cells"
    )]
    pub(super) fn merge_label_bands(
        &self,
        table: &Table,
        owners: &[usize],
        regions: &[Bbox],
        rules: &[TableRule],
    ) -> Option<CellGrid> {
        let header = table
            .cells
            .iter()
            .filter(|cell| cell.is_header)
            .map(|cell| cell.row + cell.row_span)
            .max()
            .unwrap_or(1);
        let mut grid = CellGrid::try_from(table.clone()).ok()?;
        let mut changed = false;
        for column in 0..table.column_count {
            let region = table
                .cells
                .iter()
                .zip(regions)
                .find(|(cell, _)| {
                    cell.column == column && cell.column_span == 1
                })?
                .1;
            let cuts = self.snapped(
                rules
                    .iter()
                    .filter_map(|rule| match *rule {
                        TableRule::Horizontal { y, .. }
                            if y >= self.bounds.top - self.rule_tolerance()
                                && y <= self.bounds.bottom
                                    + self.rule_tolerance()
                                && self.coverage(
                                    rules,
                                    true,
                                    y,
                                    region.left,
                                    region.right,
                                ) >= 0.9 =>
                        {
                            Some(y)
                        }
                        _ => None,
                    })
                    .collect(),
            );
            for band in cuts.windows(2) {
                let selected: Vec<_> = table
                    .cells
                    .iter()
                    .zip(regions)
                    .enumerate()
                    .filter(|(_, (cell, b))| {
                        cell.row >= header
                            && cell.column == column
                            && cell.column_span == 1
                            && b.top >= band[0] - self.rule_tolerance()
                            && b.bottom <= band[1] + self.rule_tolerance()
                    })
                    .map(|(i, _)| i)
                    .collect();
                if selected.len() < 2
                    || !selected.iter().any(|&i| table.cells[i].row_span > 1)
                {
                    continue;
                }
                let filled: Vec<_> = selected
                    .iter()
                    .copied()
                    .filter(|&i| table.cells[i].bbox.is_some())
                    .collect();
                let [filled] = filled.as_slice() else {
                    continue;
                };
                if !self.spans.iter().zip(owners).any(|(word, &owner)| {
                    owner == *filled
                        && word.text().chars().any(char::is_alphabetic)
                }) {
                    continue;
                }
                let start =
                    selected.iter().map(|&i| table.cells[i].row).min()?;
                let end = selected
                    .iter()
                    .map(|&i| table.cells[i].row + table.cells[i].row_span)
                    .max()?;
                if selected
                    .iter()
                    .map(|&i| table.cells[i].row_span)
                    .sum::<usize>()
                    != end - start
                {
                    continue;
                }
                let merged = TableCell::builder()
                    .row(start)
                    .column(column)
                    .row_span(end - start)
                    .bbox(table.cells[*filled].bbox)
                    .build();
                if grid.try_merge(merged).is_ok() {
                    changed = true;
                }
            }
        }
        if !changed {
            return None;
        }
        let mapped = owners
            .iter()
            .map(|&owner| {
                let old = table.cells.get(owner)?;
                grid.table().cells.iter().position(|cell| {
                    cell.row <= old.row
                        && cell.row + cell.row_span >= old.row + old.row_span
                        && cell.column <= old.column
                        && cell.column + cell.column_span
                            >= old.column + old.column_span
                })
            })
            .collect::<Option<Vec<_>>>()?;
        grid.bind_words(mapped, self.spans.len()).ok()
    }
}
