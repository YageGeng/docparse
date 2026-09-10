use super::*;

/// A visible phrase and its justified interval in the candidate columns.
struct HeadingPhrase {
    bbox: Bbox,
    columns: std::ops::Range<usize>,
    anchored: bool,
}

/// A complete replacement for the physical header rows, built before applying any edit.
#[derive(TypedBuilder)]
struct HeaderPlan {
    rows: Vec<GridRow>,
    consumed: usize,
    ruled: bool,
    #[builder(default)]
    title_end: Option<f64>,
}

/// The observed boundary between header levels and data rows.
struct HeaderBoundary {
    count: usize,
    end: Option<f64>,
    title_end: Option<f64>,
}

/// Header policy over immutable geometry; mutations are committed through CellGrid.
pub(super) struct HeaderRecovery<'a, 's> {
    geometry: &'a TableGeometry<'s>,
    cuts: &'a [f64],
    rules: &'a [TableRule],
}

#[allow(
    clippy::indexing_slicing,
    reason = "all axes and cell positions are bounded by the candidate topology"
)]
impl<'a, 's> HeaderRecovery<'a, 's> {
    /// Shares the observed axes and rules with the header stages.
    pub fn new(
        geometry: &'a TableGeometry<'s>,
        cuts: &'a [f64],
        rules: &'a [TableRule],
    ) -> Self {
        Self {
            geometry,
            cuts,
            rules,
        }
    }

    /// Computes and commits a complete header plan in one transaction.
    pub fn recover(&self, grid: &mut CellGrid) -> Option<usize> {
        let plan = self.plan(grid.rows())?;
        if plan.consumed == 0 {
            return Some(0);
        }
        let cells = self.cells(&plan)?;
        let count = plan.rows.len();
        grid.replace_rows(0..plan.consumed, plan.rows, cells).ok()?;
        Some(count)
    }

    /// Derives the number and geometry of header levels without altering the source grid.
    fn plan(&self, rows: &[GridRow]) -> Option<HeaderPlan> {
        let cuts = self.cuts;
        let rules = self.rules;
        let mut row_spans: Vec<Vec<usize>> =
            rows.iter().map(|r| r.words.clone()).collect();
        let HeaderBoundary {
            count: mut header_rows,
            end: header_end,
            title_end,
        } = self.boundary(&row_spans)?;
        let consumed = header_rows;
        let mut ys: Vec<_> = std::iter::once(rows.first()?.top)
            .chain(rows.iter().map(|r| r.bottom))
            .collect();
        // Partial header rules establish logical levels even when centered stubs
        // and wrapped titles contribute extra, overlapping physical baselines.
        let header_rules = self.geometry.snapped(
            rules
                .iter()
                .filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, left, right }
                        if header_rows > 1
                            && header_end.is_some_and(|end| {
                                y < end - self.geometry.rule_tolerance()
                            })
                            && row_spans
                                .iter()
                                .take(header_rows)
                                .flatten()
                                .any(|&i| {
                                    let center = self.geometry.spans[i]
                                        .span
                                        .bbox
                                        .center();
                                    center.x >= left
                                        && center.x <= right
                                        && center.y < y
                                })
                            && row_spans
                                .iter()
                                .take(header_rows)
                                .flatten()
                                .any(|&i| {
                                    let center = self.geometry.spans[i]
                                        .span
                                        .bbox
                                        .center();
                                    center.x >= left
                                        && center.x <= right
                                        && center.y > y
                                })
                            && cuts.windows(2).any(|pair| {
                                self.geometry
                                    .coverage(rules, true, y, pair[0], pair[1])
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
        // A title divider alone cannot merge distinct, non-overlapping header baselines.
        let ruled_header = !header_rules.is_empty()
            && header_rules.len() < header_rows
            && (title_end.is_none()
                || header_rules.len() > 1
                || header_rules.len() + 1 == header_rows
                || row_spans[..header_rows].windows(2).any(|pair| {
                    let bottom = pair[0]
                        .iter()
                        .map(|&i| self.geometry.spans[i].span.bbox.bottom)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let top = pair[1]
                        .iter()
                        .map(|&i| self.geometry.spans[i].span.bbox.top)
                        .fold(f64::INFINITY, f64::min);
                    bottom > top
                }));
        if ruled_header {
            let physical_count = header_rows;
            header_rows = header_rules.len() + 1;
            let mut header_ys = vec![ys[0]];
            header_ys.extend(header_rules);
            header_ys.extend_from_slice(&ys[physical_count..]);
            ys = header_ys;
            let mut header_spans = vec![Vec::new(); header_rows];
            for &index in row_spans.iter().take(physical_count).flatten() {
                let y = self.geometry.spans[index].span.bbox.center().y;
                let row = ys[..=header_rows]
                    .windows(2)
                    .position(|pair| y >= pair[0] && y <= pair[1])?;
                header_spans[row].push(index);
            }
            row_spans.splice(..physical_count, header_spans);
            tracing::debug!(
                "normalized table header at {:?} from {} physical rows to {} ruled levels",
                self.geometry.bounds,
                physical_count,
                header_rows
            );
        }
        let planned = (0..header_rows)
            .map(|row| {
                GridRow::builder()
                    .top(ys[row])
                    .bottom(ys[row + 1])
                    .physical(if ruled_header {
                        Vec::new()
                    } else {
                        rows[row].physical.clone()
                    })
                    .words(row_spans[row].clone())
                    .build()
            })
            .collect();
        Some(
            HeaderPlan::builder()
                .rows(planned)
                .consumed(consumed)
                .ruled(ruled_header)
                .title_end(title_end)
                .build(),
        )
    }

    /// Identifies the table title and header/body boundary using immutable word rows.
    fn boundary(&self, row_spans: &[Vec<usize>]) -> Option<HeaderBoundary> {
        let cuts = self.cuts;
        let rules = self.rules;
        let columns = cuts.len().checked_sub(1)?;
        let full_rules = self.geometry.snapped(
            rules
                .iter()
                .filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, left, right }
                        if right - left
                            >= self.geometry.bounds.width() * 0.7 =>
                    {
                        Some(y)
                    }
                    _ => None,
                })
                .collect(),
        );
        // A centered, uninterrupted title above a complete separator spans the table.
        // Its separator starts the column headings rather than ending the whole header.
        let title_end = row_spans.first().zip(row_spans.get(1)).and_then(
            |(first, next)| {
                let mut words = first.clone();
                words.sort_by(|&a, &b| {
                    self.geometry.spans[a]
                        .span
                        .bbox
                        .left
                        .total_cmp(&self.geometry.spans[b].span.bbox.left)
                });
                let left = self.geometry.spans[*words.first()?].span.bbox.left;
                let right = words
                    .iter()
                    .map(|&i| self.geometry.spans[i].span.bbox.right)
                    .max_by(f64::total_cmp)?;
                if ((left + right) * 0.5 - self.geometry.bounds.center().x)
                    .abs()
                    > self.geometry.font_size
                    || self.geometry.is_missing_value(&words)
                    || !words.iter().any(|&i| {
                        self.geometry.spans[i]
                            .text()
                            .chars()
                            .any(char::is_alphabetic)
                    })
                    || !words.windows(2).all(|pair| {
                        self.geometry.spans[pair[1]].span.bbox.left
                            - self.geometry.spans[pair[0]].span.bbox.right
                            < (self.geometry.font_size * 0.6).max(3.0)
                    })
                {
                    return None;
                }
                full_rules.iter().copied().find(|&y| {
                    first
                        .iter()
                        .all(|&i| self.geometry.spans[i].span.bbox.bottom < y)
                        && next
                            .iter()
                            .all(|&i| self.geometry.spans[i].span.bbox.top > y)
                        && !self.geometry.has_column_divider(
                            rules,
                            self.geometry.bounds.top,
                            y,
                        )
                        && self.geometry.coverage(
                            rules,
                            true,
                            y,
                            cuts[0],
                            cuts[columns],
                        ) >= 0.9
                })
            },
        );
        // Booktabs rules separate a textual header from a body whose physical rows have
        // no complete grid. This evidence works even when the PDF reports no bold font.
        let header_end = full_rules.iter().copied().find(|&y| {
            title_end.is_none_or(|end| y > end + self.geometry.rule_tolerance())
                && self
                    .geometry
                    .spans
                    .iter()
                    .any(|span| span.span.bbox.center().y < y)
                && self
                    .geometry
                    .spans
                    .iter()
                    .any(|span| span.span.bbox.center().y > y)
        });
        let mut header_rows = 0;
        for words in row_spans {
            let alphabetic = words.iter().any(|&i| {
                self.geometry.spans[i]
                    .text()
                    .chars()
                    .any(char::is_alphabetic)
            });
            let ruled = full_rules.len() < self.geometry.rows.len()
                && header_end.is_some_and(|end| {
                    words.iter().all(|&i| {
                        self.geometry.spans[i].span.bbox.center().y < end
                    })
                });
            let styled = words.iter().all(|&i| {
                !self.geometry.spans[i].text().chars().any(char::is_numeric)
            }) && words
                .iter()
                .filter(|&&i| self.geometry.spans[i].is_bold())
                .count()
                * 2
                >= words.len();
            // A complete separator isolates a body section from the following data.
            // A wide bold continuation can still be a subtitle; a local bold note cannot.
            let separated =
                row_spans.get(header_rows + 1).is_some_and(|next| {
                    full_rules.iter().any(|&y| {
                        words.iter().all(|&i| {
                            self.geometry.spans[i].span.bbox.bottom < y
                        }) && next
                            .iter()
                            .all(|&i| self.geometry.spans[i].span.bbox.top > y)
                    })
                });
            let spanning = cuts[1..columns].iter().any(|&x| {
                words
                    .iter()
                    .any(|&i| self.geometry.spans[i].span.bbox.right < x)
                    && words
                        .iter()
                        .any(|&i| self.geometry.spans[i].span.bbox.left > x)
            });
            if alphabetic
                && !self.geometry.is_missing_value(words)
                && (ruled
                    || (styled
                        && (header_rows == 0
                            || header_end.is_none()
                            || (!separated && spanning))))
            {
                header_rows += 1;
            } else {
                break;
            }
        }
        if header_rows >= row_spans.len() {
            header_rows = 0;
        }
        Some(HeaderBoundary {
            count: header_rows,
            end: header_end,
            title_end,
        })
    }

    /// Forms coherent heading phrases and anchors them to visible underlines.
    fn phrases(
        &self,
        words: &[usize],
        full_title: bool,
    ) -> Option<Vec<HeadingPhrase>> {
        let cuts = self.cuts;
        let rules = self.rules;
        let columns = cuts.len().checked_sub(1)?;
        let mut words = words.to_vec();
        words.sort_by(|&a, &b| {
            self.geometry.spans[a]
                .span
                .bbox
                .left
                .total_cmp(&self.geometry.spans[b].span.bbox.left)
        });
        let mut boxes: Vec<Bbox> = Vec::new();
        for index in words {
            let bbox = self.geometry.spans[index].span.bbox;
            if let Some(previous) = boxes.last_mut()
                && bbox.left - previous.right
                    < (self.geometry.font_size * 0.6).max(3.0)
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
                let overlap =
                    (bbox.right.min(pair[1]) - bbox.left.max(pair[0])).max(0.0);
                overlap / bbox.width().min(pair[1] - pair[0]).max(f64::EPSILON)
                    >= 0.3
            };
            let start = cuts.windows(2).position(significant)?;
            let end = cuts.windows(2).rposition(significant)? + 1;
            let mut phrase = HeadingPhrase {
                bbox: *bbox,
                columns: start..end,
                anchored: false,
            };
            if full_title {
                phrase.columns = 0..columns;
                phrase.anchored = true;
            }
            // A partial underline identifies groups such as Accuracy over five
            // subcolumns more reliably than the short centered heading's ink box.
            let underline = rules
                .iter()
                .filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, left, right }
                        if y >= bbox.bottom - 1.0
                            && y - bbox.bottom
                                <= self.geometry.font_size * 1.2
                            && right - left > bbox.width() * 1.2
                            && right - left
                                < self.geometry.bounds.width() * 0.9
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
            if let Some((_, left, right)) = underline
                && !phrase.anchored
            {
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
        Some(phrases)
    }

    /// Places header phrases and reserves existing stubs without duplicating text.
    fn cells(&self, plan: &HeaderPlan) -> Option<Vec<TableCell>> {
        let cuts = self.cuts;
        let columns = cuts.len().checked_sub(1)?;
        let header_rows = plan.rows.len();
        let mut headings: Vec<TableCell> = Vec::new();
        for row in 0..header_rows {
            let mut blocked = vec![false; columns];
            // Empty subheader positions continue an existing stub heading; groups with
            // children stay in their own header row instead of copying their text.
            for cell in &mut headings {
                if cell.row + cell.row_span == row
                    && !plan.rows[row].words.iter().any(|&i| {
                        let x = self.geometry.spans[i].span.bbox.center().x;
                        x >= cuts[cell.column]
                            && x < cuts[cell.column + cell.column_span]
                    })
                {
                    cell.row_span += 1;
                    if let Some(bbox) = &mut cell.bbox {
                        bbox.bottom = plan.rows[row].bottom;
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
            let mut phrases = self.phrases(
                &plan.rows[row].words,
                row == 0 && plan.title_end.is_some(),
            )?;
            let mut owners = vec![None; columns];
            for (index, phrase) in phrases.iter().enumerate() {
                for column in phrase.columns.clone() {
                    if blocked[column]
                        || owners[column].replace(index).is_some()
                    {
                        tracing::debug!(
                            "table at {:?} rejected heading row {} columns {:?}, bbox {:?}: occupied column {}",
                            self.geometry.bounds,
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
                phrase.expand(
                    index,
                    cuts,
                    &mut owners,
                    &blocked,
                    self.geometry.font_size,
                );
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
                                plan.rows[row].top,
                                cuts[phrase.columns.end],
                                plan.rows[row].bottom,
                            ])
                            .ok()?,
                        ))
                        .build(),
                );
            }
            for column in 0..columns {
                if !blocked[column] && owners[column].is_none() {
                    headings.push(
                        TableCell::builder()
                            .row(row)
                            .column(column)
                            .is_header(true)
                            .bbox(Some(
                                Bbox::try_from([
                                    cuts[column],
                                    plan.rows[row].top,
                                    cuts[column + 1],
                                    plan.rows[row].bottom,
                                ])
                                .ok()?,
                            ))
                            .build(),
                    );
                }
            }
        }
        self.merge_stubs(&mut headings, plan)?;
        Some(headings)
    }

    /// Joins vertical stubs only where the corresponding header divider is absent.
    fn merge_stubs(
        &self,
        headings: &mut Vec<TableCell>,
        plan: &HeaderPlan,
    ) -> Option<()> {
        let cuts = self.cuts;
        let rules = self.rules;
        let columns = cuts.len().checked_sub(1)?;
        let header_rows = plan.rows.len();
        if plan.ruled {
            // A stub without a divider spans the surrounding logical levels, even
            // when its title wraps or its only word is vertically centered.
            for column in 0..columns {
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
                        && self.geometry.coverage(
                            rules,
                            true,
                            plan.rows[next_row].top,
                            cuts[column],
                            cuts[column + 1],
                        ) < 0.7
                    {
                        let bottom =
                            headings[next].row + headings[next].row_span;
                        headings[index].row_span = bottom - row;
                        headings[index].bbox.as_mut()?.bottom =
                            plan.rows[bottom - 1].bottom;
                        headings.remove(next);
                    } else {
                        row = next_row;
                    }
                }
            }
        }
        Some(())
    }

    /// Collapses overlapping one-column headings after body groups have been recovered.
    pub fn collapse_wrapped(
        &self,
        grid: &mut CellGrid,
        count: usize,
    ) -> Option<()> {
        let rows = grid.rows();
        let wrapped = count > 1
            && grid
                .table()
                .cells
                .iter()
                .filter(|c| c.row < count)
                .all(|c| c.column_span == 1)
            && rows[..count].windows(2).any(|pair| {
                let bottom = pair[0]
                    .words
                    .iter()
                    .map(|&i| self.geometry.spans[i].span.bbox.bottom)
                    .fold(f64::NEG_INFINITY, f64::max);
                let top = pair[1]
                    .words
                    .iter()
                    .map(|&i| self.geometry.spans[i].span.bbox.top)
                    .fold(f64::INFINITY, f64::min);
                bottom > top
            });
        if !wrapped {
            return Some(());
        }
        let top = rows[0].top;
        let bottom = rows[count - 1].bottom;
        let row = GridRow::builder()
            .top(top)
            .bottom(bottom)
            .physical(
                rows[..count]
                    .iter()
                    .flat_map(|r| r.physical.iter().copied())
                    .collect(),
            )
            .words(
                rows[..count]
                    .iter()
                    .flat_map(|r| r.words.iter().copied())
                    .collect(),
            )
            .build();
        let cells = self
            .cuts
            .windows(2)
            .enumerate()
            .map(|(column, pair)| {
                Some(
                    TableCell::builder()
                        .row(0)
                        .column(column)
                        .is_header(true)
                        .bbox(Some(
                            Bbox::try_from([pair[0], top, pair[1], bottom])
                                .ok()?,
                        ))
                        .build(),
                )
            })
            .collect::<Option<Vec<_>>>()?;
        grid.replace_rows(0..count, vec![row], cells).ok()?;
        Some(())
    }
}

#[allow(
    clippy::indexing_slicing,
    reason = "all axes and cell positions are bounded by the candidate topology"
)]
impl HeadingPhrase {
    /// Expands into vacant columns only when the resulting interval better centers this phrase.
    fn expand(
        &mut self,
        index: usize,
        cuts: &[f64],
        owners: &mut [Option<usize>],
        blocked: &[bool],
        font_size: f64,
    ) {
        loop {
            let distance =
                ((cuts[self.columns.start] + cuts[self.columns.end]) * 0.5
                    - self.bbox.center().x)
                    .abs();
            let mut best = None;
            if self.columns.start > 0 {
                let column = self.columns.start - 1;
                let next = ((cuts[column] + cuts[self.columns.end]) * 0.5
                    - self.bbox.center().x)
                    .abs();
                if !blocked[column]
                    && owners[column].is_none()
                    && distance - next > font_size * 0.2
                {
                    best = Some((column, next));
                }
            }
            if self.columns.end < owners.len() {
                let column = self.columns.end;
                let next = ((cuts[self.columns.start] + cuts[column + 1])
                    * 0.5
                    - self.bbox.center().x)
                    .abs();
                if !blocked[column]
                    && owners[column].is_none()
                    && distance - next > font_size * 0.2
                    && best.is_none_or(|(_, score)| next < score)
                {
                    best = Some((column, next));
                }
            }
            let Some((column, _)) = best else {
                break;
            };
            owners[column] = Some(index);
            self.columns.start = self.columns.start.min(column);
            self.columns.end = self.columns.end.max(column + 1);
        }
    }
}
