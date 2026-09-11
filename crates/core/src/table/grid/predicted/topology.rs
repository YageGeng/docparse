//! Reconciliation of learned spans against independent model positions.
use super::*;

/// Dense model rows and their robust column centers.
struct ColumnAnchors {
    by_row: Vec<Vec<usize>>,
    centers: Vec<f64>,
}
impl TryFrom<&Table> for ColumnAnchors {
    type Error = String;

    /// Derives column evidence only from complete unspanned model rows.
    #[allow(
        clippy::indexing_slicing,
        reason = "row entries are indices generated from the bounded model cells"
    )]
    fn try_from(table: &Table) -> Result<Self, Self::Error> {
        let mut by_row = vec![Vec::new(); table.row_count];
        for (index, cell) in table.cells.iter().enumerate() {
            by_row
                .get_mut(cell.row)
                .ok_or("model row outside table")?
                .push(index);
        }
        let count = by_row
            .iter()
            .filter(|row| row.iter().all(|&i| table.cells[i].column_span == 1))
            .map(Vec::len)
            .max()
            .ok_or("no dense model row supports column repair")?;
        if count == 0
            || count > MAX_TABLE_COLUMNS
            || count * table.row_count > MAX_TABLE_CELLS
        {
            return Err("repaired model dimensions exceed bounded grid limits"
                .to_owned());
        }
        let mut anchors = vec![Vec::new(); count];
        for row in &by_row {
            if row.len() != count
                || row.iter().any(|&i| table.cells[i].column_span != 1)
            {
                continue;
            }
            for (column, &index) in row.iter().enumerate() {
                let b = table.cells[index]
                    .bbox
                    .ok_or("model cell has no position")?;
                anchors[column].push((b.left + b.right) * 0.5);
            }
        }
        let anchors: Vec<_> = anchors
            .into_iter()
            .map(|mut samples| {
                samples.sort_by(f64::total_cmp);
                samples[samples.len() / 2]
            })
            .collect();
        if anchors.windows(2).any(|p| p[0] >= p[1]) {
            return Err("dense model columns are not ordered".to_owned());
        }
        Ok(Self {
            by_row,
            centers: anchors,
        })
    }
}

/// A partial monotone placement and its accumulated model-position cost.
#[derive(Clone)]
struct RowPlacement {
    score: f64,
    cells: Vec<(usize, usize)>,
}

impl Table {
    /// Reconciles model topology before it enters the shared strict grid decoder.
    pub(crate) fn reconcile_predicted_grid(
        &mut self,
        bounds: Bbox,
    ) -> Result<(), String> {
        let original = (self.row_count, self.column_count, self.cells.len());
        let anchors = ColumnAnchors::try_from(&*self)?;
        self.place_predicted_columns(&anchors, bounds)?;
        self.complete_predicted_grid(&anchors, bounds)?;
        self.column_count = anchors.centers.len();
        self.cells.sort_by_key(|cell| (cell.row, cell.column));
        CellGrid::try_from(self.clone())?;
        tracing::info!(
            "reconciled predicted grid {:?} into {} rows, {} columns and {} cells using model positions",
            original,
            self.row_count,
            self.column_count,
            self.cells.len()
        );
        Ok(())
    }

    /// Places each token row monotonically while allowing gaps occupied by spanning cells.
    #[allow(
        clippy::indexing_slicing,
        reason = "DP positions are bounded by the validated column anchors"
    )]
    fn place_predicted_columns(
        &mut self,
        anchors: &ColumnAnchors,
        bounds: Bbox,
    ) -> Result<(), String> {
        let count = anchors.centers.len();
        for row in &anchors.by_row {
            if let [index] = row.as_slice() {
                let b = self.cells[*index]
                    .bbox
                    .ok_or("model cell lacks position")?;
                if b.left <= anchors.centers[0]
                    && b.right >= anchors.centers[count - 1]
                {
                    self.cells[*index].column = 0;
                    self.cells[*index].column_span = count;
                    continue;
                }
            }
            // Dynamic programming preserves the model's cell sequence while allowing missing occupied columns.
            let mut states: Vec<Option<RowPlacement>> = vec![None; count + 1];
            states[0] = Some(RowPlacement {
                score: 0.0,
                cells: Vec::new(),
            });
            for &index in row {
                let cell = &self.cells[index];
                let b = cell.bbox.ok_or("model cell lacks position")?;
                let center = (b.left + b.right) * 0.5;
                let mut next: Vec<Option<RowPlacement>> = vec![None; count + 1];
                for (end, state) in states.iter().enumerate() {
                    let Some(state) = state else {
                        continue;
                    };
                    for start in end..count {
                        let enclosed = if cell.is_header {
                            anchors
                                .centers
                                .iter()
                                .filter(|&&x| x >= b.left && x <= b.right)
                                .count()
                        } else {
                            1
                        };
                        let spans = 1..=cell
                            .column_span
                            .max(enclosed)
                            .max(1)
                            .min(count - start);
                        for span in spans {
                            let finish = start + span;
                            let expected = (anchors.centers[start]
                                + anchors.centers[finish - 1])
                                * 0.5;
                            let scale = if count == 1 {
                                bounds.width()
                            } else if start == 0 {
                                anchors.centers[1] - anchors.centers[0]
                            } else {
                                anchors.centers[start]
                                    - anchors.centers[start - 1]
                            };
                            let candidate = state.score
                                + ((center - expected) / scale).powi(2)
                                + 0.1 * span.abs_diff(cell.column_span) as f64;
                            if next[finish].as_ref().is_none_or(|previous| {
                                candidate < previous.score
                            }) {
                                let mut path = state.cells.clone();
                                path.push((start, span));
                                next[finish] = Some(RowPlacement {
                                    score: candidate,
                                    cells: path,
                                });
                            }
                        }
                    }
                }
                states = next;
            }
            let placement = states
                .into_iter()
                .flatten()
                .min_by(|a, b| a.score.total_cmp(&b.score))
                .ok_or("model row cannot fit the supported columns")?;
            for (&index, (column, span)) in row.iter().zip(placement.cells) {
                self.cells[index].column = column;
                self.cells[index].column_span = span;
            }
        }
        Ok(())
    }

    /// Repairs rowspan endpoints and adds explicit empty owners for remaining positions.
    #[allow(
        clippy::indexing_slicing,
        reason = "occupancy and positions share the bounded model dimensions"
    )]
    fn complete_predicted_grid(
        &mut self,
        anchors: &ColumnAnchors,
        bounds: Bbox,
    ) -> Result<(), String> {
        let count = anchors.centers.len();
        // A spanning owner ends where a later model cell starts in the same columns.
        // This repairs off-by-one learned rowspans without creating rows from PDF text.
        for index in 0..self.cells.len() {
            let cell = &self.cells[index];
            if cell.row_span <= 1 {
                continue;
            }
            let end = self
                .cells
                .iter()
                .filter(|other| {
                    other.row > cell.row
                        && other.column < cell.column + cell.column_span
                        && other.column + other.column_span > cell.column
                })
                .map(|other| other.row)
                .min()
                .unwrap_or(self.row_count);
            self.cells[index].row_span = end - cell.row;
        }
        let mut occupied = vec![false; self.row_count * count];
        for cell in &self.cells {
            for row in cell.row..cell.row + cell.row_span {
                for col in cell.column..cell.column + cell.column_span {
                    let slot = occupied
                        .get_mut(row * count + col)
                        .ok_or("repaired span outside table")?;
                    if std::mem::replace(slot, true) {
                        return Err(
                            "model positions imply overlapping repaired spans"
                                .to_owned(),
                        );
                    }
                }
            }
        }
        let mut columns = vec![bounds.left];
        columns.extend(anchors.centers.windows(2).map(|p| (p[0] + p[1]) * 0.5));
        columns.push(bounds.right);
        let centers: Vec<_> = anchors
            .by_row
            .iter()
            .map(|row| {
                let mut positions: Vec<_> = row
                    .iter()
                    .filter_map(|&i| {
                        let c = &self.cells[i];
                        let b = c.bbox?;
                        (c.row_span == 1).then_some((b.top + b.bottom) * 0.5)
                    })
                    .collect();
                positions.sort_by(f64::total_cmp);
                positions
                    .get(positions.len() / 2)
                    .copied()
                    .ok_or("model row lacks a position anchor")
            })
            .collect::<Result<_, _>>()?;
        let mut rows = vec![bounds.top];
        rows.extend(centers.windows(2).map(|p| (p[0] + p[1]) * 0.5));
        rows.push(bounds.bottom);
        for row in 0..self.row_count {
            for column in 0..count {
                if occupied[row * count + column] {
                    continue;
                }
                let bbox = Bbox::try_from([
                    columns[column],
                    rows[row],
                    columns[column + 1],
                    rows[row + 1],
                ])
                .map_err(|e| e.to_string())?;
                self.cells.push(
                    TableCell::builder()
                        .row(row)
                        .column(column)
                        .bbox(Some(bbox))
                        .build(),
                );
            }
        }
        Ok(())
    }
}
