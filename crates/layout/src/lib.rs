//! Layout detection interfaces and PP-DocLayoutV3 inference for docparse.

mod engine;
mod error;
mod geometry;
mod model_manifest;
mod pp_doclayout_v3;
mod types;

pub use engine::LayoutEngine;
pub use error::{
    GeometryError, LayoutError, PageImageError, PostprocessError,
    PreprocessError,
};
pub use geometry::{
    AffineTransform, Bbox, PageRotation, PageTransform, PageTransformInput,
    Point, Polygon,
};
pub use model_manifest::{
    ModelManifest, ModelManifestError, PP_DOCLAYOUT_V3_REVISION,
};
pub use pp_doclayout_v3::PpDocLayoutV3Engine;
pub use pp_doclayout_v3::schema::{
    ModelMetadataSchema, ModelSchema, SchemaDimension, TensorSchema,
    inspect_model,
};
pub use types::{
    EngineMetadata, GeometrySource, LayoutDetection, LayoutLabel,
    LayoutRequest, PageImage, PageImageInput, PixelFormat,
};

#[cfg(any(
    all(feature = "cuda", feature = "coreml"),
    all(feature = "cuda", feature = "openvino"),
    all(feature = "coreml", feature = "openvino"),
))]
compile_error!(
    "only one optional ONNX Runtime execution provider may be enabled"
);
