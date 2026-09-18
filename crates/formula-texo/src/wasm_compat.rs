//! Platform-specific scheduling around a shared batched generation state machine.
use crate::{
    TexoArtifacts, TexoEngine,
    model::{Generation, MAX_LENGTH, ModelSessions, StepOutput},
};
use docparse_formula::FormulaError;
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
#[path = "session_manager.rs"]
mod session_manager;

#[cfg(not(target_arch = "wasm32"))]
mod platform {
    pub(crate) use super::session_manager::SessionManager as SessionRunner;
    use super::*;

    impl TexoEngine {
        /// Uses the configured encoder path, its sibling merged decoder, and configured tokenizer.
        pub async fn from_config(
            config: Arc<docparse_config::ValidatedConfig>,
        ) -> Result<Self, FormulaError> {
            let docparse_config::FormulaEngineConfig::Texo(paths) =
                config.formula().engine.clone()
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
            Self::from_artifacts(config, artifacts).await
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

    impl Generation {
        /// Fails rather than silently accepting host-resident caches in the CUDA execution path.
        pub(crate) fn verify_device(
            &self,
            device: ort::memory::AllocationDevice,
        ) -> Result<(), FormulaError> {
            for value in std::iter::once(&self.hidden).chain(&self.cache) {
                let tensor =
                    value.downcast_ref::<ort::value::TensorValueType<f32>>()?;
                if tensor.memory_info().allocation_device() != device {
                    return Err(FormulaError::Invalid(format!(
                        "Texo expected cached tensor on {}, got {}",
                        device.as_str(),
                        tensor.memory_info().allocation_device().as_str()
                    )));
                }
            }
            Ok(())
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod platform {
    use super::*;
    use crate::preprocess::FormulaInput;
    use docparse_common::timing::{TimingStage, Timings};
    use docparse_formula::queue::{BatchTimings, FormulaQueue, FormulaRequest};
    use docparse_layout::{PageImage, wasm_compat::OnnxBackend};
    use ort::{session::builder::SessionBuilder, value::Tensor};
    use ort_web::{SyncDirection, ValueExt};

    /// One bounded crop queue is shared by all callers in the browser Worker.
    pub(crate) struct SessionRunner {
        queue: FormulaQueue,
    }

    impl SessionRunner {
        /// Loads both graphs using the host-initialized ORT Web backend and starts their local owner.
        pub(crate) async fn load(
            artifacts: TexoArtifacts,
            backend: OnnxBackend,
            config: &docparse_config::FormulaConfig,
        ) -> Result<Arc<Self>, FormulaError> {
            let docparse_config::FormulaEngineConfig::Texo(texo) =
                &config.engine
            else {
                return Err(FormulaError::Invalid(
                    "Texo session manager requires the Texo engine".into(),
                ));
            };
            let batch_size = config.batch_size;
            let (queue, receiver) =
                FormulaQueue::new("formula_texo", config.queue_size);
            for _ in 0..texo.session_size {
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
                let mut sessions = ModelSessions {
                    encoder,
                    decoder,
                    tokenizer: ModelSessions::tokenizer(&artifacts.tokenizer)?,
                };
                let options = ort::session::RunOptions::new()?;
                let receiver = receiver.clone();
                docparse_common::ThreadManager::spawn_async(Box::pin(
                    async move {
                        while let Some(batch) = receiver.recv().await {
                            // Drain after acquiring the browser runtime so ready requests can accumulate during another model's work.
                            let _guard = OnnxBackend::inference_guard().await;
                            let requests = batch.take_ready(batch_size);
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
                            let outputs = sessions.decoder.run_async(generation.inputs()?, &options).await?;
                            let mut output = StepOutput::try_from(outputs)?;
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
            Ok(Arc::new(Self { queue }))
        }

        /// Publishes crops independently while preserving the caller's original output order.
        pub(crate) async fn run(
            self: Arc<Self>,
            images: Vec<Arc<PageImage>>,
            timings: Timings,
        ) -> Result<Vec<String>, FormulaError> {
            self.queue.run(images, timings).await
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
    }
}

pub(crate) use platform::SessionRunner;
