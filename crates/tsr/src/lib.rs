//! Paddle table structure inference over owned RGB crops.
mod model;
mod preprocess;
mod tiles;
mod wasm_compat;

pub use docparse_layout::ModelArtifacts;
pub use model::{
    SLANET_PLUS_REVISION, SlanetPlusEngine, TsrError, TsrPrediction,
};
