//! Calibration of learned axes against immutable native words and visible separators.
use super::*;

/// Shared boundaries of one bounded model grid.
pub(super) struct PredictedAxes {
    pub columns: Vec<f64>,
    pub rows: Vec<f64>,
}

impl TryFrom<(&Table, Bbox)> for PredictedAxes {
    type Error = String;

    /// Uses robust position-head medians, retaining interpolation only at unobserved span interiors.
    fn try_from((table, bounds): (&Table, Bbox)) -> Result<Self, Self::Error> {
        let axes = [
            (true, table.column_count, bounds.left, bounds.right),
            (false, table.row_count, bounds.top, bounds.bottom),
        ]
        .map(|(horizontal, count, low, high)| {
            let mut axis = vec![low];
            for cut in 1..count {
                let mut starts = Vec::new();
                let mut ends = Vec::new();
                for cell in &table.cells {
                    let b = cell.bbox.ok_or("model cell lacks position")?;
                    let (start, span, a, b) = if horizontal {
                        (cell.column, cell.column_span, b.left, b.right)
                    } else {
                        (cell.row, cell.row_span, b.top, b.bottom)
                    };
                    if start == cut {
                        starts.push(a);
                    }
                    if start + span == cut {
                        ends.push(b);
                    }
                }
                starts.sort_by(f64::total_cmp);
                ends.sort_by(f64::total_cmp);
                let observed = starts
                    .get(starts.len() / 2)
                    .zip(ends.get(ends.len() / 2))
                    .map(|(a, b)| (a + b) * 0.5);
                axis.push(
                    observed.unwrap_or(
                        low + (high - low) * cut as f64 / count as f64,
                    ),
                );
            }
            axis.push(high);
            if axis.windows(2).any(|pair| matches!(pair, [a, b] if a >= b)) {
                return Err("model axis is not ordered".to_owned());
            }
            Ok(axis)
        });
        let [columns, rows] = axes;
        Ok(Self {
            columns: columns?,
            rows: rows?,
        })
    }
}

impl PredictedAxes {
    /// Resolves every logical cell interval through the same calibrated axes.
    pub(super) fn regions(&self, table: &Table) -> Result<Vec<Bbox>, String> {
        table
            .cells
            .iter()
            .map(|cell| {
                Bbox::try_from([
                    *self
                        .columns
                        .get(cell.column)
                        .ok_or("missing cell start column")?,
                    *self.rows.get(cell.row).ok_or("missing cell start row")?,
                    *self
                        .columns
                        .get(cell.column + cell.column_span)
                        .ok_or("missing cell end column")?,
                    *self
                        .rows
                        .get(cell.row + cell.row_span)
                        .ok_or("missing cell end row")?,
                ])
                .map_err(|error| error.to_string())
            })
            .collect()
    }
}

/// One measured source row in a candidate anchor column.
struct SourceBand {
    top: f64,
    bottom: f64,
    baseline: f64,
}

/// The best complete source column near the model's row count and positions.
struct RowAnchors {
    distance: f64,
    bands: Vec<SourceBand>,
}

impl TableGeometry<'_> {
    /// Applies calibration transactionally; an unsupported remapping retains the original model rows.
    pub(super) fn calibrated_regions(
        &self,
        table: &mut Table,
        rules: &[TableRule],
    ) -> Result<Vec<Bbox>, String> {
        let fallback = table.predicted_regions(self.bounds)?;
        let Ok(mut axes) = PredictedAxes::try_from((&*table, self.bounds))
        else {
            return Ok(fallback);
        };
        let header = table
            .cells
            .iter()
            .filter(|cell| cell.is_header)
            .map(|cell| cell.row + cell.row_span)
            .max()
            .unwrap_or(1)
            .min(table.row_count);
        let header_end =
            *axes.rows.get(header).ok_or("missing header boundary")?;
        self.snap_predicted_columns(&mut axes, header_end);
        if let Some(anchors) =
            self.source_row_anchors(table, &axes, header, header_end)
        {
            let original = axes.rows.clone();
            axes.rows = self.source_row_boundaries(
                table,
                &axes,
                &anchors.bands,
                header,
                rules,
            )?;
            if anchors.bands.len() != table.row_count - header
                && axes
                    .rows
                    .windows(2)
                    .all(|pair| matches!(pair, [a, b] if a < b))
            {
                match table.remap_predicted_rows(&original, &axes, header) {
                    Ok(candidate) => {
                        tracing::info!(
                            "aligned TSR body rows from {} to {} using a complete native anchor column at {:?}",
                            table.row_count - header,
                            anchors.bands.len(),
                            self.bounds
                        );
                        *table = candidate;
                    }
                    Err(reason) => {
                        tracing::debug!(
                            "retained original model rows at {:?}: {}",
                            self.bounds,
                            reason
                        );
                        axes.rows = original;
                    }
                }
            }
        }
        if axes
            .columns
            .windows(2)
            .chain(axes.rows.windows(2))
            .any(|pair| matches!(pair, [a, b] if a >= b))
        {
            return Ok(fallback);
        }
        axes.regions(table)
    }

    /// Moves each divider only into an observed ink gap inside its neighboring model column bands.
    #[allow(
        clippy::indexing_slicing,
        reason = "cut indices stay within the validated model axes"
    )]
    fn snap_predicted_columns(
        &self,
        axes: &mut PredictedAxes,
        header_end: f64,
    ) {
        let mut intervals: Vec<_> = self
            .spans
            .iter()
            .filter(|word| word.baseline > header_end)
            .map(|word| (word.span.bbox.left, word.span.bbox.right))
            .collect();
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut gaps = Vec::new();
        let mut edge = self.bounds.left;
        for (left, right) in intervals {
            if left - edge >= self.font_size * 0.2 {
                gaps.push((edge, left));
            }
            edge = edge.max(right);
        }
        let original = axes.columns.clone();
        for index in 1..axes.columns.len() - 1 {
            let adjusted = gaps
                .iter()
                .filter(|(a, b)| {
                    *a > axes.columns[index - 1] && *b < original[index + 1]
                })
                .map(|(a, b)| original[index].clamp(a + 1e-6, b - 1e-6))
                .min_by(|a, b| {
                    (a - original[index])
                        .abs()
                        .total_cmp(&(b - original[index]).abs())
                });
            if let Some(adjusted) = adjusted {
                axes.columns[index] = adjusted;
            }
        }
    }

    /// Selects a complete narrow source column, allowing only a bounded discrepancy in model row count.
    #[allow(
        clippy::indexing_slicing,
        reason = "word and column indices originate from validated source rows and model axes"
    )]
    fn source_row_anchors(
        &self,
        table: &Table,
        axes: &PredictedAxes,
        header: usize,
        header_end: f64,
    ) -> Option<RowAnchors> {
        if let Some(numbered) =
            self.numbered_row_anchors(table, axes, header_end)
        {
            return Some(numbered);
        }
        let body_count = table.row_count - header;
        let mut selected: Option<RowAnchors> = None;
        for column in 0..table.column_count {
            if table.cells.iter().any(|cell| {
                cell.row >= header
                    && cell.column <= column
                    && cell.column + cell.column_span > column
                    && (cell.row_span > 1 || cell.column_span > 1)
            }) {
                continue;
            }
            let mut bands = Vec::new();
            for row in &self.rows {
                if row.baseline <= header_end {
                    continue;
                }
                let words: Vec<_> = row
                    .spans
                    .iter()
                    .map(|&i| self.spans[i].span.bbox)
                    .filter(|b| {
                        b.center().x >= axes.columns[column]
                            && b.center().x < axes.columns[column + 1]
                    })
                    .collect();
                if words.is_empty() {
                    continue;
                }
                bands.push(SourceBand {
                    top: words.iter().map(|b| b.top).min_by(f64::total_cmp)?,
                    bottom: words
                        .iter()
                        .map(|b| b.bottom)
                        .max_by(f64::total_cmp)?,
                    baseline: row.baseline,
                });
            }
            if bands.is_empty() || body_count == 0 {
                continue;
            }
            if bands.len() != body_count
                && (body_count < 3
                    || bands.len() < 3
                    || bands.len().abs_diff(body_count) > 1.max(body_count / 3)
                    || axes.columns[column + 1] - axes.columns[column]
                        > self.bounds.width() * 0.4)
            {
                continue;
            }
            let distance = bands
                .iter()
                .enumerate()
                .map(|(i, band)| {
                    let expected = header
                        + i * (body_count - 1) / (bands.len() - 1).max(1);
                    ((band.top + band.bottom) * 0.5
                        - (axes.rows[expected] + axes.rows[expected + 1]) * 0.5)
                        .abs()
                })
                .sum::<f64>()
                + bands.len().abs_diff(body_count) as f64
                    * self.font_size
                    * 2.0;
            if selected
                .as_ref()
                .is_none_or(|previous| distance < previous.distance)
            {
                selected = Some(RowAnchors { distance, bands });
            }
        }
        selected
    }

    /// Requires consecutive circled row keys and aligned content in another model column before recovering collapsed rows.
    fn numbered_row_anchors(
        &self,
        table: &Table,
        axes: &PredictedAxes,
        header_end: f64,
    ) -> Option<RowAnchors> {
        if table.column_count < 2 {
            return None;
        }
        let right = *axes.columns.get(1)?;
        let mut numbered = Vec::new();
        for word in self.spans {
            if word.baseline <= header_end || word.span.bbox.center().x >= right
            {
                continue;
            }
            let mut characters = word.text().trim().chars();
            let character = characters.next()?;
            if characters.next().is_some() {
                continue;
            }
            // Unicode circled and dingbat digits have contiguous ordinal ranges; source glyphs remain unchanged.
            let ordinal = [('①', 20_u32), ('❶', 10), ('➀', 10), ('➊', 10)]
                .into_iter()
                .find_map(|(start, count)| {
                    u32::from(character)
                        .checked_sub(u32::from(start))
                        .filter(|&n| n < count)
                        .map(|n| n + 1)
                });
            if let Some(ordinal) = ordinal {
                numbered.push((ordinal, word));
            }
        }
        numbered.sort_by(|a, b| a.1.baseline.total_cmp(&b.1.baseline));
        if numbered.len() < 3
            || numbered.len() + 1 > MAX_TABLE_ROWS
            || (numbered.len() + 1) * table.column_count > MAX_TABLE_CELLS
        {
            return None;
        }
        let left = numbered.first()?.1.span.bbox.left;
        if numbered.iter().enumerate().any(|(i, (ordinal, word))| {
            *ordinal as usize != i + 1
                || (word.span.bbox.left - left).abs() > self.font_size * 0.3
                || !self.spans.iter().any(|other| {
                    other.span.bbox.center().x >= right
                        && (other.baseline - word.baseline).abs()
                            <= self.font_size * 0.6
                })
        }) {
            return None;
        }
        Some(RowAnchors {
            distance: 0.0,
            bands: numbered
                .into_iter()
                .map(|(_, word)| SourceBand {
                    top: word.span.bbox.top,
                    bottom: word.span.bbox.bottom,
                    baseline: word.baseline,
                })
                .collect(),
        })
    }

    /// Accepts paragraph envelopes only when a wide source column independently confirms every anchor row.
    #[allow(
        clippy::indexing_slicing,
        reason = "source row indices and model column bands are validated"
    )]
    fn paragraph_row_envelopes(
        &self,
        table: &Table,
        axes: &PredictedAxes,
        anchors: &[SourceBand],
        header: usize,
    ) -> Option<Vec<SourceBand>> {
        for column in 0..table.column_count {
            if axes.columns[column + 1] - axes.columns[column]
                < self.bounds.width() * 0.4
                || table.cells.iter().any(|cell| {
                    cell.row >= header
                        && cell.column <= column
                        && cell.column + cell.column_span > column
                        && (cell.row_span > 1 || cell.column_span > 1)
                })
            {
                continue;
            }
            let mut groups: Vec<SourceBand> = Vec::new();
            for row in &self.rows {
                if row.baseline <= axes.rows[header] {
                    continue;
                }
                let words: Vec<_> = row
                    .spans
                    .iter()
                    .map(|&i| self.spans[i].span.bbox)
                    .filter(|b| {
                        b.center().x >= axes.columns[column]
                            && b.center().x < axes.columns[column + 1]
                    })
                    .collect();
                if words.is_empty() {
                    continue;
                }
                let top = words.iter().map(|b| b.top).min_by(f64::total_cmp)?;
                let bottom =
                    words.iter().map(|b| b.bottom).max_by(f64::total_cmp)?;
                if let Some(last) = groups.last_mut()
                    && top - last.bottom <= self.font_size * 0.5
                {
                    last.bottom = last.bottom.max(bottom);
                } else {
                    groups.push(SourceBand {
                        top,
                        bottom,
                        baseline: row.baseline,
                    });
                }
            }
            if groups.len() == anchors.len()
                && groups.iter().zip(anchors).all(|(group, anchor)| {
                    anchor.baseline >= group.top - self.font_size
                        && anchor.baseline <= group.bottom + self.font_size
                })
            {
                return Some(groups);
            }
        }
        None
    }

    /// Keeps wrapped trailing lines before the next anchor and prefers separators spanning multiple columns.
    #[allow(
        clippy::indexing_slicing,
        reason = "model cell intervals and two-element source windows are validated"
    )]
    fn source_row_boundaries(
        &self,
        table: &Table,
        axes: &PredictedAxes,
        bands: &[SourceBand],
        header: usize,
        rules: &[TableRule],
    ) -> Result<Vec<f64>, String> {
        let mut rows = axes
            .rows
            .get(..=header)
            .ok_or("missing header rows")?
            .to_vec();
        // Compute eligible source ink once: category cells spanning rows cannot constrain a body divider.
        let ordinary: Vec<_> = self
            .spans
            .iter()
            .filter(|word| {
                let center = word.span.bbox.center().x;
                !table.cells.iter().any(|cell| {
                    cell.row >= header
                        && cell.row_span > 1
                        && center >= axes.columns[cell.column]
                        && center < axes.columns[cell.column + cell.column_span]
                })
            })
            .collect();
        let paragraphs =
            self.paragraph_row_envelopes(table, axes, bands, header);
        for (index, pair) in bands.windows(2).enumerate() {
            let lower = ordinary
                .iter()
                .filter(|word| {
                    word.baseline >= pair[0].baseline - self.font_size * 0.3
                        && word.baseline
                            < pair[1].baseline - self.font_size * 0.6
                })
                .map(|word| word.span.bbox.bottom)
                .max_by(f64::total_cmp)
                .unwrap_or(pair[0].bottom);
            let upper = ordinary
                .iter()
                .filter(|word| {
                    (word.baseline - pair[1].baseline).abs()
                        <= self.font_size * 0.3
                })
                .map(|word| word.span.bbox.top)
                .min_by(f64::total_cmp)
                .unwrap_or(pair[1].top);
            let (lower, upper) = paragraphs
                .as_ref()
                .map(|groups| (groups[index].bottom, groups[index + 1].top))
                .unwrap_or((lower, upper));
            let midpoint = if lower < upper {
                (lower + upper) * 0.5
            } else {
                (pair[0].bottom + pair[1].top) * 0.5
            };
            let cut = rules
                .iter()
                .filter_map(|rule| match *rule {
                    TableRule::Horizontal { y, left, right }
                        if y > pair[0].bottom
                            && y < pair[1].top
                            && axes
                                .columns
                                .windows(2)
                                .filter(|band| {
                                    (right.min(band[1]) - left.max(band[0]))
                                        .max(0.0)
                                        / (band[1] - band[0])
                                        >= 0.8
                                })
                                .count()
                                >= 2 =>
                    {
                        Some(y)
                    }
                    _ => None,
                })
                .min_by(|a, b| {
                    (a - midpoint).abs().total_cmp(&(b - midpoint).abs())
                })
                .unwrap_or(midpoint);
            rows.push(cut);
        }
        rows.push(self.bounds.bottom);
        Ok(rows)
    }
}

impl Table {
    /// Maps model spans onto a calibrated row axis before committing any changed row count.
    #[allow(
        clippy::indexing_slicing,
        reason = "axes are bounded and table spans were validated before remapping"
    )]
    fn remap_predicted_rows(
        &self,
        previous: &[f64],
        axes: &PredictedAxes,
        header: usize,
    ) -> Result<Self, String> {
        let mut proposed = self.clone();
        proposed.row_count = axes.rows.len() - 1;
        proposed.cells.retain(|cell| cell.row < header);
        for row in header..proposed.row_count {
            for column in 0..proposed.column_count {
                proposed.cells.push(
                    TableCell::builder()
                        .row(row)
                        .column(column)
                        .bbox(Some(
                            Bbox::try_from([
                                axes.columns[column],
                                axes.rows[row],
                                axes.columns[column + 1],
                                axes.rows[row + 1],
                            ])
                            .map_err(|error| error.to_string())?,
                        ))
                        .build(),
                );
            }
        }
        let mut grid = CellGrid::try_from(proposed)?;
        for cell in self.cells.iter().filter(|cell| {
            cell.row >= header && (cell.row_span > 1 || cell.column_span > 1)
        }) {
            let start = (header..axes.rows.len() - 1)
                .min_by(|&a, &b| {
                    (axes.rows[a] - previous[cell.row])
                        .abs()
                        .total_cmp(&(axes.rows[b] - previous[cell.row]).abs())
                })
                .ok_or("missing remapped row")?;
            let end = (start + 1..axes.rows.len())
                .min_by(|&a, &b| {
                    (axes.rows[a] - previous[cell.row + cell.row_span])
                        .abs()
                        .total_cmp(
                            &(axes.rows[b]
                                - previous[cell.row + cell.row_span])
                                .abs(),
                        )
                })
                .ok_or("missing remapped span end")?;
            let mut mapped = cell.clone();
            mapped.row = start;
            mapped.row_span = end - start;
            mapped.bbox = Some(
                Bbox::try_from([
                    axes.columns[cell.column],
                    axes.rows[start],
                    axes.columns[cell.column + cell.column_span],
                    axes.rows[end],
                ])
                .map_err(|error| error.to_string())?,
            );
            grid.try_merge(mapped)?;
        }
        Ok(grid.table().clone())
    }
}

impl Table {
    /// Extends incomplete learned content boxes only to neighboring model cells in the same logical bands.
    pub(crate) fn predicted_regions(
        &self,
        bounds: Bbox,
    ) -> Result<Vec<Bbox>, String> {
        self.cells
            .iter()
            .map(|cell| {
                let b = cell.bbox.ok_or("model cell has no position")?;
                let mut edges =
                    [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
                for other in &self.cells {
                    let c = other.bbox.ok_or("model cell has no position")?;
                    if other.row < cell.row + cell.row_span
                        && other.row + other.row_span > cell.row
                    {
                        if other.column + other.column_span == cell.column {
                            edges[0].push((c.right + b.left) * 0.5);
                        }
                        if other.column == cell.column + cell.column_span {
                            edges[2].push((b.right + c.left) * 0.5);
                        }
                    }
                    if other.column < cell.column + cell.column_span
                        && other.column + other.column_span > cell.column
                    {
                        if other.row + other.row_span == cell.row {
                            edges[1].push((c.bottom + b.top) * 0.5);
                        }
                        if other.row == cell.row + cell.row_span {
                            edges[3].push((b.bottom + c.top) * 0.5);
                        }
                    }
                }
                let left = edges[0]
                    .iter()
                    .copied()
                    .max_by(f64::total_cmp)
                    .unwrap_or(bounds.left);
                let right = edges[2]
                    .iter()
                    .copied()
                    .min_by(f64::total_cmp)
                    .unwrap_or(bounds.right);
                let top = edges[1]
                    .iter()
                    .copied()
                    .max_by(f64::total_cmp)
                    .unwrap_or(bounds.top);
                let bottom = edges[3]
                    .iter()
                    .copied()
                    .min_by(f64::total_cmp)
                    .unwrap_or(bounds.bottom);
                Ok(Bbox::try_from([
                    left.max(bounds.left),
                    top.max(bounds.top),
                    right.min(bounds.right),
                    bottom.min(bounds.bottom),
                ])
                .unwrap_or(b))
            })
            .collect()
    }
}
