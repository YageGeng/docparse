//! Table-local structure and text views over exclusively owned canonical source facts.
mod assemble;
mod evidence;
mod grid;
mod render;
mod tsr;
mod validate;
pub use tsr::{
    TableMode, TableOptions, TableStructureEngine, TableStructureError,
    TsrRequestReason, TsrTableInput, TsrTableRequest,
};

use std::ops::Range;

use docparse_layout::Bbox;
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::TextItemId;
pub(crate) use assemble::TableAssembler;
pub use evidence::{
    TableEvidence, TableRule, TableWord, TaggedTable, TaggedTableCell,
};

pub(crate) const MAX_TABLE_CELLS: usize = 4096;
pub(crate) const MAX_TABLE_COLUMNS: usize = 64;
pub(crate) const MAX_TABLE_ROWS: usize = 256;

/// Evidence used to recover a table's row/column topology.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableStructureSource {
    TaggedPdf,
    Ruled,
    TextAlignment,
    ExternalTsr,
}

/// A non-owning slice of one source item; offsets are UTF-8 bytes, not character ordinals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableTextSpan {
    pub text_item_id: TextItemId,
    pub byte_range: Range<usize>,
    pub bbox: Bbox,
}

/// One physical line inside a cell, preserving its source references in reading order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableCellLine {
    pub text: String,
    pub bbox: Bbox,
    pub spans: Vec<TableTextSpan>,
}

/// One logical cell; covered rowspan/colspan positions never become duplicate cells.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct TableCell {
    pub row: usize,
    pub column: usize,
    #[builder(default = 1)]
    pub row_span: usize,
    #[builder(default = 1)]
    pub column_span: usize,
    /// Grid bounds for geometry recovery, measured content bounds for tagged cells, or None for an empty tagged cell.
    #[builder(default)]
    pub bbox: Option<Bbox>,
    #[builder(default)]
    pub is_header: bool,
    #[builder(default)]
    pub text: String,
    #[builder(default)]
    pub lines: Vec<TableCellLine>,
}

/// Structured view of one table block; canonical source text stays in Block.lines.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct Table {
    pub row_count: usize,
    pub column_count: usize,
    pub cells: Vec<TableCell>,
    pub source: TableStructureSource,
}
