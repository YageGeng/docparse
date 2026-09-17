//! Immutable model identity shared by native files and browser-owned bytes.
use docparse_formula::FormulaError;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Author-published ONNX checkpoint with the transferred 687-token vocabulary.
pub const MODEL_REVISION: &str = "63e04c86fc96c2324811114351eeea8118bf6b28";
/// SHA-256 of the image encoder at the pinned revision.
pub const ENCODER_SHA256: &str =
    "95cccef463e5ed3623282f1541c0011a00b8a5d0828ea2cd57d6953ad4310b5b";
/// SHA-256 of the merged first-step/cached decoder at the pinned revision.
pub const DECODER_SHA256: &str =
    "10be29b751f6de5f9900c3658551020dc865257eb2c3034bc4c1e016e4d0e35d";
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
