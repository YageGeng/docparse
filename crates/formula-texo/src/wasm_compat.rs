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
            let artifacts = docparse_layout::wasm_compat::run_cpu(move || {
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
    use docparse_layout::{
        PageImage,
        timing::{TimingStage, Timings},
        wasm_compat::OnnxBackend,
    };
    use ort::{session::builder::SessionBuilder, value::Tensor};
    use ort_web::{SyncDirection, ValueExt};
    use tokio::sync::{mpsc, oneshot};

    /// Bounded actor messages own all pixels until promises and tensor readback finish.
    type Request = (
        Vec<Arc<PageImage>>,
        Timings,
        docparse_layout::timing::StageTimer,
        oneshot::Sender<Result<Vec<String>, FormulaError>>,
    );
    pub(crate) struct SessionRunner {
        sender: mpsc::Sender<Request>,
    }

    impl SessionRunner {
        /// Loads both graphs using the host-initialized ORT Web backend and starts their local owner.
        pub(crate) async fn load(
            artifacts: TexoArtifacts,
            backend: OnnxBackend,
            _config: &docparse_config::FormulaConfig,
        ) -> Result<Arc<Self>, FormulaError> {
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
                            format!("ort_web.preferred_output_location.{name}"),
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
            let (sender, mut receiver) = mpsc::channel::<Request>(1);
            wasm_bindgen_futures::spawn_local(async move {
                while let Some((images, timings, queued, response)) =
                    receiver.recv().await
                {
                    if response.is_closed() {
                        continue;
                    }
                    // Retain the shared exclusion boundary through logits readback and cache replacement.
                    let _guard = OnnxBackend::inference_guard().await;
                    if response.is_closed() {
                        continue;
                    }
                    drop(queued);
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
                            if generation.cancel(std::iter::repeat_n(response.is_closed(), batch)) { return Err(FormulaError::Invalid("Texo request canceled".into())); }
                            let outputs = sessions.decoder.run_async(generation.inputs()?, &options).await?;
                            let mut output = StepOutput::try_from(outputs)?;
                            // KV caches stay in ORT Web; downloading them every token would dominate generation.
                            output.logits.sync(SyncDirection::Rust).await.map_err(|error| FormulaError::Invalid(format!("Texo logits readback failed: {error}")))?;
                            if generation.advance(output)? { break; }
                        }
                        drop(inference);
                        let _decoding = timings.start(TimingStage::FormulaDecode);
                        generation.decode(&sessions.tokenizer).into_iter().collect()
                    }.await;
                    let _ = response.send(result);
                }
                tracing::debug!("closed browser Texo sessions");
            });
            Ok(Arc::new(Self { sender }))
        }

        /// Queues a real batch and lets dropping its receiver cancel queued or iterative work.
        pub(crate) async fn run(
            self: Arc<Self>,
            images: Vec<Arc<PageImage>>,
            timings: Timings,
        ) -> Result<Vec<String>, FormulaError> {
            let queued = timings.start(TimingStage::FormulaQueue);
            let (response, receiver) = oneshot::channel();
            self.sender
                .send((images, timings, queued, response))
                .await
                .map_err(|error| {
                    FormulaError::Invalid(format!(
                        "Texo actor stopped: {error}"
                    ))
                })?;
            receiver.await.map_err(|error| {
                FormulaError::Invalid(format!("Texo response lost: {error}"))
            })?
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
