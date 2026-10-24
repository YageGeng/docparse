//! Platform-specific scheduling around a shared batched generation state machine.
use crate::{
    TexoArtifacts, TexoEngine,
    model::{Generation, MAX_LENGTH, ModelSessions, StepOutput},
};
use docparse_formula::{
    FormulaError,
    queue::{FormulaRequest, FormulaWorkers},
};
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
#[path = "cuda.rs"]
mod cuda;

#[cfg(not(target_arch = "wasm32"))]
#[path = "session_manager.rs"]
mod session_manager;

#[cfg(not(target_arch = "wasm32"))]
mod platform {
    pub(crate) use super::session_manager::SessionManager as SessionRunner;
    use super::*;
    use crate::model::CACHE_NAMES;
    use ort::session::SessionInputValue;
    use ort::value::Tensor;
    use std::borrow::Cow;

    // Native decoding borrows values; browser decoding temporarily owns them across awaits.
    impl Generation {
        /// Borrows hidden features and cached tensors without cloning or copying their data.
        pub(crate) fn inputs(
            &self,
        ) -> Result<Vec<(Cow<'static, str>, SessionInputValue<'_>)>, FormulaError>
        {
            let mut inputs = ort::inputs![
                "input_ids" => Tensor::from_array(([self.next.len(), 1], self.next.clone()))?,
                "encoder_hidden_states" => &self.hidden,
                "use_cache_branch" => Tensor::from_array(([1], vec![self.step > 0]))?,
            ];
            for (name, value) in CACHE_NAMES.into_iter().zip(&self.cache) {
                inputs.push((Cow::Borrowed(name), value.into()));
            }
            Ok(inputs)
        }
    }

    impl TexoEngine {
        /// Uses the configured encoder path, its sibling merged decoder, and configured tokenizer.
        pub async fn from_config(
            config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, FormulaError> {
            let artifacts = Self::load_artifacts(&config).await?;
            Self::from_artifacts(config, artifacts).await
        }

        /// Loads this group's artifacts and attaches its sessions to the shared queue.
        pub async fn spawn_from_config(
            config: Arc<docparse_config::ValidatedConfig>,
            receiver: docparse_common::Queue<FormulaRequest>,
        ) -> Result<FormulaWorkers, FormulaError> {
            let artifacts = Self::load_artifacts(&config).await?;
            Self::spawn_from_artifacts(config, artifacts, receiver).await
        }

        /// Reads this group's configured bytes for standalone or shared execution.
        async fn load_artifacts(
            config: &docparse_config::ValidatedConfig,
        ) -> Result<TexoArtifacts, FormulaError> {
            let docparse_config::FormulaEngineConfig::Texo(paths) =
                config.formula().single_engine()?.clone()
            else {
                tracing::error!(
                    "Texo loader requires formula.engine.type = texo"
                );
                return Err(FormulaError::Invalid(
                    "Texo loader requires formula.engine.type = texo".into(),
                ));
            };
            let artifacts = docparse_common::run_cpu(move || {
                TexoArtifacts::try_from(&paths)
            })
            .await??;
            Ok(artifacts)
        }
    }
    impl TryFrom<&docparse_config::TexoFormulaConfig> for TexoArtifacts {
        type Error = FormulaError;

        /// Reads exactly the paths belonging to the explicitly selected Texo variant.
        fn try_from(
            paths: &docparse_config::TexoFormulaConfig,
        ) -> Result<Self, Self::Error> {
            let read = |path: &std::path::Path| {
                std::fs::read(path).map(Arc::from).map_err(|error| {
                    tracing::error!(
                        "failed to read Texo artifact {}: {}",
                        path.display(),
                        error
                    );
                    FormulaError::Artifacts(format!(
                        "{}: {error}",
                        path.display()
                    ))
                })
            };
            Ok(Self {
                encoder: read(&paths.encoder_path)?,
                decoder: read(&paths.decoder_path)?,
                tokenizer: read(&paths.tokenizer_path)?,
            })
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod platform {
    use super::*;
    use crate::preprocess::FormulaInput;
    use docparse_common::timing::TimingStage;
    use docparse_formula::queue::BatchTimings;
    use docparse_layout::wasm_compat::OnnxBackend;
    use ort::{session::builder::SessionBuilder, value::Tensor};
    use ort_web::{SyncDirection, ValueExt};

    impl Generation {
        /// Moves decoder inputs into an owned slice, then restores hidden states and KV caches unchanged.
        async fn run_decoder(
            mut self,
            decoder: &mut ort::session::Session,
            input_order: &[usize; 11],
            options: &ort::session::RunOptions,
        ) -> Result<(Self, StepOutput), FormulaError> {
            use crate::model::CACHE_NAMES;
            use ort::session::SessionInputValue;
            if self.cache.len() != CACHE_NAMES.len() {
                return Err(FormulaError::Invalid(
                    "missing retained Texo cache".into(),
                ));
            }
            // Canonical slots are token IDs, hidden states, cache flag, then the eight KV tensors.
            let values = [
                Tensor::from_array(([self.next.len(), 1], self.next.clone()))?
                    .into_dyn(),
                self.hidden,
                Tensor::from_array(([1], vec![self.step > 0]))?.into_dyn(),
            ]
            .into_iter()
            .chain(self.cache.drain(..));
            let mut inputs: [Option<SessionInputValue<'static>>; 11] =
                std::array::from_fn(|_| None);
            for (&index, value) in input_order.iter().zip(values) {
                *inputs.get_mut(index).expect("validated Texo input index") =
                    Some(SessionInputValue::Owned(value));
            }
            // Initialization validates a complete permutation; the cache-length check fills every slot.
            let inputs =
                inputs.map(|input| input.expect("validated Texo input slot"));
            let output = StepOutput::try_from(
                decoder.run_async(inputs.as_slice(), options).await?,
            )?;
            // Recover only owned handles by their validated indices, without touching GPU storage.
            let mut retained = inputs.map(|input| match input {
                SessionInputValue::Owned(value) => Some(value),
                _ => None,
            });
            self.hidden = retained
                .get_mut(input_order[1])
                .and_then(Option::take)
                .ok_or_else(|| {
                    FormulaError::Invalid("missing Texo hidden states".into())
                })?;
            // Reuse the drained cache allocation for every autoregressive step.
            for &index in &input_order[3..] {
                self.cache.push(
                    retained.get_mut(index).and_then(Option::take).ok_or_else(
                        || {
                            FormulaError::Invalid(
                                "missing retained Texo cache".into(),
                            )
                        },
                    )?,
                );
            }
            Ok((self, output))
        }
    }

    /// One bounded crop queue is shared by all callers in the browser Worker.
    pub(crate) struct SessionRunner;

    impl SessionRunner {
        /// Loads both graphs using the host-initialized ORT Web backend and starts their local owner.
        pub(crate) async fn load(
            artifacts: TexoArtifacts,
            backend: OnnxBackend,
            config: &docparse_config::FormulaConfig,
            receiver: docparse_common::Queue<FormulaRequest>,
        ) -> Result<FormulaWorkers, FormulaError> {
            let docparse_config::FormulaEngineConfig::Texo(texo) =
                config.single_engine()?
            else {
                return Err(FormulaError::Invalid(
                    "Texo session manager requires the Texo engine".into(),
                ));
            };
            let batch_size = texo.batch_size;
            // Shared consumers own execution only, never a producer handle.
            let mut workers = FormulaWorkers::new(format!(
                "texo-transfer-onnx-{}",
                backend.execution_provider()
            ));
            for _ in 0..texo.worker_size {
                // Apply the shared runtime settings to both graphs, including their memory policy.
                let mut encoder_builder = SessionBuilder::try_from(backend)?;
                let mut decoder_builder = SessionBuilder::try_from(backend)?;
                if backend.execution_provider()
                    == docparse_layout::ExecutionProvider::WebGpu
                {
                    encoder_builder = encoder_builder
                    .with_config_entry(
                        "ort_web.preferred_output_location.last_hidden_state",
                        "gpu-buffer",
                    )
                    .map_err(ort::Error::from)?;
                    for name in crate::model::PRESENT_NAMES {
                        decoder_builder = decoder_builder
                            .with_config_entry(
                                format!(
                                    "ort_web.preferred_output_location.{name}"
                                ),
                                "gpu-buffer",
                            )
                            .map_err(ort::Error::from)?;
                    }
                    decoder_builder = decoder_builder
                        .with_config_entry(
                            "ort_web.preferred_output_location.logits",
                            "cpu",
                        )
                        .map_err(ort::Error::from)?;
                }
                let encoder = encoder_builder
                    .commit_from_memory(&artifacts.encoder)
                    .await?;
                let decoder = decoder_builder
                    .commit_from_memory(&artifacts.decoder)
                    .await?;
                // Input names never change within a session; resolve canonical slots once per worker.
                let mut input_order = [usize::MAX; 11];
                for (index, outlet) in decoder.inputs().iter().enumerate() {
                    let slot = [
                        "input_ids",
                        "encoder_hidden_states",
                        "use_cache_branch",
                    ]
                    .into_iter()
                    .chain(crate::model::CACHE_NAMES)
                    .position(|name| name == outlet.name())
                    .ok_or_else(|| {
                        FormulaError::Invalid(format!(
                            "unexpected Texo decoder input {}",
                            outlet.name()
                        ))
                    })?;
                    let position = input_order
                        .get_mut(slot)
                        .expect("known Texo input slot");
                    if *position != usize::MAX {
                        return Err(FormulaError::Invalid(format!(
                            "duplicate Texo decoder input {}",
                            outlet.name()
                        )));
                    }
                    *position = index;
                }
                if input_order.contains(&usize::MAX) {
                    return Err(FormulaError::Invalid(
                        "Texo decoder is missing required inputs".into(),
                    ));
                }
                let mut sessions = ModelSessions {
                    encoder,
                    decoder,
                    tokenizer: ModelSessions::tokenizer(&artifacts.tokenizer)?,
                };
                let options = ort::session::RunOptions::new()?;
                let receiver = receiver.clone();
                workers.spawn(Box::pin(
                    async move {
                        while let Some(batch) = receiver.recv().await {
                            let mut requests = batch.take_ready(batch_size);
                            for request in &mut requests { request.engine = format!("texo-transfer-onnx-{}", backend.execution_provider()); }
                            if requests.is_empty() {
                                continue;
                            }
                            let images = requests
                                .iter()
                                .map(|request| Arc::clone(&request.image))
                                .collect::<Vec<_>>();
                            let timings: BatchTimings = requests
                                .iter()
                                .map(|request| &request.context)
                                .collect();
                            // Waiting for another local model must not exclude HTTP consumers from the shared queue.
                            let queued = timings.start(TimingStage::FormulaQueue);
                            let _guard = OnnxBackend::inference_guard().await;
                            drop(queued);
                            tracing::debug!(
                                "browser Texo session running {} ready crops",
                                images.len()
                            );
                            let result = async {
                        let batch = images.len();
                        let preprocessing = timings.start(TimingStage::FormulaPreprocess);
                        let input = FormulaInput::try_from(images)?;
                        drop(preprocessing);
                        let inference = timings.start(TimingStage::FormulaInference);
                        let mut outputs = sessions.encoder.run_async(ort::inputs!["pixel_values" => Tensor::from_array(input.0)?], &options).await?;
                        let hidden = outputs.remove("last_hidden_state").ok_or_else(|| FormulaError::Invalid("missing Texo image features".into()))?;
                        drop(outputs);
                        let mut generation = Generation::new(hidden, batch)?;
                        for _ in 1..MAX_LENGTH {
                            if generation.cancel(requests.iter().map(FormulaRequest::cancelled)) { return Err(FormulaError::Invalid("Texo batch canceled".into())); }
                            // Own the existing GPU handles across the await without cloning their buffers.
                            let (retained, mut output) = generation.run_decoder(&mut sessions.decoder, &input_order, &options).await?;
                            generation = retained;
                            // KV caches remain on the device throughout merged browser requests.
                            output.logits.sync(SyncDirection::Rust).await.map_err(|error| FormulaError::Invalid(format!("Texo logits readback failed: {error}")))?;
                            if generation.advance(output)? { break; }
                        }
                        drop(inference);
                        let _decoding = timings.start(TimingStage::FormulaDecode);
                        Ok(generation.decode(&sessions.tokenizer))
                    }.await;
                            FormulaRequest::complete_batch(requests, result);
                        }
                        tracing::debug!("closed browser Texo queue");
                    },
                ))?;
            }
            Ok(workers)
        }
    }

    impl TexoEngine {
        /// Browser hosts must supply explicit model bytes and initialize ORT Web before loading.
        pub async fn from_config(
            _config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, FormulaError> {
            tracing::error!(
                "browser Texo initialization requires explicit artifacts"
            );
            Err(FormulaError::ArtifactsRequired)
        }
        /// Browser consumers require explicit bytes rather than filesystem access.
        pub async fn spawn_from_config(
            _config: Arc<docparse_config::ValidatedConfig>,
            _receiver: docparse_common::Queue<FormulaRequest>,
        ) -> Result<FormulaWorkers, FormulaError> {
            Err(FormulaError::ArtifactsRequired)
        }
    }
}

pub(crate) use platform::SessionRunner;
