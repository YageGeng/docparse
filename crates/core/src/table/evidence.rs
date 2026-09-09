use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use docparse_layout::Bbox;
use pdfium::{
    Page, RectF, SegmentKind, StructureAttributeValue, StructureElement,
};
use typed_builder::TypedBuilder;

use super::{MAX_TABLE_CELLS, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS};
use crate::{Baseline, TextItemId};

/// Compact measured word geometry retained only until the page's tables are assembled.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct TableWord {
    pub byte_range: Range<usize>,
    pub bbox: Bbox,
    #[builder(default)]
    pub baseline: Option<Baseline>,
    #[builder(default)]
    pub mcid: Option<i32>,
}

/// A visible axis-aligned separator in canonical viewport points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TableRule {
    Horizontal { y: f64, left: f64, right: f64 },
    Vertical { x: f64, top: f64, bottom: f64 },
}

impl TableRule {
    /// Rejects curves and diagonal decorations while retaining finite straight separators.
    fn segment(a: (f64, f64), b: (f64, f64)) -> Option<Self> {
        if ![a.0, a.1, b.0, b.1].iter().all(|value| value.is_finite()) {
            return None;
        }
        if (a.1 - b.1).abs() <= 0.5 && (a.0 - b.0).abs() >= 3.0 {
            Some(Self::Horizontal {
                y: (a.1 + b.1) * 0.5,
                left: a.0.min(b.0),
                right: a.0.max(b.0),
            })
        } else if (a.0 - b.0).abs() <= 0.5 && (a.1 - b.1).abs() >= 3.0 {
            Some(Self::Vertical {
                x: (a.0 + b.0) * 0.5,
                top: a.1.min(b.1),
                bottom: a.1.max(b.1),
            })
        } else {
            None
        }
    }
}

/// One explicitly tagged cell; marked-content IDs refer to the source page's text objects.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct TaggedTableCell {
    pub row: usize,
    pub column: usize,
    pub row_span: usize,
    pub column_span: usize,
    pub is_header: bool,
    pub mcids: BTreeSet<i32>,
}

/// A complete, bounded tagged grid before MCIDs are matched to a detected layout.
#[derive(Debug, Clone, PartialEq)]
pub struct TaggedTable {
    pub row_count: usize,
    pub column_count: usize,
    pub cells: Vec<TaggedTableCell>,
}

impl TaggedTable {
    /// Honors explicit row/column spans and rejects ambiguous or excessively large structures.
    #[allow(
        clippy::cast_sign_loss,
        reason = "span values are checked as finite positive integers before conversion"
    )]
    fn from_element(root: &StructureElement) -> Option<Self> {
        let mut stack: Vec<_> = root.children.iter().rev().collect();
        let mut rows = Vec::new();
        while let Some(node) = stack.pop() {
            match node.element_type.as_str() {
                "TR" => rows.push(node),
                "Table" => return None,
                _ => stack.extend(node.children.iter().rev()),
            }
        }
        if rows.is_empty() || rows.len() > MAX_TABLE_ROWS {
            return None;
        }
        let mut cells = Vec::new();
        let mut occupied = BTreeSet::new();
        let mut row_count = rows.len();
        let mut column_count = 0;
        for (row, element) in rows.into_iter().enumerate() {
            let mut column = 0_usize;
            for cell in element.children.iter().filter(|child| {
                matches!(child.element_type.as_str(), "TD" | "TH")
            }) {
                while occupied.contains(&(row, column)) {
                    column += 1;
                }
                let span = |name: &str| match cell.attributes.get(name) {
                    None => Some(1),
                    Some(StructureAttributeValue::Number(value))
                        if value.is_finite()
                            && *value >= 1.0
                            && value.fract() == 0.0
                            && *value <= MAX_TABLE_ROWS as f32 =>
                    {
                        Some(*value as usize)
                    }
                    _ => None,
                };
                let row_span = span("RowSpan")?;
                let column_span = span("ColSpan")?;
                let last_row = row.checked_add(row_span)?;
                let last_column = column.checked_add(column_span)?;
                if last_row > MAX_TABLE_ROWS
                    || last_column > MAX_TABLE_COLUMNS
                    || last_row * last_column > MAX_TABLE_CELLS
                {
                    return None;
                }
                for r in row..last_row {
                    for c in column..last_column {
                        if !occupied.insert((r, c)) {
                            return None;
                        }
                    }
                }
                let mut descendants = vec![cell];
                let mut mcids = BTreeSet::new();
                while let Some(node) = descendants.pop() {
                    if node.element_type == "Table" {
                        return None;
                    }
                    mcids.extend(node.marked_content_ids.iter().copied());
                    descendants.extend(&node.children);
                }
                cells.push(
                    TaggedTableCell::builder()
                        .row(row)
                        .column(column)
                        .row_span(row_span)
                        .column_span(column_span)
                        .is_header(cell.element_type == "TH")
                        .mcids(mcids)
                        .build(),
                );
                row_count = row_count.max(last_row);
                column_count = column_count.max(last_column);
                column = last_column;
            }
        }
        (!cells.is_empty() && row_count * column_count <= MAX_TABLE_CELLS)
            .then_some(Self {
                row_count,
                column_count,
                cells,
            })
    }
}

/// Transient evidence shared with pure table reconstruction; no live PDFium handles escape.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TableEvidence {
    pub words: BTreeMap<TextItemId, Vec<TableWord>>,
    pub rules: Vec<TableRule>,
    pub tagged_tables: Vec<TaggedTable>,
}

impl TableEvidence {
    /// Copies only table-relevant path/structure facts while PDFium is already scanning the page.
    pub(crate) fn read_geometry(
        &mut self,
        page: &Page<'_, '_>,
        view_box: &RectF,
    ) {
        for path in page.path_objects(view_box) {
            // Thin fills are a common representation of booktabs rules; broad fills are backgrounds.
            if path.is_filled
                && path.fill_color.is_some_and(|color| color.a > 0)
            {
                let b = path.bbox;
                let width = f64::from(b.right - b.left);
                let height = f64::from(b.bottom - b.top);
                if height > 0.0 && height <= 2.0 && width >= height * 4.0 {
                    self.rules.push(TableRule::Horizontal {
                        y: f64::from(b.top + b.bottom) * 0.5,
                        left: f64::from(b.left),
                        right: f64::from(b.right),
                    });
                } else if width > 0.0 && width <= 2.0 && height >= width * 4.0 {
                    self.rules.push(TableRule::Vertical {
                        x: f64::from(b.left + b.right) * 0.5,
                        top: f64::from(b.top),
                        bottom: f64::from(b.bottom),
                    });
                }
            }
            if path.is_stroked
                && path.stroke_color.is_some_and(|color| color.a > 0)
            {
                let mut first = None;
                let mut previous = None;
                for segment in path.segments {
                    let point = (f64::from(segment.x), f64::from(segment.y));
                    if segment.kind == SegmentKind::MoveTo {
                        first = Some(point);
                    } else if segment.kind == SegmentKind::LineTo
                        && let Some(a) = previous
                        && let Some(rule) = TableRule::segment(a, point)
                    {
                        self.rules.push(rule);
                    }
                    if segment.close
                        && let Some(start) = first
                        && let Some(rule) = TableRule::segment(point, start)
                    {
                        self.rules.push(rule);
                    }
                    previous = Some(point);
                }
            }
            if self.rules.len() > 8192 {
                // A truncated grid could invent merged cells; discard it and use text evidence instead.
                tracing::warn!(
                    "discarding excessive table rule evidence on a dense graphics page"
                );
                self.rules.clear();
                break;
            }
        }
        let roots = page.structure_tree();
        let mut pending: Vec<_> = roots.iter().rev().collect();
        while let Some(element) = pending.pop() {
            if element.element_type == "Table"
                && let Some(table) = TaggedTable::from_element(element)
            {
                self.tagged_tables.push(table);
            }
            pending.extend(element.children.iter().rev());
        }
    }
}
