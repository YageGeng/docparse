//! Immutable model identity shared by native files and browser-owned bytes.
use docparse_formula::FormulaError;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Author-published checkpoint with corrected output shapes and the transferred 687-token vocabulary.
pub const MODEL_REVISION: &str = "b2668efe5112082846fde4d446b9bfaab3989533";
/// SHA-256 of the image encoder at the pinned revision.
pub const ENCODER_SHA256: &str =
    "fbd69cf63cf833db1e2ef40013d859b560671c1253278441a01bde4516b624ae";
/// SHA-256 of the merged first-step/cached decoder at the pinned revision.
pub const DECODER_SHA256: &str =
    "61d4e9e60e3caa62af3f28a15a22bc13567eb4e618c87917d9597461e54c46be";
/// SHA-256 of the WordLevel tokenizer matching both graphs.
pub const TOKENIZER_SHA256: &str =
    "1240f9d178e1ad2a0076fe95ba62e332871c702accdd5ce3ae3ef33ffd6c3a1e";

/// Complete filesystem-independent Texo input; hashes are compiled into this crate.
#[derive(Debug, Clone)]
pub struct TexoArtifacts {
    /// Contents of `encoder_model.onnx`.
    pub encoder: Arc<[u8]>,
    /// Contents of `decoder_model_merged.onnx`.
    pub decoder: Arc<[u8]>,
    /// Matching `tokenizer.json`; GGUF/BPE checkpoints are not interchangeable.
    pub tokenizer: Arc<[u8]>,
}

impl TexoArtifacts {
    /// Rejects mixed, truncated, or modified assets before initializing any runtime.
    pub fn verify(&self) -> Result<(), FormulaError> {
        for (name, bytes, expected) in [
            ("encoder_model.onnx", self.encoder.as_ref(), ENCODER_SHA256),
            (
                "decoder_model_merged.onnx",
                self.decoder.as_ref(),
                DECODER_SHA256,
            ),
            ("tokenizer.json", self.tokenizer.as_ref(), TOKENIZER_SHA256),
        ] {
            let actual: String = Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if actual != expected {
                tracing::error!(
                    "Texo {} SHA-256 mismatch: expected {}, got {}",
                    name,
                    expected,
                    actual
                );
                return Err(FormulaError::Artifacts(format!(
                    "Texo {name} SHA-256 mismatch"
                )));
            }
        }
        Ok(())
    }
}
