//! Shared execution-provider registration for layout, OCR and table structure models.
use crate::LayoutError;
use docparse_config::{OptimizationLevel, ValidatedConfig};
use ort::session::{
    Session,
    builder::{GraphOptimizationLevel, SessionBuilder},
};
use serde::Serialize;

/// Identifies the ONNX backend selected by build features or the browser runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionProvider {
    Cpu,
    Cuda,
    #[serde(rename = "coreml")]
    CoreMl,
    /// Apple GPU through CoreML with CPUAndGPU compute units.
    Metal,
    Openvino,
    /// Browser WebGPU execution, validated at the platform boundary.
    WebGpu,
}

impl ExecutionProvider {
    /// Returns the stable diagnostic name of a backend.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::CoreMl => "coreml",
            Self::Metal => "metal",
            Self::Openvino => "openvino",
            Self::WebGpu => "webgpu",
        }
    }
}
impl std::fmt::Display for ExecutionProvider {
    /// Formats backend names consistently across model initialization and inference logs.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Carries the provider and parser-wide graph and memory policies to every session constructor.
#[derive(Debug, Clone, Copy)]
pub struct OnnxBackend {
    provider: ExecutionProvider,
    optimization_level: OptimizationLevel,
    memory_pattern: bool,
}

impl OnnxBackend {
    /// Reports the selected backend without exposing a runtime mutation mechanism.
    pub const fn execution_provider(self) -> ExecutionProvider {
        self.provider
    }

    /// Chooses the compiled native provider with the shared configuration's default graph level.
    #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
    pub fn compiled() -> Self {
        let provider = if cfg!(feature = "cuda") {
            ExecutionProvider::Cuda
        } else if cfg!(feature = "metal") {
            ExecutionProvider::Metal
        } else if cfg!(feature = "coreml") {
            ExecutionProvider::CoreMl
        } else if cfg!(feature = "openvino") {
            ExecutionProvider::Openvino
        } else {
            ExecutionProvider::Cpu
        };
        let runtime = docparse_config::RuntimeConfig::default();
        Self {
            provider,
            optimization_level: runtime.optimization_level,
            memory_pattern: runtime.memory_pattern,
        }
    }

    /// Applies global graph and memory settings to inspection, compatibility sessions, and accelerated builders.
    pub fn cpu_builder(self) -> Result<SessionBuilder, LayoutError> {
        let level = match self.optimization_level {
            OptimizationLevel::Level1 => GraphOptimizationLevel::Level1,
            OptimizationLevel::Level2 => GraphOptimizationLevel::Level2,
            OptimizationLevel::Level3 => GraphOptimizationLevel::Level3,
            OptimizationLevel::All => GraphOptimizationLevel::All,
        };
        // Every model inherits this switch; ORT Web independently forces it off for WebGPU.
        Ok(Session::builder()?
            .with_optimization_level(level)
            .map_err(ort::Error::from)?
            .with_memory_pattern(self.memory_pattern)
            .map_err(ort::Error::from)?)
    }
}

impl From<&ValidatedConfig> for OnnxBackend {
    /// Combines parser-wide graph settings with the compiled native or host-selected browser provider.
    fn from(config: &ValidatedConfig) -> Self {
        #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
        {
            Self {
                optimization_level: config.runtime().optimization_level,
                memory_pattern: config.runtime().memory_pattern,
                ..Self::compiled()
            }
        }
        #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
        {
            Self {
                provider: if config.webgpu_enabled() {
                    ExecutionProvider::WebGpu
                } else {
                    ExecutionProvider::Cpu
                },
                optimization_level: config.runtime().optimization_level,
                memory_pattern: config.runtime().memory_pattern,
            }
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl OnnxBackend {
    /// Serializes browser sessions through output readback, including work whose caller timed out.
    pub async fn inference_guard() -> tokio::sync::MutexGuard<'static, ()> {
        // ORT WebGPU shares download buffers across sessions. Overlapping layout and
        // TSR runs can unmap a buffer while the other session is still reading it.
        static INFERENCE: tokio::sync::Mutex<()> =
            tokio::sync::Mutex::const_new(());
        INFERENCE.lock().await
    }
}

impl TryFrom<OnnxBackend> for SessionBuilder {
    type Error = LayoutError;

    /// Registers requested accelerators strictly; unsupported builds never silently select CPU.
    fn try_from(backend: OnnxBackend) -> Result<Self, Self::Error> {
        // Register the selected provider only after applying the shared per-parser session options.
        let builder = backend.cpu_builder()?;
        let provider = backend.provider;
        let dispatch = match provider {
            ExecutionProvider::Cpu => return Ok(builder),
            ExecutionProvider::Cuda => {
                #[cfg(all(not(target_arch = "wasm32"), feature = "cuda"))]
                {
                    // OCR widths and partial batches keep changing after warmup; select
                    // convolution kernels heuristically instead of benchmarking every new shape.
                    Some(
                        ort::ep::CUDA::default()
                            .with_conv_algorithm_search(
                                ort::ep::cuda::ConvAlgorithmSearch::Heuristic,
                            )
                            .build(),
                    )
                }
                #[cfg(not(all(
                    not(target_arch = "wasm32"),
                    feature = "cuda"
                )))]
                {
                    None
                }
            }
            ExecutionProvider::CoreMl | ExecutionProvider::Metal => {
                #[cfg(all(not(target_arch = "wasm32"), feature = "coreml"))]
                {
                    let units = if provider == ExecutionProvider::Metal {
                        ort::ep::coreml::ComputeUnits::CPUAndGPU
                    } else {
                        ort::ep::coreml::ComputeUnits::All
                    };
                    tracing::info!(
                        "configuring CoreML with {:?} compute units and FastPrediction specialization",
                        units
                    );
                    Some(
                        ort::ep::CoreML::default()
                            .with_compute_units(units)
                            // Sessions are reused across pages; favor steady-state prediction over compilation latency.
                            .with_specialization_strategy(
                                ort::ep::coreml::SpecializationStrategy::FastPrediction,
                            )
                            .build(),
                    )
                }
                #[cfg(not(all(
                    not(target_arch = "wasm32"),
                    feature = "coreml"
                )))]
                {
                    None
                }
            }
            ExecutionProvider::Openvino => {
                #[cfg(all(not(target_arch = "wasm32"), feature = "openvino"))]
                {
                    Some(ort::ep::OpenVINO::default().build())
                }
                #[cfg(not(all(
                    not(target_arch = "wasm32"),
                    feature = "openvino"
                )))]
                {
                    None
                }
            }
            ExecutionProvider::WebGpu => {
                #[cfg(all(target_arch = "wasm32", feature = "wasm"))]
                {
                    Some(ort::ep::WebGPU::default().build())
                }
                #[cfg(not(all(target_arch = "wasm32", feature = "wasm")))]
                {
                    None
                }
            }
        };
        let dispatch: ort::ep::ExecutionProviderDispatch = dispatch
            .ok_or_else(|| {
                tracing::error!(
                    "ONNX execution provider {} is unavailable in this build",
                    provider
                );
                LayoutError::ExecutionProviderUnavailable {
                    provider: provider.as_str(),
                }
            })?;
        tracing::info!("registering ONNX execution provider {}", provider);
        builder
            .with_execution_providers([dispatch.error_on_failure()])
            .map_err(|error| {
                tracing::error!(
                    "ONNX execution provider {} initialization failed: {}",
                    provider,
                    error
                );
                LayoutError::from(ort::Error::from(error))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every native model receives the compiled backend even when built from code-default configuration.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn native_models_share_the_compiled_backend() {
        let config =
            ValidatedConfig::try_from(docparse_config::RawConfig::default())
                .expect("config");
        let selected = OnnxBackend::from(&config).execution_provider();
        assert_eq!(selected, OnnxBackend::compiled().execution_provider());
        for (provider, enabled) in [
            (ExecutionProvider::Cuda, cfg!(feature = "cuda")),
            (ExecutionProvider::Metal, cfg!(feature = "metal")),
            (
                ExecutionProvider::CoreMl,
                cfg!(feature = "coreml") && !cfg!(feature = "metal"),
            ),
            (ExecutionProvider::Openvino, cfg!(feature = "openvino")),
            (
                ExecutionProvider::Cpu,
                !cfg!(any(
                    feature = "cuda",
                    feature = "coreml",
                    feature = "openvino"
                )),
            ),
        ] {
            assert_eq!(selected == provider, enabled, "{provider}");
        }
    }

    /// A missing accelerator feature must fail instead of silently creating a CPU session.
    #[test]
    fn unsupported_providers_are_rejected() {
        for (provider, enabled) in [
            (ExecutionProvider::Cuda, cfg!(feature = "cuda")),
            (ExecutionProvider::CoreMl, cfg!(feature = "coreml")),
            (ExecutionProvider::Metal, cfg!(feature = "coreml")),
            (ExecutionProvider::Openvino, cfg!(feature = "openvino")),
            (
                ExecutionProvider::WebGpu,
                cfg!(all(feature = "wasm", target_arch = "wasm32")),
            ),
        ] {
            if !enabled {
                assert!(
                    matches!(SessionBuilder::try_from(OnnxBackend { provider, optimization_level: OptimizationLevel::default(), memory_pattern: false }),
                    Err(LayoutError::ExecutionProviderUnavailable { provider: name }) if name == provider.as_str())
                );
            }
        }
    }
}
