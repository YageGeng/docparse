use std::fs;
use std::path::Path;
use std::sync::Arc;

use docparse_config::ValidatedConfig;
use serde::Deserialize;

use crate::{
    LayoutDetection, LayoutEngine, LayoutError, LayoutRequest, ModelManifest,
    PP_DOCLAYOUT_V3_REVISION,
};

pub(crate) mod pool;
pub(crate) mod postprocess;
pub(crate) mod preprocess;
pub(crate) mod schema;
pub(crate) mod session;

use pool::LayoutSessionPool;
use preprocess::preprocess;

const LABELS: [&str; 25] = [
    "abstract",
    "algorithm",
    "aside_text",
    "chart",
    "content",
    "display_formula",
    "doc_title",
    "figure_title",
    "footer",
    "footer_image",
    "footnote",
    "formula_number",
    "header",
    "header_image",
    "image",
    "inline_formula",
    "number",
    "paragraph_title",
    "reference",
    "reference_content",
    "seal",
    "table",
    "text",
    "vertical_text",
    "vision_footnote",
];

#[derive(Debug, Deserialize)]
struct InferenceConfig {
    #[serde(rename = "Global")]
    global: GlobalConfig,
    #[serde(rename = "Preprocess")]
    preprocess: Vec<PreprocessStep>,
    label_list: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GlobalConfig {
    model_name: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum PreprocessStep {
    Resize {
        interp: u8,
        keep_ratio: bool,
        target_size: [u32; 2],
    },
    NormalizeImage {
        mean: [f32; 3],
        norm_type: String,
        std: [f32; 3],
    },
    Permute,
}

/// Real PP-DocLayoutV3 implementation backed by a bounded ORT session pool.
pub struct PpDocLayoutV3Engine {
    pool: Arc<LayoutSessionPool>,
    score_threshold: f64,
}

impl PpDocLayoutV3Engine {
    /// Validates all fixed artifacts and creates the configured session pool off-thread.
    pub async fn from_config(
        config: Arc<ValidatedConfig>,
    ) -> Result<Self, LayoutError> {
        let config_for_build = Arc::clone(&config);
        tokio::task::spawn_blocking(move || Self::build(&config_for_build))
            .await
            .map_err(|source| LayoutError::TaskJoin { source })?
    }

    /// Performs synchronous artifact validation and session creation.
    fn build(config: &ValidatedConfig) -> Result<Self, LayoutError> {
        let layout = config.layout();
        tracing::info!(
            "loading PP-DocLayoutV3 model from {}",
            layout.model_path.display()
        );
        ModelManifest::load_and_verify(
            &layout.model_path,
            &layout.model_config_path,
            &layout.model_manifest_path,
        )?;
        verify_model_config(&layout.model_config_path)?;
        let pool = LayoutSessionPool::new(
            &layout.model_path,
            layout.execution_provider,
            layout.session_pool_size,
        )?;
        tracing::info!(
            "loaded PP-DocLayoutV3 revision {} with {} session(s)",
            PP_DOCLAYOUT_V3_REVISION,
            layout.session_pool_size
        );
        Ok(Self {
            pool,
            score_threshold: layout.score_threshold,
        })
    }
}

#[async_trait::async_trait]
impl LayoutEngine for PpDocLayoutV3Engine {
    /// Returns the stable concrete engine name.
    fn name(&self) -> &str {
        "pp-doclayout-v3-onnx"
    }

    /// Returns the fixed Hugging Face model revision.
    fn model_revision(&self) -> &str {
        PP_DOCLAYOUT_V3_REVISION
    }

    /// Preprocesses and runs one page on a uniquely leased ORT session.
    async fn detect(
        &self,
        request: LayoutRequest,
    ) -> Result<Vec<LayoutDetection>, LayoutError> {
        let page_number = request.page_number;
        let image = Arc::clone(&request.image);
        let transform = request.transform;
        let threshold = self.score_threshold;
        // CPU preprocessing happens before leasing the scarce session so later pages can prepare
        // tensors while the current page is using the GPU.
        let preprocess_transform = transform.clone();
        let inputs = tokio::task::spawn_blocking(move || {
            preprocess(image.as_ref(), &preprocess_transform)
        })
        .await
        .map_err(|source| LayoutError::TaskJoin { source })??;
        let lease = Arc::clone(&self.pool).acquire().await?;
        tracing::info!(
            "starting PP-DocLayoutV3 inference for page {}",
            page_number
        );
        let detections = tokio::task::spawn_blocking(move || {
            lease.detect(&inputs, &transform, threshold)
        })
        .await
        .map_err(|source| LayoutError::TaskJoin { source })??;
        tracing::info!(
            "completed PP-DocLayoutV3 inference for page {} with {} detections",
            page_number,
            detections.len()
        );
        Ok(detections)
    }
}

/// Parses and verifies the fixed preprocessing and label contract in inference.yml.
fn verify_model_config(path: &Path) -> Result<(), LayoutError> {
    let bytes =
        fs::read(path).map_err(|source| LayoutError::ModelConfigRead {
            path: path.to_path_buf(),
            source,
        })?;
    let config: InferenceConfig =
        serde_yml::from_slice(&bytes).map_err(|source| {
            LayoutError::ModelConfigParse {
                path: path.to_path_buf(),
                source: Box::new(source),
            }
        })?;
    if config.global.model_name != "PP-DocLayoutV3" {
        return Err(LayoutError::UnsupportedModelConfig {
            reason: format!(
                "unexpected model_name {}",
                config.global.model_name
            ),
        });
    }
    let [
        PreprocessStep::Resize {
            interp,
            keep_ratio,
            target_size,
        },
        PreprocessStep::NormalizeImage {
            mean,
            norm_type,
            std,
        },
        PreprocessStep::Permute,
    ] = config.preprocess.as_slice()
    else {
        return Err(LayoutError::UnsupportedModelConfig {
            reason: format!(
                "unexpected preprocessing steps: {:?}",
                config.preprocess
            ),
        });
    };
    let zero_mean = mean.iter().all(|value| value.abs() <= f32::EPSILON);
    let unit_std = std.iter().all(|value| (*value - 1.0).abs() <= f32::EPSILON);
    if *interp != 2
        || *keep_ratio
        || *target_size != [800, 800]
        || norm_type != "none"
        || !zero_mean
        || !unit_std
    {
        return Err(LayoutError::UnsupportedModelConfig {
            reason: "resize or normalization contract mismatch".to_owned(),
        });
    }
    if !config.label_list.iter().map(String::as_str).eq(LABELS) {
        return Err(LayoutError::UnsupportedModelConfig {
            reason: "label list mismatch".to_owned(),
        });
    }
    Ok(())
}
