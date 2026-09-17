//! Caller-supplied table topology with no detector or model implementation.
mod builtin;
mod decode;
mod detection;

use std::sync::Arc;

use docparse_layout::{AffineTransform, Bbox, PageImage};
use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::BlockId;

pub use docparse_config::TableMode;

/// Per-parse table limits; the default tries local rules before the configured TSR engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(default, deny_unknown_fields)]
pub struct TableOptions {
    #[builder(default)]
    pub mode: TableMode,
    /// Bounds in-flight table requests per parse, including waits for shared model sessions.
    #[builder(default = 2)]
    pub table_jobs: usize,
    #[builder(default = 60_000)]
    pub timeout_ms: u64,
}

impl Default for TableOptions {
    /// Tries local rules before TSR unless the caller explicitly selects another mode.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl TableOptions {
    /// Validates limits and provider availability before allocating parse resources.
    pub(crate) fn validate(
        &self,
        has_engine: bool,
    ) -> Result<(), TableStructureError> {
        if self.table_jobs == 0
            || self.table_jobs > 32
            || self.timeout_ms == 0
            || self.timeout_ms > 86_400_000
        {
            return Err(TableStructureError::InvalidOptions {
                reason: "table_jobs must be 1..=32 and timeout_ms 1..=86400000"
                    .to_owned(),
            });
        }
        if self.mode != TableMode::RulesOnly && !has_engine {
            return Err(TableStructureError::InvalidOptions {
                reason: "external table mode requires a table structure engine"
                    .to_owned(),
            });
        }
        Ok(())
    }
}

/// Why the existing layout table is being sent to an external structure engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TsrRequestReason {
    RulesFailed { message: String },
    TsrOnly,
}

/// Owned crop facts for exactly one layout table; external engines cannot retarget it.
#[derive(Debug, Clone, TypedBuilder)]
pub struct TsrTableRequest {
    pub request_id: String,
    /// One-based DocParse page number.
    pub page_number: u32,
    pub block_id: BlockId,
    /// Crop bounds in canonical viewport points, with a top-left origin.
    pub crop_bbox: Bbox,
    pub image: Arc<PageImage>,
    /// Converts original crop pixels to canonical viewport points, including rounding and scale limits.
    pub crop_to_viewport: AffineTransform,
    pub reason: TsrRequestReason,
    /// Stage observations remain local to the parser and are not serialized to external providers.
    #[builder(default)]
    pub timings: docparse_layout::timing::Timings,
}

/// External structure tokens and one crop-pixel box for each cell opening tag.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct TsrTableInput {
    pub request_id: String,
    pub structure_tokens: Vec<String>,
    /// Either [left, top, right, bottom] or four perimeter vertices (eight numbers).
    pub cell_bboxes: Vec<Vec<f64>>,
    /// Independent crop-pixel detections; count and ordering are unrelated to structure tokens.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[builder(default)]
    pub detected_cell_bboxes: Vec<Vec<f64>>,
}

/// Stable failure categories at the external structure boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TableStructureError {
    #[error("invalid table options: {reason}")]
    InvalidOptions { reason: String },
    #[error("invalid external table structure: {reason}")]
    InvalidInput { reason: String },
    #[error("external table engine failed: {message}")]
    Engine { message: String },
    #[error("table source text could not be assigned: {message}")]
    SourceText { message: String },
    #[error("external table request exceeded {timeout_ms} ms")]
    Timeout { timeout_ms: u64 },
}

impl TableStructureError {
    /// Stable machine-readable diagnostic code shared by native and browser callers.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidOptions { .. } => "InvalidTableOptions",
            Self::InvalidInput { .. } => "InvalidTsrInput",
            Self::Engine { .. } => "TableExternalFailed",
            Self::SourceText { .. } => "TableTextAssignmentFailed",
            Self::Timeout { .. } => "TableExternalTimeout",
        }
    }
}

/// Distinguishes authoritative caller structures from model predictions requiring source calibration.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TsrGeometryPolicy {
    /// Preserves declared topology, header decisions, and geometry under strict validation.
    #[default]
    Declared,
    /// Allows source-supported correction of learned topology, headers, and positions.
    Predicted,
}

/// Async extension point; callers supply service/model adapters and cancellation-aware futures.
pub trait TableStructureEngine:
    crate::wasm_compat::WasmCompatSend + crate::wasm_compat::WasmCompatSync
{
    /// Identifies the engine in diagnostics without exposing service credentials.
    fn name(&self) -> &str;

    /// Declared structures remain exact; model adapters may opt into validated source-supported refinement.
    fn geometry_policy(&self) -> TsrGeometryPolicy {
        TsrGeometryPolicy::Declared
    }

    /// Recognizes structure within the supplied crop, returning boxes in its original pixel coordinates.
    fn recognize(
        &self,
        request: TsrTableRequest,
    ) -> crate::wasm_compat::WasmBoxedFuture<
        '_,
        Result<TsrTableInput, TableStructureError>,
    >;
}

impl From<&docparse_config::TsrConfig> for TableOptions {
    /// Resolves the parser instance policy into one call's bounded table stage.
    fn from(config: &docparse_config::TsrConfig) -> Self {
        Self::builder()
            .mode(config.mode)
            .table_jobs(config.table_jobs)
            .timeout_ms(config.timeout_ms)
            .build()
    }
}
