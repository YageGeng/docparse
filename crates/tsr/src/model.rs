//! Pinned Paddle model identity, output decoding, and the public inference engine.
use crate::{preprocess::SlanetInput, wasm_compat::SessionRunner};
use docparse_layout::{
    ModelArtifacts, ModelContract, PageImage,
    timing::{TimingStage, Timings},
};
use ndarray::{Array3, Ix3};
use ort::session::SessionOutputs;
use serde::Deserialize;
use std::sync::Arc;

pub const SLANET_PLUS_REVISION: &str =
    "7dbe640e127602bf506815e822c09758de73c482";

/// Model-local failures preserve typed causes without exposing runtime handles.
#[derive(Debug, thiserror::Error)]
pub enum TsrError {
    #[error("TSR artifact verification failed: {0}")]
    Manifest(#[from] docparse_layout::ModelManifestError),
    #[error("TSR backend initialization failed: {0}")]
    Backend(#[from] docparse_layout::LayoutError),
    #[error("TSR ONNX Runtime failed: {0}")]
    Ort(#[from] ort::Error),
    #[error("TSR worker failed: {0}")]
    Task(#[from] docparse_layout::wasm_compat::TaskError),
    #[error("invalid TSR model configuration: {reason}")]
    InvalidModel { reason: String },
    #[error("invalid TSR input or output: {reason}")]
    InvalidInput { reason: String },
    #[error("TSR inference failed: {message}")]
    Inference { message: String },
    #[error("browser TSR initialization requires model artifacts")]
    ModelArtifactsRequired,
}

/// The model predicts geometry and structure; PDF text is deliberately absent.
#[derive(Debug, Clone)]
pub struct TsrPrediction {
    pub structure_tokens: Vec<String>,
    pub cell_bboxes: Vec<Vec<f64>>,
    pub score: f64,
}

/// Owned outputs survive the ORT session lease and browser synchronization.
pub(crate) struct ModelOutputs {
    locations: Array3<f32>,
    probabilities: Array3<f32>,
}

impl TryFrom<&SessionOutputs<'_>> for ModelOutputs {
    type Error = TsrError;

    /// Validates the fixed tensor contract before copying the bounded prediction.
    fn try_from(outputs: &SessionOutputs<'_>) -> Result<Self, Self::Error> {
        let missing = |name| TsrError::InvalidInput {
            reason: format!("missing TSR output {name}"),
        };
        let locations = outputs
            .get("fetch_name_0")
            .ok_or_else(|| missing("fetch_name_0"))?
            .try_extract_array::<f32>()?
            .into_dimensionality::<Ix3>()
            .map_err(|error| TsrError::InvalidInput {
                reason: error.to_string(),
            })?;
        let probabilities = outputs
            .get("fetch_name_1")
            .ok_or_else(|| missing("fetch_name_1"))?
            .try_extract_array::<f32>()?
            .into_dimensionality::<Ix3>()
            .map_err(|error| TsrError::InvalidInput {
                reason: error.to_string(),
            })?;
        if locations.shape().first() != Some(&1)
            || locations.shape().get(2) != Some(&8)
            || probabilities.shape().first() != Some(&1)
            || probabilities.shape().get(2) != Some(&50)
            || locations.shape().get(1) != probabilities.shape().get(1)
            || locations
                .shape()
                .get(1)
                .is_none_or(|&steps| steps == 0 || steps > 512)
        {
            return Err(TsrError::InvalidInput { reason: "expected [1,T,8] cell boxes and [1,T,50] probabilities with 1..=512 steps".to_owned() });
        }
        Ok(Self {
            locations: locations.to_owned(),
            probabilities: probabilities.to_owned(),
        })
    }
}

impl TryFrom<(ModelOutputs, &[String], u32, u32)> for TsrPrediction {
    type Error = TsrError;

    /// Decodes argmax tokens and converts selected location predictions into crop-pixel boxes.
    fn try_from(
        (outputs, dictionary, width, height): (
            ModelOutputs,
            &[String],
            u32,
            u32,
        ),
    ) -> Result<Self, Self::Error> {
        let invalid = |reason: &str| TsrError::InvalidInput {
            reason: reason.to_owned(),
        };
        if dictionary.len() != 50 || width == 0 || height == 0 {
            return Err(invalid(
                "invalid TSR dictionary or original image dimensions",
            ));
        }
        let mut structure_tokens = vec![
            "<html>".to_owned(),
            "<body>".to_owned(),
            "<table>".to_owned(),
        ];
        let mut cell_bboxes = Vec::new();
        let mut scores = Vec::new();
        let mut ended = false;
        let locations = outputs.locations.index_axis(ndarray::Axis(0), 0);
        let probabilities =
            outputs.probabilities.index_axis(ndarray::Axis(0), 0);
        for (location, probabilities) in
            locations.outer_iter().zip(probabilities.outer_iter())
        {
            if probabilities
                .iter()
                .any(|v| !v.is_finite() || *v < 0.0 || *v > 1.0)
            {
                return Err(invalid(
                    "non-finite or out-of-range TSR probability",
                ));
            }
            // A strict comparison retains the first class on ties, matching NumPy argmax.
            let (mut selected, mut score) = (0, f32::NEG_INFINITY);
            for (index, &value) in probabilities.iter().enumerate() {
                if value > score {
                    selected = index;
                    score = value;
                }
            }
            if selected == 49 {
                ended = true;
                break;
            }
            if selected == 0 {
                continue;
            }
            let token = dictionary
                .get(selected)
                .ok_or_else(|| invalid("TSR class index outside dictionary"))?;
            if matches!(token.as_str(), "<td" | "<td></td>") {
                if location.iter().any(|v| !v.is_finite()) {
                    return Err(invalid("non-finite cell location"));
                }
                let mut left = f64::INFINITY;
                let mut top = f64::INFINITY;
                let mut right = f64::NEG_INFINITY;
                let mut bottom = f64::NEG_INFINITY;
                for (index, &coordinate) in location.iter().enumerate() {
                    let coordinate = (f64::from(coordinate)
                        * f64::from(width.max(height)))
                    .trunc();
                    if index % 2 == 0 {
                        left = left.min(coordinate);
                        right = right.max(coordinate);
                    } else {
                        top = top.min(coordinate);
                        bottom = bottom.max(coordinate);
                    }
                }
                // The learned position head may overshoot an edge; it cannot enlarge the source crop.
                let bounds = [
                    left.max(0.0),
                    top.max(0.0),
                    right.min(f64::from(width)),
                    bottom.min(f64::from(height)),
                ];
                let bbox = docparse_layout::Bbox::try_from(bounds).map_err(
                    |error| invalid(&format!("empty predicted cell: {error}")),
                )?;
                cell_bboxes.push(vec![
                    bbox.left,
                    bbox.top,
                    bbox.right,
                    bbox.bottom,
                ]);
            }
            structure_tokens.push(token.clone());
            scores.push(f64::from(score));
        }
        if !ended || scores.is_empty() || cell_bboxes.is_empty() {
            return Err(invalid(&format!(
                "TSR output is incomplete: ended={ended}, steps={}, tokens={}, cells={}",
                locations.len_of(ndarray::Axis(0)),
                scores.len(),
                cell_bboxes.len()
            )));
        }
        structure_tokens.extend([
            "</table>".to_owned(),
            "</body>".to_owned(),
            "</html>".to_owned(),
        ]);
        Ok(Self {
            structure_tokens,
            cell_bboxes,
            score: scores.iter().sum::<f64>() / scores.len() as f64,
        })
    }
}

#[derive(Deserialize)]
struct InferenceConfig {
    #[serde(rename = "Global")]
    global: GlobalConfig,
    #[serde(rename = "PostProcess")]
    postprocess: PostprocessConfig,
}
#[derive(Deserialize)]
struct GlobalConfig {
    model_name: String,
}
#[derive(Deserialize)]
struct PostprocessConfig {
    character_dict: Vec<String>,
}

/// Independent, reusable SLANet_plus engine with one cancellation-safe ONNX session.
pub struct SlanetPlusEngine {
    runner: Arc<SessionRunner>,
    dictionary: Vec<String>,
    provider: docparse_config::ExecutionProviderConfig,
}

impl SlanetPlusEngine {
    /// Verifies immutable artifacts and initializes the configured shared ONNX backend.
    pub async fn from_artifacts(
        config: Arc<docparse_config::ValidatedConfig>,
        artifacts: ModelArtifacts,
    ) -> Result<Self, TsrError> {
        tracing::info!(
            "loading SLANet_plus ONNX revision {} from {} bytes",
            SLANET_PLUS_REVISION,
            artifacts.model.len()
        );
        let (artifacts, dictionary) = docparse_layout::wasm_compat::run_cpu(move || {
            let contract = ModelContract::builder()
                .repository("PaddlePaddle/SLANet_plus_onnx".to_owned()).revision(SLANET_PLUS_REVISION.to_owned())
                .license("Apache-2.0".to_owned())
                .model_sha256("7790c0c13ce064782c9d22ebeb16b4da8216f83d3ba576da962c106ef58386da".to_owned())
                .config_sha256("8a6372d3269a6f112fe13a2da7952a84da6e112c10a3146cbb43de5bd01d19fa".to_owned()).build();
            artifacts.verify_against(&contract)?;
            let config: InferenceConfig = serde_yml::from_slice(&artifacts.config).map_err(|error| TsrError::InvalidModel { reason: error.to_string() })?;
            if config.global.model_name != "SLANet_plus" { return Err(TsrError::InvalidModel { reason: "model name must be SLANet_plus".to_owned() }); }
            let mut dictionary = config.postprocess.character_dict;
            dictionary.retain(|token| token != "<td>");
            dictionary.insert(0, "sos".to_owned());
            dictionary.extend(["<td></td>".to_owned(), "eos".to_owned()]);
            if dictionary.len() != 50 { return Err(TsrError::InvalidModel { reason: "SLANet_plus needs 50 token classes".to_owned() }); }
            Ok::<_, TsrError>((artifacts, dictionary))
        }).await??;
        let provider = config.tsr().execution_provider;
        tracing::info!(
            "initializing SLANet_plus ONNX with provider {}",
            provider
        );
        let runner = SessionRunner::load(artifacts, provider).await.map_err(
            |error| {
                tracing::error!(
                    "SLANet_plus provider {} initialization failed: {}",
                    provider,
                    error
                );
                error
            },
        )?;
        tracing::info!("loaded SLANet_plus ONNX with provider {}", provider);
        Ok(Self {
            runner,
            dictionary,
            provider,
        })
    }

    /// Reports the registered provider; unsupported graph operators may still execute on CPU.
    pub fn execution_provider(
        &self,
    ) -> docparse_config::ExecutionProviderConfig {
        self.provider
    }

    /// Runs a real model on the supplied crop; geometry remains in that crop's original pixel frame.
    pub async fn predict(
        &self,
        image: Arc<PageImage>,
        timings: Timings,
    ) -> Result<TsrPrediction, TsrError> {
        let mut pending = vec![crate::tiles::TableSlice::new(image)];
        let mut parts = Vec::new();
        while let Some(slice) = pending.pop() {
            match self
                .predict_once(Arc::clone(&slice.image), timings.clone())
                .await
            {
                Ok(mut prediction) => {
                    for bbox in &mut prediction.cell_bboxes {
                        for coordinate in bbox.iter_mut().skip(1).step_by(2) {
                            *coordinate += f64::from(slice.offset);
                        }
                    }
                    parts.push((slice.offset, prediction));
                }
                Err(error @ TsrError::InvalidInput { .. }) => {
                    let Some([top, bottom]) = slice.split() else {
                        tracing::warn!(
                            "SLANet_plus could not recover the crop at pixel {}: {}",
                            slice.offset,
                            error
                        );
                        return Err(error);
                    };
                    tracing::info!(
                        "retrying incomplete TSR crop at pixel {} as two image segments after {}",
                        bottom.offset,
                        error
                    );
                    pending.extend([bottom, top]);
                }
                Err(error) => {
                    tracing::warn!("SLANet_plus inference failed: {}", error);
                    return Err(error);
                }
            }
        }
        TsrPrediction::from_segments(parts)
    }

    /// Executes exactly one bounded decoder run, retaining the actual failure for segmented retries.
    async fn predict_once(
        &self,
        image: Arc<PageImage>,
        timings: Timings,
    ) -> Result<TsrPrediction, TsrError> {
        let (width, height) = (image.width(), image.height());
        let preparing = timings.clone();
        let input = docparse_layout::wasm_compat::run_cpu(move || {
            let _timer = preparing.start(TimingStage::TsrPreprocess);
            SlanetInput::try_from(image.as_ref())
        })
        .await??;
        tracing::debug!(
            "starting SLANet_plus inference for {}x{} crop",
            width,
            height
        );
        let output = Arc::clone(&self.runner).run(input, timings.clone()).await;
        let result = output.and_then(|output| {
            let _timer = timings.start(TimingStage::TsrPostprocess);
            TsrPrediction::try_from((
                output,
                self.dictionary.as_slice(),
                width,
                height,
            ))
        });
        match &result {
            Ok(prediction) => tracing::info!(
                "completed SLANet_plus inference with {} cells and confidence {:.4}",
                prediction.cell_bboxes.len(),
                prediction.score
            ),
            Err(error) => tracing::debug!(
                "SLANet_plus attempt failed for {}x{} crop: {}",
                width,
                height,
                error
            ),
        }
        result
    }
}

impl TsrPrediction {
    /// Stitches completed image segments without promoting continuation rows into new column headers.
    fn from_segments(mut parts: Vec<(u32, Self)>) -> Result<Self, TsrError> {
        if parts.is_empty()
            || parts.iter().any(|(_, part)| part.cell_bboxes.is_empty())
        {
            return Err(TsrError::InvalidInput {
                reason: "empty TSR segment sequence".to_owned(),
            });
        }
        parts.sort_by_key(|(offset, _)| *offset);
        let mut result = TsrPrediction {
            structure_tokens: vec![
                "<html>".to_owned(),
                "<body>".to_owned(),
                "<table>".to_owned(),
            ],
            cell_bboxes: Vec::new(),
            score: 0.0,
        };
        let mut weight = 0;
        for (offset, part) in parts {
            weight += part.cell_bboxes.len();
            result.score += part.score * part.cell_bboxes.len() as f64;
            result.structure_tokens.extend(
                part.structure_tokens
                    .into_iter()
                    .filter(|token| {
                        !matches!(
                            token.as_str(),
                            "<html>"
                                | "</html>"
                                | "<body>"
                                | "</body>"
                                | "<table>"
                                | "</table>"
                        )
                    })
                    .map(|token| match (offset > 0, token.as_str()) {
                        // A continuation crop cannot introduce a new document-level column header.
                        (true, "<thead>") => "<tbody>".to_owned(),
                        (true, "</thead>") => "</tbody>".to_owned(),
                        _ => token,
                    }),
            );
            result.cell_bboxes.extend(part.cell_bboxes);
        }
        result.score /= weight as f64;
        result.structure_tokens.extend([
            "</table>".to_owned(),
            "</body>".to_owned(),
            "</html>".to_owned(),
        ]);
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decoder limit must not silently turn a table prefix into a complete prediction.
    #[test]
    fn incomplete_sequences_are_rejected_before_publication() {
        let mut dictionary = vec![String::new(); 50];
        for (index, token) in
            [(1, "<tr>"), (2, "<td></td>"), (3, "</tr>"), (49, "eos")]
        {
            *dictionary.get_mut(index).expect("class") = token.to_owned();
        }
        for ended in [false, true] {
            let steps = if ended { 4 } else { 3 };
            let mut probabilities = Array3::zeros((1, steps, 50));
            for (step, class) in
                [1, 2, 3, 49].into_iter().take(steps).enumerate()
            {
                *probabilities
                    .get_mut((0, step, class))
                    .expect("probability") = 1.0;
            }
            let locations =
                Array3::from_shape_fn((1, steps, 8), |(_, _, coordinate)| {
                    match coordinate {
                        0 | 1 | 3 | 6 => 0.1,
                        _ => 0.9,
                    }
                });
            let result = TsrPrediction::try_from((
                ModelOutputs {
                    locations,
                    probabilities,
                },
                dictionary.as_slice(),
                100,
                100,
            ));
            if ended {
                assert_eq!(result.expect("complete").cell_bboxes.len(), 1);
            } else {
                assert!(
                    result
                        .expect_err("truncated prefix")
                        .to_string()
                        .contains("ended=false")
                );
            }
        }
    }

    /// Continuation crops retain data rows and already restored coordinates without repeating a column header.
    #[test]
    fn segment_stitching_preserves_positions_and_header_scope() {
        let part = TsrPrediction {
            structure_tokens: [
                "<html>",
                "<body>",
                "<table>",
                "<thead>",
                "<tr>",
                "<td></td>",
                "</tr>",
                "</thead>",
                "</table>",
                "</body>",
                "</html>",
            ]
            .map(str::to_owned)
            .to_vec(),
            cell_bboxes: vec![vec![0.0, 0.0, 20.0, 10.0]],
            score: 0.8,
        };
        let mut later = part.clone();
        later.cell_bboxes = vec![vec![0.0, 100.0, 20.0, 110.0]];
        let merged =
            TsrPrediction::from_segments(vec![(100, later), (0, part)])
                .expect("segments");
        assert_eq!(
            merged
                .structure_tokens
                .iter()
                .filter(|t| t.as_str() == "<thead>")
                .count(),
            1
        );
        assert_eq!(
            merged
                .structure_tokens
                .iter()
                .filter(|t| t.as_str() == "<tbody>")
                .count(),
            1
        );
        assert_eq!(
            merged.cell_bboxes.get(1),
            Some(&vec![0.0, 100.0, 20.0, 110.0])
        );
        TsrPrediction::from_segments(Vec::new())
            .expect_err("empty segment sequence");
    }
}
