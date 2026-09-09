//! Native session leases and the browser inference actor behind one pool interface.
use crate::pp_doclayout_v3::{preprocess::ModelInputs, session::ModelOutputs};
use crate::timing::{TimingStage, Timings};
use crate::{LayoutError, ModelArtifacts, ModelSchema};
use docparse_config::{ExecutionProviderConfig, ValidatedConfig};
use ort::session::{OutputSelector, RunOptions, Session};
use ort::value::TensorRef;
use std::sync::Arc;

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use crate::wasm_compat::run_cpu;
    use ort::session::HasSelectedOutputs;
    use std::sync::Mutex;
    use tokio::sync::{OwnedSemaphorePermit, Semaphore};

    /// One mutable native ORT session held by a unique lease.
    struct LayoutSession {
        session: Session,
        options: RunOptions<HasSelectedOutputs>,
    }

    impl LayoutSession {
        /// Creates one native session from the same bytes that passed artifact verification.
        fn load(
            bytes: &[u8],
            provider: ExecutionProviderConfig,
        ) -> Result<Self, LayoutError> {
            let builder = Session::builder()?;
            let mut builder = match provider {
                ExecutionProviderConfig::Cpu => builder,
                ExecutionProviderConfig::WebGpu => {
                    return Err(LayoutError::ExecutionProviderUnavailable {
                        provider: "webgpu",
                    });
                }
                ExecutionProviderConfig::Cuda => {
                    #[cfg(feature = "cuda")]
                    {
                        builder
                            .with_execution_providers([
                                // A requested accelerator must fail visibly instead of silently using CPU.
                                ort::ep::CUDA::default()
                                    .build()
                                    .error_on_failure(),
                            ])
                            .map_err(|source| {
                                LayoutError::from(ort::Error::from(source))
                            })?
                    }
                    #[cfg(not(feature = "cuda"))]
                    {
                        let _builder = builder;
                        return Err(
                            LayoutError::ExecutionProviderUnavailable {
                                provider: "cuda",
                            },
                        );
                    }
                }
                ExecutionProviderConfig::CoreMl => {
                    #[cfg(feature = "coreml")]
                    {
                        builder
                            .with_execution_providers([
                                // A requested accelerator must fail visibly instead of silently using CPU.
                                ort::ep::CoreML::default()
                                    .build()
                                    .error_on_failure(),
                            ])
                            .map_err(|source| {
                                LayoutError::from(ort::Error::from(source))
                            })?
                    }
                    #[cfg(not(feature = "coreml"))]
                    {
                        let _builder = builder;
                        return Err(
                            LayoutError::ExecutionProviderUnavailable {
                                provider: "coreml",
                            },
                        );
                    }
                }
                ExecutionProviderConfig::Openvino => {
                    #[cfg(feature = "openvino")]
                    {
                        builder
                            .with_execution_providers([
                                // A requested accelerator must fail visibly instead of silently using CPU.
                                ort::ep::OpenVINO::default()
                                    .build()
                                    .error_on_failure(),
                            ])
                            .map_err(|source| {
                                LayoutError::from(ort::Error::from(source))
                            })?
                    }
                    #[cfg(not(feature = "openvino"))]
                    {
                        let _builder = builder;
                        return Err(
                            LayoutError::ExecutionProviderUnavailable {
                                provider: "openvino",
                            },
                        );
                    }
                }
            };
            let session = builder.commit_from_memory(bytes)?;
            ModelSchema::from_session(&session)?.validate_pp_doclayout_v3()?;
            let options = RunOptions::new()?.with_outputs(
                OutputSelector::no_default()
                    .with("fetch_name_0")
                    .with("fetch_name_1"),
            );
            Ok(Self { session, options })
        }

        /// Executes the three-input graph and copies only the two consumed outputs.
        fn run(
            &mut self,
            inputs: &ModelInputs,
            timings: &Timings,
        ) -> Result<ModelOutputs, LayoutError> {
            let inference = timings.start(TimingStage::LayoutInference);
            let outputs = self.session.run_with_options(ort::inputs! {
                "im_shape" => TensorRef::from_array_view(&inputs.image_size)?,
                "image" => TensorRef::from_array_view(&inputs.image)?,
                "scale_factor" => TensorRef::from_array_view(&inputs.scale_factor)?,
            }, &self.options)?;
            drop(inference);
            let _readback = timings.start(TimingStage::LayoutReadback);
            ModelOutputs::try_from(&outputs)
        }
    }

    /// Bounded native sessions with permits held until actual blocking work completes.
    pub(crate) struct LayoutSessionPool {
        sessions: Vec<Mutex<LayoutSession>>,
        available: Mutex<Vec<usize>>,
        semaphore: Arc<Semaphore>,
    }

    impl LayoutSessionPool {
        /// Creates the configured native sessions outside the async executor.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            config: Arc<ValidatedConfig>,
        ) -> Result<Arc<Self>, LayoutError> {
            run_cpu(move || {
                let size = config.layout().session_pool_size;
                let sessions = (0..size)
                    .map(|_| {
                        LayoutSession::load(
                            &artifacts.model,
                            config.layout().execution_provider,
                        )
                        .map(Mutex::new)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(Arc::new(Self {
                    sessions,
                    available: Mutex::new((0..size).rev().collect()),
                    semaphore: Arc::new(Semaphore::new(size)),
                }))
            })
            .await
            .map_err(|source| LayoutError::TaskJoin { source })?
        }

        /// Leases one session without holding a synchronous lock while waiting.
        pub(crate) async fn acquire(
            self: Arc<Self>,
        ) -> Result<LayoutSessionLease, LayoutError> {
            let permit = Arc::clone(&self.semaphore)
                .acquire_owned()
                .await
                .map_err(|error| LayoutError::SessionPool {
                    message: error.to_string(),
                })?;
            let index = self
                .available
                .lock()
                .map_err(|error| LayoutError::SessionPool {
                    message: error.to_string(),
                })?
                .pop()
                .ok_or(LayoutError::SessionPool {
                    message: "permit without an available session".into(),
                })?;
            Ok(LayoutSessionLease {
                pool: self,
                index,
                _permit: permit,
            })
        }

        /// Holds the lease inside the blocking closure even if its caller is cancelled.
        pub(crate) async fn run(
            self: Arc<Self>,
            inputs: ModelInputs,
            timings: Timings,
        ) -> Result<ModelOutputs, LayoutError> {
            let queued = timings.start(TimingStage::LayoutQueue);
            let lease = self.acquire().await?;
            run_cpu(move || {
                // Include semaphore and blocking-executor wait, but not model execution.
                drop(queued);
                lease.run(&inputs, &timings)
            })
            .await
            .map_err(|source| LayoutError::TaskJoin { source })?
        }
    }

    /// Returns its native pool slot only when the executing closure releases ownership.
    pub(crate) struct LayoutSessionLease {
        pool: Arc<LayoutSessionPool>,
        index: usize,
        _permit: OwnedSemaphorePermit,
    }

    impl LayoutSessionLease {
        /// Keeps the whole lease captured by its blocking task, including the permit.
        fn run(
            &self,
            inputs: &ModelInputs,
            timings: &Timings,
        ) -> Result<ModelOutputs, LayoutError> {
            let slot = self.pool.sessions.get(self.index).ok_or(
                LayoutError::SessionPool {
                    message: "invalid session slot".into(),
                },
            )?;
            let mut session =
                slot.lock().map_err(|error| LayoutError::SessionPool {
                    message: error.to_string(),
                })?;
            session.run(inputs, timings)
        }
    }

    impl Drop for LayoutSessionLease {
        /// Restores availability before releasing the semaphore permit.
        fn drop(&mut self) {
            self.pool
                .available
                .lock()
                .unwrap_or_else(|poisoned| {
                    tracing::warn!(
                        "recovering poisoned layout session availability queue"
                    );
                    poisoned.into_inner()
                })
                .push(self.index);
        }
    }

    #[cfg(test)]
    mod tests {
        use std::path::{Path, PathBuf};
        use std::sync::Arc;
        use std::time::Duration;

        use super::LayoutSessionPool;

        /// Resolves a repository path from the layout crate directory.
        fn repository_path(relative: &str) -> PathBuf {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(relative)
        }

        /// Verifies a dropped lease returns its unique session slot and semaphore permit.
        #[tokio::test]
        #[ignore = "requires fixed PP-DocLayoutV3 model"]
        async fn lease_drop_returns_session_to_pool() {
            let config = docparse_config::ValidatedConfig::try_from(
                docparse_config::RawConfig::default(),
            )
            .expect("valid defaults");
            let artifacts = crate::ModelArtifacts::from_paths(
                &repository_path("models/pp-doclayout-v3/inference.onnx"),
                &repository_path("models/pp-doclayout-v3/inference.yml"),
                &repository_path("models/pp-doclayout-v3/model-manifest.json"),
            )
            .expect("model artifacts");
            let pool = LayoutSessionPool::load(artifacts, Arc::new(config))
                .await
                .expect("the real one-session pool must initialize");
            let first = Arc::clone(&pool)
                .acquire()
                .await
                .expect("the first lease must be available");

            let blocked = tokio::time::timeout(
                Duration::from_millis(20),
                Arc::clone(&pool).acquire(),
            )
            .await;
            let blocked_failure = match blocked {
                Err(_elapsed) => None,
                Ok(_lease) => {
                    Some("a second lease unexpectedly acquired a session")
                }
            };
            assert_eq!(blocked_failure, None);

            drop(first);
            let returned = tokio::time::timeout(
                Duration::from_secs(1),
                Arc::clone(&pool).acquire(),
            )
            .await;
            let returned_failure = match returned {
                Ok(Ok(_lease)) => None,
                Ok(Err(error)) => {
                    Some(format!("returned lease failed: {error}"))
                }
                Err(error) => {
                    Some(format!("returned lease timed out: {error}"))
                }
            };
            assert_eq!(returned_failure, None);
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;
    use tokio::sync::{mpsc, oneshot};

    /// Owned inference request whose buffers survive cancellation of the waiting caller.
    #[derive(typed_builder::TypedBuilder)]
    struct InferenceRequest {
        inputs: ModelInputs,
        response: oneshot::Sender<Result<ModelOutputs, LayoutError>>,
        timings: Timings,
        queued: crate::timing::StageTimer,
    }

    /// The sender for one browser-local actor that owns its ORT session across awaits.
    pub(crate) struct LayoutSessionPool {
        sender: mpsc::Sender<InferenceRequest>,
    }

    impl LayoutSessionPool {
        /// Creates a browser session after the embedding host initializes ort-web once.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            config: Arc<ValidatedConfig>,
        ) -> Result<Arc<Self>, LayoutError> {
            let mut builder = Session::builder()?;
            match config.layout().execution_provider {
                ExecutionProviderConfig::Cpu => {}
                ExecutionProviderConfig::WebGpu => {
                    builder = builder
                        .with_execution_providers([ort::ep::WebGPU::default()
                            .build()
                            .error_on_failure()])
                        .map_err(|error| {
                            LayoutError::from(ort::Error::from(error))
                        })?;
                }
                _ => {
                    return Err(LayoutError::ExecutionProviderUnavailable {
                        provider: "native provider on Web",
                    });
                }
            }
            let mut session =
                builder.commit_from_memory(&artifacts.model).await?;
            ModelSchema::from_session(&session)?.validate_pp_doclayout_v3()?;
            let options = RunOptions::new()?.with_outputs(
                OutputSelector::no_default()
                    .with("fetch_name_0")
                    .with("fetch_name_1"),
            );
            let (sender, mut receiver) = mpsc::channel::<InferenceRequest>(1);
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(request) = receiver.recv().await {
                    // The actor keeps inputs and the session alive until the JS Promise completes.
                    drop(request.queued);
                    let result = async {
                        let inference = request.timings.start(TimingStage::LayoutInference);
                        let mut outputs = session.run_async(ort::inputs! {
                            "im_shape" => TensorRef::from_array_view(&request.inputs.image_size)?,
                            "image" => TensorRef::from_array_view(&request.inputs.image)?,
                            "scale_factor" => TensorRef::from_array_view(&request.inputs.scale_factor)?,
                        }, &options).await?;
                        drop(inference);
                        // The Promise may finish before GPU outputs are CPU-readable.
                        let _readback = request.timings.start(TimingStage::LayoutReadback);
                        ort_web::sync_outputs(&mut outputs).await.map_err(|error| LayoutError::Engine { message: format!("Web tensor synchronization failed: {error}") })?;
                        ModelOutputs::try_from(&outputs)
                    }.await;
                    if let Err(error) = &result {
                        tracing::error!(
                            "browser model inference failed: {}",
                            error
                        );
                    }
                    let _ = request.response.send(result);
                }
                tracing::debug!("closed browser layout session");
            });
            Ok(Arc::new(Self { sender }))
        }

        /// Queues one owned input without borrowing session state across an await.
        pub(crate) async fn run(
            self: Arc<Self>,
            inputs: ModelInputs,
            timings: Timings,
        ) -> Result<ModelOutputs, LayoutError> {
            let queued = timings.start(TimingStage::LayoutQueue);
            let (response, receiver) = oneshot::channel();
            self.sender
                .send(
                    InferenceRequest::builder()
                        .inputs(inputs)
                        .response(response)
                        .timings(timings)
                        .queued(queued)
                        .build(),
                )
                .await
                .map_err(|_error| LayoutError::SessionPool {
                    message: "browser session stopped".into(),
                })?;
            receiver.await.map_err(|_error| LayoutError::SessionPool {
                message: "browser session response lost".into(),
            })?
        }
    }
}

pub(crate) use platform::LayoutSessionPool;
