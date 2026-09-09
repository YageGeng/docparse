use std::collections::BTreeMap;

use docparse_layout::Bbox;

use super::{MAX_TABLE_CELLS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, Table};
use crate::{Block, TextItemId};

impl Table {
    /// Validates grid occupancy, UTF-8 references, complete source coverage, and cached text projections.
    pub(crate) fn validate(&self, block: &Block) -> Result<(), String> {
        if self.row_count == 0
            || self.column_count == 0
            || self.row_count > MAX_TABLE_ROWS
            || self.column_count > MAX_TABLE_COLUMNS
            || self.row_count * self.column_count > MAX_TABLE_CELLS
            || self.cells.len() > MAX_TABLE_CELLS
        {
            return Err(
                "table dimensions exceed the bounded grid contract".to_owned()
            );
        }
        let sources: BTreeMap<_, _> = block
            .lines
            .iter()
            .flat_map(|line| &line.text_items)
            .map(|item| (item.id.clone(), item))
            .collect();
        let mut coverage: BTreeMap<TextItemId, Vec<std::ops::Range<usize>>> =
            BTreeMap::new();
        let mut occupied = vec![false; self.row_count * self.column_count];
        for cell in &self.cells {
            let last_row = cell
                .row
                .checked_add(cell.row_span)
                .ok_or("table row span overflow")?;
            let last_column = cell
                .column
                .checked_add(cell.column_span)
                .ok_or("table column span overflow")?;
            if cell.row_span == 0
                || cell.column_span == 0
                || last_row > self.row_count
                || last_column > self.column_count
            {
                return Err("cell span extends outside the table".to_owned());
            }
            for row in cell.row..last_row {
                for column in cell.column..last_column {
                    let slot = occupied
                        .get_mut(row * self.column_count + column)
                        .ok_or("cell outside grid")?;
                    if std::mem::replace(slot, true) {
                        return Err("table cells overlap in grid coordinates"
                            .to_owned());
                    }
                }
            }
            if let Some(bbox) = cell.bbox {
                Self::validate_bounds(bbox, block.bbox)?;
            } else if !cell.lines.is_empty() {
                return Err("a populated cell needs measured bounds".to_owned());
            }
            for line in &cell.lines {
                Self::validate_bounds(line.bbox, block.bbox)?;
                if line.spans.is_empty() {
                    return Err(
                        "a cell line needs source references".to_owned()
                    );
                }
                for span in &line.spans {
                    let item = sources.get(&span.text_item_id).ok_or(
                        "cell references text outside its parent block",
                    )?;
                    if span.byte_range.is_empty()
                        || item.raw_text.get(span.byte_range.clone()).is_none()
                    {
                        return Err("cell reference is not a valid non-empty UTF-8 byte range".to_owned());
                    }
                    let cell_bbox = cell
                        .bbox
                        .ok_or("a populated cell needs measured bounds")?;
                    let overlap = (span.bbox.right.min(cell_bbox.right)
                        - span.bbox.left.max(cell_bbox.left))
                    .max(0.0)
                        * (span.bbox.bottom.min(cell_bbox.bottom)
                            - span.bbox.top.max(cell_bbox.top))
                        .max(0.0);
                    if overlap / span.bbox.area().max(f64::EPSILON) < 0.8 - 1e-9
                    {
                        return Err(
                            "cell geometry does not own its source text"
                                .to_owned(),
                        );
                    }
                    Self::validate_bounds(span.bbox, item.bbox)?;
                    Self::validate_bounds(span.bbox, line.bbox)?;
                    coverage
                        .entry(span.text_item_id.clone())
                        .or_default()
                        .push(span.byte_range.clone());
                }
                if line.derive_text(&sources).as_deref()
                    != Some(line.text.as_str())
                {
                    return Err("cell line text differs from its measured source projection".to_owned());
                }
            }
            if cell
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join("\n")
                != cell.text
            {
                return Err(
                    "cell text differs from its ordered lines".to_owned()
                );
            }
        }
        if occupied.contains(&false) {
            return Err(
                "grid positions need either a cell or a spanning owner"
                    .to_owned(),
            );
        }
        for (id, source) in sources {
            let mut ranges = coverage.remove(&id).unwrap_or_default();
            ranges.sort_by_key(|range| (range.start, range.end));
            let mut end = 0;
            for range in ranges {
                if range.start < end {
                    return Err(
                        "source characters are referenced by multiple cells"
                            .to_owned(),
                    );
                }
                if !source
                    .raw_text
                    .get(end..range.start)
                    .is_some_and(|gap| gap.chars().all(char::is_whitespace))
                {
                    return Err("table cells omit source characters".to_owned());
                }
                end = range.end;
            }
            if !source
                .raw_text
                .get(end..)
                .is_some_and(|gap| gap.chars().all(char::is_whitespace))
            {
                return Err(
                    "table cells omit trailing source characters".to_owned()
                );
            }
        }
        Ok(())
    }

    /// Accepts sub-point glyph/loose-box rounding while rejecting non-finite or unrelated geometry.
    fn validate_bounds(bbox: Bbox, parent: Bbox) -> Result<(), String> {
        if ![bbox.left, bbox.top, bbox.right, bbox.bottom]
            .iter()
            .all(|value| value.is_finite())
            || bbox.width() <= 0.0
            || bbox.height() <= 0.0
            || bbox.left < parent.left - 1.0
            || bbox.top < parent.top - 1.0
            || bbox.right > parent.right + 1.0
            || bbox.bottom > parent.bottom + 1.0
        {
            return Err(
                "table geometry is invalid or outside its source bounds"
                    .to_owned(),
            );
        }
        Ok(())
    }
}
