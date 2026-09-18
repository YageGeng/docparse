//! Bounded ready-request batching with one session per model and owned native/JS inputs.
use crate::{
    SlanetPlusEngine, TsrError, artifacts::ModelKind, model::ModelResult,
    preprocess::ModelInput,
};
use docparse_layout::wasm_compat::OnnxBackend;
use docparse_layout::{
    ModelArtifacts,
    timing::{TimingStage, Timings},
};
use ort::session::builder::SessionBuilder;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};

/// Each crop retains its own response and tracing context when batches span pages or documents.
#[derive(typed_builder::TypedBuilder)]
struct Request {
    input: ModelInput,
    response: oneshot::Sender<Result<ModelResult, TsrError>>,
    timings: Timings,
    #[builder(default)]
    queued: Option<docparse_layout::timing::StageTimer>,
    /// Native blocking replies outlive a canceled future, so they need a separate caller-lifetime signal.
    #[builder(default)]
    caller: Option<oneshot::Sender<()>>,
    span: tracing::Span,
    dispatch: tracing::Dispatch,
}

impl Request {
    /// Collects only ready work after the first request, independently of how the platform waits for it.
    fn batch(
        first: Self,
        receiver: &mut mpsc::Receiver<Self>,
        batch_size: usize,
    ) -> Vec<Self> {
        // ponytail: batch ready work only; add a coalescing wait only if measurements justify the latency.
        let mut requests = Vec::with_capacity(batch_size);
        let mut next = Some(first);
        while let Some(mut request) = next {
            if request.cancelled() {
                let queued = request.queued.take();
                tracing::dispatcher::with_default(&request.dispatch, || {
                    request.span.in_scope(|| drop(queued))
                });
            } else {
                requests.push(request);
            }
            if requests.len() == batch_size {
                break;
            }
            next = receiver.try_recv().ok();
        }
        requests
    }

    /// Recognizes native caller cancellation even while its blocking completion receiver remains alive.
    fn cancelled(&self) -> bool {
        self.response.is_closed()
            || self.caller.as_ref().is_some_and(oneshot::Sender::is_closed)
    }

    /// Ends each caller's queue interval and starts its share of the batch execution interval.
    fn start(
        &mut self,
        kind: ModelKind,
    ) -> docparse_layout::timing::StageTimer {
        let queued = self.queued.take();
        tracing::dispatcher::with_default(&self.dispatch, || {
            self.span.in_scope(|| {
                drop(queued);
                self.timings.start(kind.timing())
            })
        })
    }

    /// Delivers one ordered result per crop and attributes shared batch time to each original caller.
    fn complete(
        requests: Vec<Self>,
        timers: Vec<docparse_layout::timing::StageTimer>,
        result: Result<Vec<ModelResult>, TsrError>,
    ) {
        let mut results = result.and_then(|outputs| {
            if outputs.len() != requests.len() {
                return Err(TsrError::InvalidInput {
                    reason: "TSR batch result count mismatch".to_owned(),
                });
            }
            Ok(outputs.into_iter())
        });
        for (request, timer) in requests.into_iter().zip(timers) {
            let result = match &mut results {
                Ok(outputs) => {
                    outputs.next().ok_or_else(|| TsrError::Inference {
                        message: "missing TSR batch result".to_owned(),
                    })
                }
                // Preserve the category used by per-crop segmented recovery after a malformed structure result.
                Err(TsrError::InvalidInput { reason }) => {
                    Err(TsrError::InvalidInput {
                        reason: reason.clone(),
                    })
                }
                Err(error) => Err(TsrError::Inference {
                    message: error.to_string(),
                }),
            };
            tracing::dispatcher::with_default(&request.dispatch, || {
                request.span.in_scope(|| {
                    drop(timer);
                    let _ = request.response.send(result);
                })
            });
        }
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
            .timings(timings)
            .queued(Some(queued))
            .span(tracing::Span::current())
            .dispatch(tracing::dispatcher::get_default(Clone::clone))
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
        let (sender, mut receiver) = mpsc::channel(5);
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
                .try_send(
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
                        .timings(Timings::default())
                        .span(tracing::Span::none())
                        .dispatch(tracing::dispatcher::get_default(
                            Clone::clone,
                        ))
                        .build(),
                )
                .map_err(|error| error.to_string())
                .expect("ready queue has space for every test request");
            replies.push(reply);
        }
        let cancelled_reply = replies.remove(2);
        drop(replies.remove(2));
        drop(sender);
        let kind = ModelKind::Structure(docparse_config::TsrModel::SlanetPlus);
        for expected_size in [2, 1] {
            let first = receiver.recv().await.expect("ready request");
            let mut requests = Request::batch(first, &mut receiver, 2);
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
            .timings(Timings::default())
            .span(tracing::Span::none())
            .dispatch(tracing::dispatcher::get_default(Clone::clone))
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
    use docparse_layout::wasm_compat::{TaskError, run_cpu};

    /// Owns the model thread independently of every Tokio runtime that submits work.
    pub(crate) struct SessionRunner {
        sender: Option<mpsc::Sender<Request>>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Drop for SessionRunner {
        /// Closes the request queue before joining the thread that also destroys the native session.
        fn drop(&mut self) {
            drop(self.sender.take());
            if let Some(thread) = self.thread.take()
                && thread.join().is_err()
            {
                tracing::error!("TSR model thread panicked during shutdown");
            }
        }
    }

    impl SessionRunner {
        /// Initializes and batches on one native thread so a caller's runtime shutdown cannot stop the model.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
            batch_size: usize,
        ) -> Result<Arc<Self>, TsrError> {
            let span = tracing::Span::current();
            let dispatch = tracing::dispatcher::get_default(Clone::clone);
            let initialize = move || {
                // Consume the initializer so verified bytes and its tracing context are released after loading.
                let model = artifacts.model;
                tracing::dispatcher::with_default(&dispatch, || {
                    span.in_scope(|| {
                        // Specialize spatial dimensions only; partial batches keep the batch axis dynamic.
                        let mut builder = SessionBuilder::try_from(backend)?
                            .with_intra_threads(1)
                            .map_err(ort::Error::from)?;
                        if matches!(
                            kind,
                            ModelKind::Structure(
                                docparse_config::TsrModel::SlanetPlus
                            )
                        ) {
                            builder = builder
                                .with_dimension_override(
                                    "DynamicDimension.1",
                                    488,
                                )
                                .map_err(ort::Error::from)?
                                .with_dimension_override(
                                    "DynamicDimension.2",
                                    488,
                                )
                                .map_err(ort::Error::from)?;
                        }
                        Ok::<_, TsrError>(builder.commit_from_memory(&model)?)
                    })
                })
            };
            run_cpu(move || {
                let (sender, mut receiver) = mpsc::channel(batch_size);
                let (ready, initialized) = oneshot::channel();
                let thread = std::thread::Builder::new()
                    .name("docparse-tsr".into())
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
                        while let Some(first) = receiver.blocking_recv() {
                            let mut requests = Request::batch(
                                first,
                                &mut receiver,
                                batch_size,
                            );
                            if requests.is_empty() {
                                continue;
                            }
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
                                let outputs = session.run(input.values()?)?;
                                ModelResult::from_batch(
                                    kind,
                                    requests.len(),
                                    &outputs,
                                )
                            })();
                            Request::complete(requests, timers, result);
                        }
                    })
                    .map_err(|error| {
                        TaskError::from_message(error.to_string())
                    })?;
                // Install the join owner before waiting so failed or canceled initialization also reaps the thread.
                let runner = Arc::new(Self {
                    sender: Some(sender),
                    thread: Some(thread),
                });
                initialized.blocking_recv().map_err(|_closed| {
                    TaskError::from_message("TSR initialization thread stopped")
                })??;
                Ok(runner)
            })
            .await?
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
                self.sender
                    .as_ref()
                    .ok_or_else(|| TsrError::Inference {
                        message: "TSR worker stopped".to_owned(),
                    })?
                    .blocking_send(request)
                    .map_err(|_closed| TsrError::Inference {
                        message: "TSR worker stopped".to_owned(),
                    })?;
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
            let artifacts = docparse_layout::wasm_compat::run_cpu(move || {
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
        sender: mpsc::Sender<Request>,
    }

    impl SessionRunner {
        /// Creates the selected browser session after the host initializes ort-web.
        pub(crate) async fn load(
            artifacts: ModelArtifacts,
            backend: OnnxBackend,
            kind: ModelKind,
            batch_size: usize,
        ) -> Result<Arc<Self>, TsrError> {
            let mut session = SessionBuilder::try_from(backend)?
                .commit_from_memory(&artifacts.model)
                .await?;
            let options = RunOptions::new()?;
            let (sender, mut receiver) = mpsc::channel(batch_size);
            wasm_bindgen_futures::spawn_local(async move {
                while let Some(first) = receiver.recv().await {
                    let mut requests =
                        Request::batch(first, &mut receiver, batch_size);
                    if requests.is_empty() {
                        continue;
                    }
                    let _inference = OnnxBackend::inference_guard().await;
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
                        ModelResult::from_batch(kind, requests.len(), &outputs)
                    }
                    .await;
                    Request::complete(requests, timers, result);
                }
                tracing::debug!("closed browser TSR session");
            });
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
