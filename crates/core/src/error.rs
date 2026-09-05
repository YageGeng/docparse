/// Errors returned when a canonical result violates an ownership or schema invariant.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ValidationError {
    /// One exact node path contains invalid or conflicting data.
    #[error("invalid result node {path}: {reason}")]
    InvalidNode { path: String, reason: String },
}

/// Errors produced while converting PDFium character facts into text items.
#[derive(Debug, thiserror::Error)]
pub enum ExtractError {
    /// A stable extraction index exceeded the public ID representation.
    #[error("text extraction index exceeds u32")]
    ExtractionIndexOverflow,

    /// A visible PDFium character has no usable finite geometry.
    #[error("character {index} has no valid geometry")]
    MissingCharacterGeometry { index: i32 },

    /// Layout geometry validation rejected extracted PDF coordinates.
    #[error(transparent)]
    Geometry(#[from] docparse_layout::GeometryError),
}

/// Errors produced while freezing document-wide context from page probes.
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    /// A page probe has an invalid page number or finite geometry.
    #[error("invalid page probe {page_number}: {reason}")]
    InvalidProbe { page_number: u32, reason: String },

    /// More than one probe was supplied for the same page.
    #[error("duplicate page probe {page_number}")]
    DuplicatePage { page_number: u32 },

    /// The final probe set does not match the declared page count.
    #[error("page probe count mismatch: expected {expected}, got {actual}")]
    ProbeCount { expected: u32, actual: usize },
}

/// Errors produced while grouping text items into conservative line fragments.
#[derive(Debug, thiserror::Error)]
pub enum LineError {
    /// Metrics cannot be calculated for an empty line fragment.
    #[error("line fragments cannot be empty")]
    EmptyFragment,

    /// Unioned text geometry unexpectedly became invalid.
    #[error(transparent)]
    Geometry(#[from] docparse_layout::GeometryError),
}

/// Errors produced while turning owned fragments into final semantic blocks.
#[derive(Debug, thiserror::Error)]
pub enum SemanticError {
    /// A fallback tree referenced a fragment that was already moved or out of range.
    #[error("fallback region referenced unavailable fragment index {index}")]
    MissingFallbackFragment { index: usize },

    /// Owner-local line reconstruction rejected empty or invalid fragment geometry.
    #[error(transparent)]
    Line(#[from] LineError),

    /// Unioned final content geometry unexpectedly became invalid.
    #[error(transparent)]
    Geometry(#[from] docparse_layout::GeometryError),
}

/// Errors produced when page-local order constraints cannot be resolved safely.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OrderError {
    /// An edge references a node that was never registered.
    #[error("order edge references unknown block {block_id}")]
    UnknownBlock { block_id: String },

    /// Only invariant-strength edges remain inside a contradictory cycle.
    #[error("strong page-order constraints form an irreducible cycle")]
    InternalOrderConflict,

    /// More than one final block uses the same stable identity.
    #[error("duplicate block ID {block_id} during order resolution")]
    DuplicateBlock { block_id: String },

    /// A resolved graph identity could not be recovered from owned blocks.
    #[error("resolved block ID {block_id} is missing from ownership storage")]
    MissingBlock { block_id: String },
}

/// Errors returned by the pure page-local analysis pipeline.
#[derive(Debug, thiserror::Error)]
pub enum PageAnalysisError {
    /// Extracted page dimensions, rotation, number, or context membership are invalid.
    #[error("invalid page analysis input: {reason}")]
    InvalidInput { reason: String },

    /// Native facts changed multiplicity while moving through page ownership containers.
    #[error("native text conservation failed on page {page_number}")]
    NativeTextConservation { page_number: u32 },

    /// Conservative line assembly failed before ownership fusion.
    #[error(transparent)]
    Line(#[from] LineError),

    /// Semantic block construction failed after ownership assignment.
    #[error(transparent)]
    Semantic(#[from] SemanticError),

    /// Ordering constraints could not be resolved safely.
    #[error(transparent)]
    Order(#[from] OrderError),

    /// The completed page violated a canonical result invariant.
    #[error(transparent)]
    Validation(#[from] ValidationError),
}
