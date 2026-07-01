//! PP-DocLayoutV3 JSON configuration loading.

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Context;
use anyhow::{Result, bail};
use serde::Deserialize;

#[cfg(not(target_arch = "wasm32"))]
use crate::pp_doclayout::weights::PpDocLayoutFiles;

/// Model architecture metadata read from `config.json`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PpDocLayoutConfig {
    pub(crate) model_type: String,
    pub(crate) architectures: Vec<String>,
    pub(crate) d_model: usize,
    pub(crate) num_queries: usize,
    pub(crate) decoder_layers: usize,
    pub(crate) decoder_attention_heads: usize,
    pub(crate) decoder_n_points: usize,
    pub(crate) feature_strides: Vec<usize>,
    pub(crate) id2label: std::collections::BTreeMap<String, String>,
}

/// Image preprocessing metadata read from `preprocessor_config.json`.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PpDocLayoutPreprocessorConfig {
    pub(crate) do_resize: bool,
    pub(crate) size: PpDocLayoutSize,
    pub(crate) image_mean: Vec<f32>,
    pub(crate) image_std: Vec<f32>,
}

/// Target input dimensions for PP-DocLayoutV3 preprocessing.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct PpDocLayoutSize {
    pub(crate) height: usize,
    pub(crate) width: usize,
}

/// Reads PP-DocLayoutV3 configuration from native files.
///
/// This wrapper keeps filesystem access out of the wasm path while preserving
/// the existing native download/load flow.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn read_pp_doclayout_config(
    files: &PpDocLayoutFiles,
) -> Result<(PpDocLayoutConfig, PpDocLayoutPreprocessorConfig)> {
    let config_bytes =
        std::fs::read(&files.config_path).with_context(|| {
            format!("failed to read {}", files.config_path.display())
        })?;
    let preprocessor_bytes = std::fs::read(&files.preprocessor_path)
        .with_context(|| {
            format!("failed to read {}", files.preprocessor_path.display())
        })?;

    read_pp_doclayout_config_from_bytes(&config_bytes, &preprocessor_bytes)
}

/// Reads and validates PP-DocLayoutV3 model metadata from in-memory bytes.
pub(crate) fn read_pp_doclayout_config_from_bytes(
    config_bytes: &[u8],
    preprocessor_bytes: &[u8],
) -> Result<(PpDocLayoutConfig, PpDocLayoutPreprocessorConfig)> {
    let config = serde_json::from_slice::<PpDocLayoutConfig>(config_bytes)?;
    let preprocessor = serde_json::from_slice::<PpDocLayoutPreprocessorConfig>(
        preprocessor_bytes,
    )?;

    validate_pp_doclayout_config(&config, &preprocessor)?;
    Ok((config, preprocessor))
}

/// Verifies that the downloaded config matches the native implementation.
fn validate_pp_doclayout_config(
    config: &PpDocLayoutConfig,
    preprocessor: &PpDocLayoutPreprocessorConfig,
) -> Result<()> {
    if config.model_type != "pp_doclayout_v3" {
        bail!("unsupported layout model type {}", config.model_type);
    }
    if !config
        .architectures
        .iter()
        .any(|name| name == "PPDocLayoutV3ForObjectDetection")
    {
        bail!("PP-DocLayoutV3 object detection architecture missing");
    }
    if config.d_model != 256 || config.num_queries != 300 {
        bail!("unexpected PP-DocLayoutV3 dimensions");
    }
    if config.decoder_layers != 6
        || config.decoder_attention_heads != 8
        || config.decoder_n_points != 4
    {
        bail!("unexpected PP-DocLayoutV3 decoder config");
    }
    if config.feature_strides != [8, 16, 32] {
        bail!("unexpected PP-DocLayoutV3 feature strides");
    }
    if config.id2label.is_empty() {
        bail!("PP layout config must define at least one label");
    }
    if preprocessor.do_resize
        && (preprocessor.size.height == 0 || preprocessor.size.width == 0)
    {
        bail!("PP-DocLayoutV3 resize dimensions must be positive");
    }

    Ok(())
}
