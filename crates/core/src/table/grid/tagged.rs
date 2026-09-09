use super::*;

impl TableGrid<'_> {
    /// Uses tagged cell ownership only when every word resolves to one unambiguous MCID owner.
    pub fn tagged(&self, tables: &[TaggedTable]) -> Option<RecoveredGrid> {
        for table in tables {
            if table.row_count == 0
                || table.column_count == 0
                || table.row_count > MAX_TABLE_ROWS
                || table.column_count > MAX_TABLE_COLUMNS
                || table.row_count * table.column_count > MAX_TABLE_CELLS
                || table.cells.len() > MAX_TABLE_CELLS
            {
                continue;
            }
            if table.cells.iter().any(|cell| {
                cell.row_span == 0
                    || cell.column_span == 0
                    || cell
                        .row
                        .checked_add(cell.row_span)
                        .is_none_or(|end| end > table.row_count)
                    || cell
                        .column
                        .checked_add(cell.column_span)
                        .is_none_or(|end| end > table.column_count)
            }) {
                continue;
            }
            let mut owners = BTreeMap::new();
            let mut ambiguous = false;
            for (index, cell) in table.cells.iter().enumerate() {
                for &mcid in &cell.mcids {
                    if owners.insert(mcid, index).is_some() {
                        ambiguous = true;
                    }
                }
            }
            if ambiguous {
                continue;
            }
            let Some(assignment) = self
                .spans
                .iter()
                .map(|span| {
                    span.mcid.and_then(|mcid| owners.get(&mcid).copied())
                })
                .collect::<Option<Vec<_>>>()
            else {
                continue;
            };
            let mut boxes: Vec<Option<Bbox>> = vec![None; table.cells.len()];
            for (span, &cell) in self.spans.iter().zip(&assignment) {
                let bounds = boxes.get_mut(cell)?;
                *bounds = Some(match *bounds {
                    Some(previous) => Bbox::try_from([
                        previous.left.min(span.span.bbox.left),
                        previous.top.min(span.span.bbox.top),
                        previous.right.max(span.span.bbox.right),
                        previous.bottom.max(span.span.bbox.bottom),
                    ])
                    .ok()?,
                    None => span.span.bbox,
                });
            }
            let Some(columns) = self.tagged_columns(table, &boxes) else {
                continue;
            };
            let mut cells = Vec::new();
            let mut occupied = BTreeSet::new();
            for ((cell, bbox), column) in
                table.cells.iter().zip(boxes).zip(columns)
            {
                for row in cell.row..cell.row + cell.row_span {
                    for column in column..column + cell.column_span {
                        occupied.insert((row, column));
                    }
                }
                cells.push(
                    TableCell::builder()
                        .row(cell.row)
                        .column(column)
                        .row_span(cell.row_span)
                        .column_span(cell.column_span)
                        .bbox(bbox)
                        .is_header(cell.is_header)
                        .build(),
                );
            }
            for row in 0..table.row_count {
                for column in 0..table.column_count {
                    if !occupied.contains(&(row, column)) {
                        cells.push(
                            TableCell::builder()
                                .row(row)
                                .column(column)
                                .build(),
                        );
                    }
                }
            }
            return Some(RecoveredGrid {
                table: Table::builder()
                    .row_count(table.row_count)
                    .column_count(table.column_count)
                    .cells(cells)
                    .source(TableStructureSource::TaggedPdf)
                    .build(),
                assignment,
            });
        }
        None
    }

    /// Realigns page-filtered tagged rows when PDFium omitted empty cells from the tree.
    fn tagged_columns(
        &self,
        table: &TaggedTable,
        boxes: &[Option<Bbox>],
    ) -> Option<Vec<usize>> {
        let mut occupied_rows = vec![BTreeSet::new(); table.row_count];
        for cell in &table.cells {
            for row in cell.row..cell.row + cell.row_span {
                occupied_rows
                    .get_mut(row)?
                    .extend(cell.column..cell.column + cell.column_span);
            }
        }
        if occupied_rows
            .iter()
            .all(|row| row.len() == table.column_count)
        {
            return Some(table.cells.iter().map(|cell| cell.column).collect());
        }
        // Complete rows anchor physical columns. Median edges tolerate header widths
        // and a few shifted nominal indices after a filtered rowspan-bearing row.
        let mut anchors = vec![Vec::<Bbox>::new(); table.column_count];
        for (cell, bbox) in table.cells.iter().zip(boxes) {
            if cell.column_span == 1
                && occupied_rows.get(cell.row)?.len() == table.column_count
                && let Some(bbox) = bbox
            {
                anchors.get_mut(cell.column)?.push(*bbox);
            }
        }
        let mut edges = Vec::new();
        for column in anchors {
            let mut lefts: Vec<_> =
                column.iter().map(|bbox| bbox.left).collect();
            let mut rights: Vec<_> =
                column.iter().map(|bbox| bbox.right).collect();
            lefts.sort_by(f64::total_cmp);
            rights.sort_by(f64::total_cmp);
            edges.push((
                *lefts.get(lefts.len() / 2)?,
                *rights.get(rights.len() / 2)?,
            ));
        }
        let mut cuts = vec![self.bounds.left];
        for pair in edges.windows(2) {
            let previous = pair.first()?;
            let next = pair.get(1)?;
            if previous.0 >= next.0 {
                return None;
            }
            cuts.push((previous.1 + next.0) * 0.5);
        }
        cuts.push(self.bounds.right);
        if cuts.windows(2).any(|pair| pair.first() >= pair.get(1)) {
            return None;
        }
        let mut used = BTreeSet::new();
        let mut columns = vec![0; table.cells.len()];
        for row in 0..table.row_count {
            let row_cells: Vec<_> = table
                .cells
                .iter()
                .enumerate()
                .filter(|(_, cell)| cell.row == row)
                .collect();
            let free = table.column_count
                - used.iter().filter(|(r, _)| *r == row).count();
            let complete = row_cells
                .iter()
                .map(|(_, cell)| cell.column_span)
                .sum::<usize>()
                == free;
            let mut previous_end = 0;
            for (index, cell) in row_cells {
                let fits = |column: usize| {
                    column >= previous_end
                        && column + cell.column_span <= table.column_count
                        && (row..row + cell.row_span).all(|r| {
                            (column..column + cell.column_span)
                                .all(|c| !used.contains(&(r, c)))
                        })
                };
                let column = if complete {
                    (0..table.column_count).find(|&column| fits(column))?
                } else {
                    let bbox = boxes.get(index)?.as_ref()?;
                    let choices: Vec<_> = (0..table.column_count)
                        .filter(|&column| {
                            if !fits(column) {
                                return false;
                            }
                            let Some((&left, &right)) = cuts
                                .get(column)
                                .zip(cuts.get(column + cell.column_span))
                            else {
                                return false;
                            };
                            if cell.column_span == 1 {
                                bbox.center().x >= left
                                    && bbox.center().x < right
                            } else {
                                (bbox.right.min(right) - bbox.left.max(left))
                                    .max(0.0)
                                    >= bbox.width() * 0.8
                            }
                        })
                        .collect();
                    if choices.len() != 1 {
                        return None;
                    }
                    *choices.first()?
                };
                *columns.get_mut(index)? = column;
                previous_end = column + cell.column_span;
                for r in row..row + cell.row_span {
                    for c in column..column + cell.column_span {
                        used.insert((r, c));
                    }
                }
            }
        }
        Some(columns)
    }
}
