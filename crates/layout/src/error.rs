/// Errors produced by page image construction or validation.
#[derive(Debug, thiserror::Error)]
pub enum PageImageError {
    /// One image dimension cannot be represented by the host address space.
    #[error("page image dimension {field} cannot be represented: {source}")]
    DimensionConversion {
        field: &'static str,
        #[source]
        source: std::num::TryFromIntError,
    },

    /// Width, height, and channel multiplication overflowed `usize`.
    #[error("page image dimensions overflow addressable memory")]
    ArithmeticOverflow,

    /// The supplied byte buffer does not match the declared image shape.
    #[error(
        "page image buffer length mismatch: expected {expected}, got {actual}"
    )]
    BufferLength { expected: usize, actual: usize },
}

/// Errors produced by validated geometry construction and transforms.
#[derive(Debug, thiserror::Error)]
pub enum GeometryError {
    /// A geometry field contains NaN or infinity.
    #[error("geometry field {field} must be finite")]
    NonFinite { field: &'static str },

    /// A width, height, or pixel dimension is zero or negative.
    #[error("geometry dimension {field} must be greater than zero")]
    InvalidDimension { field: &'static str },

    /// A bounding box has no positive area.
    #[error("bounding box must have positive width and height")]
    DegenerateBbox,

    /// An affine transform cannot be inverted.
    #[error("page-to-viewport affine transform is not invertible")]
    NonInvertibleTransform,

    /// A polygon fails finite, topology, or area validation.
    #[error("invalid polygon: {reason}")]
    InvalidPolygon { reason: String },
}

/// A numeric index outside the fixed PP-DocLayoutV3 label set.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("layout label index {index} is outside the fixed model label set")]
pub struct LayoutLabelIndexError {
    /// The original numeric input, preserved across signed and unsigned conversions.
    pub index: String,
}

/// Errors returned by layout engines.
#[derive(Debug, thiserror::Error)]
pub enum LayoutError {
    /// A browser engine was requested without owned model artifacts.
    #[error("browser model initialization requires explicit ModelArtifacts")]
    ModelArtifactsRequired,
    /// The in-memory YAML artifact could not be decoded.
    #[error("failed to parse model configuration bytes: {source}")]
    ModelConfigContentParse {
        #[source]
        source: Box<serde_yml::Error>,
    },
    /// A configured ONNX model path does not exist.
    #[error("layout model not found: {path}")]
    ModelNotFound { path: std::path::PathBuf },

    /// The model input/output contract is not the supported fixed schema.
    #[error("unsupported PP-DocLayoutV3 model schema: {reason}")]
    UnsupportedModelSchema { reason: String },

    /// The fixed inference YAML file could not be read.
    #[error("failed to read model configuration {path}: {source}")]
    ModelConfigRead {
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The fixed inference YAML file could not be decoded.
    #[error("failed to parse model configuration {path}: {source}")]
    ModelConfigParse {
        path: std::path::PathBuf,
        #[source]
        source: Box<serde_yml::Error>,
    },

    /// The decoded inference YAML differs from the supported preprocessing contract.
    #[error("unsupported PP-DocLayoutV3 model configuration: {reason}")]
    UnsupportedModelConfig { reason: String },

    /// Configuration requests an execution provider not compiled into this build.
    #[error("execution provider '{provider}' is not enabled in this build")]
    ExecutionProviderUnavailable { provider: &'static str },

    /// ONNX Runtime failed while loading, inspecting, or running a model.
    #[error("ONNX Runtime failed: {source}")]
    Ort {
        #[source]
        source: Box<ort::Error>,
    },

    /// A required named ONNX output is absent.
    #[error("ONNX Runtime did not return required output '{name}'")]
    MissingOutput { name: &'static str },

    /// A runtime output shape or value differs from the inspected schema.
    #[error("invalid ONNX output '{name}': {reason}")]
    InvalidOutput { name: &'static str, reason: String },

    /// Session pool state became unavailable or inconsistent.
    #[error("layout session pool failed: {message}")]
    SessionPool { message: String },

    /// A blocking inference task panicked or was cancelled.
    #[error("layout blocking task failed: {source}")]
    TaskJoin {
        #[source]
        source: crate::wasm_compat::TaskError,
    },

    /// Input geometry violates the public layout contract.
    #[error(transparent)]
    Geometry(#[from] GeometryError),

    /// Input image shape and byte storage disagree.
    #[error(transparent)]
    PageImage(#[from] PageImageError),

    /// Fixed model preprocessing could not construct valid tensor inputs.
    #[error(transparent)]
    Preprocess(#[from] PreprocessError),

    /// Fixed model output rows violate the supported postprocess schema.
    #[error(transparent)]
    Postprocess(#[from] PostprocessError),

    /// Fixed model provenance or artifact bytes are invalid.
    #[error(transparent)]
    ModelManifest(#[from] crate::ModelManifestError),

    /// A concrete engine failed without a more specific public category.
    #[error("layout engine failed: {message}")]
    Engine { message: String },
}

impl From<ort::Error> for LayoutError {
    /// Boxes the comparatively large ORT error at the crate boundary.
    fn from(source: ort::Error) -> Self {
        Self::Ort {
            source: Box::new(source),
        }
    }
}

/// Errors produced while resizing and arranging model input tensors.
#[derive(Debug, thiserror::Error)]
pub enum PreprocessError {
    /// The shared image dimensions differ from the page transform render dimensions.
    #[error(
        "page image {image_width}x{image_height} does not match transform render size {render_width}x{render_height}"
    )]
    ImageTransformMismatch {
        image_width: u32,
        image_height: u32,
        render_width: u32,
        render_height: u32,
    },

    /// An image or tensor buffer size overflowed addressable memory.
    #[error("preprocessing dimensions overflow addressable memory")]
    ArithmeticOverflow,

    /// A checked source or destination pixel index was unexpectedly unavailable.
    #[error(
        "preprocessing pixel index {index} is outside buffer length {length}"
    )]
    PixelIndex { index: usize, length: usize },

    /// A clamped interpolation accumulator could not convert to RGB8.
    #[error(
        "interpolated pixel value {value} cannot be represented as RGB8: {source}"
    )]
    PixelConversion {
        value: i64,
        #[source]
        source: std::num::TryFromIntError,
    },

    /// ndarray rejected the validated tensor shape and data length.
    #[error("failed to construct model tensor: {source}")]
    TensorShape {
        #[source]
        source: ndarray::ShapeError,
    },
}

/// Errors produced before or while decoding exported bbox rows.
#[derive(Debug, thiserror::Error)]
pub enum PostprocessError {
    /// The configured score threshold is not finite or in the unit interval.
    #[error("postprocess threshold must be finite and within [0, 1]")]
    InvalidThreshold,

    /// The exported bbox tensor does not have seven columns.
    #[error("bbox output column mismatch: expected 7, got {actual}")]
    InvalidColumns { actual: usize },

    /// `bbox_num` cannot select a valid prefix of the exported rows.
    #[error("bbox count {count} is invalid for {rows} rows")]
    InvalidCount { count: i32, rows: usize },

    /// A row view unexpectedly has no contiguous seven-value representation.
    #[error("bbox row {index} is not a contiguous seven-value row")]
    InvalidRow { index: usize },

    /// A stable source row index cannot fit the public identifier width.
    #[error("bbox source index {index} exceeds u32")]
    SourceIndexOverflow { index: usize },

    /// A surviving box could not convert to canonical geometry.
    #[error(transparent)]
    Geometry(#[from] GeometryError),
}
