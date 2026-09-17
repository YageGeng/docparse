//! Platform-specific scheduling around a shared batched generation state machine.
use crate::{
    TexoArtifacts, TexoEngine,
    model::{Generation, MAX_LENGTH, ModelSessions, StepOutput},
    preprocess::FormulaInput,
};
use docparse_formula::FormulaError;
use docparse_layout::{
    PageImage,
    timing::{TimingStage, Timings},
    wasm_compat::OnnxBackend,
};
use ort::{session::builder::SessionBuilder, value::Tensor};
use std::sync::Arc;

#[cfg(not(target_arch = "wasm32"))]
mod platform {
    use super::*;
    use docparse_layout::wasm_compat::SessionWorker;

    /// One bounded owner keeps initialization, inference, and destruction off async executor threads.
    pub(crate) struct SessionRunner {
        session: SessionWorker<ModelSessions>,
        device: Option<ort::memory::AllocationDevice>,
    }

    impl SessionRunner {
        /// Registers the same configured provider for both graphs without forcing a CPU fallback.
        pub(crate) async fn load(
            artifacts: TexoArtifacts,
            backend: OnnxBackend,
        ) -> Result<Arc<Self>, FormulaError> {
            let session = SessionWorker::new(move || {
                let encoder = SessionBuilder::try_from(backend)?
                    .with_intra_threads(1)
                    .map_err(ort::Error::from)?
                    .commit_from_memory(&artifacts.encoder)?;
                let decoder = SessionBuilder::try_from(backend)?
                    .with_intra_threads(1)
                    .map_err(ort::Error::from)?
                    .commit_from_memory(&artifacts.decoder)?;
                Ok::<_, FormulaError>(ModelSessions {
                    encoder,
                    decoder,
                    tokenizer: ModelSessions::tokenizer(&artifacts.tokenizer)?,
                })
            })
            .await?;
            let device = (backend.execution_provider()
                == docparse_layout::ExecutionProvider::Cuda)
                .then_some(ort::memory::AllocationDevice::CUDA);
            Ok(Arc::new(Self { session, device }))
        }

        /// Terminates canceled native calls and releases all per-batch caches before admitting another batch.
        pub(crate) async fn run(
            self: Arc<Self>,
            images: Vec<Arc<PageImage>>,
            timings: Timings,
        ) -> Result<Vec<String>, FormulaError> {
            let queued = timings.start(TimingStage::FormulaQueue);
            let options = Arc::new(ort::session::RunOptions::new()?);
            let mut cancel = CancelRun(Some(Arc::clone(&options)));
            let device = self.device;
            let result = self
                .session
                .run(move |sessions| {
                    drop(queued);
                    let batch = images.len();
                    let preprocessing =
                        timings.start(TimingStage::FormulaPreprocess);
                    let input = FormulaInput::try_from(images)?;
                    drop(preprocessing);
                    let inference =
                        timings.start(TimingStage::FormulaInference);
                    let memory = device
                        .map(|device| {
                            ort::memory::MemoryInfo::new(
                                device,
                                0,
                                ort::memory::AllocatorType::Device,
                                ort::memory::MemoryType::Default,
                            )
                        })
                        .transpose()?;
                    let pixels = Tensor::from_array(input.0)?;
                    let hidden = if let Some(memory) = &memory {
                        let mut binding = sessions.encoder.create_binding()?;
                        binding.bind_input("pixel_values", &pixels)?;
                        binding.bind_output_to_device(
                            "last_hidden_state",
                            memory,
                        )?;
                        let mut outputs = sessions
                            .encoder
                            .run_binding_with_options(&binding, &options)?;
                        binding.synchronize_outputs()?;
                        outputs.remove("last_hidden_state").ok_or_else(
                            || {
                                FormulaError::Invalid(
                                    "missing Texo image features".into(),
                                )
                            },
                        )?
                    } else {
                        let mut outputs = sessions.encoder.run_with_options(
                            ort::inputs!["pixel_values" => pixels],
                            &options,
                        )?;
                        outputs.remove("last_hidden_state").ok_or_else(
                            || {
                                FormulaError::Invalid(
                                    "missing Texo image features".into(),
                                )
                            },
                        )?
                    };
                    let mut generation = Generation::new(hidden, batch)?;
                    for _ in 1..MAX_LENGTH {
                        let output = if let Some(memory) = &memory {
                            // Fresh output bindings prevent the growing cache from overwriting tensors still used as inputs.
                            let mut binding =
                                sessions.decoder.create_binding()?;
                            for (name, value) in generation.inputs()? {
                                binding.bind_input(name, &*value)?;
                            }
                            for name in crate::model::PRESENT_NAMES {
                                binding.bind_output_to_device(name, memory)?;
                            }
                            let cpu = ort::memory::MemoryInfo::new(
                                ort::memory::AllocationDevice::CPU,
                                0,
                                ort::memory::AllocatorType::Device,
                                ort::memory::MemoryType::Default,
                            )?;
                            binding.bind_output_to_device("logits", &cpu)?;
                            let outputs = sessions
                                .decoder
                                .run_binding_with_options(&binding, &options)?;
                            binding.synchronize_outputs()?;
                            StepOutput::try_from(outputs)?
                        } else {
                            StepOutput::try_from(
                                sessions.decoder.run_with_options(
                                    generation.inputs()?,
                                    &options,
                                )?,
                            )?
                        };
                        if generation.advance(output)? {
                            break;
                        }
                        if let Some(device) = device {
                            generation.verify_device(device)?;
                        }
                    }
                    drop(inference);
                    let _decoding = timings.start(TimingStage::FormulaDecode);
                    generation.decode(&sessions.tokenizer)
                })
                .await?;
            cancel.0.take();
            result
        }
    }

    /// Keeps cancellation tied to the caller while the worker owns all native tensor lifetimes.
    struct CancelRun(Option<Arc<ort::session::RunOptions>>);
    impl Drop for CancelRun {
        /// Interrupts both encoder and decoder at the next ORT cancellation boundary.
        fn drop(&mut self) {
            if let Some(options) = &self.0
                && let Err(error) = options.terminate()
            {
                tracing::warn!(
                    "failed to terminate canceled Texo inference: {}",
                    error
                );
            }
        }
    }

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
    #[cfg(test)]
    mod tests {
        use super::*;

        /// Exercise the same growing-cache I/O-binding path on CPU when CUDA hardware is absent.
        #[tokio::test]
        #[ignore = "requires models/texo"]
        async fn bound_outputs_preserve_batched_and_repeated_results() {
            let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../models/texo");
            let paths = docparse_config::TexoFormulaConfig {
                encoder_path: directory.join("encoder_model.onnx"),
                decoder_path: directory.join("decoder_model_merged.onnx"),
                tokenizer_path: directory.join("tokenizer.json"),
            };
            let artifacts = TexoArtifacts::try_from(&paths).expect("artifacts");
            artifacts.verify().expect("identity");
            let mut runner =
                SessionRunner::load(artifacts, OnnxBackend::compiled())
                    .await
                    .expect("sessions");
            Arc::get_mut(&mut runner).expect("single owner").device =
                Some(ort::memory::AllocationDevice::CPU);
            let reference: serde_json::Value = serde_json::from_str(
                include_str!("../tests/fixtures/reference.json"),
            )
            .expect("reference");
            let mut images = Vec::new();
            let mut expected = Vec::new();
            for case in reference
                .get("cases")
                .expect("cases")
                .as_array()
                .expect("array")
            {
                let image = image::open(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures")
                        .join(
                            case.get("image")
                                .expect("image")
                                .as_str()
                                .expect("string"),
                        ),
                )
                .expect("PNG")
                .to_rgb8();
                images.push(Arc::new(
                    PageImage::try_from(
                        docparse_layout::PageImageInput::builder()
                            .width(image.width())
                            .height(image.height())
                            .pixel_format(docparse_layout::PixelFormat::Rgb8)
                            .data(Arc::from(image.into_raw()))
                            .build(),
                    )
                    .expect("pixels"),
                ));
                expected.push(
                    case.get("latex")
                        .expect("latex")
                        .as_str()
                        .expect("string")
                        .to_owned(),
                );
            }
            for _ in 0..2 {
                assert_eq!(
                    Arc::clone(&runner)
                        .run(images.clone(), Timings::default())
                        .await
                        .expect("bound batch"),
                    expected
                );
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
mod platform {
    use super::*;
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
                            if response.is_closed() { return Err(FormulaError::Invalid("Texo request canceled".into())); }
                            let outputs = sessions.decoder.run_async(generation.inputs()?, &options).await?;
                            let mut output = StepOutput::try_from(outputs)?;
                            // KV caches stay in ORT Web; downloading them every token would dominate generation.
                            output.logits.sync(SyncDirection::Rust).await.map_err(|error| FormulaError::Invalid(format!("Texo logits readback failed: {error}")))?;
                            if generation.advance(output)? { break; }
                        }
                        drop(inference);
                        let _decoding = timings.start(TimingStage::FormulaDecode);
                        generation.decode(&sessions.tokenizer)
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
