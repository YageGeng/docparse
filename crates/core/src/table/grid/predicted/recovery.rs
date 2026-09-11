//! Source-supported refinements applied transactionally to a predicted grid.
use super::*;

impl TableGeometry<'_> {
    /// Splits a lost numeric column only when every body row supplies the same wide two-value gutter.
    #[allow(
        clippy::indexing_slicing,
        reason = "source and cell indices are derived from validated assignments"
    )]
    pub(super) fn split_numeric_column(
        &self,
        table: &Table,
        owners: &[usize],
        regions: &[Bbox],
        rules: &[TableRule],
    ) -> Option<CellGrid> {
        let header = table
            .cells
            .iter()
            .filter(|c| c.is_header)
            .map(|c| c.row + c.row_span)
            .max()
            .unwrap_or(1);
        if table.row_count < header + 3
            || (table.column_count + 1) * table.row_count > MAX_TABLE_CELLS
        {
            return None;
        }
        for column in 0..table.column_count {
            let Some((cut, supporting_rows)) =
                self.numeric_column_gap(table, owners, column, header)
            else {
                continue;
            };
            let proposed = table.split_predicted_column(column, cut, header)?;
            let mut x = vec![self.bounds.left];
            for c in 1..proposed.column_count {
                let left = proposed
                    .cells
                    .iter()
                    .filter(|cell| {
                        cell.row >= header
                            && cell.column + cell.column_span == c
                    })
                    .filter_map(|cell| cell.bbox.map(|b| b.right))
                    .max_by(f64::total_cmp)?;
                let right = proposed
                    .cells
                    .iter()
                    .filter(|cell| cell.row >= header && cell.column == c)
                    .filter_map(|cell| cell.bbox.map(|b| b.left))
                    .min_by(f64::total_cmp)?;
                x.push((left + right) * 0.5);
            }
            x.push(self.bounds.right);
            let mut y = vec![self.bounds.top; table.row_count + 1];
            y[table.row_count] = self.bounds.bottom;
            for (cell, b) in table.cells.iter().zip(regions) {
                y[cell.row] = b.top;
                y[cell.row + cell.row_span] = b.bottom;
            }
            let rows = (0..table.row_count)
                .map(|r| {
                    let physical: Vec<_> = self
                        .rows
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| {
                            row.baseline >= y[r] && row.baseline < y[r + 1]
                        })
                        .map(|(i, _)| i)
                        .collect();
                    let words = physical
                        .iter()
                        .flat_map(|&i| self.rows[i].spans.iter().copied())
                        .collect();
                    GridRow::builder()
                        .top(y[r])
                        .bottom(y[r + 1])
                        .physical(physical)
                        .words(words)
                        .build()
                })
                .collect();
            let mut grid =
                CellGrid::try_from(proposed).ok()?.with_rows(rows).ok()?;
            // Reuse the shared header transaction against the repaired model columns; body rows stay model-owned.
            let recovered =
                header::HeaderRecovery::new(self, &x, rules).recover(&mut grid);
            tracing::debug!(
                "numeric column {} at {:?}: header recovery {:?}, {} columns",
                column,
                self.bounds,
                recovered,
                grid.table().column_count
            );
            let repaired =
                if let Some(assignment) = self.assign_words(grid.table()) {
                    grid.bind_words(assignment, self.spans.len()).ok()
                } else {
                    self.assign_predicted(grid, rules).ok()
                };
            if let Some(grid) = repaired {
                tracing::info!(
                    "recovered one omitted numeric TSR column from {} aligned source rows at {:?}",
                    supporting_rows,
                    self.bounds
                );
                return Some(grid);
            }
        }
        None
    }

    /// Requires the same two numeric fields and wide gutter in every ordinary body row.
    #[allow(
        clippy::indexing_slicing,
        reason = "candidate rows and source groups provide validated indices"
    )]
    fn numeric_column_gap(
        &self,
        table: &Table,
        owners: &[usize],
        column: usize,
        header: usize,
    ) -> Option<(f64, usize)> {
        let cells: Vec<_> = table
            .cells
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.row >= header
                    && c.column <= column
                    && c.column + c.column_span > column
            })
            .collect();
        if cells.len() != table.row_count - header
            || cells
                .iter()
                .any(|(_, c)| c.column_span != 1 || c.row_span != 1)
        {
            return None;
        }
        let mut low = f64::NEG_INFINITY;
        let mut high = f64::INFINITY;
        let mut supported = true;
        for (index, _) in &cells {
            let mut words: Vec<_> = self
                .spans
                .iter()
                .zip(owners)
                .filter(|(_, owner)| **owner == *index)
                .map(|(word, _)| word)
                .collect();
            words.sort_by(|a, b| a.span.bbox.left.total_cmp(&b.span.bbox.left));
            let mut groups: Vec<Vec<&LocatedSpan<'_>>> = Vec::new();
            let mut edge = f64::NEG_INFINITY;
            for word in words {
                if word.span.bbox.left - edge >= self.font_size * 1.5 {
                    groups.push(Vec::new());
                }
                groups.last_mut()?.push(word);
                edge = edge.max(word.span.bbox.right);
            }
            if groups.len() != 2
                || groups.iter().any(|g| {
                    g.iter()
                        .map(|w| w.text().trim())
                        .collect::<String>()
                        .parse::<f64>()
                        .is_err()
                })
            {
                supported = false;
                break;
            }
            let baseline = groups[0][0].baseline;
            if groups
                .iter()
                .flatten()
                .any(|w| (w.baseline - baseline).abs() > self.font_size * 0.6)
            {
                supported = false;
                break;
            }
            low = low.max(
                groups[0]
                    .iter()
                    .map(|w| w.span.bbox.right)
                    .max_by(f64::total_cmp)?,
            );
            high = high.min(
                groups[1]
                    .iter()
                    .map(|w| w.span.bbox.left)
                    .min_by(f64::total_cmp)?,
            );
        }
        if !supported || high - low < self.font_size {
            return None;
        }
        Some(((low + high) * 0.5, cells.len()))
    }

    /// Rebuilds only declared header levels, then reuses shared body-span evidence without changing source ownership.
    #[allow(
        clippy::indexing_slicing,
        reason = "indices come from validated axes, model cells, and source row plans"
    )]
    pub(super) fn recover_predicted_header(
        &self,
        table: &Table,
        owners: &[usize],
        regions: &[Bbox],
        rules: &[TableRule],
    ) -> Option<CellGrid> {
        let declared_header = table
            .cells
            .iter()
            .filter(|c| c.is_header)
            .map(|c| c.row + c.row_span)
            .max();
        let old_header = declared_header.unwrap_or(1);
        let mut x = vec![None; table.column_count + 1];
        let mut y = vec![None; table.row_count + 1];
        for (cell, b) in table.cells.iter().zip(regions) {
            for (axis, index, value) in [
                (true, cell.column, b.left),
                (true, cell.column + cell.column_span, b.right),
                (false, cell.row, b.top),
                (false, cell.row + cell.row_span, b.bottom),
            ] {
                let position = if axis {
                    x.get_mut(index)?
                } else {
                    y.get_mut(index)?
                };
                if position.is_some_and(|previous: f64| {
                    (previous - value).abs() > 1e-6
                }) {
                    return None;
                }
                *position = Some(value);
            }
        }
        let x = x.into_iter().collect::<Option<Vec<_>>>()?;
        let mut y = y.into_iter().collect::<Option<Vec<_>>>()?;
        let mut proposed = table.clone();
        if declared_header.is_some() {
            // Model headers often flatten two visual levels into one row. Expand that bounded
            // header region into physical rows before the existing header transaction resolves spans.
            let physical: Vec<_> = self
                .rows
                .iter()
                .filter(|row| row.baseline < y[old_header])
                .collect();
            if physical.len() > old_header && physical.len() <= old_header + 4 {
                let delta = physical.len() - old_header;
                let mut boundaries = vec![y[0]];
                boundaries.extend(physical.windows(2).map(|pair| {
                    // Centered stubs span both levels and must not move a divider through ordinary header ink.
                    let bottom = pair[0]
                        .spans
                        .iter()
                        .map(|&i| &self.spans[i])
                        .filter(|word| {
                            (word.baseline - pair[0].baseline).abs()
                                <= self.font_size * 0.3
                        })
                        .map(|word| word.span.bbox.bottom)
                        .max_by(f64::total_cmp);
                    let top = pair[1]
                        .spans
                        .iter()
                        .map(|&i| &self.spans[i])
                        .filter(|word| {
                            (word.baseline - pair[1].baseline).abs()
                                <= self.font_size * 0.3
                        })
                        .map(|word| word.span.bbox.top)
                        .min_by(f64::total_cmp);
                    bottom.zip(top).filter(|(bottom, top)| bottom < top).map_or(
                        (pair[0].baseline + pair[1].baseline) * 0.5,
                        |(bottom, top)| (bottom + top) * 0.5,
                    )
                }));
                boundaries.extend(y.iter().skip(old_header).copied());
                y = boundaries;
                proposed.row_count += delta;
                proposed.cells.retain(|cell| cell.row >= old_header);
                for cell in &mut proposed.cells {
                    cell.row += delta;
                }
                for row in 0..physical.len() {
                    for column in 0..table.column_count {
                        proposed.cells.push(
                            TableCell::builder()
                                .row(row)
                                .column(column)
                                .is_header(true)
                                .bbox(Some(
                                    Bbox::try_from([
                                        x[column],
                                        y[row],
                                        x[column + 1],
                                        y[row + 1],
                                    ])
                                    .ok()?,
                                ))
                                .build(),
                        );
                    }
                }
            }
        }
        let rows = (0..proposed.row_count)
            .map(|r| {
                let physical: Vec<_> = self
                    .rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| {
                        row.baseline >= y[r] && row.baseline < y[r + 1]
                    })
                    .map(|(i, _)| i)
                    .collect();
                let words = physical
                    .iter()
                    .flat_map(|&i| self.rows[i].spans.iter().copied())
                    .collect();
                GridRow::builder()
                    .top(y[r])
                    .bottom(y[r + 1])
                    .physical(physical)
                    .words(words)
                    .build()
            })
            .collect();
        let mut grid =
            CellGrid::try_from(proposed).ok()?.with_rows(rows).ok()?;
        let header = if declared_header.is_some() {
            header::HeaderRecovery::new(self, &x, rules).recover(&mut grid)?
        } else {
            old_header
        };
        tracing::debug!(
            "proposed TSR headers at {:?}: {} rows, {} header rows",
            self.bounds,
            grid.table().row_count,
            header
        );
        let mut body_candidate = grid.clone();
        if self
            .recover_body_bands(&mut body_candidate, &x, rules, header)
            .is_some()
        {
            grid = body_candidate;
        }
        if declared_header.is_some() {
            let mut wrapped = grid.clone();
            if header::HeaderRecovery::new(self, &x, rules)
                .collapse_wrapped(&mut wrapped, header)
                .is_some()
            {
                grid = wrapped;
            }
        }
        let header = if declared_header.is_some() {
            grid.table()
                .cells
                .iter()
                .filter(|c| c.is_header)
                .map(|c| c.row + c.row_span)
                .max()
                .unwrap_or(header)
        } else {
            header
        };
        if grid.table() == table {
            return None;
        }
        let delta = grid.table().row_count as isize - table.row_count as isize;
        let previous_header = header.checked_add_signed(-delta)?;
        let mut mapped = Vec::with_capacity(owners.len());
        for (word, &owner) in self.spans.iter().zip(owners) {
            let old = table.cells.get(owner)?;
            if old.row >= previous_header {
                let row = old.row.checked_add_signed(delta)?;
                mapped.push(grid.table().cells.iter().position(|c| {
                    c.row <= row
                        && c.row + c.row_span >= row + old.row_span
                        && c.column <= old.column
                        && c.column + c.column_span
                            >= old.column + old.column_span
                })?);
            } else {
                let b = word.span.bbox;
                let candidates: Vec<_> = grid
                    .table()
                    .cells
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| {
                        c.row < header
                            && c.bbox.is_some_and(|c| {
                                (b.right.min(c.right) - b.left.max(c.left))
                                    .max(0.0)
                                    * (b.bottom.min(c.bottom)
                                        - b.top.max(c.top))
                                    .max(0.0)
                                    / b.area()
                                    >= 0.8
                            })
                    })
                    .map(|(i, _)| i)
                    .collect();
                let [owner] = candidates.as_slice() else {
                    tracing::debug!(
                        "TSR header candidate has {} owners for {} bytes {:?}",
                        candidates.len(),
                        word.item.id.as_str(),
                        word.span.byte_range
                    );
                    return None;
                };
                mapped.push(*owner);
            }
        }
        grid.bind_words(mapped, self.spans.len()).ok()
    }
}

impl Table {
    /// Inserts one supported body column while extending existing header and body spans across it.
    fn split_predicted_column(
        &self,
        column: usize,
        cut: f64,
        header: usize,
    ) -> Option<Self> {
        let mut proposed = self.clone();
        proposed.column_count += 1;
        proposed.cells.clear();
        for cell in &self.cells {
            let mut cell = cell.clone();
            if cell.column > column {
                cell.column += 1;
            } else if cell.column + cell.column_span > column {
                if cell.row >= header
                    && cell.row_span == 1
                    && cell.column_span == 1
                {
                    let b = cell.bbox?;
                    let mut right = cell.clone();
                    right.column += 1;
                    cell.bbox = Some(
                        Bbox::try_from([b.left, b.top, cut, b.bottom]).ok()?,
                    );
                    right.bbox = Some(
                        Bbox::try_from([cut, b.top, b.right, b.bottom]).ok()?,
                    );
                    proposed.cells.extend([cell, right]);
                    continue;
                }
                cell.column_span += 1;
            }
            proposed.cells.push(cell);
        }
        Some(proposed)
    }
}
