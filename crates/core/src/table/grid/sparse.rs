use super::*;

/// A bounded sparse-column plan shared by the two row recovery modes.
#[derive(TypedBuilder)]
struct SparseLayout<'a, 's> {
    geometry: &'a TableGeometry<'s>,
    rules: Vec<TableRule>,
    header_end: f64,
    phrases: Vec<Bbox>,
    gutters: BTreeMap<usize, (f64, f64)>,
    cuts: Vec<f64>,
    body: Vec<usize>,
}

/// Rows are established by an independent anchor column or complete ruled bands.
enum SparseRows {
    Anchored {
        columns: Vec<Vec<PhysicalRow>>,
        anchor: usize,
        top_aligned: bool,
    },
    RuledBands,
}

impl TableGeometry<'_> {
    /// Recovers sparse tables by composing independent column and row plans.
    pub fn sparse(&self, rules: &[TableRule]) -> Option<CellGrid> {
        SparseLayout::infer_columns(self, rules)?.build_grid()
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "axes and source indices are bounded by column and row plans"
)]
impl<'a, 's> SparseLayout<'a, 's> {
    /// Intersects body gutters with header whitespace to establish supported columns.
    fn infer_columns(
        geometry: &'a TableGeometry<'s>,
        rules: &[TableRule],
    ) -> Option<Self> {
        let rules = geometry.local_rules(rules);
        let header_end = geometry
            .snapped(
                rules
                    .iter()
                    .filter_map(|rule| match *rule {
                        TableRule::Horizontal { y, left, right }
                            if right - left
                                >= geometry.bounds.width() * 0.7 =>
                        {
                            Some(y)
                        }
                        _ => None,
                    })
                    .collect(),
            )
            .into_iter()
            .find(|&y| {
                geometry
                    .spans
                    .iter()
                    .any(|span| span.span.bbox.center().y < y)
                    && geometry
                        .spans
                        .iter()
                        .any(|span| span.span.bbox.center().y > y)
            })?;
        let (header, body): (Vec<_>, Vec<_>) = (0..geometry.spans.len())
            .partition(|&i| {
                geometry.spans[i].span.bbox.center().y < header_end
            });
        let first = header
            .iter()
            .map(|&i| geometry.spans[i].baseline)
            .min_by(f64::total_cmp)?;
        let last = header
            .iter()
            .map(|&i| geometry.spans[i].baseline)
            .max_by(f64::total_cmp)?;
        // Wrapped labels may share one header band. Internal header separators
        // identify distinct levels, which remain owned by the multi-level strategy.
        if body.is_empty()
            || (last - first > geometry.font_size * 0.6 && rules.iter().any(|rule| {
                matches!(*rule, TableRule::Horizontal { y, left, right }
                    if y > first && y < last && right - left > geometry.font_size * 2.0)
            }))
        {
            return None;
        }
        let mut header_boxes: Vec<_> = header
            .iter()
            .map(|&i| geometry.spans[i].span.bbox)
            .collect();
        header_boxes.sort_by(|a, b| a.left.total_cmp(&b.left));
        let mut phrases: Vec<Bbox> = Vec::new();
        for bbox in header_boxes {
            if let Some(previous) = phrases.last_mut()
                && bbox.left - previous.right
                    < (geometry.font_size * 0.6).max(3.0)
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
                let bbox = geometry.spans[i].span.bbox;
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
            for gap in 0..=phrases.len() {
                // A wide body gutter can partly overlap a longer heading. Keep
                // the shared whitespace instead of rejecting its original midpoint.
                let from = end.max(if gap == 0 {
                    geometry.bounds.left
                } else {
                    phrases[gap - 1].right
                });
                let to = left.min(if gap == phrases.len() {
                    geometry.bounds.right
                } else {
                    phrases[gap].left
                });
                if left - end < 3.0 || to - from <= 0.1 {
                    continue;
                }
                let x = (from + to) * 0.5;
                let support = geometry
                    .rows
                    .iter()
                    .filter(|row| {
                        if row.baseline < header_end {
                            return false;
                        }
                        let mut edge = None;
                        for &i in &row.spans {
                            let b = geometry.spans[i].span.bbox;
                            if edge.is_some_and(|edge| {
                                edge < x
                                    && b.left > x
                                    && b.left - edge
                                        >= (geometry.font_size * 0.6).max(3.0)
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
                    let previous = gutters.entry(gap).or_insert((from, to));
                    if to - from > previous.1 - previous.0 {
                        *previous = (from, to);
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
        let mut cuts = vec![geometry.bounds.left];
        cuts.extend(gutters.values().map(|(left, right)| (left + right) * 0.5));
        cuts.push(geometry.bounds.right);
        let columns = cuts.len() - 1;
        if !(2..=MAX_TABLE_COLUMNS).contains(&columns) {
            return None;
        }
        Some(
            Self::builder()
                .geometry(geometry)
                .rules(rules)
                .header_end(header_end)
                .phrases(phrases)
                .gutters(gutters)
                .cuts(cuts)
                .body(body)
                .build(),
        )
    }

    /// Selects a row strategy without changing the inferred columns.
    fn infer_rows(&self) -> Option<SparseRows> {
        let geometry = self.geometry;
        let cuts = &self.cuts;
        let columns = cuts.len().checked_sub(1)?;
        let mut column_rows: Vec<Vec<PhysicalRow>> =
            (0..columns).map(|_| Vec::new()).collect();
        let mut body = self.body.clone();
        body.sort_by(|&a, &b| {
            geometry.spans[a]
                .baseline
                .total_cmp(&geometry.spans[b].baseline)
                .then_with(|| {
                    geometry.spans[a]
                        .span
                        .bbox
                        .left
                        .total_cmp(&geometry.spans[b].span.bbox.left)
                })
        });
        for &index in &body {
            let span = &geometry.spans[index];
            let x = span.span.bbox.center().x;
            let column = cuts
                .windows(2)
                .position(|pair| x >= pair[0] && x <= pair[1])?;
            let rows = &mut column_rows[column];
            if let Some(row) = rows.last_mut()
                && span.baseline - row.baseline <= geometry.font_size * 0.6
            {
                row.spans.push(index);
            } else {
                rows.push(PhysicalRow {
                    baseline: span.baseline,
                    spans: vec![index],
                });
            }
        }
        let anchor = column_rows
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
                            geometry.spans[i]
                                .text()
                                .chars()
                                .any(char::is_numeric)
                        }) && !row.spans.iter().any(|&i| {
                            geometry.spans[i]
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
                            < geometry.font_size * 1.4
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
            });
        Some(match anchor {
            Some((anchor, top_aligned)) => SparseRows::Anchored {
                columns: column_rows,
                anchor,
                top_aligned,
            },
            None => SparseRows::RuledBands,
        })
    }

    /// Dispatches the row plan into the shared candidate-grid representation.
    fn build_grid(&self) -> Option<CellGrid> {
        match self.infer_rows()? {
            SparseRows::RuledBands => self.ruled_bands(),
            SparseRows::Anchored {
                columns,
                anchor,
                top_aligned,
            } => self.anchored_rows(&columns, anchor, top_aligned),
        }
    }

    /// Uses complete horizontal bands for prose tables without independent row anchors.
    fn ruled_bands(&self) -> Option<CellGrid> {
        let geometry = self.geometry;
        let rules = &self.rules;
        let cuts = &self.cuts;
        let phrases = &self.phrases;
        let body = &self.body;
        let header_end = self.header_end;
        let columns = cuts.len().checked_sub(1)?;
        // All columns can contain wrapped prose. Complete horizontal rules
        // still establish rows without a separate one-line label column.
        // Reuse ruled topology with only the supported vertical gutters;
        // each heading must describe exactly one column in this fallback.
        if phrases.len() != columns {
            return None;
        }
        let mut grid_rules: Vec<_> = rules.iter().copied().filter(|rule| {
                    matches!(*rule, TableRule::Horizontal { y, .. }
                        if geometry.coverage(rules, true, y, cuts[0], cuts[columns]) >= 0.9)
                }).collect();
        let top = grid_rules
            .iter()
            .filter_map(|rule| match *rule {
                TableRule::Horizontal { y, .. } if y < header_end => Some(y),
                _ => None,
            })
            .min_by(f64::total_cmp)
            .unwrap_or(geometry.bounds.top);
        let bottom = grid_rules
            .iter()
            .filter_map(|rule| match *rule {
                TableRule::Horizontal { y, .. } => Some(y),
                _ => None,
            })
            .max_by(f64::total_cmp)?;
        if body.iter().any(|&i| {
            geometry.spans[i].span.bbox.bottom
                > bottom + geometry.rule_tolerance()
        }) {
            return None;
        }
        grid_rules.extend(cuts.iter().map(|&x| TableRule::Vertical {
            x,
            top,
            bottom,
        }));
        let grid = geometry.ruled(&grid_rules)?;
        // The ruled path already binds words; header inference is supplied before binding below.
        let (table, assignment) = grid.into_parts().ok()?;
        let mut grid = CellGrid::try_from(table).ok()?;
        grid.set_header(0).ok()?;
        let grid = grid.bind_words(assignment, geometry.spans.len()).ok()?;
        tracing::debug!(
            "recovered wrapped prose table at {:?} from complete row rules and {} supported columns",
            geometry.bounds,
            columns
        );
        Some(grid)
    }

    /// Derives logical row edges from dense columns and real separators.
    fn row_edges(
        &self,
        column_rows: &[Vec<PhysicalRow>],
        anchor_column: usize,
        top_aligned: bool,
    ) -> Option<Vec<f64>> {
        let geometry = self.geometry;
        let rules = &self.rules;
        let cuts = &self.cuts;
        let header_end = self.header_end;
        let anchors = &column_rows[anchor_column];
        let anchor_for = |index: usize| {
            let baseline = geometry.spans[index].baseline;
            if top_aligned {
                anchors
                    .partition_point(|row| {
                        row.baseline <= baseline + geometry.font_size * 0.6
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
                let bbox = geometry.spans[index].span.bbox;
                let extent = &mut extents[anchor_for(index)];
                extent.0 = extent.0.min(bbox.top);
                extent.1 = extent.1.max(bbox.bottom);
            }
        }
        let mut ys = vec![geometry.bounds.top, header_end];
        for (index, pair) in anchors.windows(2).enumerate() {
            let midpoint = (extents[index].1 + extents[index + 1].0) * 0.5;
            let boundary = rules
                .iter()
                .filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, .. }
                        if y > pair[0].baseline
                            && y < pair[1].baseline
                            && geometry.coverage(
                                rules,
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
        ys.push(geometry.bounds.bottom);
        Some(ys)
    }

    /// Builds sparse rowspans from the column plan and chosen row anchors.
    fn anchored_rows(
        &self,
        column_rows: &[Vec<PhysicalRow>],
        anchor_column: usize,
        top_aligned: bool,
    ) -> Option<CellGrid> {
        let geometry = self.geometry;
        let rules = &self.rules;
        let cuts = &self.cuts;
        let phrases = &self.phrases;
        let gutters = &self.gutters;
        let header_end = self.header_end;
        let columns = cuts.len().checked_sub(1)?;
        let anchors = &column_rows[anchor_column];
        let row_count = anchors.len() + 1;
        if row_count > MAX_TABLE_ROWS || row_count * columns > MAX_TABLE_CELLS {
            return None;
        }
        let ys = self.row_edges(column_rows, anchor_column, top_aligned)?;
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
                let mut bands = vec![header_end, geometry.bounds.bottom];
                bands.extend(rules.iter().filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, .. }
                        if y > header_end
                            && y < geometry.bounds.bottom
                            && geometry.coverage(
                                rules,
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
                for band in geometry.snapped(bands).windows(2) {
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
                                <= geometry.font_size * 1.4
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
            geometry.bounds,
            row_count,
            columns,
            anchor_column
        );
        geometry.assign(
            CellGrid::try_from(
                Table::builder()
                    .row_count(row_count)
                    .column_count(columns)
                    .cells(cells)
                    .source(TableStructureSource::Ruled)
                    .build(),
            )
            .ok()?,
        )
    }
}
