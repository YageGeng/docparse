use super::*;

impl TableGrid<'_> {
    /// Recovers topology from visible separators; only rectangular missing-edge components become spans.
    #[allow(
        clippy::indexing_slicing,
        reason = "positive bounded grid dimensions and neighbor guards constrain every index"
    )]
    pub fn ruled(&self, rules: &[TableRule]) -> Option<RecoveredGrid> {
        let source_rules = self.local_rules(rules);
        // A separator may be painted as several touching paths. Validate their
        // union so an endpoint in the middle of a column does not erase the row.
        let mut strokes: Vec<_> = source_rules
            .iter()
            .filter_map(|rule| match *rule {
                TableRule::Horizontal { y, left, right } => {
                    Some((y, left, right))
                }
                _ => None,
            })
            .collect();
        strokes.sort_by(|a, b| {
            a.0.total_cmp(&b.0).then_with(|| a.1.total_cmp(&b.1))
        });
        let mut rules: Vec<_> = source_rules
            .iter()
            .copied()
            .filter(|rule| matches!(rule, TableRule::Vertical { .. }))
            .collect();
        let stroke_tolerance = self.rule_tolerance();
        let mut remaining = strokes.as_mut_slice();
        while let Some(&(y, _, _)) = remaining.first() {
            // Anchor each band at its first coordinate: pairwise comparisons can
            // chain unrelated rows together through a sequence of small offsets.
            let count = remaining
                .partition_point(|stroke| stroke.0 - y <= stroke_tolerance);
            let (band, rest) = remaining.split_at_mut(count);
            remaining = rest;
            band.sort_by(|a, b| {
                a.1.total_cmp(&b.1).then_with(|| a.2.total_cmp(&b.2))
            });
            for (_, left, right) in band.iter().copied() {
                if let Some(TableRule::Horizontal {
                    y: previous_y,
                    right: end,
                    ..
                }) = rules.last_mut()
                    && (y - *previous_y).abs() <= f64::EPSILON
                    // Tiny paint seams must not expose interior segment endpoints
                    // to column-edge validation and erase an otherwise complete row.
                    && left <= *end + stroke_tolerance
                {
                    *end = end.max(right);
                } else {
                    rules.push(TableRule::Horizontal { y, left, right });
                }
            }
        }
        let mut xs = Vec::new();
        let mut ys = Vec::new();
        let mut outer_left = f64::INFINITY;
        let mut outer_right = f64::NEG_INFINITY;
        let mut horizontal = 0;
        let mut vertical = 0;
        for rule in &rules {
            match *rule {
                TableRule::Horizontal { left, right, .. } => {
                    horizontal += 1;
                    if right - left >= self.bounds.width() * 0.5 {
                        // Only the outer envelope supplies unpainted side borders.
                        // A wide equation bar must not create interior columns that
                        // would then incorrectly validate that same bar as a row.
                        outer_left = outer_left.min(left);
                        outer_right = outer_right.max(right);
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
        // Padded horizontal endpoints inside explicit side borders are not columns.
        outer_left = xs.iter().copied().fold(outer_left, f64::min);
        outer_right = xs.iter().copied().fold(outer_right, f64::max);
        xs.extend([outer_left, outer_right]);
        let xs = self.snapped(xs);
        // Fraction bars and radical overbars are cell content. Only horizontal
        // rules joining established column edges may introduce a grid row.
        // Keep partial separators when they span one or more complete columns.
        let tolerance = (self.font_size * 0.25).clamp(0.5, 2.0);
        let rule_count = rules.len();
        let rules: Vec<_> = rules
            .iter()
            .copied()
            .filter(|rule| match *rule {
                TableRule::Horizontal { y, left, right } => {
                    // Match the bounded endpoint padding used by coverage while
                    // retaining the column-edge evidence that rejects formula bars.
                    let padding = (self.font_size * 0.4)
                        .min((right - left) * 0.2)
                        .max(tolerance);
                    xs.iter().any(|&from| {
                        (from - left).abs() <= padding
                            && xs.iter().any(|&to| {
                                to > from
                                    && (to - right).abs() <= padding
                                    && self.coverage(
                                        &source_rules,
                                        true,
                                        y,
                                        from,
                                        to,
                                    ) >= 0.7
                            })
                    })
                }
                _ => true,
            })
            .collect();
        if rules.len() < rule_count {
            tracing::debug!(
                "excluded {} in-cell horizontal marks from the table grid at {:?}",
                rule_count - rules.len(),
                self.bounds
            );
        }
        ys.extend(rules.iter().filter_map(|rule| match *rule {
            TableRule::Horizontal { y, .. } => Some(y),
            _ => None,
        }));
        let ys = self.snapped(ys);
        // Joined envelopes establish topology, but only original painted intervals
        // may satisfy coverage. Bridging seams must never turn sparse dashes into ink.
        let painted_rules: Vec<_> = source_rules.into_iter().filter(|rule| match *rule {
            TableRule::Horizontal { y, left, right } => rules.iter().any(|accepted| {
                matches!(*accepted, TableRule::Horizontal { y: at, left: from, right: to }
                    if (y - at).abs() <= stroke_tolerance && left >= from && right <= to)
            }),
            _ => true,
        }).collect();
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
                        && self.coverage(
                            &painted_rules,
                            horizontal,
                            at,
                            from,
                            to,
                        ) < 0.7
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
            return self.aligned(&painted_rules);
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
