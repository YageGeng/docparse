//! Shared execution-provider registration for layout and table structure models.
use crate::LayoutError;
use docparse_config::ExecutionProviderConfig;
use ort::session::{Session, builder::SessionBuilder};

/// Selects the same platform-specific ONNX backend for every model.
pub struct OnnxBackend(pub ExecutionProviderConfig);

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
            ExecutionProviderConfig::Cpu => return Ok(builder),
            ExecutionProviderConfig::Cuda => {
                #[cfg(all(not(target_arch = "wasm32"), feature = "cuda"))]
                {
                    Some(ort::ep::CUDA::default().build())
                }
                #[cfg(not(all(
                    not(target_arch = "wasm32"),
                    feature = "cuda"
                )))]
                {
                    None
                }
            }
            ExecutionProviderConfig::CoreMl
            | ExecutionProviderConfig::Metal => {
                #[cfg(all(not(target_arch = "wasm32"), feature = "coreml"))]
                {
                    let units = if provider == ExecutionProviderConfig::Metal {
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
            ExecutionProviderConfig::Openvino => {
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
            ExecutionProviderConfig::WebGpu => {
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

    /// A missing accelerator feature must fail instead of silently creating a CPU session.
    #[test]
    fn unsupported_providers_are_rejected() {
        for (provider, enabled) in [
            (ExecutionProviderConfig::Cuda, cfg!(feature = "cuda")),
            (ExecutionProviderConfig::CoreMl, cfg!(feature = "coreml")),
            (ExecutionProviderConfig::Metal, cfg!(feature = "coreml")),
            (
                ExecutionProviderConfig::Openvino,
                cfg!(feature = "openvino"),
            ),
            (
                ExecutionProviderConfig::WebGpu,
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
