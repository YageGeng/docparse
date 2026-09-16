//! Pinned formula artifact identity and verification independent of filesystem access.
use crate::FormulaError;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Default Plus-S graph published by OAR-OCR; changing it requires a new parity evaluation.
pub const MODEL_SHA256: &str =
    "449d205c8fb2fe0a9b134a5e4a0f2421c2e7812fd902ea67dfda4e9ef4588978";
/// Optional Plus-M graph with the same 384-pixel input as Plus-S.
pub const PLUS_M_MODEL_SHA256: &str =
    "9e3539c2b4eeed28f2d35e342fd5bb0bdaa7f6034a475fc7e890c92780910618";
/// Optional Plus-L graph retained for callers selecting the larger model explicitly.
pub const PLUS_L_MODEL_SHA256: &str =
    "b4924d69c731365048de3d11a5d1829f3dfd8b98b4dbfd82437f934c2611934f";
pub const TOKENIZER_SHA256: &str =
    "2811d82701ec97c192fa256aa2b4516929373870ae660326cc5b1dc879b95ff2";
pub const MODEL_REVISION: &str = "7feb044d74be09e3e2078a89cec0f0f8688e942b";

/// Verified graph identity determines preprocessing and the reported engine name.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ModelKind {
    PlusS,
    PlusM,
    PlusL,
}

impl ModelKind {
    /// Identifies the selected artifact independently of its caller-supplied path.
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PlusS => "pp-formulanet-plus-s",
            Self::PlusM => "pp-formulanet-plus-m",
            Self::PlusL => "pp-formulanet-plus-l",
        }
    }
}

/// Filesystem-independent formula assets supplied explicitly by browser callers.
#[derive(Debug, Clone)]
pub struct FormulaArtifacts {
    pub model: Arc<[u8]>,
    pub tokenizer: Arc<[u8]>,
    pub manifest: Arc<[u8]>,
}

impl FormulaArtifacts {
    /// Rejects mismatched provenance and bytes before ONNX or the tokenizer are initialized.
    pub(crate) fn verify(&self) -> Result<ModelKind, FormulaError> {
        let manifest: docparse_layout::ModelManifest =
            serde_json::from_slice(&self.manifest)
                .map_err(|error| FormulaError::Artifacts(error.to_string()))?;
        if manifest.repository != "GreatV/oar-ocr"
            || manifest.revision != MODEL_REVISION
            || manifest.license != "Apache-2.0"
        {
            return Err(FormulaError::Artifacts(
                "unexpected PP-FormulaNet model identity".into(),
            ));
        }
        let (kind, model_hash) =
            match manifest.files.get("inference.onnx").map(String::as_str) {
                Some(MODEL_SHA256) => (ModelKind::PlusS, MODEL_SHA256),
                Some(PLUS_M_MODEL_SHA256) => {
                    (ModelKind::PlusM, PLUS_M_MODEL_SHA256)
                }
                Some(PLUS_L_MODEL_SHA256) => {
                    (ModelKind::PlusL, PLUS_L_MODEL_SHA256)
                }
                _ => {
                    return Err(FormulaError::Artifacts(
                        "unsupported formula graph identity".into(),
                    ));
                }
            };
        for (name, bytes, expected) in [
            ("inference.onnx", self.model.as_ref(), model_hash),
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
        Ok(kind)
    }
}
