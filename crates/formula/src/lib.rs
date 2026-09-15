//! PP-FormulaNet Plus-L: verified model bytes, real batches, and lossless LaTeX decoding.
mod artifacts;
mod model;
mod preprocess;
mod wasm_compat;

pub use artifacts::{
    FormulaArtifacts, MODEL_REVISION, MODEL_SHA256, TOKENIZER_SHA256,
};
pub use model::{FormulaEngine, FormulaError, PpFormulaNetEngine};
