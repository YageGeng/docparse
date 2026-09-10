use super::*;

/// A visible heading phrase and the column interval justified by its placement or underline.
struct HeadingPhrase {
    bbox: Bbox,
    columns: std::ops::Range<usize>,
    anchored: bool,
}

impl TableGrid<'_> {
    /// Recovers heading groups and centered row labels from sparse-rule evidence without reducing body columns.
    #[allow(
        clippy::indexing_slicing,
        reason = "grid axes, physical-row indices, and cell positions are bounded by the caller"
    )]
    pub(super) fn recover_spans(
        &self,
        mut table: Table,
        cuts: &[f64],
        ys: &[f64],
        groups: &[Vec<usize>],
        rules: &[TableRule],
    ) -> Option<Table> {
        let full_rules = self.snapped(
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
        );
        let mut row_spans: Vec<Vec<usize>> = groups
            .iter()
            .map(|group| {
                group
                    .iter()
                    .flat_map(|&row| self.rows[row].spans.iter().copied())
                    .collect()
            })
            .collect();
        // Booktabs rules separate a textual header from a body whose physical rows have
        // no complete grid. This evidence works even when the PDF reports no bold font.
        let header_end = (full_rules.len() < self.rows.len())
            .then(|| {
                full_rules.iter().copied().find(|&y| {
                    self.spans.iter().any(|span| span.span.bbox.center().y < y)
                        && self
                            .spans
                            .iter()
                            .any(|span| span.span.bbox.center().y > y)
                })
            })
            .flatten();
        let mut header_rows = 0;
        for words in &row_spans {
            let alphabetic = words.iter().any(|&i| {
                self.spans[i].text().chars().any(char::is_alphabetic)
            });
            let ruled = header_end.is_some_and(|end| {
                words
                    .iter()
                    .all(|&i| self.spans[i].span.bbox.center().y < end)
            });
            let styled = words
                .iter()
                .all(|&i| !self.spans[i].text().chars().any(char::is_numeric))
                && words.iter().filter(|&&i| self.spans[i].is_bold()).count()
                    * 2
                    >= words.len();
            if alphabetic && (ruled || styled) {
                header_rows += 1;
            } else {
                break;
            }
        }
        if header_rows >= table.row_count {
            header_rows = 0;
        }
        let mut groups = groups.to_vec();
        let mut ys = ys.to_vec();
        // Partial header rules establish logical levels even when centered stubs
        // and wrapped titles contribute extra, overlapping physical baselines.
        let header_rules = self.snapped(
            rules
                .iter()
                .filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, left, right }
                        if header_rows > 1
                            && header_end.is_some_and(|end| {
                                y < end - self.rule_tolerance()
                            })
                            && row_spans
                                .iter()
                                .take(header_rows)
                                .flatten()
                                .any(|&i| {
                                    let center =
                                        self.spans[i].span.bbox.center();
                                    center.x >= left
                                        && center.x <= right
                                        && center.y < y
                                })
                            && row_spans
                                .iter()
                                .take(header_rows)
                                .flatten()
                                .any(|&i| {
                                    let center =
                                        self.spans[i].span.bbox.center();
                                    center.x >= left
                                        && center.x <= right
                                        && center.y > y
                                })
                            && cuts.windows(2).any(|pair| {
                                self.coverage(rules, true, y, pair[0], pair[1])
                                    >= 0.7
                            }) =>
                    {
                        Some(y)
                    }
                    _ => None,
                })
                .collect(),
        );
        // Coalesce observed rows only; decorative ink must not invent extra levels
        // or expand a candidate beyond the grid limits checked by the caller.
        let ruled_header =
            !header_rules.is_empty() && header_rules.len() < header_rows;
        if ruled_header {
            let physical_count = header_rows;
            header_rows = header_rules.len() + 1;
            let mut header_ys = vec![ys[0]];
            header_ys.extend(header_rules);
            header_ys.extend_from_slice(&ys[physical_count..]);
            ys = header_ys;
            let mut header_spans = vec![Vec::new(); header_rows];
            for &index in row_spans.iter().take(physical_count).flatten() {
                let y = self.spans[index].span.bbox.center().y;
                let row = ys[..=header_rows]
                    .windows(2)
                    .position(|pair| y >= pair[0] && y <= pair[1])?;
                header_spans[row].push(index);
            }
            row_spans.splice(..physical_count, header_spans);
            // Header geometry now follows individual words; only body bands below
            // use the original physical-row indices to recover centered row labels.
            groups.splice(..physical_count, vec![Vec::new(); header_rows]);
            table.cells.retain(|cell| cell.row >= physical_count);
            for cell in &mut table.cells {
                cell.row = cell.row - physical_count + header_rows;
            }
            table.row_count = table.row_count - physical_count + header_rows;
            tracing::debug!(
                "normalized table header at {:?} from {} physical rows to {} ruled levels",
                self.bounds,
                physical_count,
                header_rows
            );
        }
        let mut headings: Vec<TableCell> = Vec::new();
        for row in 0..header_rows {
            let mut blocked = vec![false; table.column_count];
            // Empty subheader positions continue an existing stub heading; groups with
            // children stay in their own header row instead of copying their text.
            for cell in &mut headings {
                if cell.row + cell.row_span == row
                    && !row_spans[row].iter().any(|&i| {
                        let x = self.spans[i].span.bbox.center().x;
                        x >= cuts[cell.column]
                            && x < cuts[cell.column + cell.column_span]
                    })
                {
                    cell.row_span += 1;
                    if let Some(bbox) = &mut cell.bbox {
                        bbox.bottom = ys[row + 1];
                    }
                }
                if cell.row + cell.row_span > row {
                    for slot in blocked
                        .iter_mut()
                        .skip(cell.column)
                        .take(cell.column_span)
                    {
                        *slot = true;
                    }
                }
            }
            let mut words = row_spans[row].clone();
            words.sort_by(|&a, &b| {
                self.spans[a]
                    .span
                    .bbox
                    .left
                    .total_cmp(&self.spans[b].span.bbox.left)
            });
            let mut boxes: Vec<Bbox> = Vec::new();
            for index in words {
                let bbox = self.spans[index].span.bbox;
                if let Some(previous) = boxes.last_mut()
                    && bbox.left - previous.right
                        < (self.font_size * 0.6).max(3.0)
                {
                    *previous = Bbox::try_from([
                        previous.left.min(bbox.left),
                        previous.top.min(bbox.top),
                        previous.right.max(bbox.right),
                        previous.bottom.max(bbox.bottom),
                    ])
                    .ok()?;
                } else {
                    boxes.push(bbox);
                }
            }
            let mut phrases = Vec::new();
            for bbox in &boxes {
                // A wide heading may overhang a data gutter slightly. Only substantial
                // horizontal coverage can claim a column; touching it is not a colspan.
                let significant = |pair: &[f64]| {
                    let overlap = (bbox.right.min(pair[1])
                        - bbox.left.max(pair[0]))
                    .max(0.0);
                    overlap
                        / bbox.width().min(pair[1] - pair[0]).max(f64::EPSILON)
                        >= 0.3
                };
                let start = cuts.windows(2).position(significant)?;
                let end = cuts.windows(2).rposition(significant)? + 1;
                let mut phrase = HeadingPhrase {
                    bbox: *bbox,
                    columns: start..end,
                    anchored: false,
                };
                // A partial underline identifies groups such as Accuracy over five
                // subcolumns more reliably than the short centered heading's ink box.
                let underline = rules
                    .iter()
                    .filter_map(|rule| match *rule {
                        TableRule::Horizontal { y, left, right }
                            if y >= bbox.bottom - 1.0
                                && y - bbox.bottom <= self.font_size * 1.2
                                && right - left > bbox.width() * 1.2
                                && right - left < self.bounds.width() * 0.9
                                && bbox.center().x >= left
                                && bbox.center().x <= right
                                && !boxes.iter().any(|other| {
                                    other != bbox
                                        && other.center().x >= left
                                        && other.center().x <= right
                                }) =>
                        {
                            Some((y, left, right))
                        }
                        _ => None,
                    })
                    .min_by(|a, b| a.0.total_cmp(&b.0));
                if let Some((_, left, right)) = underline {
                    let covered: Vec<_> = cuts
                        .windows(2)
                        .enumerate()
                        .filter(|(_, pair)| {
                            (pair[0] + pair[1]) * 0.5 >= left
                                && (pair[0] + pair[1]) * 0.5 <= right
                        })
                        .map(|(column, _)| column)
                        .collect();
                    if let Some((&first, &last)) =
                        covered.first().zip(covered.last())
                    {
                        phrase.columns = first..last + 1;
                        phrase.anchored = true;
                    }
                }
                phrases.push(phrase);
            }
            let mut owners = vec![None; table.column_count];
            for (index, phrase) in phrases.iter().enumerate() {
                for column in phrase.columns.clone() {
                    if blocked[column]
                        || owners[column].replace(index).is_some()
                    {
                        tracing::debug!(
                            "table at {:?} rejected heading row {} columns {:?}, bbox {:?}: occupied column {}",
                            self.bounds,
                            row,
                            phrase.columns,
                            phrase.bbox,
                            column
                        );
                        return None;
                    }
                }
            }
            // Expand only into vacant heading positions when doing so improves the
            // phrase's centering. Body columns are never collapsed to fit a heading.
            for (index, phrase) in phrases
                .iter_mut()
                .enumerate()
                .filter(|(_, phrase)| !phrase.anchored)
            {
                loop {
                    let distance = ((cuts[phrase.columns.start]
                        + cuts[phrase.columns.end])
                        * 0.5
                        - phrase.bbox.center().x)
                        .abs();
                    let mut best = None;
                    if phrase.columns.start > 0 {
                        let column = phrase.columns.start - 1;
                        let next = ((cuts[column] + cuts[phrase.columns.end])
                            * 0.5
                            - phrase.bbox.center().x)
                            .abs();
                        if !blocked[column]
                            && owners[column].is_none()
                            && distance - next > self.font_size * 0.2
                        {
                            best = Some((column, next));
                        }
                    }
                    if phrase.columns.end < table.column_count {
                        let column = phrase.columns.end;
                        let next = ((cuts[phrase.columns.start]
                            + cuts[column + 1])
                            * 0.5
                            - phrase.bbox.center().x)
                            .abs();
                        if !blocked[column]
                            && owners[column].is_none()
                            && distance - next > self.font_size * 0.2
                            && best.is_none_or(|(_, score)| next < score)
                        {
                            best = Some((column, next));
                        }
                    }
                    let Some((column, _)) = best else {
                        break;
                    };
                    owners[column] = Some(index);
                    phrase.columns.start = phrase.columns.start.min(column);
                    phrase.columns.end = phrase.columns.end.max(column + 1);
                }
            }
            for phrase in phrases {
                headings.push(
                    TableCell::builder()
                        .row(row)
                        .column(phrase.columns.start)
                        .column_span(phrase.columns.len())
                        .is_header(true)
                        .bbox(Some(
                            Bbox::try_from([
                                cuts[phrase.columns.start],
                                ys[row],
                                cuts[phrase.columns.end],
                                ys[row + 1],
                            ])
                            .ok()?,
                        ))
                        .build(),
                );
            }
            for column in 0..table.column_count {
                if !blocked[column] && owners[column].is_none() {
                    headings.push(
                        TableCell::builder()
                            .row(row)
                            .column(column)
                            .is_header(true)
                            .bbox(Some(
                                Bbox::try_from([
                                    cuts[column],
                                    ys[row],
                                    cuts[column + 1],
                                    ys[row + 1],
                                ])
                                .ok()?,
                            ))
                            .build(),
                    );
                }
            }
        }
        if ruled_header {
            // A stub without a divider spans the surrounding logical levels, even
            // when its title wraps or its only word is vertically centered.
            for column in 0..table.column_count {
                let mut row = 0;
                while row < header_rows {
                    let Some(index) = headings.iter().position(|cell| {
                        cell.row == row
                            && cell.column == column
                            && cell.column_span == 1
                    }) else {
                        row += 1;
                        continue;
                    };
                    let next_row = row + headings[index].row_span;
                    let next = headings.iter().position(|cell| {
                        cell.row == next_row
                            && cell.column == column
                            && cell.column_span == 1
                    });
                    if let Some(next) = next
                        && self.coverage(
                            rules,
                            true,
                            ys[next_row],
                            cuts[column],
                            cuts[column + 1],
                        ) < 0.7
                    {
                        let bottom =
                            headings[next].row + headings[next].row_span;
                        headings[index].row_span = bottom - row;
                        headings[index].bbox.as_mut()?.bottom = ys[bottom];
                        headings.remove(next);
                    } else {
                        row = next_row;
                    }
                }
            }
        }
        table.cells.retain(|cell| cell.row >= header_rows);
        table.cells.extend(headings);

        // Sparse horizontal bands often delimit model/metric groups. A single text
        // label centered over several populated rows owns a rowspan, not a new data row.
        for band in full_rules.windows(2) {
            let rows: Vec<_> = groups
                .iter()
                .enumerate()
                .filter(|(row, group)| {
                    *row >= header_rows
                        && group.iter().all(|&physical| {
                            self.rows[physical].baseline > band[0]
                                && self.rows[physical].baseline < band[1]
                        })
                })
                .map(|(row, _)| row)
                .collect();
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
                self.rows[*groups[lower_middle].first()?].baseline;
            let center_end = self.rows[*groups[upper_middle].last()?].baseline;
            for column in 0..table.column_count {
                let words: Vec<_> = rows
                    .iter()
                    .flat_map(|&row| row_spans[row].iter().copied())
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
                table.cells.retain(|cell| {
                    !(cell.column == column
                        && cell.row >= first
                        && cell.row <= last)
                });
                table.cells.push(
                    TableCell::builder()
                        .row(first)
                        .column(column)
                        .row_span(rows.len())
                        .bbox(Some(
                            Bbox::try_from([
                                cuts[column],
                                ys[first],
                                cuts[column + 1],
                                ys[last + 1],
                            ])
                            .ok()?,
                        ))
                        .build(),
                );
            }
        }
        // Wrapped one-column headings can share a band with vertically centered
        // neighbors. A global cut through that band clips the next heading line
        // and makes otherwise valid words fail cell ownership. Grouped headings
        // retain their distinct levels and explicit colspans.
        let wrapped_header = header_rows > 1
            && table
                .cells
                .iter()
                .filter(|cell| cell.is_header)
                .all(|cell| cell.column_span == 1)
            && row_spans
                .iter()
                .take(header_rows)
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| {
                    let bottom = pair[0]
                        .iter()
                        .map(|&i| self.spans[i].span.bbox.bottom)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let top = pair[1]
                        .iter()
                        .map(|&i| self.spans[i].span.bbox.top)
                        .fold(f64::INFINITY, f64::min);
                    bottom > top
                });
        if wrapped_header {
            table.cells.retain(|cell| !cell.is_header);
            for cell in &mut table.cells {
                cell.row -= header_rows - 1;
            }
            table.row_count -= header_rows - 1;
            for column in 0..table.column_count {
                table.cells.push(
                    TableCell::builder()
                        .row(0)
                        .column(column)
                        .is_header(true)
                        .bbox(Some(
                            Bbox::try_from([
                                cuts[column],
                                ys[0],
                                cuts[column + 1],
                                ys[header_rows],
                            ])
                            .ok()?,
                        ))
                        .build(),
                );
            }
        }
        table.cells.sort_by_key(|cell| (cell.row, cell.column));
        Some(table)
    }
}
