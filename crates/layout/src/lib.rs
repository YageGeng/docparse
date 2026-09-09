//! Layout detection interfaces and PP-DocLayoutV3 inference for docparse.

pub mod timing;
pub mod wasm_compat;
pub use wasm_compat::*;

mod engine;
mod error;
mod geometry;
mod model_manifest;
mod pp_doclayout_v3;
mod types;

pub use engine::LayoutEngine;
pub use error::{
    GeometryError, LayoutError, LayoutLabelIndexError, PageImageError,
    PostprocessError, PreprocessError,
};
pub use geometry::{
    AffineTransform, Bbox, PageRotation, PageTransform, PageTransformInput,
    Point, Polygon, Quad,
};
pub use model_manifest::{
    ModelArtifacts, ModelManifest, ModelManifestError, PP_DOCLAYOUT_V3_REVISION,
};
pub use pp_doclayout_v3::PpDocLayoutV3Engine;
pub use pp_doclayout_v3::schema::{
    ModelMetadataSchema, ModelSchema, SchemaDimension, TensorSchema,
};
pub use types::{
    EngineMetadata, GeometrySource, LayoutDetection, LayoutLabel,
    LayoutRequest, PageImage, PageImageInput, PixelFormat,
};
