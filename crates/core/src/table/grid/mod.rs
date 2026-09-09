//! Shared bounded grid geometry and source ownership.
mod aligned;
mod ruled;
mod spans;
mod tagged;

use std::collections::{BTreeMap, BTreeSet};

use docparse_layout::Bbox;
use typed_builder::TypedBuilder;

use super::assemble::LocatedSpan;
use super::{
    MAX_TABLE_CELLS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, Table, TableCell,
    TableRule, TableStructureSource, TaggedTable,
};

/// A candidate grid with exactly one proposed cell for every source slice.
pub(super) struct RecoveredGrid {
    pub table: Table,
    pub assignment: Vec<usize>,
}

/// One physical source row; logical rows may contain several such baselines.
struct PhysicalRow {
    baseline: f64,
    spans: Vec<usize>,
}

/// Page-local geometry and row evidence used by all reconstruction strategies.
#[derive(TypedBuilder)]
pub(super) struct TableGrid<'a> {
    bounds: Bbox,
    spans: &'a [LocatedSpan<'a>],
    rows: Vec<PhysicalRow>,
    font_size: f64,
}

impl<'a> TableGrid<'a> {
    /// Combines already established line baselines across columns without losing word geometry.
    #[allow(
        clippy::indexing_slicing,
        reason = "indices are generated from the immutable source slice"
    )]
    pub fn new(bounds: Bbox, spans: &'a [LocatedSpan<'a>]) -> Self {
        let mut sizes: Vec<_> =
            spans.iter().map(LocatedSpan::font_size).collect();
        sizes.sort_by(f64::total_cmp);
        let font_size = sizes.get(sizes.len() / 2).copied().unwrap_or(10.0);
        let mut indices: Vec<_> = (0..spans.len()).collect();
        indices.sort_by(|&a, &b| {
            spans[a]
                .baseline
                .total_cmp(&spans[b].baseline)
                .then_with(|| {
                    spans[a].span.bbox.left.total_cmp(&spans[b].span.bbox.left)
                })
        });
        let mut rows: Vec<PhysicalRow> = Vec::new();
        for index in indices {
            let baseline = spans[index].baseline;
            if let Some(row) = rows.last_mut()
                && (row.baseline - baseline).abs() <= font_size * 0.6
            {
                row.spans.push(index);
            } else {
                rows.push(PhysicalRow {
                    baseline,
                    spans: vec![index],
                });
            }
        }
        for row in &mut rows {
            row.spans.sort_by(|&a, &b| {
                spans[a].span.bbox.left.total_cmp(&spans[b].span.bbox.left)
            });
        }
        Self::builder()
            .bounds(bounds)
            .spans(spans)
            .rows(rows)
            .font_size(font_size)
            .build()
    }

    /// Copies separators intersecting this parent only, excluding neighboring tables and captions.
    fn local_rules(&self, rules: &[TableRule]) -> Vec<TableRule> {
        rules
            .iter()
            .filter_map(|rule| match *rule {
                TableRule::Horizontal { y, left, right }
                    if y >= self.bounds.top - 1.0
                        && y <= self.bounds.bottom + 1.0
                        && right > self.bounds.left
                        && left < self.bounds.right =>
                {
                    Some(TableRule::Horizontal {
                        y: y.clamp(self.bounds.top, self.bounds.bottom),
                        left: left.max(self.bounds.left),
                        right: right.min(self.bounds.right),
                    })
                }
                TableRule::Vertical { x, top, bottom }
                    if x >= self.bounds.left - 1.0
                        && x <= self.bounds.right + 1.0
                        && bottom > self.bounds.top
                        && top < self.bounds.bottom =>
                {
                    Some(TableRule::Vertical {
                        x: x.clamp(self.bounds.left, self.bounds.right),
                        top: top.max(self.bounds.top),
                        bottom: bottom.min(self.bounds.bottom),
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// Shares bounded paint-coordinate tolerance between snapping, stroke merging, and coverage.
    fn rule_tolerance(&self) -> f64 {
        (self.font_size * 0.15).clamp(0.5, 2.0)
    }

    /// Coalesces paint-width jitter against a fixed cluster anchor instead of chaining distant rules.
    fn snapped(&self, mut positions: Vec<f64>) -> Vec<f64> {
        positions.retain(|value| value.is_finite());
        positions.sort_by(f64::total_cmp);
        let tolerance = self.rule_tolerance();
        positions.dedup_by(|a, b| (*a - *b).abs() <= tolerance);
        positions
    }

    /// Measures union coverage so duplicate path edges cannot fabricate a complete separator.
    fn coverage(
        &self,
        rules: &[TableRule],
        horizontal: bool,
        at: f64,
        from: f64,
        to: f64,
    ) -> f64 {
        // Typeset rules often stop short of a crossing to leave visual padding.
        // Ignore bounded endpoint padding, but still measure missing interior ink;
        // a short header separator must not collapse all header columns into one.
        let padding = (self.font_size * 0.4).min((to - from) * 0.2);
        let from = from + padding;
        let to = to - padding;
        // Coverage must recognize the same near-collinear strokes as grid snapping.
        let tolerance = self.rule_tolerance();
        let mut intervals: Vec<_> = rules
            .iter()
            .filter_map(|rule| match *rule {
                TableRule::Horizontal { y, left, right }
                    if horizontal && (y - at).abs() <= tolerance =>
                {
                    Some((left.max(from), right.min(to)))
                }
                TableRule::Vertical { x, top, bottom }
                    if !horizontal && (x - at).abs() <= tolerance =>
                {
                    Some((top.max(from), bottom.min(to)))
                }
                _ => None,
            })
            .filter(|(a, b)| b > a)
            .collect();
        intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut end = from;
        let mut covered = 0.0;
        for (a, b) in intervals {
            covered += (b - a.max(end)).max(0.0);
            end = end.max(b);
        }
        covered / (to - from).max(f64::EPSILON)
    }

    /// Accepts only a dominant geometric owner and identifies conservative textual header evidence.
    #[allow(
        clippy::indexing_slicing,
        reason = "assignments contain only indices returned by cells.iter().enumerate()"
    )]
    fn assign(&self, mut table: Table) -> Option<RecoveredGrid> {
        let mut assignment = Vec::with_capacity(self.spans.len());
        for span in self.spans {
            let b = span.span.bbox;
            let owner = table
                .cells
                .iter()
                .enumerate()
                .filter_map(|(index, cell)| {
                    let c = cell.bbox?;
                    let area = (b.right.min(c.right) - b.left.max(c.left))
                        .max(0.0)
                        * (b.bottom.min(c.bottom) - b.top.max(c.top)).max(0.0);
                    (area / b.area().max(f64::EPSILON) >= 0.8).then_some(index)
                })
                .collect::<Vec<_>>();
            if owner.len() != 1 {
                tracing::debug!(
                    "table {:?} {}x{} rejected source {} range {:?} at {:?}: {} cell owners",
                    table.source,
                    table.row_count,
                    table.column_count,
                    span.item.id.as_str(),
                    span.span.byte_range,
                    span.span.bbox,
                    owner.len()
                );
                return None;
            }
            assignment.push(*owner.first()?);
        }
        // Consecutive bold textual rows may form a multi-level header. Numeric rows
        // cannot become headers merely because Markdown requires a first header row.
        for row in 0..table.row_count {
            let header_words: Vec<_> = self
                .spans
                .iter()
                .zip(&assignment)
                .filter(|(_, cell)| table.cells[**cell].row == row)
                .map(|(span, _)| span)
                .collect();
            let header = !header_words.is_empty()
                && header_words
                    .iter()
                    .all(|span| !span.text().chars().any(|ch| ch.is_numeric()))
                && header_words.iter().filter(|span| span.is_bold()).count()
                    * 2
                    >= header_words.len();
            if !header {
                break;
            }
            for cell in table.cells.iter_mut().filter(|cell| cell.row == row) {
                cell.is_header = true;
            }
        }
        Some(RecoveredGrid { table, assignment })
    }
}
