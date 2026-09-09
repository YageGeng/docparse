use super::*;

impl TableGrid<'_> {
    /// Recovers topology from visible separators; only rectangular missing-edge components become spans.
    #[allow(
        clippy::indexing_slicing,
        reason = "positive bounded grid dimensions and neighbor guards constrain every index"
    )]
    pub fn ruled(&self, rules: &[TableRule]) -> Option<RecoveredGrid> {
        let rules = self.local_rules(rules);
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        let mut horizontal = 0;
        let mut vertical = 0;
        for rule in &rules {
            match *rule {
                TableRule::Horizontal { y, left, right } => {
                    ys.push(y);
                    horizontal += 1;
                    if right - left >= self.bounds.width() * 0.5 {
                        xs.extend([left, right]);
                    }
                }
                TableRule::Vertical { x, top, bottom } => {
                    xs.push(x);
                    vertical += 1;
                    if bottom - top >= self.bounds.height() * 0.5 {
                        ys.extend([top, bottom]);
                    }
                }
            }
        }
        if horizontal < 2 || vertical == 0 {
            return None;
        }
        let xs = self.snapped(xs);
        let ys = self.snapped(ys);
        let columns = xs.len().checked_sub(1)?;
        let rows = ys.len().checked_sub(1)?;
        if columns < 2
            || rows < 2
            || columns > MAX_TABLE_COLUMNS
            || rows > MAX_TABLE_ROWS
            || columns * rows > MAX_TABLE_CELLS
        {
            return None;
        }
        let mut visited = vec![false; columns * rows];
        let mut cells = Vec::new();
        for start in 0..visited.len() {
            if visited[start] {
                continue;
            }
            let mut stack = vec![start];
            visited[start] = true;
            let mut component = Vec::new();
            while let Some(index) = stack.pop() {
                component.push(index);
                let row = index / columns;
                let column = index % columns;
                let neighbors = [
                    (column > 0).then(|| {
                        (index - 1, false, xs[column], ys[row], ys[row + 1])
                    }),
                    (column + 1 < columns).then(|| {
                        (index + 1, false, xs[column + 1], ys[row], ys[row + 1])
                    }),
                    (row > 0).then(|| {
                        (
                            index - columns,
                            true,
                            ys[row],
                            xs[column],
                            xs[column + 1],
                        )
                    }),
                    (row + 1 < rows).then(|| {
                        (
                            index + columns,
                            true,
                            ys[row + 1],
                            xs[column],
                            xs[column + 1],
                        )
                    }),
                ];
                for (neighbor, horizontal, at, from, to) in
                    neighbors.into_iter().flatten()
                {
                    if !visited[neighbor]
                        && self.coverage(&rules, horizontal, at, from, to) < 0.7
                    {
                        visited[neighbor] = true;
                        stack.push(neighbor);
                    }
                }
            }
            let top = component.iter().map(|index| index / columns).min()?;
            let bottom =
                component.iter().map(|index| index / columns).max()? + 1;
            let left = component.iter().map(|index| index % columns).min()?;
            let right =
                component.iter().map(|index| index % columns).max()? + 1;
            // Missing or decorative rules can create an L-shaped component. Never turn it into a rectangle that steals another cell.
            if component.len() != (bottom - top) * (right - left) {
                return None;
            }
            cells.push(
                TableCell::builder()
                    .row(top)
                    .column(left)
                    .row_span(bottom - top)
                    .column_span(right - left)
                    .bbox(Some(
                        Bbox::try_from([
                            xs[left], ys[top], xs[right], ys[bottom],
                        ])
                        .ok()?,
                    ))
                    .build(),
            );
        }
        if cells.len() < 4 {
            return None;
        }
        if self.has_numeric_subcolumns(&cells) {
            tracing::debug!(
                "refining partial rule grid at {:?}: repeated numeric columns remain inside a coarse cell",
                self.bounds
            );
            return self.aligned(&rules);
        }
        self.assign(
            Table::builder()
                .row_count(rows)
                .column_count(columns)
                .cells(cells)
                .source(TableStructureSource::Ruled)
                .build(),
        )
    }

    /// Detects incomplete separators without mistaking ordinary wrapped prose for a finer table.
    fn has_numeric_subcolumns(&self, cells: &[TableCell]) -> bool {
        for cell in cells {
            let Some(bbox) = cell.bbox else {
                continue;
            };
            // Explicit spans already express missing internal borders. Refine only
            // ordinary coarse cells with enough space for multiple numeric tracks.
            if cell.row_span > 1
                || cell.column_span > 1
                || bbox.width() < self.font_size * 8.0
                || bbox.height() < self.font_size * 2.0
            {
                continue;
            }
            let mut supported_rows = 0;
            for row in &self.rows {
                let mut end = None;
                let mut numeric = None;
                let mut numeric_groups = 0;
                for span in
                    row.spans.iter().filter_map(|&index| self.spans.get(index))
                {
                    let center = span.span.bbox.center();
                    if center.x < bbox.left
                        || center.x > bbox.right
                        || center.y < bbox.top
                        || center.y > bbox.bottom
                    {
                        continue;
                    }
                    if end.is_some_and(|right| {
                        span.span.bbox.left - right
                            >= (self.font_size * 0.6).max(3.0)
                    }) {
                        numeric_groups += usize::from(numeric == Some(true));
                        numeric = None;
                    }
                    if numeric.is_none() {
                        numeric = span
                            .text()
                            .trim_start_matches(|ch: char| {
                                ch.is_whitespace() || "+-−<>≈~".contains(ch)
                            })
                            .chars()
                            .next()
                            .map(char::is_numeric);
                    }
                    end =
                        Some(end.map_or(span.span.bbox.right, |right: f64| {
                            right.max(span.span.bbox.right)
                        }));
                }
                numeric_groups += usize::from(numeric == Some(true));
                if numeric_groups >= 2 {
                    supported_rows += 1;
                }
                if supported_rows >= 2 {
                    return true;
                }
            }
        }
        false
    }
}
