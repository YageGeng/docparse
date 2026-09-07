use std::sync::Arc;

use docparse_config::ValidatedConfig;
use serde::Deserialize;

use crate::wasm_compat::LayoutSessionPool;
use crate::{
    LayoutDetection, LayoutEngine, LayoutError, LayoutRequest, ModelArtifacts,
    PP_DOCLAYOUT_V3_REVISION,
};

pub(crate) mod postprocess;
pub(crate) mod preprocess;
pub(crate) mod schema;
pub(crate) mod session;

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
    /// Verifies artifact content once before creating platform-specific sessions.
    pub async fn from_artifacts(
        config: Arc<ValidatedConfig>,
        artifacts: ModelArtifacts,
    ) -> Result<Self, LayoutError> {
        tracing::info!(
            "loading PP-DocLayoutV3 revision {} from {} model bytes",
            PP_DOCLAYOUT_V3_REVISION,
            artifacts.model.len()
        );
        let artifacts = crate::wasm_compat::run_cpu(move || {
            artifacts.verify()?;
            verify_model_config(&artifacts.config)?;
            Ok::<_, LayoutError>(artifacts)
        })
        .await
        .map_err(|source| LayoutError::TaskJoin { source })??;
        let pool =
            LayoutSessionPool::load(artifacts, Arc::clone(&config)).await?;
        tracing::info!(
            "loaded PP-DocLayoutV3 with {} session(s)",
            config.layout().session_pool_size
        );
        Ok(Self {
            pool,
            score_threshold: config.layout().score_threshold,
        })
    }
}

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
    fn detect(
        &self,
        request: LayoutRequest,
    ) -> crate::wasm_compat::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    > {
        Box::pin(async move {
            let page_number = request.page_number;
            let image = Arc::clone(&request.image);
            let transform = request.transform;
            let threshold = self.score_threshold;
            // CPU preprocessing happens before leasing the scarce session so later pages can prepare
            // tensors while the current page is using the GPU.
            let preprocess_transform = transform.clone();
            let inputs = crate::wasm_compat::run_cpu(move || {
                preprocess(image.as_ref(), &preprocess_transform)
            })
            .await
            .map_err(|source| LayoutError::TaskJoin { source })??;
            tracing::info!(
                "starting PP-DocLayoutV3 inference for page {}",
                page_number
            );
            let outputs = Arc::clone(&self.pool).run(inputs).await?;
            let detections = postprocess::postprocess_page(
                outputs.boxes.view(),
                outputs.count,
                threshold,
                &transform,
            )?;
            tracing::info!(
                "completed PP-DocLayoutV3 inference for page {} with {} detections",
                page_number,
                detections.len()
            );
            Ok(detections)
        })
    }
}

/// Parses and verifies the fixed preprocessing and label contract in inference.yml.
fn verify_model_config(bytes: &[u8]) -> Result<(), LayoutError> {
    let config: InferenceConfig =
        serde_yml::from_slice(bytes).map_err(|source| {
            LayoutError::ModelConfigContentParse {
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
