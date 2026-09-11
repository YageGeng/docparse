//! Recovery of source-supported shared-value sections in otherwise dense model grids.
use super::*;

#[allow(
    clippy::indexing_slicing,
    reason = "word indices and band endpoints come from immutable source selections"
)]
impl TableGeometry<'_> {
    /// Restores a ruled section whose repeated values are centered across all data columns.
    #[allow(
        clippy::indexing_slicing,
        reason = "source and region indices come from bounded model rows"
    )]
    pub(super) fn merge_shared_value_sections(
        &self,
        table: &mut Table,
        regions: &mut Vec<Bbox>,
        rules: &[TableRule],
    ) -> Result<(), String> {
        if table.column_count < 3 {
            return Ok(());
        }
        let Some(stub_end) = table
            .cells
            .iter()
            .zip(regions.iter())
            .find(|(c, _)| c.column == 0 && c.column_span == 1)
            .map(|(_, b)| b.right)
        else {
            return Ok(());
        };
        let borders = self.snapped(
            rules
                .iter()
                .filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, .. }
                        if y >= self.bounds.top
                            && y <= self.bounds.bottom
                            && self.coverage(
                                rules,
                                true,
                                y,
                                self.bounds.left,
                                self.bounds.right,
                            ) >= 0.9 =>
                    {
                        Some(y)
                    }
                    _ => None,
                })
                .collect(),
        );
        // A single sparse row is ambiguous. Require a section heading followed by
        // at least three aligned label/value rows between complete separators.
        for band in borders.windows(2) {
            if self.has_column_divider(rules, band[0], band[1]) {
                continue;
            }
            let rows = self.section_rows(table, regions, [band[0], band[1]]);
            if !self.has_shared_values(&rows, stub_end) {
                continue;
            }
            let mut proposed = CellGrid::try_from(table.clone())?;
            let mut accepted = true;
            for (index, (&row, source)) in rows.iter().enumerate() {
                // Even a divider confined to one row vetoes a section-wide reinterpretation.
                if self.has_column_divider(rules, source.top, source.bottom) {
                    accepted = false;
                    break;
                }
                let column = usize::from(index > 0);
                let cell = TableCell::builder()
                    .row(row)
                    .column(column)
                    .column_span(table.column_count - column)
                    .is_header(index == 0)
                    .bbox(Some(
                        Bbox::try_from([
                            if column == 0 {
                                self.bounds.left
                            } else {
                                stub_end
                            },
                            source.top,
                            self.bounds.right,
                            source.bottom,
                        ])
                        .map_err(|e| e.to_string())?,
                    ))
                    .build();
                if proposed.try_merge(cell).is_err() {
                    accepted = false;
                    break;
                }
            }
            if !accepted {
                continue;
            }
            let next = proposed.table();
            let next_regions = next
                .cells
                .iter()
                .map(|cell| {
                    table
                        .cells
                        .iter()
                        .zip(regions.iter())
                        .find(|(old, _)| {
                            (old.row, old.column, old.row_span, old.column_span)
                                == (
                                    cell.row,
                                    cell.column,
                                    cell.row_span,
                                    cell.column_span,
                                )
                        })
                        .map(|(_, region)| *region)
                        .or(cell.bbox)
                        .ok_or("merged model cell lacks region".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?;
            tracing::info!(
                "recovered TSR section at row {} with {} shared values across {} columns at {:?}",
                rows.first_key_value()
                    .map(|(row, _)| row)
                    .ok_or("missing section row")?,
                rows.len() - 1,
                table.column_count - 1,
                self.bounds
            );
            *table = next.clone();
            *regions = next_regions;
        }
        Ok(())
    }

    /// Collects canonical words in consecutive model rows inside a fully ruled band.
    fn section_rows(
        &self,
        table: &Table,
        regions: &[Bbox],
        band: [f64; 2],
    ) -> BTreeMap<usize, GridRow> {
        let bounds: BTreeMap<_, _> = table
            .cells
            .iter()
            .zip(regions)
            .filter(|(cell, _)| !cell.is_header && cell.row_span == 1)
            .map(|(cell, b)| (cell.row, (b.top, b.bottom)))
            .collect();
        bounds
            .into_iter()
            .filter_map(|(row, (top, bottom))| {
                let mut words: Vec<_> = self
                    .spans
                    .iter()
                    .enumerate()
                    .filter(|(_, word)| {
                        let y = word.baseline - self.font_size * 0.3;
                        y >= top && y < bottom
                    })
                    .map(|(index, _)| index)
                    .collect();
                if words.is_empty()
                    || words.iter().any(|&index| {
                        self.spans[index].baseline <= band[0]
                            || self.spans[index].baseline >= band[1]
                    })
                {
                    return None;
                }
                words.sort_by(|&a, &b| {
                    self.spans[a]
                        .span
                        .bbox
                        .left
                        .total_cmp(&self.spans[b].span.bbox.left)
                });
                Some((
                    row,
                    GridRow::builder()
                        .top(top)
                        .bottom(bottom)
                        .physical(Vec::new())
                        .words(words)
                        .build(),
                ))
            })
            .collect()
    }

    /// Requires a wide section label plus at least three repeated stub-and-centered-value pairs.
    fn has_shared_values(
        &self,
        rows: &BTreeMap<usize, GridRow>,
        stub_end: f64,
    ) -> bool {
        let Some((&first, heading)) = rows.first_key_value() else {
            return false;
        };
        let Some((&last, _)) = rows.last_key_value() else {
            return false;
        };
        if rows.len() < 4 || last - first + 1 != rows.len() {
            return false;
        }
        let phrase = |words: &[usize]| {
            !words.is_empty()
                && words.windows(2).all(|p| {
                    (self.spans[p[0]].baseline - self.spans[p[1]].baseline)
                        .abs()
                        <= self.font_size * 0.6
                        && self.spans[p[1]].span.bbox.left
                            - self.spans[p[0]].span.bbox.right
                            < (self.font_size * 0.6).max(3.0)
                })
        };
        let heading = &heading.words;
        let Some((&left, &right)) = heading.first().zip(heading.last()) else {
            return false;
        };
        if !phrase(heading)
            || self.is_missing_value(heading)
            || !heading
                .iter()
                .any(|&i| self.spans[i].text().chars().any(char::is_alphabetic))
            || heading
                .iter()
                .any(|&i| self.spans[i].text().chars().any(char::is_numeric))
            || (self.spans[left].span.bbox.left - self.bounds.left).abs()
                > self.font_size
            || self.spans[right].span.bbox.right
                <= stub_end + self.font_size * 0.5
        {
            return false;
        }
        let shared_center = (stub_end + self.bounds.right) * 0.5;
        rows.values().skip(1).all(|row| {
            let split = row.words.partition_point(|&i| {
                self.spans[i].span.bbox.center().x < stub_end
            });
            let (label, value) = row.words.split_at(split);
            let Some((&label_left, (&value_left, &value_right))) =
                label.first().zip(value.first().zip(value.last()))
            else {
                return false;
            };
            phrase(label)
                && phrase(value)
                && label.iter().all(|&i| {
                    self.spans[i].span.bbox.right
                        <= stub_end + self.font_size * 0.25
                })
                && (self.spans[label_left].span.bbox.left - self.bounds.left)
                    .abs()
                    <= self.font_size
                && ((self.spans[value_left].span.bbox.left
                    + self.spans[value_right].span.bbox.right)
                    * 0.5
                    - shared_center)
                    .abs()
                    <= self.font_size * 1.5
        })
    }
}
