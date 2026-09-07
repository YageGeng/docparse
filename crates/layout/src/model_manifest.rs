use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use typed_builder::TypedBuilder;

const PP_DOCLAYOUT_V3_REPOSITORY: &str = "PaddlePaddle/PP-DocLayoutV3_onnx";
pub const PP_DOCLAYOUT_V3_REVISION: &str =
    "46bbdf188bb0a772c08aed74882ce7e51a8f1ea6";
const PP_DOCLAYOUT_V3_LICENSE: &str = "Apache-2.0";
const PP_DOCLAYOUT_V3_MODEL_SHA256: &str =
    "45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba";
const PP_DOCLAYOUT_V3_CONFIG_SHA256: &str =
    "506fcfac13b3b546ae40d7886b44126420f392adb694e3f8bb6a6286a1f90fdc";

/// Owned immutable artifacts shared by native and browser model creation.
#[derive(Debug, Clone)]
pub struct ModelArtifacts {
    pub model: Arc<[u8]>,
    pub config: Arc<[u8]>,
    pub manifest: Arc<[u8]>,
}

impl ModelArtifacts {
    /// Verifies the immutable model identity and the actual bytes before session creation.
    pub fn verify(&self) -> Result<ModelManifest, ModelManifestError> {
        let result = (|| {
            let manifest: ModelManifest =
                serde_json::from_slice(&self.manifest).map_err(|source| {
                    ModelManifestError::ContentParse { source }
                })?;
            let contract = ModelContract::pp_doclayout_v3();
            manifest.verify_identity(&contract)?;
            for (artifact, bytes, expected) in [
                (
                    "inference.onnx",
                    self.model.as_ref(),
                    &contract.model_sha256,
                ),
                (
                    "inference.yml",
                    self.config.as_ref(),
                    &contract.config_sha256,
                ),
            ] {
                let actual = lowercase_hex(Sha256::digest(bytes).as_ref());
                if actual != *expected {
                    return Err(ModelManifestError::ContentHashMismatch {
                        artifact,
                        expected: expected.clone(),
                        actual,
                    });
                }
            }
            Ok(manifest)
        })();
        if let Err(error) = &result {
            tracing::error!("model artifact verification failed: {}", error);
        }
        result
    }
}

/// Fixed identity and artifact hashes expected for one supported model export.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub(crate) struct ModelContract {
    pub(crate) repository: String,
    pub(crate) revision: String,
    pub(crate) license: String,
    pub(crate) model_sha256: String,
    pub(crate) config_sha256: String,
}

impl ModelContract {
    /// Builds the immutable contract for the supported PP-DocLayoutV3 export.
    pub(crate) fn pp_doclayout_v3() -> Self {
        Self::builder()
            .repository(PP_DOCLAYOUT_V3_REPOSITORY.to_owned())
            .revision(PP_DOCLAYOUT_V3_REVISION.to_owned())
            .license(PP_DOCLAYOUT_V3_LICENSE.to_owned())
            .model_sha256(PP_DOCLAYOUT_V3_MODEL_SHA256.to_owned())
            .config_sha256(PP_DOCLAYOUT_V3_CONFIG_SHA256.to_owned())
            .build()
    }
}

/// Reproducible provenance and checksums emitted beside downloaded model files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct ModelManifest {
    pub repository: String,
    pub revision: String,
    pub license: String,
    #[builder(default)]
    pub generated_at: Option<String>,
    pub files: BTreeMap<String, String>,
}

impl ModelManifest {
    /// Verifies manifest provenance and declared hashes before reading large files.
    pub(crate) fn verify_identity(
        &self,
        contract: &ModelContract,
    ) -> Result<(), ModelManifestError> {
        Self::require_equal(
            "repository",
            &contract.repository,
            &self.repository,
        )?;
        Self::require_equal("revision", &contract.revision, &self.revision)?;
        Self::require_equal("license", &contract.license, &self.license)?;
        Self::require_equal(
            "files.inference.onnx",
            &contract.model_sha256,
            self.files
                .get("inference.onnx")
                .map(String::as_str)
                .unwrap_or("<missing>"),
        )?;
        Self::require_equal(
            "files.inference.yml",
            &contract.config_sha256,
            self.files
                .get("inference.yml")
                .map(String::as_str)
                .unwrap_or("<missing>"),
        )?;
        Ok(())
    }

    /// Compares one manifest value while retaining stable mismatch context.
    pub(crate) fn require_equal(
        field: &'static str,
        expected: &str,
        actual: &str,
    ) -> Result<(), ModelManifestError> {
        if actual == expected {
            Ok(())
        } else {
            Err(ModelManifestError::ContractMismatch {
                field,
                expected: expected.to_owned(),
                actual: actual.to_owned(),
            })
        }
    }
}

/// Encodes digest bytes without relying on formatting traits removed by `sha2` 0.11.
pub(crate) fn lowercase_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            let digit = match nibble {
                0..=9 => b'0' + nibble,
                _ => b'a' + (nibble - 10),
            };
            encoded.push(char::from(digit));
        }
    }
    encoded
}

/// Errors produced while loading and validating fixed model artifacts.
#[derive(Debug, thiserror::Error)]
pub enum ModelManifestError {
    /// A native default model path was not resolved before loading.
    #[error("model artifact path must be absolute: {path}")]
    RelativePath { path: PathBuf },
    /// In-memory provenance cannot be decoded.
    #[error("failed to parse in-memory model manifest: {source}")]
    ContentParse {
        #[source]
        source: serde_json::Error,
    },
    /// Content differs from the approved immutable artifact.
    #[error(
        "model artifact {artifact} hash mismatch: expected {expected}, got {actual}"
    )]
    ContentHashMismatch {
        artifact: &'static str,
        expected: String,
        actual: String,
    },
    /// The required provenance manifest is absent.
    #[error("model manifest not found: {path}")]
    ManifestNotFound { path: PathBuf },

    /// A required model or configuration artifact is absent.
    #[error("model artifact not found: {path}")]
    ArtifactNotFound { path: PathBuf },

    /// A model artifact or manifest could not be read.
    #[error("failed to read model artifact {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// The provenance manifest is not valid strict JSON.
    #[error("failed to parse model manifest {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    /// Manifest provenance differs from the compiled model contract.
    #[error(
        "model contract mismatch at {field}: expected {expected}, got {actual}"
    )]
    ContractMismatch {
        field: &'static str,
        expected: String,
        actual: String,
    },

    /// Actual artifact bytes differ from the fixed digest.
    #[error(
        "model artifact hash mismatch for {path}: expected {expected}, got {actual}"
    )]
    ArtifactHashMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },

    /// A reader reported more bytes than the supplied buffer can contain.
    #[error(
        "invalid read length for {path}: reader returned {read} bytes for capacity {capacity}"
    )]
    InvalidReadLength {
        path: PathBuf,
        read: usize,
        capacity: usize,
    },
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use serde_json::json;
    use sha2::{Digest, Sha256};

    use super::{ModelContract, ModelManifest, ModelManifestError};

    struct ModelPaths {
        model: PathBuf,
        config: PathBuf,
        manifest: PathBuf,
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        paths: ModelPaths,
        contract: ModelContract,
    }

    impl Fixture {
        /// Creates deterministic model, config, contract, and manifest files.
        fn new() -> Self {
            let directory = tempfile::tempdir()
                .expect("the fixture directory must be created");
            let paths = ModelPaths {
                model: directory.path().join("inference.onnx"),
                config: directory.path().join("inference.yml"),
                manifest: directory.path().join("model-manifest.json"),
            };
            fs::write(&paths.model, b"model-bytes")
                .expect("the model fixture must be writable");
            fs::write(&paths.config, b"config-bytes")
                .expect("the config fixture must be writable");
            let contract = ModelContract::builder()
                .repository("example/model".to_owned())
                .revision("fixed-revision".to_owned())
                .license("Apache-2.0".to_owned())
                .model_sha256(Self::sha256(b"model-bytes"))
                .config_sha256(Self::sha256(b"config-bytes"))
                .build();
            let fixture = Self {
                _directory: directory,
                paths,
                contract,
            };
            fixture.write_manifest("fixed-revision", None);
            fixture
        }

        /// Computes a lowercase SHA-256 fixture value independently of file verification.
        fn sha256(bytes: &[u8]) -> String {
            let digest = Sha256::digest(bytes);
            super::lowercase_hex(digest.as_ref())
        }

        /// Writes a manifest, optionally replacing the declared model hash.
        fn write_manifest(&self, revision: &str, model_hash: Option<&str>) {
            let manifest = json!({
                "repository": self.contract.repository,
                "revision": revision,
                "license": self.contract.license,
                "generated_at": "2026-09-04T00:00:00Z",
                "files": {
                    "inference.onnx": model_hash.unwrap_or(&self.contract.model_sha256),
                    "inference.yml": self.contract.config_sha256,
                }
            });
            fs::write(
                &self.paths.manifest,
                serde_json::to_vec_pretty(&manifest)
                    .expect("the manifest fixture must serialize"),
            )
            .expect("the manifest fixture must be writable");
        }

        /// Loads the fixture through the private generic contract path.
        fn verify(&self) -> Result<ModelManifest, ModelManifestError> {
            ModelManifest::load_and_verify_contract(
                &self.paths.model,
                &self.paths.config,
                &self.paths.manifest,
                &self.contract,
            )
        }
    }

    /// Verifies a matching manifest and both matching files are accepted.
    #[test]
    fn matching_contract_is_accepted() {
        let fixture = Fixture::new();

        let manifest = fixture.verify().expect("the fixture must verify");

        assert_eq!(manifest.revision, "fixed-revision");
        assert_eq!(
            manifest.generated_at.as_deref(),
            Some("2026-09-04T00:00:00Z")
        );
    }

    /// Verifies a manifest from another model revision is rejected.
    #[test]
    fn wrong_revision_is_rejected() {
        let fixture = Fixture::new();
        fixture.write_manifest("floating-main", None);

        let error = fixture.verify().expect_err("the revision must be fixed");

        assert!(matches!(
            error,
            ModelManifestError::ContractMismatch {
                field: "revision",
                ..
            }
        ));
    }

    /// Verifies a required model artifact cannot be absent.
    #[test]
    fn missing_model_file_is_rejected() {
        let fixture = Fixture::new();
        fs::remove_file(&fixture.paths.model)
            .expect("the model fixture must be removable");

        let error = fixture.verify().expect_err("a missing artifact must fail");

        assert!(matches!(
            error,
            ModelManifestError::ArtifactNotFound { path }
                if path == fixture.paths.model
        ));
    }

    /// Verifies a manifest cannot replace the fixed contract hash.
    #[test]
    fn wrong_declared_hash_is_rejected() {
        let fixture = Fixture::new();
        fixture.write_manifest("fixed-revision", Some(&"0".repeat(64)));

        let error = fixture
            .verify()
            .expect_err("a changed manifest hash must fail");

        assert!(matches!(
            error,
            ModelManifestError::ContractMismatch {
                field: "files.inference.onnx",
                ..
            }
        ));
    }

    /// Verifies modified artifact bytes cannot satisfy an unchanged manifest.
    #[test]
    fn wrong_actual_hash_is_rejected() {
        let fixture = Fixture::new();
        fs::write(&fixture.paths.config, b"tampered")
            .expect("the config fixture must be writable");

        let error = fixture
            .verify()
            .expect_err("modified artifact bytes must fail");

        assert!(matches!(
            error,
            ModelManifestError::ArtifactHashMismatch { path, .. }
                if path == fixture.paths.config
        ));
    }
}
