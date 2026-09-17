//! Texo's pinned 687-token model with batched, cached ONNX decoding.
mod artifacts;
mod model;
mod preprocess;
mod resample;
mod wasm_compat;

pub use artifacts::{
    DECODER_SHA256, ENCODER_SHA256, MODEL_REVISION, TOKENIZER_SHA256,
    TexoArtifacts,
};
pub use model::TexoEngine;
