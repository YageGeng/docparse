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
    // A span enters its own subscriber but does not make that subscriber the thread's default dispatcher.
    let span = tracing::Span::current();
    let dispatcher = tracing::dispatcher::get_default(Clone::clone);
    tokio::task::spawn_blocking(move || {
        tracing::dispatcher::with_default(&dispatcher, || {
            span.in_scope(operation)
        })
    })
    .await
    .map_err(TaskError::from)
}

type SessionOperation<S> = Box<dyn FnOnce(&mut S) + Send>;

/// Keeps model initialization, execution, and destruction on one bounded native thread.
pub struct SessionWorker<S> {
    sender: tokio::sync::mpsc::Sender<SessionOperation<S>>,
}

impl<S: 'static> SessionWorker<S> {
    /// Initializes a session on its owning thread so CUDA per-thread handles are reused across every request.
    pub async fn new<F, E>(initialize: F) -> Result<Self, E>
    where
        F: FnOnce() -> Result<S, E> + Send + 'static,
        E: From<TaskError> + Send + 'static,
    {
        let (sender, mut requests) =
            tokio::sync::mpsc::channel::<SessionOperation<S>>(1);
        let (ready, initialized) = tokio::sync::oneshot::channel();
        // Initialization may use a local subscriber, but its context must end before this shared actor starts serving calls.
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        let initialize = move || {
            tracing::dispatcher::with_default(&dispatcher, || {
                span.in_scope(initialize)
            })
        };
        std::thread::Builder::new()
            .name("docparse-onnx".into())
            .spawn(move || {
                let mut session = match initialize() {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                while let Some(operation) = requests.blocking_recv() {
                    operation(&mut session);
                }
            })
            .map_err(|error| TaskError::from_message(error.to_string()))?;
        // Transport and initialization failures converge through the caller's existing error conversion.
        initialized.await.map_err(|_closed| {
            TaskError::from_message("model initialization thread stopped")
        })??;
        Ok(Self { sender })
    }

    /// Owns each input closure through actual execution, skipping queued work whose caller has already cancelled.
    pub async fn run<F, R>(&self, operation: F) -> Result<R, TaskError>
    where
        F: FnOnce(&mut S) -> R + Send + 'static,
        R: Send + 'static,
    {
        let (response, result) = tokio::sync::oneshot::channel();
        // Capture each invocation separately because the same session thread serves different PDFs.
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        self.sender
            .send(Box::new(move |session| {
                tracing::dispatcher::with_default(&dispatcher, || {
                    span.in_scope(|| {
                        if response.is_closed() {
                            return;
                        }
                        let value = operation(session);
                        let _ = response.send(value);
                    })
                })
            }))
            .await
            .map_err(|_closed| {
                TaskError::from_message("model execution thread stopped")
            })?;
        result.await.map_err(|_closed| {
            TaskError::from_message("model execution response lost")
        })
    }
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

impl TryFrom<&docparse_config::ModelFiles> for ModelArtifacts {
    type Error = ModelManifestError;

    /// Loads explicit model/config/manifest paths without reconstructing filenames from a directory.
    fn try_from(
        files: &docparse_config::ModelFiles,
    ) -> Result<Self, Self::Error> {
        Self::from_paths(
            &files.model_path,
            &files.model_config_path,
            &files.model_manifest_path,
        )
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

#[cfg(test)]
mod tests {
    use super::{SessionWorker, TaskError};
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    /// Flattening initialization errors must retain the concrete model failure rather than reclassifying it as a thread failure.
    #[tokio::test]
    async fn session_initialization_preserves_model_error() {
        let result = SessionWorker::<()>::new(|| {
            Err(crate::LayoutError::Engine {
                message: "model initialization rejected".into(),
            })
        })
        .await;
        assert!(
            matches!(result, Err(crate::LayoutError::Engine { message }) if message == "model initialization rejected")
        );
    }

    /// Multiple async callers must execute on the same thread that initialized the session.
    #[tokio::test]
    async fn session_worker_preserves_thread_affinity() {
        let worker = Arc::new(
            SessionWorker::new(|| {
                Ok::<_, TaskError>(std::thread::current().id())
            })
            .await
            .expect("session"),
        );
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..32 {
            let worker = Arc::clone(&worker);
            tasks.spawn(async move {
                worker
                    .run(|owner| (*owner, std::thread::current().id()))
                    .await
            });
        }
        while let Some(result) = tasks.join_next().await {
            let (initialized, executing) =
                result.expect("join").expect("execution");
            assert_eq!(initialized, executing);
        }
    }

    /// Tracks real actor-state destruction without adding any test-only production fields.
    struct Lifetime {
        alive: Arc<AtomicBool>,
        dropped: Option<tokio::sync::oneshot::Sender<()>>,
    }
    impl Drop for Lifetime {
        /// Signals only after the in-flight operation and its session have actually finished.
        fn drop(&mut self) {
            self.alive.store(false, Ordering::SeqCst);
            if let Some(dropped) = self.dropped.take() {
                let _ = dropped.send(());
            }
        }
    }

    /// Cancelling a caller must not destroy a session while native inference is still using it.
    #[tokio::test]
    async fn cancellation_retains_running_session_until_completion() {
        let alive = Arc::new(AtomicBool::new(true));
        let state_alive = Arc::clone(&alive);
        let (dropped, destroyed) = tokio::sync::oneshot::channel();
        let worker = Arc::new(
            SessionWorker::new(move || {
                Ok::<_, TaskError>(Lifetime {
                    alive: state_alive,
                    dropped: Some(dropped),
                })
            })
            .await
            .expect("session"),
        );
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let caller = Arc::clone(&worker);
        let task = tokio::spawn(async move {
            caller
                .run(move |_state| {
                    let _ = started.send(());
                    blocked.recv().expect("release native operation");
                })
                .await
        });
        entered.await.expect("operation started");
        // A queued cancelled request must be discarded before invoking its captured input closure.
        let queued_ran = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&queued_ran);
        let queued_worker = Arc::clone(&worker);
        let queued = tokio::spawn(async move {
            queued_worker
                .run(move |_state| flag.store(true, Ordering::SeqCst))
                .await
        });
        while worker.sender.capacity() != 0 {
            tokio::task::yield_now().await;
        }
        queued.abort();
        assert!(
            queued
                .await
                .expect_err("queued caller cancelled")
                .is_cancelled()
        );
        task.abort();
        assert!(task.await.expect_err("caller cancelled").is_cancelled());
        drop(worker);
        assert!(alive.load(Ordering::SeqCst));
        release.send(()).expect("unblock native operation");
        destroyed.await.expect("session eventually dropped");
        assert!(!alive.load(Ordering::SeqCst));
        assert!(!queued_ran.load(Ordering::SeqCst));
    }
}
