use super::*;

impl TableGrid<'_> {
    /// Recovers sparse ruled tables from persistent gutters and a column of independent row labels.
    #[allow(
        clippy::indexing_slicing,
        reason = "source indices, column cuts, and bounded row intervals are established locally"
    )]
    pub fn sparse(&self, rules: &[TableRule]) -> Option<RecoveredGrid> {
        let rules = self.local_rules(rules);
        let header_end = self
            .snapped(
                rules
                    .iter()
                    .filter_map(|rule| match *rule {
                        TableRule::Horizontal { y, left, right }
                            if right - left >= self.bounds.width() * 0.7 =>
                        {
                            Some(y)
                        }
                        _ => None,
                    })
                    .collect(),
            )
            .into_iter()
            .find(|&y| {
                self.spans.iter().any(|span| span.span.bbox.center().y < y)
                    && self
                        .spans
                        .iter()
                        .any(|span| span.span.bbox.center().y > y)
            })?;
        let (header, body): (Vec<_>, Vec<_>) = (0..self.spans.len())
            .partition(|&i| self.spans[i].span.bbox.center().y < header_end);
        let first = header
            .iter()
            .map(|&i| self.spans[i].baseline)
            .min_by(f64::total_cmp)?;
        let last = header
            .iter()
            .map(|&i| self.spans[i].baseline)
            .max_by(f64::total_cmp)?;
        // Multi-level headings already have a separate reconstruction path. Here a
        // single heading band constrains which persistent spaces can really be columns.
        if last - first > self.font_size * 0.6 || body.is_empty() {
            return None;
        }
        let mut header_boxes: Vec<_> =
            header.iter().map(|&i| self.spans[i].span.bbox).collect();
        header_boxes.sort_by(|a, b| a.left.total_cmp(&b.left));
        let mut phrases: Vec<Bbox> = Vec::new();
        for bbox in header_boxes {
            if let Some(previous) = phrases.last_mut()
                && bbox.left - previous.right < (self.font_size * 0.6).max(3.0)
            {
                previous.right = previous.right.max(bbox.right);
                previous.top = previous.top.min(bbox.top);
                previous.bottom = previous.bottom.max(bbox.bottom);
            } else {
                phrases.push(bbox);
            }
        }
        if phrases.len() < 2 {
            return None;
        }
        // Repeated prose spaces and gaps inside fractions cannot establish a gutter
        // through other body rows. A small ink overhang remains below assign()'s
        // ownership tolerance, accommodating centered labels of different widths.
        let mut intervals: Vec<_> = body
            .iter()
            .map(|&i| {
                let bbox = self.spans[i].span.bbox;
                (
                    bbox.left + bbox.width() * 0.1,
                    bbox.right - bbox.width() * 0.1,
                )
            })
            .collect();
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut gutters: BTreeMap<usize, (f64, f64)> = BTreeMap::new();
        let mut end = intervals.first()?.1;
        for (left, right) in intervals.into_iter().skip(1) {
            let x = (end + left) * 0.5;
            if left - end >= 3.0
                && !phrases
                    .iter()
                    .any(|phrase| x > phrase.left && x < phrase.right)
            {
                let support = self
                    .rows
                    .iter()
                    .filter(|row| {
                        if row.baseline < header_end {
                            return false;
                        }
                        let mut edge = None;
                        for &i in &row.spans {
                            let b = self.spans[i].span.bbox;
                            if edge.is_some_and(|edge| {
                                edge < x
                                    && b.left > x
                                    && b.left - edge
                                        >= (self.font_size * 0.6).max(3.0)
                            }) {
                                return true;
                            }
                            edge = Some(edge.map_or(b.right, |edge: f64| {
                                edge.max(b.right)
                            }));
                        }
                        false
                    })
                    .count();
                if support >= 2 {
                    let gap =
                        phrases.partition_point(|phrase| phrase.right < x);
                    let previous = gutters.entry(gap).or_insert((end, left));
                    if left - end > previous.1 - previous.0 {
                        *previous = (end, left);
                    }
                }
            }
            end = end.max(right);
        }
        // Every neighboring heading needs its own supported body boundary. An
        // outer gutter may subdivide a heading such as a category plus task name.
        if !(1..phrases.len()).all(|gap| gutters.contains_key(&gap)) {
            return None;
        }
        let mut cuts = vec![self.bounds.left];
        cuts.extend(gutters.values().map(|(left, right)| (left + right) * 0.5));
        cuts.push(self.bounds.right);
        let columns = cuts.len() - 1;
        if !(2..=MAX_TABLE_COLUMNS).contains(&columns) {
            return None;
        }
        let mut column_rows: Vec<Vec<PhysicalRow>> =
            (0..columns).map(|_| Vec::new()).collect();
        let mut body = body;
        body.sort_by(|&a, &b| {
            self.spans[a]
                .baseline
                .total_cmp(&self.spans[b].baseline)
                .then_with(|| {
                    self.spans[a]
                        .span
                        .bbox
                        .left
                        .total_cmp(&self.spans[b].span.bbox.left)
                })
        });
        for &index in &body {
            let span = &self.spans[index];
            let x = span.span.bbox.center().x;
            let column = cuts
                .windows(2)
                .position(|pair| x >= pair[0] && x <= pair[1])?;
            let rows = &mut column_rows[column];
            if let Some(row) = rows.last_mut()
                && span.baseline - row.baseline <= self.font_size * 0.6
            {
                row.spans.push(index);
            } else {
                rows.push(PhysicalRow {
                    baseline: span.baseline,
                    spans: vec![index],
                });
            }
        }
        let (anchor_column, top_aligned) = column_rows
            .iter()
            .enumerate()
            .filter_map(|(column, rows)| {
                if rows.len() < 2 {
                    return None;
                }
                let numeric = rows
                    .iter()
                    .filter(|row| {
                        row.spans.iter().any(|&i| {
                            self.spans[i].text().chars().any(char::is_numeric)
                        }) && !row.spans.iter().any(|&i| {
                            self.spans[i]
                                .text()
                                .chars()
                                .any(char::is_alphabetic)
                        })
                    })
                    .count()
                    * 5
                    >= rows.len() * 4;
                let wraps = rows
                    .windows(2)
                    .filter(|pair| {
                        pair[1].baseline - pair[0].baseline
                            < self.font_size * 1.4
                    })
                    .count();
                (numeric || wraps * 5 < rows.len()).then_some((column, numeric))
            })
            .max_by_key(|&(column, numeric)| {
                (
                    column_rows[column].len(),
                    numeric,
                    std::cmp::Reverse(column),
                )
            })?;
        let anchors = &column_rows[anchor_column];
        let row_count = anchors.len() + 1;
        if row_count > MAX_TABLE_ROWS || row_count * columns > MAX_TABLE_CELLS {
            return None;
        }
        let anchor_for = |index: usize| {
            let baseline = self.spans[index].baseline;
            if top_aligned {
                anchors
                    .partition_point(|row| {
                        row.baseline <= baseline + self.font_size * 0.6
                    })
                    .saturating_sub(1)
            } else {
                anchors
                    .iter()
                    .enumerate()
                    .min_by(|(_, a), (_, b)| {
                        (a.baseline - baseline)
                            .abs()
                            .total_cmp(&(b.baseline - baseline).abs())
                    })
                    .map_or(0, |(index, _)| index)
            }
        };
        let mut extents =
            vec![(f64::INFINITY, f64::NEG_INFINITY); anchors.len()];
        for rows in column_rows
            .iter()
            .filter(|rows| rows.len() >= anchors.len())
        {
            for &index in rows.iter().flat_map(|row| &row.spans) {
                let bbox = self.spans[index].span.bbox;
                let extent = &mut extents[anchor_for(index)];
                extent.0 = extent.0.min(bbox.top);
                extent.1 = extent.1.max(bbox.bottom);
            }
        }
        let mut ys = vec![self.bounds.top, header_end];
        for (index, pair) in anchors.windows(2).enumerate() {
            let midpoint = (extents[index].1 + extents[index + 1].0) * 0.5;
            let boundary = rules
                .iter()
                .filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, .. }
                        if y > pair[0].baseline
                            && y < pair[1].baseline
                            && self.coverage(
                                &rules,
                                true,
                                y,
                                cuts[anchor_column],
                                cuts[anchor_column + 1],
                            ) >= 0.7 =>
                    {
                        Some(y)
                    }
                    _ => None,
                })
                .min_by(|a, b| {
                    (a - midpoint).abs().total_cmp(&(b - midpoint).abs())
                })
                .unwrap_or(midpoint);
            ys.push(boundary);
        }
        ys.push(self.bounds.bottom);
        let mut cells = Vec::new();
        let mut first_column = 0;
        for heading in 0..phrases.len() {
            let next_column = if heading + 1 == phrases.len() {
                columns
            } else {
                gutters.keys().position(|&gap| gap == heading + 1)? + 1
            };
            cells.push(
                TableCell::builder()
                    .row(0)
                    .column(first_column)
                    .column_span(next_column - first_column)
                    .is_header(true)
                    .bbox(Some(
                        Bbox::try_from([
                            cuts[first_column],
                            ys[0],
                            cuts[next_column],
                            ys[1],
                        ])
                        .ok()?,
                    ))
                    .build(),
            );
            first_column = next_column;
        }
        for (column, rows) in column_rows.iter().enumerate() {
            let mut merged = BTreeMap::new();
            if rows.len() < anchors.len() {
                let mut bands = vec![header_end, self.bounds.bottom];
                bands.extend(rules.iter().filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, .. }
                        if y > header_end
                            && y < self.bounds.bottom
                            && self.coverage(
                                &rules,
                                true,
                                y,
                                cuts[column],
                                cuts[column + 1],
                            ) >= 0.7 =>
                    {
                        Some(y)
                    }
                    _ => None,
                }));
                for band in self.snapped(bands).windows(2) {
                    let start =
                        anchors.partition_point(|row| row.baseline < band[0]);
                    let stop =
                        anchors.partition_point(|row| row.baseline < band[1]);
                    let labels: Vec<_> = rows
                        .iter()
                        .filter(|row| {
                            row.baseline > band[0] && row.baseline < band[1]
                        })
                        .collect();
                    // Only one coherent label (possibly wrapped) can own a ruled
                    // group. Separated values retain their independent rows.
                    if stop > start + 1
                        && !labels.is_empty()
                        && labels.windows(2).all(|pair| {
                            pair[1].baseline - pair[0].baseline
                                <= self.font_size * 1.4
                        })
                    {
                        merged.insert(start + 1, stop - start);
                    }
                }
            }
            let mut row = 1;
            while row < row_count {
                let span = merged.get(&row).copied().unwrap_or(1);
                cells.push(
                    TableCell::builder()
                        .row(row)
                        .column(column)
                        .row_span(span)
                        .bbox(Some(
                            Bbox::try_from([
                                cuts[column],
                                ys[row],
                                cuts[column + 1],
                                ys[row + span],
                            ])
                            .ok()?,
                        ))
                        .build(),
                );
                row += span;
            }
        }
        cells.sort_by_key(|cell| (cell.row, cell.column));
        tracing::debug!(
            "recovering sparse table at {:?} with {} rows, {} columns and anchor column {}",
            self.bounds,
            row_count,
            columns,
            anchor_column
        );
        self.assign(
            Table::builder()
                .row_count(row_count)
                .column_count(columns)
                .cells(cells)
                .source(TableStructureSource::Ruled)
                .build(),
        )
    }
}
