use super::*;

impl TableGrid<'_> {
    /// Finds column gutters supported across rows, preserving empty positions and cautious text wraps.
    #[allow(
        clippy::indexing_slicing,
        reason = "row and word indices are generated locally; window and predecessor accesses are guarded"
    )]
    pub fn aligned(&self, rules: &[TableRule]) -> Option<RecoveredGrid> {
        if self.rows.len() < 2 || self.rows.len() > MAX_TABLE_ROWS {
            return None;
        }
        // A majority of vertical/oblique source text cannot establish horizontal row semantics.
        if self
            .spans
            .iter()
            .filter(|span| {
                span.item
                    .rotation
                    .rem_euclid(180.0)
                    .min(180.0 - span.item.rotation.rem_euclid(180.0))
                    > 5.0
            })
            .count()
            * 2
            > self.spans.len()
        {
            return None;
        }
        let mut candidates = Vec::new();
        let mut row_word_gaps = Vec::new();
        for row in &self.rows {
            let mut gaps = Vec::new();
            let mut edge = None;
            for &index in &row.spans {
                let bbox = self.spans[index].span.bbox;
                if let Some(right) = edge
                    && bbox.left - right >= (self.font_size * 0.6).max(3.0)
                {
                    gaps.push((right, bbox.left));
                }
                edge =
                    Some(edge.map_or(bbox.right, |right: f64| {
                        right.max(bbox.right)
                    }));
            }
            if !gaps.is_empty() && gaps.len() < MAX_TABLE_COLUMNS {
                candidates.push(gaps.clone());
            }
            row_word_gaps.push(gaps);
        }
        candidates.sort_by_key(|gaps| std::cmp::Reverse(gaps.len()));
        tracing::debug!(
            "table at {:?} has candidate column counts {:?}",
            self.bounds,
            candidates
                .iter()
                .map(|gaps| gaps.len() + 1)
                .collect::<Vec<_>>()
        );
        // Prefer the strongest repeated column count. Fewer columns must not become
        // a fallback just because a spanning heading does not fit an ordinary cell.
        if let Some(maximum) = candidates.first().map(Vec::len) {
            candidates.retain(|gaps| gaps.len() == maximum);
        }
        let mut row_gaps: Vec<_> = self
            .rows
            .windows(2)
            .map(|pair| pair[1].baseline - pair[0].baseline)
            .collect();
        row_gaps.sort_by(f64::total_cmp);
        let ordinary_gap = row_gaps
            .get(row_gaps.len() / 2)
            .copied()
            .unwrap_or(self.font_size * 2.0);
        let local = self.local_rules(rules);
        for mut gaps in candidates {
            let mut original_cuts = vec![self.bounds.left];
            original_cuts
                .extend(gaps.iter().map(|(left, right)| (left + right) * 0.5));
            original_cuts.push(self.bounds.right);
            // Complete rows, and contiguous partial rows such as subheaders, narrow
            // each corresponding gutter. Short right-aligned numbers must not place
            // a boundary through a wider column heading on another physical row.
            for (row, other) in self.rows.iter().zip(&row_word_gaps) {
                if other.is_empty() {
                    continue;
                }
                let offset = if other.len() == gaps.len() {
                    Some(0)
                } else {
                    let first_left =
                        self.spans[*row.spans.first()?].span.bbox.left;
                    let last_right = row
                        .spans
                        .iter()
                        .map(|&i| self.spans[i].span.bbox.right)
                        .fold(f64::NEG_INFINITY, f64::max);
                    let first_center = (first_left + other.first()?.0) * 0.5;
                    let last_center = (other.last()?.1 + last_right) * 0.5;
                    let first = original_cuts.windows(2).position(|pair| {
                        first_center >= pair[0] && first_center <= pair[1]
                    });
                    let last = original_cuts.windows(2).position(|pair| {
                        last_center >= pair[0] && last_center <= pair[1]
                    });
                    first.zip(last).and_then(|(first, last)| {
                        (last.checked_sub(first) == Some(other.len()))
                            .then_some(first)
                    })
                };
                if let Some(offset) = offset {
                    for (index, &(left, right)) in other.iter().enumerate() {
                        if let Some(gap) = gaps.get_mut(offset + index) {
                            gap.0 = gap.0.max(left);
                            gap.1 = gap.1.min(right);
                        }
                    }
                }
            }
            if gaps.iter().any(|(left, right)| right - left <= 0.1) {
                continue;
            }
            let mut cuts = vec![self.bounds.left];
            for (left, right) in gaps {
                let x = (left + right) * 0.5;
                let support = self
                    .rows
                    .iter()
                    .filter(|row| {
                        !row.spans.iter().any(|&index| {
                            let b = self.spans[index].span.bbox;
                            b.left + 0.5 < x && b.right - 0.5 > x
                        })
                    })
                    .count();
                if support * 3 >= self.rows.len() * 2 {
                    cuts.push(x);
                }
            }
            cuts.push(self.bounds.right);
            let columns = cuts.len() - 1;
            if columns < 2 {
                continue;
            }
            let mut occupied = Vec::new();
            for row in &self.rows {
                let mut set = BTreeSet::new();
                for &index in &row.spans {
                    let x = self.spans[index].span.bbox.center().x;
                    set.insert(
                        cuts.windows(2).position(|bounds| {
                            x >= bounds[0] && x <= bounds[1]
                        })?,
                    );
                }
                occupied.push(set);
            }
            if occupied.iter().filter(|set| set.len() >= 2).count() < 2 {
                continue;
            }
            let mut groups: Vec<Vec<usize>> = Vec::new();
            for (index, row) in self.rows.iter().enumerate() {
                let continuation = index > 0 && occupied[index].len() == 1
                    && occupied[index].is_subset(&occupied[index-1])
                    && row.baseline - self.rows[index - 1].baseline <= (self.font_size * 1.6).min(ordinary_gap*0.8)
                    && row.spans.iter().any(|&i| self.spans[i].text().chars().any(char::is_alphabetic))
                    && (!row.spans.iter().any(|&i| self.spans[i].is_bold())
                        || self.rows[index-1].spans.iter().all(|&i| self.spans[i].is_bold()))
                    && !local.iter().any(|rule| matches!(rule, TableRule::Horizontal { y, left, right } if *y > self.rows[index - 1].baseline && *y < row.baseline && right - left > self.bounds.width() * 0.7));
                if continuation && let Some(group) = groups.last_mut() {
                    group.push(index);
                } else {
                    groups.push(vec![index]);
                }
            }
            if groups.len() < 2 || groups.len() * columns > MAX_TABLE_CELLS {
                continue;
            }
            let mut ys = vec![self.bounds.top];
            for pair in groups.windows(2) {
                let previous = *pair[0].last()?;
                let next = *pair[1].first()?;
                let bottom = self.rows[previous]
                    .spans
                    .iter()
                    .map(|&index| self.spans[index].span.bbox.bottom)
                    .fold(f64::NEG_INFINITY, f64::max);
                let top = self.rows[next]
                    .spans
                    .iter()
                    .map(|&index| self.spans[index].span.bbox.top)
                    .fold(f64::INFINITY, f64::min);
                ys.push((bottom + top) * 0.5);
            }
            ys.push(self.bounds.bottom);
            let mut cells = Vec::new();
            for row in 0..groups.len() {
                for column in 0..columns {
                    cells.push(
                        TableCell::builder()
                            .row(row)
                            .column(column)
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
            let source = if local.iter().filter(|rule| matches!(rule, TableRule::Horizontal { left, right, .. } if right-left >= self.bounds.width()*0.7)).count() >= 2 { TableStructureSource::Ruled } else { TableStructureSource::TextAlignment };
            let Some(table) = self.recover_spans(
                Table::builder()
                    .row_count(groups.len())
                    .column_count(columns)
                    .cells(cells)
                    .source(source)
                    .build(),
                &cuts,
                &ys,
                &groups,
                &local,
            ) else {
                continue;
            };
            if let Some(grid) = self.assign(table) {
                return Some(grid);
            }
        }
        None
    }
}
