//! Paddle table structure inference over owned RGB crops.
mod artifacts;
mod model;
mod preprocess;
mod tatr;
mod tiles;
mod wasm_compat;

pub use artifacts::TsrArtifacts;
pub use docparse_layout::ModelArtifacts;
// Keep historical names while exposing a model-neutral entry point for TATR callers.
pub use model::SlanetPlusEngine as TsrEngine;
pub use model::SlanetPlusEngine as PaddleTsrEngine;
pub use model::{
    SLANET_PLUS_REVISION, SlanetPlusEngine, TsrError, TsrPrediction,
};
