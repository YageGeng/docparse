//! Matches independent cell observations to logical structure without assuming detector order.
use crate::table::Table;
use docparse_layout::Bbox;
use std::collections::BTreeMap;

impl Table {
    /// Uses learned anchors when available, otherwise aligns detector rows with the declared topology.
    #[allow(
        clippy::indexing_slicing,
        reason = "indices are produced from immutable enumerated cells and nonempty detector rows; grid dimensions are bounded by token decoding"
    )]
    pub(super) fn match_detected_cells(
        &mut self,
        mut boxes: Vec<Bbox>,
    ) -> Result<(), String> {
        if self.cells.is_empty() || boxes.is_empty() {
            return Err("independent cell matching requires structure cells and detections".to_owned());
        }
        // Remove only near-identical observations; spanning cells remain independent of their neighbors.
        boxes.sort_by(|a, b| b.area().total_cmp(&a.area()));
        let mut unique = Vec::<Bbox>::new();
        for bbox in boxes {
            if !unique.iter().any(|other| bbox.iou(*other) > 0.9) {
                unique.push(bbox);
            }
        }

        let mut heights: Vec<_> =
            unique.iter().map(|bbox| bbox.height()).collect();
        heights.sort_by(f64::total_cmp);
        let tolerance = (heights[heights.len() / 2] * 0.2).clamp(0.5, 5.0);
        unique.sort_by(|a, b| {
            a.top
                .total_cmp(&b.top)
                .then_with(|| a.left.total_cmp(&b.left))
        });
        let mut rows = Vec::<Vec<Bbox>>::new();
        for bbox in unique {
            if let Some(row) = rows.last_mut()
                && (bbox.top - row[0].top).abs() <= tolerance
            {
                row.push(bbox);
            } else {
                rows.push(vec![bbox]);
            }
        }
        for row in &mut rows {
            row.sort_by(|a, b| a.left.total_cmp(&b.left));
        }
        let mut logical = BTreeMap::<usize, Vec<usize>>::new();
        for (index, cell) in self.cells.iter().enumerate() {
            logical.entry(cell.row).or_default().push(index);
        }
        // A complete unmerged row can anchor missing empty cells elsewhere without inventing column positions.
        let dense = logical.values().zip(&rows).find_map(|(indices, row)| {
            (indices.len() == self.column_count
                && row.len() == self.column_count
                && indices.iter().all(|&i| self.cells[i].column_span == 1))
            .then_some(row)
        });
        // Row-consistent independent geometry can correct crossed or shifted structure-head anchors.
        let row_compatible = rows.len() == logical.len()
            && (dense.is_some()
                || logical
                    .values()
                    .zip(&rows)
                    .all(|(indices, row)| indices.len() == row.len()));
        if self.cells.iter().all(|cell| cell.bbox.is_some()) && !row_compatible
        {
            let unique: Vec<_> = rows.iter().flatten().copied().collect();
            let mut pairs = Vec::new();
            for (cell_index, cell) in self.cells.iter().enumerate() {
                let anchor = cell.bbox.ok_or("missing structure anchor")?;
                for (detection_index, bbox) in unique.iter().enumerate() {
                    let overlap = anchor.intersection_area(*bbox);
                    if overlap / anchor.area() < 0.5 {
                        continue;
                    }
                    // A coarse detection spanning another logical row cannot replace this finer position.
                    let crosses_rows = self.cells.iter().enumerate().any(
                        |(other_index, other)| {
                            other_index != cell_index
                                && (other.row >= cell.row + cell.row_span
                                    || cell.row >= other.row + other.row_span)
                                && other.column < cell.column + cell.column_span
                                && cell.column
                                    < other.column + other.column_span
                                && other.bbox.is_some_and(|other_box| {
                                    bbox.intersection_area(other_box)
                                        / other_box.area()
                                        >= 0.5
                                })
                        },
                    );
                    if crosses_rows {
                        continue;
                    }
                    let distance = (anchor.center().x - bbox.center().x).abs()
                        / bbox.width()
                        + (anchor.center().y - bbox.center().y).abs()
                            / bbox.height();
                    pairs.push((
                        anchor.iou(*bbox) + overlap / anchor.area()
                            - distance * 0.1,
                        cell_index,
                        detection_index,
                    ));
                }
            }
            pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
            let mut cells_used = vec![false; self.cells.len()];
            let mut boxes_used = vec![false; unique.len()];
            let mut matched = 0;
            for (_, cell_index, detection_index) in pairs {
                if cells_used[cell_index] || boxes_used[detection_index] {
                    continue;
                }
                self.cells[cell_index].bbox = Some(unique[detection_index]);
                cells_used[cell_index] = true;
                boxes_used[detection_index] = true;
                matched += 1;
            }
            tracing::info!(
                "matched {} of {} structure anchors to {} independent cell detections",
                matched,
                self.cells.len(),
                unique.len()
            );
            return Ok(());
        }

        if rows.len() != logical.len() {
            // ponytail: topology-only matching requires observed row starts; add native-baseline row alignment when these measured failures need recovery.
            return Err(format!(
                "detected {} row starts but structure declares {}: no unambiguous cell geometry",
                rows.len(),
                logical.len()
            ));
        }
        let columns = dense.map(|row| {
            let mut edges = vec![row[0].left];
            edges.extend(
                row.windows(2)
                    .map(|pair| (pair[0].right + pair[1].left) * 0.5),
            );
            edges.push(row[row.len() - 1].right);
            edges
        });
        let mut row_edges = vec![0.0; self.row_count + 1];
        for ((row_index, _), observed) in logical.iter().zip(&rows) {
            row_edges[*row_index] = observed[0].top;
        }
        row_edges[self.row_count] = rows
            .iter()
            .flatten()
            .map(|b| b.bottom)
            .fold(f64::NEG_INFINITY, f64::max);
        for ((row_index, indices), observed) in logical.iter().zip(&rows) {
            if indices.len() == observed.len() {
                for (&index, bbox) in indices.iter().zip(observed) {
                    self.cells[index].bbox = Some(*bbox);
                }
            } else {
                let columns = columns.as_ref().ok_or_else(|| format!("row {row_index} has {} detections for {} cells without a complete column anchor", observed.len(), indices.len()))?;
                for &index in indices {
                    let cell = &mut self.cells[index];
                    let (right, bottom) = (
                        cell.column + cell.column_span,
                        cell.row + cell.row_span,
                    );
                    let bbox = Bbox::try_from([
                        *columns
                            .get(cell.column)
                            .ok_or("cell starts outside detected columns")?,
                        *row_edges
                            .get(cell.row)
                            .ok_or("cell starts outside detected rows")?,
                        *columns
                            .get(right)
                            .ok_or("cell spans outside detected columns")?,
                        *row_edges
                            .get(bottom)
                            .ok_or("cell spans outside detected rows")?,
                    ])
                    .map_err(|error| {
                        format!("invalid detected grid: {error}")
                    })?;
                    cell.bbox = Some(bbox);
                }
            }
        }
        tracing::info!(
            "bound {} topology-only cells using {} detected rows",
            self.cells.len(),
            rows.len()
        );
        Ok(())
    }
}
