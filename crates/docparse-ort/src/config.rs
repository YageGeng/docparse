//! Native ORT backend configuration.

use std::path::PathBuf;

/// Native execution provider preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeExecutionProvider {
    /// Portable CPU execution provider.
    Cpu,
    /// Apple CoreML execution provider.
    CoreMl,
    /// NVIDIA CUDA execution provider.
    Cuda,
    /// NVIDIA TensorRT execution provider.
    TensorRt,
}

/// Configuration for native ORT layout analysis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrtLayoutConfig {
    /// Path to `inference.onnx`.
    pub model_path: PathBuf,
    /// Ordered execution-provider preference.
    pub execution_providers: Vec<NativeExecutionProvider>,
}

impl Default for OrtLayoutConfig {
    fn default() -> Self {
        Self {
            model_path: PathBuf::from(
                "models/pp-structure-v3-onnx/inference.onnx",
            ),
            execution_providers: vec![NativeExecutionProvider::Cpu],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NativeExecutionProvider, OrtLayoutConfig};

    #[test]
    fn default_config_points_to_shared_model_directory() {
        let config = OrtLayoutConfig::default();

        assert_eq!(
            config.model_path,
            std::path::PathBuf::from(
                "models/pp-structure-v3-onnx/inference.onnx"
            )
        );
        assert_eq!(
            config.execution_providers,
            vec![NativeExecutionProvider::Cpu]
        );
    }
}
