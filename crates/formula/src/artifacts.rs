//! Pinned formula artifact identity and verification independent of filesystem access.
use crate::FormulaError;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Fixed graph/tokenizer pair published by OAR-OCR; changing either requires a new parity evaluation.
pub const MODEL_SHA256: &str =
    "b4924d69c731365048de3d11a5d1829f3dfd8b98b4dbfd82437f934c2611934f";
pub const TOKENIZER_SHA256: &str =
    "2811d82701ec97c192fa256aa2b4516929373870ae660326cc5b1dc879b95ff2";
pub const MODEL_REVISION: &str = "7feb044d74be09e3e2078a89cec0f0f8688e942b";

/// Filesystem-independent formula assets supplied explicitly by browser callers.
#[derive(Debug, Clone)]
pub struct FormulaArtifacts {
    pub model: Arc<[u8]>,
    pub tokenizer: Arc<[u8]>,
    pub manifest: Arc<[u8]>,
}

impl FormulaArtifacts {
    /// Rejects mismatched provenance and bytes before ONNX or the tokenizer are initialized.
    pub(crate) fn verify(&self) -> Result<(), FormulaError> {
        let manifest: docparse_layout::ModelManifest =
            serde_json::from_slice(&self.manifest)
                .map_err(|error| FormulaError::Artifacts(error.to_string()))?;
        if manifest.repository != "GreatV/oar-ocr"
            || manifest.revision != MODEL_REVISION
            || manifest.license != "Apache-2.0"
        {
            return Err(FormulaError::Artifacts(
                "unexpected Plus-L model identity".into(),
            ));
        }
        for (name, bytes, expected) in [
            ("inference.onnx", self.model.as_ref(), MODEL_SHA256),
            ("tokenizer.json", self.tokenizer.as_ref(), TOKENIZER_SHA256),
        ] {
            let actual: String = Sha256::digest(bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            if actual != expected
                || manifest.files.get(name).map(String::as_str)
                    != Some(expected)
            {
                return Err(FormulaError::Artifacts(format!(
                    "{name} SHA-256 mismatch"
                )));
            }
        }
        Ok(())
    }
}
