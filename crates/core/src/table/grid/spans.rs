use super::header::HeaderRecovery;
use super::*;

impl TableGeometry<'_> {
    /// Recovers header and body spans through independently validated grid transactions.
    pub(super) fn recover_spans(
        &self,
        mut grid: CellGrid,
        cuts: &[f64],
        rules: &[TableRule],
    ) -> Option<CellGrid> {
        let header = HeaderRecovery::new(self, cuts, rules);
        let count = header.recover(&mut grid)?;
        self.recover_body_bands(&mut grid, cuts, rules, count)?;
        header.collapse_wrapped(&mut grid, count)?;
        Some(grid)
    }

    /// Applies section-title and centered row-label evidence only within its horizontal band.
    #[allow(
        clippy::indexing_slicing,
        reason = "row and source indices are derived from the validated candidate"
    )]
    fn recover_body_bands(
        &self,
        grid: &mut CellGrid,
        cuts: &[f64],
        rules: &[TableRule],
        header_rows: usize,
    ) -> Option<()> {
        let columns = grid.table().column_count;
        let logical = grid.rows().to_vec();
        let full_rules = self.snapped(
            rules
                .iter()
                .filter_map(|r| match *r {
                    TableRule::Horizontal { y, left, right }
                        if right - left >= self.bounds.width() * 0.7 =>
                    {
                        Some(y)
                    }
                    _ => None,
                })
                .collect(),
        );
        // Sparse horizontal bands delimit section titles and model/metric groups.
        // An isolated centered title spans columns; a label beside several rows spans rows.
        for band in full_rules.windows(2) {
            let rows: Vec<_> = logical
                .iter()
                .enumerate()
                .filter(|(row, group)| {
                    *row >= header_rows
                        && group.physical.iter().all(|&physical| {
                            self.rows[physical].baseline > band[0]
                                && self.rows[physical].baseline < band[1]
                        })
                })
                .map(|(row, _)| row)
                .collect();
            if let [row] = rows.as_slice() {
                let mut words = logical[*row].words.clone();
                words.sort_by(|&a, &b| {
                    self.spans[a]
                        .span
                        .bbox
                        .left
                        .total_cmp(&self.spans[b].span.bbox.left)
                });
                let left = words.first().map(|&i| self.spans[i].span.bbox.left);
                let right = words
                    .iter()
                    .map(|&i| self.spans[i].span.bbox.right)
                    .max_by(f64::total_cmp);
                // Require a single phrase between complete rules. Left-aligned
                // section labels additionally need bold evidence; ordinary notes stay local.
                let title = logical[*row].physical.len() == 1
                    && !self.is_missing_value(&words)
                    && !self.has_column_divider(rules, band[0], band[1])
                    && words.iter().any(|&i| {
                        self.spans[i].text().chars().any(char::is_alphabetic)
                    })
                    && words.windows(2).all(|pair| {
                        self.spans[pair[1]].span.bbox.left
                            - self.spans[pair[0]].span.bbox.right
                            < (self.font_size * 0.6).max(3.0)
                    })
                    && left.zip(right).is_some_and(|(left, right)| {
                        ((left + right) * 0.5 - self.bounds.center().x).abs()
                            <= self.font_size
                            || ((left - self.bounds.left).abs()
                                <= self.font_size
                                && words
                                    .iter()
                                    .all(|&i| self.spans[i].is_bold()))
                    })
                    && band.iter().all(|&y| {
                        self.coverage(rules, true, y, cuts[0], cuts[columns])
                            >= 0.9
                    });
                if title {
                    grid.try_merge(
                        TableCell::builder()
                            .row(*row)
                            .column(0)
                            .column_span(columns)
                            .is_header(true)
                            .bbox(Some(
                                Bbox::try_from([
                                    cuts[0],
                                    logical[*row].top,
                                    cuts[columns],
                                    logical[*row].bottom,
                                ])
                                .ok()?,
                            ))
                            .build(),
                    )
                    .ok()?;
                    tracing::debug!(
                        "recovered section row {} across {} table columns at {:?}",
                        row,
                        columns,
                        self.bounds
                    );
                }
            }
            if rows.len() < 2 {
                continue;
            }
            let first = *rows.first()?;
            let last = *rows.last()?;
            if last - first + 1 != rows.len() {
                continue;
            }
            // An even row count has two central baselines. Typeset multirow labels
            // commonly use the upper one instead of the arithmetic midpoint.
            let lower_middle = rows[(rows.len() - 1) / 2];
            let upper_middle = rows[rows.len() / 2];
            let center_start =
                self.rows[*logical[lower_middle].physical.first()?].baseline;
            let center_end =
                self.rows[*logical[upper_middle].physical.last()?].baseline;
            for column in 0..columns {
                let words: Vec<_> = rows
                    .iter()
                    .flat_map(|&row| logical[row].words.iter().copied())
                    .filter(|&i| {
                        let x = self.spans[i].span.bbox.center().x;
                        x >= cuts[column] && x < cuts[column + 1]
                    })
                    .collect();
                if words.is_empty()
                    || !words.iter().any(|&i| {
                        self.spans[i].text().chars().any(char::is_alphabetic)
                    })
                {
                    continue;
                }
                let top = words
                    .iter()
                    .map(|&i| self.spans[i].baseline)
                    .fold(f64::INFINITY, f64::min);
                let bottom = words
                    .iter()
                    .map(|&i| self.spans[i].baseline)
                    .fold(f64::NEG_INFINITY, f64::max);
                if bottom - top > self.font_size * 0.4
                    || (top + bottom) * 0.5
                        < center_start - self.font_size * 0.4
                    || (top + bottom) * 0.5 > center_end + self.font_size * 0.4
                {
                    continue;
                }
                grid.try_merge(
                    TableCell::builder()
                        .row(first)
                        .column(column)
                        .row_span(rows.len())
                        .bbox(Some(
                            Bbox::try_from([
                                cuts[column],
                                logical[first].top,
                                cuts[column + 1],
                                logical[last].bottom,
                            ])
                            .ok()?,
                        ))
                        .build(),
                )
                .ok()?;
            }
        }
        Some(())
    }
}
