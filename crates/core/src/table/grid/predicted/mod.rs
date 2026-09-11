//! Source-aware synthesis over bounded model topology and immutable PDF facts.
mod axes;
mod binding;
mod recovery;
mod sections;
mod spans;
mod topology;
use super::*;

impl TableGeometry<'_> {
    /// Uses canonical line baselines for scripts, then validates unique ownership against measured ink.
    #[allow(
        clippy::indexing_slicing,
        reason = "canonical reordering uses indices generated from the validated cell vector"
    )]
    pub(in crate::table) fn assign_predicted(
        &self,
        grid: CellGrid,
        rules: &[TableRule],
    ) -> Result<CellGrid, String> {
        let mut table = grid.table().clone();
        let mut regions = self.calibrated_regions(&mut table, rules)?;
        self.split_ruled_spans(&mut table, &mut regions, rules)?;
        self.merge_shared_value_sections(&mut table, &mut regions, rules)?;
        let mut assignment = binding::WordAssignment::try_from((
            self,
            &table,
            regions.as_slice(),
        ))?;
        assignment.align_phrases(self, &table)?;
        let ink = assignment.measure(self, table.cells.len())?;
        for (cell, measured) in table.cells.iter_mut().zip(ink) {
            let Some(measured) = measured else {
                cell.bbox = None;
                continue;
            };
            // Cell geometry describes its measured content; row/column topology remains model-owned.
            // Per-cell extents allow a tall formula beside a shorter cell without imposing a global row cut.
            cell.bbox = Some(
                Bbox::try_from([
                    measured.left.max(self.bounds.left),
                    measured.top.max(self.bounds.top),
                    measured.right.min(self.bounds.right),
                    measured.bottom.min(self.bounds.bottom),
                ])
                .map_err(|e| e.to_string())?,
            );
        }
        let mut owners = assignment.0;
        self.merge_ruled_blanks(&mut table, &mut owners, &mut regions, rules)?;
        // Learned content rectangles may overlap: ownership is the explicit source assignment,
        // not the set of every rectangle touching that word. The common validator still requires
        // complete UTF-8 source coverage, no duplicate references, and 80% ink coverage by its owner.
        if let Some(grid) =
            self.split_numeric_column(&table, &owners, &regions, rules)
        {
            return Ok(grid);
        }
        if let Some(grid) =
            self.merge_label_bands(&table, &owners, &regions, rules)
        {
            return Ok(grid);
        }
        if let Some(grid) =
            self.recover_predicted_header(&table, &owners, &regions, rules)
        {
            return Ok(grid);
        }
        let mut indexed: Vec<_> = table.cells.into_iter().enumerate().collect();
        indexed.sort_by_key(|(_, cell)| (cell.row, cell.column));
        let mut remap = vec![0; indexed.len()];
        table.cells = indexed
            .into_iter()
            .enumerate()
            .map(|(index, (old, cell))| {
                remap[old] = index;
                cell
            })
            .collect();
        let assignment = owners.into_iter().map(|owner| remap[owner]).collect();
        tracing::debug!(
            "bound predicted {}x{} grid to {} measured source words at {:?}",
            table.row_count,
            table.column_count,
            self.spans.len(),
            self.bounds
        );
        CellGrid::try_from(table)?.bind_words(assignment, self.spans.len())
    }
}
