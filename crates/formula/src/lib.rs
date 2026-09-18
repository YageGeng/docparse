//! PP-FormulaNet Plus-S/Plus-M/Plus-L: verified model bytes, real batches, and lossless LaTeX decoding.
mod artifacts;
mod model;
mod preprocess;
pub mod queue;
mod wasm_compat;

pub use artifacts::{
    FormulaArtifacts, MODEL_REVISION, MODEL_SHA256, PLUS_L_MODEL_SHA256,
    PLUS_M_MODEL_SHA256, TOKENIZER_SHA256,
};
pub use model::{FormulaEngine, FormulaError, PpFormulaNetEngine};
pub use wasm_compat::spawn_worker;
