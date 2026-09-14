//! Shared execution-provider registration for layout, OCR and table structure models.
use crate::LayoutError;
use docparse_config::ValidatedConfig;
use ort::session::{Session, builder::SessionBuilder};
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

/// Opaque backend selection prevents model callers from bypassing the compiled native provider.
#[derive(Debug, Clone, Copy)]
pub struct OnnxBackend(ExecutionProvider);

impl OnnxBackend {
    /// Reports the selected backend without exposing a runtime mutation mechanism.
    pub const fn execution_provider(self) -> ExecutionProvider {
        self.0
    }

    /// Chooses the single native backend enabled for the shared inference crate.
    #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
    pub const fn compiled() -> Self {
        if cfg!(feature = "cuda") {
            Self(ExecutionProvider::Cuda)
        } else if cfg!(feature = "metal") {
            Self(ExecutionProvider::Metal)
        } else if cfg!(feature = "coreml") {
            Self(ExecutionProvider::CoreMl)
        } else if cfg!(feature = "openvino") {
            Self(ExecutionProvider::Openvino)
        } else {
            Self(ExecutionProvider::Cpu)
        }
    }
}

impl From<&ValidatedConfig> for OnnxBackend {
    /// Native selection is compile-time; browser selection comes from the host-only platform capability.
    fn from(config: &ValidatedConfig) -> Self {
        #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
        {
            let _ = config;
            Self::compiled()
        }
        #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
        {
            Self(if config.webgpu_enabled() {
                ExecutionProvider::WebGpu
            } else {
                ExecutionProvider::Cpu
            })
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
    fn try_from(
        OnnxBackend(provider): OnnxBackend,
    ) -> Result<Self, Self::Error> {
        let builder = Session::builder()?;
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
                    Some(
                        ort::ep::CoreML::default()
                            .with_compute_units(units)
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
                    matches!(SessionBuilder::try_from(OnnxBackend(provider)),
                    Err(LayoutError::ExecutionProviderUnavailable { provider: name }) if name == provider.as_str())
                );
            }
        }
    }
}
