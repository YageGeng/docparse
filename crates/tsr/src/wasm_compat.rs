//! Bounded ready-request batching with one session per model and owned native/JS inputs.
use crate::{
    SlanetPlusEngine, TsrError, artifacts::ModelKind, model::ModelResult,
    preprocess::ModelInput,
};
use docparse_common::timing::TimingContext;
use docparse_common::timing::{TimingStage, Timings};
use docparse_layout::ModelArtifacts;
use docparse_layout::wasm_compat::OnnxBackend;
use ort::session::builder::SessionBuilder;
use std::sync::Arc;
use tokio::sync::oneshot;

/// Each crop retains its own response and tracing context when batches span pages or documents.
#[derive(typed_builder::TypedBuilder)]
struct Request {
    // Keep the render delivery occupied until actual inference and input cleanup finish.
    #[builder(default = docparse_common::PageLease::current())]
    _page_lease: Option<docparse_common::PageLease>,
    input: ModelInput,
    response: oneshot::Sender<Result<ModelResult, TsrError>>,
    context: TimingContext,
    #[builder(default)]
    queued: Option<docparse_common::timing::StageTimer>,
    /// Native blocking replies outlive a canceled future, so they need a separate caller-lifetime signal.
    #[builder(default)]
    caller: Option<oneshot::Sender<()>>,
}

impl Request {
    /// Recognizes native caller cancellation even while its blocking completion receiver remains alive.
    fn cancelled(&self) -> bool {
        self.response.is_closed()
            || self.caller.as_ref().is_some_and(oneshot::Sender::is_closed)
    }

    /// Ends each caller's queue interval and starts its share of the batch execution interval.
    fn start(
        &mut self,
        kind: ModelKind,
    ) -> docparse_common::timing::ContextTimer {
        docparse_common::SessionRequest::end_queue(self);
        self.context.start(kind.timing())
    }

    /// Routes model results while retaining the InvalidInput category required by segmented recovery.
    fn complete(
        requests: Vec<Self>,
        timers: Vec<docparse_common::timing::ContextTimer>,
        result: Result<Vec<ModelResult>, TsrError>,
    ) {
        let mut results = result.and_then(|outputs| {
            if outputs.len() != requests.len() {
                return Err(TsrError::InvalidInput {
                    reason: "TSR batch result count mismatch".into(),
                });
            }
            Ok(outputs.into_iter())
        });
        for (request, timer) in requests.into_iter().zip(timers) {
            let result = match &mut results {
                Ok(outputs) => {
                    outputs.next().ok_or_else(|| TsrError::Inference {
                        message: "missing TSR batch result".into(),
                    })
                }
                Err(TsrError::InvalidInput { reason }) => {
                    Err(TsrError::InvalidInput {
                        reason: reason.clone(),
                    })
                }
                Err(error) => Err(TsrError::Inference {
                    message: error.to_string(),
                }),
            };
            drop(timer);
            request.context.in_scope(|| {
                let _ = request.response.send(result);
            });
        }
    }
}

impl docparse_common::SessionRequest for Request {
    /// Skips canceled inputs before they consume an inference batch slot.
    fn cancelled(&self) -> bool {
        self.cancelled()
    }
    /// Restores the originating request context when admission ends.
    fn end_queue(&mut self) {
        let queued = self.queued.take();
        self.context.in_scope(|| drop(queued));
    }
}

impl SessionRunner {
    /// Retains each crop until inference finishes while cancellation marks only its original caller.
    pub(crate) async fn run(
        self: Arc<Self>,
        input: ModelInput,
        timings: Timings,
    ) -> Result<ModelResult, TsrError> {
        let queued = timings.start(TimingStage::TsrQueue);
        let (response, receiver) = oneshot::channel();
        let request = Request::builder()
            .input(input)
            .response(response)
            .context(TimingContext::new(timings))
            .queued(Some(queued))
            .build();
        self.submit(request, receiver).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ready batches honor their cap, skip canceled crops, flush a short tail, and route each result once.
    #[tokio::test]
    async fn ready_batches_preserve_replies_and_flush_partial_work() {
        let (sender, receiver) =
            docparse_common::Queue::<Request>::new("test", 5);
        let mut replies = Vec::new();
        for value in 0..5 {
            let (response, reply) = oneshot::channel();
            // Native cancellation must work even though its blocking reply receiver is still alive.
            let caller = if value == 2 {
                let (caller, lifetime) = oneshot::channel();
                drop(lifetime);
                Some(caller)
            } else {
                None
            };
            sender
                .send(
                    Request::builder()
                        .input(ModelInput::Structure(
                            crate::preprocess::SlanetInput(
                                ndarray::Array4::from_elem(
                                    (1, 3, 2, 2),
                                    value as f32,
                                ),
                            ),
                        ))
                        .response(response)
                        .caller(caller)
                        .context(TimingContext::new(Timings::default()))
                        .build(),
                )
                .await
                .map_err(|error| error.to_string())
                .expect("ready queue has space for every test request");
            replies.push(reply);
        }
        let cancelled_reply = replies.remove(2);
        drop(replies.remove(2));
        drop(sender);
        let kind = ModelKind::Structure(docparse_config::TsrModel::SlanetPlus);
        for expected_size in [2, 1] {
            let mut requests =
                receiver.recv().await.expect("ready request").take_ready(2);
            assert_eq!(requests.len(), expected_size);
            if requests.is_empty() {
                continue;
            }
            let timers = requests
                .iter_mut()
                .map(|request| request.start(kind))
                .collect();
            let input = ModelInput::batch(
                &requests
                    .iter()
                    .map(|request| &request.input)
                    .collect::<Vec<_>>(),
            )
            .expect("batch inputs");
            let ModelInput::Structure(input) = input else {
                unreachable!("structure")
            };
            let outputs = input
                .0
                .outer_iter()
                .map(|row| {
                    ModelResult::Cells(ndarray::Array2::from_elem(
                        (1, 6),
                        *row.get((0, 0, 0)).expect("pixel"),
                    ))
                })
                .collect();
            Request::complete(requests, timers, Ok(outputs));
        }
        assert!(receiver.recv().await.is_none());
        assert!(cancelled_reply.await.is_err());
        for (reply, expected) in replies.into_iter().zip([0.0, 1.0, 4.0]) {
            let ModelResult::Cells(boxes) =
                reply.await.expect("response").expect("result")
            else {
                unreachable!("cells")
            };
            assert_eq!(boxes.get((0, 0)), Some(&expected));
        }
        // Invalid structure output must still reach the existing segmented-retry path as InvalidInput.
        let (response, reply) = oneshot::channel();
        let mut request = Request::builder()
            .input(ModelInput::Structure(crate::preprocess::SlanetInput(
                ndarray::Array4::zeros((1, 3, 2, 2)),
            )))
            .response(response)
            .context(TimingContext::new(Timings::default()))
            .build();
        let timer = request.start(kind);
        Request::complete(
            vec![request],
            vec![timer],
            Err(TsrError::InvalidInput {
                reason: "invalid structure output".to_owned(),
            }),
        );
        assert!(matches!(
            reply.await.expect("failure response"),
            Err(TsrError::InvalidInput { .. })
        ));
    }
}

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use super::*;
    use crate::TsrArtifacts;
    use docparse_common::run_cpu;

    use docparse_common::SessionManager;

    /// Independent structure or detector owners consume the same bounded model queue.
    pub(crate) struct SessionRunner {
        manager: Arc<SessionManager<Request>>,
    }

    impl SessionRunner {
        /// Initializes owners over an independently sized queue and batches ready crops across callers.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
            batch_size: usize,
            session_size: usize,
            queue_size: usize,
        ) -> Result<Arc<Self>, TsrError> {
            let manager = SessionManager::load(
                kind.metric_name(),
                session_size,
                batch_size,
                queue_size,
                move || {
                    let mut builder = SessionBuilder::try_from(backend)?
                        .with_intra_threads(1)
                        .map_err(ort::Error::from)?;
                    if matches!(
                        kind,
                        ModelKind::Structure(
                            docparse_config::TsrModel::SlanetPlus
                        )
                    ) {
                        // Preserve dynamic batches while specializing the model's fixed spatial input.
                        builder = builder
                            .with_dimension_override("DynamicDimension.1", 488)
                            .map_err(ort::Error::from)?
                            .with_dimension_override("DynamicDimension.2", 488)
                            .map_err(ort::Error::from)?;
                    }
                    let mut session =
                        builder.commit_from_memory(&artifacts.model)?;
                    Ok::<_, TsrError>(move |mut requests: Vec<Request>| {
                        let timers = requests
                            .iter_mut()
                            .map(|request| request.start(kind))
                            .collect();
                        let result = (|| {
                            let input = ModelInput::batch(
                                &requests
                                    .iter()
                                    .map(|request| &request.input)
                                    .collect::<Vec<_>>(),
                            )?;
                            let values = input.values()?;
                            let physical =
                                docparse_common::telemetry::Inference::new(
                                    kind.metric_name(),
                                    "model",
                                    requests.len(),
                                );
                            let outputs = session.run(values);
                            physical.finish(outputs.is_ok());
                            let outputs = outputs?;
                            ModelResult::from_batch(
                                kind,
                                requests.len(),
                                &outputs,
                            )
                        })();
                        Request::complete(requests, timers, result);
                    })
                },
            )
            .await?;
            Ok(Arc::new(Self { manager }))
        }

        /// Keeps the last model owner off the inference thread until even canceled native work has completed.
        pub(super) async fn submit(
            self: Arc<Self>,
            mut request: Request,
            receiver: oneshot::Receiver<Result<ModelResult, TsrError>>,
        ) -> Result<ModelResult, TsrError> {
            let (caller, _caller_lifetime) = oneshot::channel();
            request.caller = Some(caller);
            run_cpu(move || {
                self.manager.send(request)?;
                // Only finite work occupies the caller's blocking pool; idle models use their own threads.
                receiver.blocking_recv().map_err(|_closed| {
                    TsrError::Inference {
                        message: "TSR response lost".to_owned(),
                    }
                })?
            })
            .await?
        }
    }

    impl SlanetPlusEngine {
        /// Reads fixed native model artifacts from validated configuration paths.
        pub async fn from_config(
            config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, TsrError> {
            let paths = Arc::clone(&config);
            let artifacts = docparse_common::run_cpu(move || {
                let structure = ModelArtifacts::from_paths(
                    &paths.tsr().model_path,
                    &paths.tsr().model_config_path,
                    &paths.tsr().model_manifest_path,
                )?;
                let cell_detection = paths
                    .tsr()
                    .cell_detection
                    .as_ref()
                    .filter(|cells| cells.enabled)
                    .map(|cells| {
                        ModelArtifacts::from_paths(
                            &cells.files.model_path,
                            &cells.files.model_config_path,
                            &cells.files.model_manifest_path,
                        )
                    })
                    .transpose()?;
                Ok::<_, TsrError>(TsrArtifacts {
                    structure,
                    cell_detection,
                })
            })
            .await??;
            Self::from_artifacts(config, artifacts).await
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    use super::*;
    use ort::session::RunOptions;

    /// Browser sessions stay on their owning Worker while queued inputs outlive canceled calls.
    pub(crate) struct SessionRunner {
        sender: docparse_common::queue::QueueSender<Request>,
    }

    impl SessionRunner {
        /// Creates the selected browser session after the host initializes ort-web.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
            batch_size: usize,
            session_size: usize,
            queue_size: usize,
        ) -> Result<Arc<Self>, TsrError> {
            let (sender, receiver) = docparse_common::Queue::<Request>::new(
                kind.metric_name(),
                queue_size,
            );
            for _ in 0..session_size {
                let mut session = SessionBuilder::try_from(backend)?
                    .commit_from_memory(&artifacts.model)
                    .await?;
                let options = RunOptions::new()?;
                let receiver = receiver.clone();
                let _worker = docparse_common::ThreadManager::spawn_async(
                    Box::pin(async move {
                        while let Some(batch) = receiver.recv().await {
                            let _inference =
                                OnnxBackend::inference_guard().await;
                            let mut requests = batch.take_ready(batch_size);
                            if requests.is_empty() {
                                continue;
                            }
                            // A deadline may expire while another model owns the browser runtime.
                            requests.retain(|request| !request.cancelled());
                            if requests.is_empty() {
                                continue;
                            }
                            let timers = requests
                                .iter_mut()
                                .map(|request| request.start(kind))
                                .collect();
                            let result = async {
                            let input = ModelInput::batch(
                                &requests
                                    .iter()
                                    .map(|request| &request.input)
                                    .collect::<Vec<_>>(),
                            )?;
                            let mut outputs = session
                                .run_async(input.values()?, &options)
                                .await?;
                            ort_web::sync_outputs(&mut outputs).await.map_err(
                            |error| TsrError::Inference {
                                message: format!(
                                    "TSR output synchronization failed: {error}"
                                ),
                            },
                        )?;
                            ModelResult::from_batch(
                                kind,
                                requests.len(),
                                &outputs,
                            )
                        }
                        .await;
                            Request::complete(requests, timers, result);
                        }
                        tracing::debug!("closed browser TSR session");
                    }),
                )?;
            }
            Ok(Arc::new(Self { sender }))
        }

        /// Uses asynchronous browser channels because the Worker must remain available to drive ORT promises.
        pub(super) async fn submit(
            self: Arc<Self>,
            request: Request,
            receiver: oneshot::Receiver<Result<ModelResult, TsrError>>,
        ) -> Result<ModelResult, TsrError> {
            self.sender.send(request).await.map_err(|_closed| {
                TsrError::Inference {
                    message: "TSR worker stopped".to_owned(),
                }
            })?;
            receiver.await.map_err(|_closed| TsrError::Inference {
                message: "TSR response lost".to_owned(),
            })?
        }
    }

    impl SlanetPlusEngine {
        /// Browser hosts must provide bytes rather than relying on filesystem configuration.
        pub async fn from_config(
            _config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, TsrError> {
            tracing::error!(
                "browser TSR initialization requires explicit artifacts"
            );
            Err(TsrError::ModelArtifactsRequired)
        }
    }
}

pub(crate) use platform::SessionRunner;
