//! Table-local structure and text views over exclusively owned canonical source facts.
//! API metadata is derived from these same serialized table types.
mod assemble;
mod evidence;
mod grid;
mod render;
mod tsr;
mod validate;
pub use tsr::{
    TableMode, TableOptions, TableStructureEngine, TableStructureError,
    TsrGeometryPolicy, TsrRequestReason, TsrTableInput, TsrTableRequest,
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
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TableStructureSource {
    /// Local reconstruction from explicit PDF table tags.
    TaggedPdf,
    /// Local rules using painted separators, including supported geometric refinement.
    Ruled,
    /// Local rules using aligned source text and whitespace.
    TextAlignment,
    /// Topology supplied by a TSR model or caller, then filled with canonical source text.
    ExternalTsr,
}

/// A non-owning slice of one source item; offsets are UTF-8 bytes, not character ordinals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TableTextSpan {
    pub text_item_id: TextItemId,
    #[schema(schema_with = byte_range_schema)]
    pub byte_range: Range<usize>,
    pub bbox: Bbox,
}

/// One physical line inside a cell, preserving its source references in reading order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TableCellLine {
    pub text: String,
    pub bbox: Bbox,
    pub spans: Vec<TableTextSpan>,
}

/// One logical cell; covered rowspan/colspan positions never become duplicate cells.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
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
    /// Formula-enriched presentation; canonical text and source spans remain unchanged.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[builder(default)]
    pub lines: Vec<TableCellLine>,
}

/// Structured view of one table block; canonical source text stays in Block.lines.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct Table {
    pub row_count: usize,
    pub column_count: usize,
    pub cells: Vec<TableCell>,
    /// Records the accepted reconstruction path, independently of layout detection provenance.
    pub source: TableStructureSource,
}

/// Describes Serde's foreign Range type as the actual half-open UTF-8 byte-offset object.
fn byte_range_schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema>
{
    utoipa::openapi::schema::ObjectBuilder::new()
        .property("start", utoipa::schema!(usize))
        .property("end", utoipa::schema!(usize))
        .required("start")
        .required("end")
        .into()
}
