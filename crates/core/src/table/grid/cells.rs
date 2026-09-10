//! Transactional topology edits over one bounded table candidate.

use std::ops::Range;

use docparse_layout::Bbox;
use typed_builder::TypedBuilder;

use super::{
    MAX_TABLE_CELLS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, Table, TableCell,
};

/// One logical row and the immutable source rows/words it represents.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct GridRow {
    pub top: f64,
    pub bottom: f64,
    pub physical: Vec<usize>,
    pub words: Vec<usize>,
}

/// A private candidate whose topology is frozen once source words are bound.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct CellGrid {
    table: Table,
    #[builder(default)]
    rows: Vec<GridRow>,
    owners: Vec<usize>,
    #[builder(default)]
    assignment: Option<Vec<usize>>,
}

impl TryFrom<Table> for CellGrid {
    type Error = String;

    /// Rejects holes, overlapping cells, invalid geometry, and excessive allocations.
    fn try_from(table: Table) -> Result<Self, Self::Error> {
        if table.row_count == 0
            || table.row_count > MAX_TABLE_ROWS
            || table.column_count == 0
            || table.column_count > MAX_TABLE_COLUMNS
            || table
                .row_count
                .checked_mul(table.column_count)
                .is_none_or(|n| n > MAX_TABLE_CELLS)
            || table.cells.len() > MAX_TABLE_CELLS
        {
            return Err(
                "table dimensions exceed the bounded grid contract".to_owned()
            );
        }
        let mut owners = vec![usize::MAX; table.row_count * table.column_count];
        for (index, cell) in table.cells.iter().enumerate() {
            let last_row = cell
                .row
                .checked_add(cell.row_span)
                .ok_or("row span overflow")?;
            let last_column = cell
                .column
                .checked_add(cell.column_span)
                .ok_or("column span overflow")?;
            if cell.row_span == 0
                || cell.column_span == 0
                || last_row > table.row_count
                || last_column > table.column_count
            {
                return Err("cell span extends outside the table".to_owned());
            }
            if let Some(b) = cell.bbox {
                Bbox::try_from([b.left, b.top, b.right, b.bottom])
                    .map_err(|e| e.to_string())?;
            }
            for row in cell.row..last_row {
                for column in cell.column..last_column {
                    let owner = owners
                        .get_mut(row * table.column_count + column)
                        .ok_or("cell outside grid")?;
                    if std::mem::replace(owner, index) != usize::MAX {
                        return Err("table cells overlap in grid coordinates"
                            .to_owned());
                    }
                }
            }
        }
        if owners.contains(&usize::MAX) {
            return Err(
                "grid positions need either a cell or a spanning owner"
                    .to_owned(),
            );
        }
        Ok(Self::builder().table(table).owners(owners).build())
    }
}

impl CellGrid {
    /// Reads the candidate without exposing independent mutable arrays.
    pub fn table(&self) -> &Table {
        &self.table
    }

    /// Reads synchronized logical-row evidence when supplied by a geometry strategy.
    pub(super) fn rows(&self) -> &[GridRow] {
        &self.rows
    }

    /// Installs source row bands before topology editing or source binding.
    pub(super) fn with_rows(
        mut self,
        rows: Vec<GridRow>,
    ) -> Result<Self, String> {
        if self.assignment.is_some()
            || rows.len() != self.table.row_count
            || rows.iter().any(|r| {
                !r.top.is_finite() || !r.bottom.is_finite() || r.bottom <= r.top
            })
        {
            return Err("invalid logical row plan".to_owned());
        }
        self.rows = rows;
        Ok(self)
    }

    /// Replaces complete cells in one rectangle without cutting an existing span.
    pub(super) fn replace_region(
        &mut self,
        rows: Range<usize>,
        columns: Range<usize>,
        cells: Vec<TableCell>,
    ) -> Result<(), String> {
        if self.assignment.is_some()
            || rows.is_empty()
            || columns.is_empty()
            || rows.end > self.table.row_count
            || columns.end > self.table.column_count
        {
            return Err("invalid or frozen grid edit".to_owned());
        }
        let mut proposed = self.table.clone();
        let mut kept = Vec::new();
        let mut selected = vec![false; proposed.cells.len()];
        for row in rows.clone() {
            for column in columns.clone() {
                let owner = *self
                    .owners
                    .get(row * self.table.column_count + column)
                    .ok_or("missing grid owner")?;
                *selected.get_mut(owner).ok_or("invalid grid owner")? = true;
            }
        }
        for (index, cell) in proposed.cells.iter().enumerate() {
            let bottom = cell.row + cell.row_span;
            let right = cell.column + cell.column_span;
            let intersects =
                selected.get(index).copied().ok_or("missing selection")?;
            if intersects {
                if cell.row < rows.start
                    || bottom > rows.end
                    || cell.column < columns.start
                    || right > columns.end
                {
                    return Err("grid edit cuts an existing span".to_owned());
                }
                if !cell.lines.is_empty() || !cell.text.is_empty() {
                    return Err(
                        "grid edit cannot discard populated cells".to_owned()
                    );
                }
            } else {
                kept.push(cell.clone());
            }
        }
        if cells.iter().any(|c| {
            c.row < rows.start
                || c.column < columns.start
                || c.row
                    .checked_add(c.row_span)
                    .is_none_or(|end| end > rows.end)
                || c.column
                    .checked_add(c.column_span)
                    .is_none_or(|end| end > columns.end)
        }) {
            return Err("replacement extends outside its region".to_owned());
        }
        kept.extend(cells);
        proposed.cells = kept;
        let mut next = Self::try_from(proposed)?;
        next.rows.clone_from(&self.rows);
        *self = next;
        Ok(())
    }

    /// Replaces a row band and remaps later cells and row evidence in one validated transaction.
    pub(super) fn replace_rows(
        &mut self,
        range: Range<usize>,
        rows: Vec<GridRow>,
        cells: Vec<TableCell>,
    ) -> Result<(), String> {
        if self.assignment.is_some()
            || range.is_empty()
            || range.end > self.table.row_count
            || self.rows.len() != self.table.row_count
            || rows.is_empty()
        {
            return Err("invalid or frozen row edit".to_owned());
        }
        let count = rows.len();
        let mut proposed = self.table.clone();
        let mut kept = Vec::new();
        for cell in &proposed.cells {
            let bottom = cell.row + cell.row_span;
            if cell.row < range.end && bottom > range.start {
                if cell.row < range.start
                    || bottom > range.end
                    || !cell.text.is_empty()
                    || !cell.lines.is_empty()
                {
                    return Err(
                        "row edit cuts a span or discards text".to_owned()
                    );
                }
            } else {
                let mut cell = cell.clone();
                if cell.row >= range.end {
                    cell.row = cell.row - range.len() + count;
                }
                kept.push(cell);
            }
        }
        if cells.iter().any(|c| {
            c.row < range.start
                || c.row
                    .checked_add(c.row_span)
                    .is_none_or(|end| end > range.start + count)
        }) {
            return Err("replacement extends outside its row band".to_owned());
        }
        kept.extend(cells);
        proposed.cells = kept;
        proposed.row_count = proposed.row_count - range.len() + count;
        let mut next = Self::try_from(proposed)?;
        let mut all_rows = self.rows.clone();
        all_rows.splice(range, rows);
        next = next.with_rows(all_rows)?;
        *self = next;
        Ok(())
    }

    /// Merges a rectangular region while preserving the caller's measured enclosure.
    pub(super) fn try_merge(&mut self, cell: TableCell) -> Result<(), String> {
        let end_row = cell
            .row
            .checked_add(cell.row_span)
            .ok_or("row span overflow")?;
        let end_column = cell
            .column
            .checked_add(cell.column_span)
            .ok_or("column span overflow")?;
        self.replace_region(
            cell.row..end_row,
            cell.column..end_column,
            vec![cell],
        )
    }

    /// Marks a local inferred header before binding, without changing any cell geometry.
    pub(super) fn set_header(&mut self, row: usize) -> Result<(), String> {
        if self.assignment.is_some() || row >= self.table.row_count {
            return Err("invalid or frozen header edit".to_owned());
        }
        for cell in self.table.cells.iter_mut().filter(|cell| cell.row == row) {
            cell.is_header = true;
        }
        Ok(())
    }

    /// Freezes topology after assigning exactly one existing cell to each source word.
    pub fn bind_words(
        mut self,
        assignment: Vec<usize>,
        word_count: usize,
    ) -> Result<Self, String> {
        if self.assignment.is_some()
            || assignment.len() != word_count
            || assignment
                .iter()
                .any(|&cell| cell >= self.table.cells.len())
        {
            return Err("invalid source word assignment".to_owned());
        }
        self.assignment = Some(assignment);
        Ok(self)
    }

    /// Consumes a frozen candidate for the shared source-preserving text population step.
    pub fn into_parts(self) -> Result<(Table, Vec<usize>), String> {
        Ok((
            self.table,
            self.assignment.ok_or("table words were not assigned")?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TableStructureSource;

    /// Builds a small canonical grid with explicit empty cells.
    fn grid() -> CellGrid {
        let cells = (0..2)
            .flat_map(|row| {
                (0..2).map(move |column| {
                    TableCell::builder().row(row).column(column).build()
                })
            })
            .collect();
        CellGrid::try_from(
            Table::builder()
                .row_count(2)
                .column_count(2)
                .cells(cells)
                .source(TableStructureSource::Ruled)
                .build(),
        )
        .expect("grid")
    }

    /// A covered slot shares its owner's identity, and rejected edits leave every layer unchanged.
    #[test]
    fn merges_are_atomic_and_do_not_cut_spans() {
        let mut grid = grid();
        grid.try_merge(
            TableCell::builder().row(0).column(0).column_span(2).build(),
        )
        .expect("merge");
        assert_eq!(grid.owners.first(), grid.owners.get(1));
        let before = grid.clone();
        assert!(
            grid.try_merge(
                TableCell::builder().row(0).column(1).row_span(2).build()
            )
            .is_err()
        );
        assert_eq!(grid, before);
        assert!(grid.replace_region(1..2, 0..2, Vec::new()).is_err());
        assert_eq!(grid, before);
    }

    /// Binding cannot lose a source word and prevents later structure edits.
    #[test]
    fn binding_freezes_topology() {
        assert_eq!(
            grid().bind_words(vec![0], 2).expect_err("missing word"),
            "invalid source word assignment"
        );
        let mut bound = grid().bind_words(vec![0, 3], 2).expect("bound");
        assert!(bound.set_header(0).is_err());
        assert!(
            bound
                .try_merge(
                    TableCell::builder()
                        .row(0)
                        .column(0)
                        .column_span(2)
                        .build()
                )
                .is_err()
        );
        assert_eq!(bound.into_parts().expect("parts").1, [0, 3]);
    }

    /// Invalid row plans roll back both cells and evidence, while a valid split remaps later rows together.
    #[test]
    fn row_edits_preserve_atomicity_and_source_bands() {
        let rows = (0..2)
            .map(|i| {
                GridRow::builder()
                    .top(i as f64 * 10.0)
                    .bottom((i + 1) as f64 * 10.0)
                    .physical(vec![i])
                    .words(vec![i])
                    .build()
            })
            .collect();
        let mut grid = grid().with_rows(rows).expect("row evidence");
        let before = grid.clone();
        let mut replacement = before.rows.first().expect("first row").clone();
        replacement.bottom = f64::NAN;
        let cell = TableCell::builder().row(0).column(0).column_span(2).build();
        assert!(
            grid.replace_rows(0..1, vec![replacement], vec![cell])
                .is_err()
        );
        assert_eq!(grid, before);
        assert!(
            grid.replace_region(
                0..1,
                0..2,
                vec![
                    TableCell::builder()
                        .row(0)
                        .column(0)
                        .column_span(2)
                        .build(),
                    TableCell::builder().row(0).column(1).build(),
                ]
            )
            .is_err()
        );
        assert_eq!(grid, before);

        let mut first = before.rows.first().expect("first row").clone();
        let mut second = first.clone();
        first.bottom = 5.0;
        second.top = 5.0;
        grid.replace_rows(
            0..1,
            vec![first, second],
            vec![
                TableCell::builder().row(0).column(0).column_span(2).build(),
                TableCell::builder().row(1).column(0).column_span(2).build(),
            ],
        )
        .expect("split row");
        assert_eq!(grid.table.row_count, 3);
        assert_eq!(grid.rows.get(2), before.rows.get(1));
        assert_eq!(grid.table.cells.iter().filter(|c| c.row == 2).count(), 2);
        assert_eq!(grid.owners.len(), 6);
    }
}
