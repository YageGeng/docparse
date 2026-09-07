//! Native model files, metadata inspection, and blocking task execution.
use super::{TaskError, WasmCompatSend};
use crate::model_manifest::{ModelContract, lowercase_hex};
use crate::{
    LayoutError, ModelArtifacts, ModelManifest, ModelManifestError,
    ModelMetadataSchema, ModelSchema,
};
use ort::session::Session;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::Path;
use std::sync::Arc;

use crate::PpDocLayoutV3Engine;
use docparse_config::ValidatedConfig;

impl PpDocLayoutV3Engine {
    /// Loads the legacy native file configuration through the verified bytes entry point.
    pub async fn from_config(
        config: Arc<ValidatedConfig>,
    ) -> Result<Self, LayoutError> {
        let source = Arc::clone(&config);
        let artifacts = run_cpu(move || {
            ModelArtifacts::from_paths(
                &source.layout().model_path,
                &source.layout().model_config_path,
                &source.layout().model_manifest_path,
            )
        })
        .await
        .map_err(|source| LayoutError::TaskJoin { source })??;
        Self::from_artifacts(config, artifacts).await
    }
}

impl From<tokio::task::JoinError> for TaskError {
    /// Retains the native task failure without exposing Tokio in shared APIs.
    fn from(error: tokio::task::JoinError) -> Self {
        Self(error.to_string())
    }
}

/// Runs owned CPU work away from the native async executor.
pub async fn run_cpu<F, T>(operation: F) -> Result<T, TaskError>
where
    F: FnOnce() -> T + WasmCompatSend + 'static,
    T: WasmCompatSend + 'static,
{
    tokio::task::spawn_blocking(operation)
        .await
        .map_err(TaskError::from)
}

impl ModelArtifacts {
    /// Reads native model artifacts exactly once after checking resolved paths.
    pub fn from_paths(
        model: &Path,
        config: &Path,
        manifest: &Path,
    ) -> Result<Self, ModelManifestError> {
        for path in [model, config, manifest] {
            if !path.is_absolute() {
                tracing::error!(
                    "model artifact path is not absolute: {}",
                    path.display()
                );
                return Err(ModelManifestError::RelativePath {
                    path: path.to_path_buf(),
                });
            }
        }
        let [model, config, manifest] = [model, config, manifest].map(|path| {
            fs::read(path).map(Arc::from).map_err(|source| {
                tracing::error!(
                    "failed to read model artifact {}: {}",
                    path.display(),
                    source
                );
                ModelManifestError::Read {
                    path: path.to_path_buf(),
                    source,
                }
            })
        });
        Ok(Self {
            model: model?,
            config: config?,
            manifest: manifest?,
        })
    }
}
impl ModelManifest {
    /// Loads and verifies the supported PP-DocLayoutV3 model, config, and manifest.
    pub fn load_and_verify(
        model_path: impl AsRef<Path>,
        config_path: impl AsRef<Path>,
        manifest_path: impl AsRef<Path>,
    ) -> Result<Self, ModelManifestError> {
        Self::load_and_verify_contract(
            model_path.as_ref(),
            config_path.as_ref(),
            manifest_path.as_ref(),
            &ModelContract::pp_doclayout_v3(),
        )
    }

    /// Loads and verifies artifacts against an explicit internal contract.
    pub(crate) fn load_and_verify_contract(
        model_path: &Path,
        config_path: &Path,
        manifest_path: &Path,
        contract: &ModelContract,
    ) -> Result<Self, ModelManifestError> {
        if !manifest_path.is_file() {
            return Err(ModelManifestError::ManifestNotFound {
                path: manifest_path.to_path_buf(),
            });
        }
        let bytes = fs::read(manifest_path).map_err(|source| {
            ModelManifestError::Read {
                path: manifest_path.to_path_buf(),
                source,
            }
        })?;
        let manifest: Self =
            serde_json::from_slice(&bytes).map_err(|source| {
                ModelManifestError::Parse {
                    path: manifest_path.to_path_buf(),
                    source,
                }
            })?;

        manifest.verify_identity(contract)?;
        manifest.verify_artifact(
            model_path,
            "inference.onnx",
            &contract.model_sha256,
        )?;
        manifest.verify_artifact(
            config_path,
            "inference.yml",
            &contract.config_sha256,
        )?;
        Ok(manifest)
    }

    /// Verifies one artifact exists and matches both manifest and fixed contract hashes.
    fn verify_artifact(
        &self,
        path: &Path,
        manifest_name: &'static str,
        expected_hash: &str,
    ) -> Result<(), ModelManifestError> {
        if !path.is_file() {
            return Err(ModelManifestError::ArtifactNotFound {
                path: path.to_path_buf(),
            });
        }
        let declared_hash = self
            .files
            .get(manifest_name)
            .map(String::as_str)
            .unwrap_or("<missing>");
        Self::require_equal(manifest_name, expected_hash, declared_hash)?;

        let actual_hash = Self::sha256_file(path)?;
        if actual_hash != expected_hash {
            return Err(ModelManifestError::ArtifactHashMismatch {
                path: path.to_path_buf(),
                expected: expected_hash.to_owned(),
                actual: actual_hash,
            });
        }
        Ok(())
    }

    /// Streams one artifact through SHA-256 without buffering the model in memory.
    fn sha256_file(path: &Path) -> Result<String, ModelManifestError> {
        let file =
            File::open(path).map_err(|source| ModelManifestError::Read {
                path: path.to_path_buf(),
                source,
            })?;
        let mut reader = BufReader::new(file);
        let mut digest = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).map_err(|source| {
                ModelManifestError::Read {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            if read == 0 {
                break;
            }
            let chunk = buffer.get(..read).ok_or_else(|| {
                ModelManifestError::InvalidReadLength {
                    path: path.to_path_buf(),
                    read,
                    capacity: buffer.len(),
                }
            })?;
            digest.update(chunk);
        }
        let digest = digest.finalize();
        Ok(lowercase_hex(digest.as_ref()))
    }
}
/// Loads one ONNX file and returns only neutral schema information.
pub fn inspect_model(
    path: impl AsRef<Path>,
) -> Result<ModelSchema, LayoutError> {
    let path = path.as_ref();
    if !path.is_file() {
        return Err(LayoutError::ModelNotFound {
            path: path.to_path_buf(),
        });
    }
    let session = Session::builder()?.commit_from_file(path)?;
    ModelSchema::from_session(&session)
}

/// Extracts stable model metadata and sorts custom keys.
pub fn model_metadata(
    session: &Session,
) -> Result<ModelMetadataSchema, LayoutError> {
    let metadata = session.metadata()?;
    let mut custom = BTreeMap::new();
    for key in metadata.custom_keys()? {
        if let Some(value) = metadata.custom(&key) {
            custom.insert(key, value);
        }
    }
    Ok(ModelMetadataSchema::builder()
        .name(metadata.name())
        .producer(metadata.producer())
        .domain(metadata.domain())
        .description(metadata.description())
        .graph_description(metadata.graph_description())
        .version(metadata.version())
        .custom(custom)
        .build())
}
