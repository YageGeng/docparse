//! Pinned Paddle model identity, output decoding, and the public inference engine.
use crate::{
    TsrArtifacts,
    artifacts::ModelKind,
    preprocess::{CellInput, ModelInput, SlanetInput},
    wasm_compat::SessionRunner,
};
use docparse_common::timing::{TimingStage, Timings};
use docparse_layout::PageImage;
use ndarray::{Array2, Array3, Axis, Ix1, Ix2, Ix3};
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
    Task(#[from] docparse_common::TaskError),
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
#[derive(Debug, Clone, serde::Serialize, typed_builder::TypedBuilder)]
pub struct TsrPrediction {
    pub structure_tokens: Vec<String>,
    pub cell_bboxes: Vec<Vec<f64>>,
    pub score: f64,
    /// Independent detections are unordered and need not match the structure cell count.
    #[builder(default)]
    pub detected_cell_bboxes: Vec<Vec<f64>>,
}

/// Copied outputs never retain a session-owned native or browser buffer.
pub(crate) enum ModelResult {
    Structure(ModelOutputs),
    Cells(Array2<f32>),
    Tatr(crate::tatr::TatrOutput),
}

impl ModelResult {
    /// Splits model outputs by sample, using detector counts rather than assuming equal box counts.
    pub(crate) fn from_batch(
        kind: ModelKind,
        batch: usize,
        outputs: &SessionOutputs<'_>,
    ) -> Result<Vec<Self>, TsrError> {
        let invalid = |reason: &str| TsrError::InvalidInput {
            reason: reason.to_owned(),
        };
        if !(1..=32).contains(&batch) {
            return Err(invalid("invalid TSR output batch size"));
        }
        match kind {
            // Decode TATR's fixed object queries independently of Paddle's token sequence.
            ModelKind::Structure(docparse_config::TsrModel::Tatr) => {
                Ok(crate::tatr::TatrOutput::from_batch(outputs, batch)?
                    .into_iter()
                    .map(Self::Tatr)
                    .collect())
            }
            ModelKind::Structure(_) => {
                let outputs = ModelOutputs::try_from(outputs)?;
                if outputs.locations.len_of(Axis(0)) != batch {
                    return Err(invalid(
                        "TSR output batch does not match its input",
                    ));
                }
                Ok((0..batch)
                    .map(|index| {
                        Self::Structure(ModelOutputs {
                            locations: outputs
                                .locations
                                .slice_axis(Axis(0), (index..index + 1).into())
                                .to_owned(),
                            probabilities: outputs
                                .probabilities
                                .slice_axis(Axis(0), (index..index + 1).into())
                                .to_owned(),
                        })
                    })
                    .collect())
            }
            ModelKind::Cells(_) => {
                let boxes = outputs
                    .get("fetch_name_0")
                    .ok_or_else(|| TsrError::InvalidInput {
                        reason: "missing detector boxes".to_owned(),
                    })?
                    .try_extract_array::<f32>()?
                    .into_dimensionality::<Ix2>()
                    .map_err(|e| TsrError::InvalidInput {
                        reason: e.to_string(),
                    })?;
                if boxes.ncols() != 6
                    || boxes.nrows() > 300 * batch
                    || boxes.iter().any(|v| !v.is_finite())
                {
                    return Err(invalid(
                        "cell detector requires finite [N,6] output with at most 300 cells per image",
                    ));
                }
                let counts = outputs
                    .get("fetch_name_1")
                    .ok_or_else(|| invalid("missing detector box counts"))?
                    .try_extract_array::<i32>()?
                    .into_dimensionality::<Ix1>()
                    .map_err(|error| {
                        invalid(&format!(
                            "detector counts must have shape [B]: {error}"
                        ))
                    })?;
                if counts.len() != batch {
                    return Err(invalid(
                        "detector count batch does not match its input",
                    ));
                }
                let mut offset = 0;
                let mut results = Vec::with_capacity(batch);
                for &count in &counts {
                    let count = usize::try_from(count).map_err(|error| {
                        invalid(&format!(
                            "invalid detector box count {count}: {error}"
                        ))
                    })?;
                    if count > 300 || count > boxes.nrows() - offset {
                        return Err(invalid(
                            "detector box count exceeds the output tensor",
                        ));
                    }
                    let end = offset + count;
                    results.push(Self::Cells(
                        boxes
                            .slice_axis(Axis(0), (offset..end).into())
                            .to_owned(),
                    ));
                    offset = end;
                }
                if offset != boxes.nrows() {
                    return Err(invalid(
                        "detector counts do not cover the output tensor",
                    ));
                }
                Ok(results)
            }
        }
    }
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
        // Preserve the singleton decoder contract after validating and splitting this batched tensor.
        let batch = locations.len_of(Axis(0));
        if !(1..=32).contains(&batch)
            || locations.shape().get(2) != Some(&8)
            || probabilities.len_of(Axis(0)) != batch
            || probabilities.shape().get(2) != Some(&50)
            || locations.shape().get(1) != probabilities.shape().get(1)
            || locations
                .shape()
                .get(1)
                .is_none_or(|&steps| steps == 0 || steps > 512)
        {
            return Err(TsrError::InvalidInput { reason: "expected [B,T,8] cell boxes and [B,T,50] probabilities with 1..=32 crops and 1..=512 steps".to_owned() });
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
        Self::decode(outputs, dictionary, width, height, true)
    }
}

impl TsrPrediction {
    /// SLANeXt contributes topology only; its invalid position head must not enter geometry matching.
    fn decode(
        outputs: ModelOutputs,
        dictionary: &[String],
        width: u32,
        height: u32,
        use_positions: bool,
    ) -> Result<Self, TsrError> {
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
            if use_positions && matches!(token.as_str(), "<td" | "<td></td>") {
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
        if !ended
            || scores.is_empty()
            || !structure_tokens
                .iter()
                .any(|t| matches!(t.as_str(), "<td" | "<td></td>"))
        {
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
        Ok(Self::builder()
            .structure_tokens(structure_tokens)
            .cell_bboxes(cell_bboxes)
            .score(scores.iter().sum::<f64>() / scores.len() as f64)
            .build())
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

/// Configured Paddle structure/cell pipeline; the original public type name remains compatible.
#[derive(typed_builder::TypedBuilder)]
pub struct SlanetPlusEngine {
    runner: Arc<SessionRunner>,
    dictionary: Vec<String>,
    provider: docparse_layout::ExecutionProvider,
    model: docparse_config::TsrModel,
    #[builder(default)]
    detector: Option<(Arc<SessionRunner>, f64)>,
    name: String,
}

impl SlanetPlusEngine {
    /// Verifies immutable artifacts and initializes the configured shared ONNX backend.
    pub async fn from_artifacts(
        config: Arc<docparse_config::ValidatedConfig>,
        artifacts: impl Into<TsrArtifacts>,
    ) -> Result<Self, TsrError> {
        let artifacts = artifacts.into();
        let model = config.tsr().model;
        let kind = ModelKind::Structure(model);
        let cell_config = config
            .tsr()
            .cell_detection
            .as_ref()
            .filter(|cells| cells.enabled)
            .cloned();
        let TsrArtifacts {
            structure: artifacts,
            cell_detection,
        } = artifacts;
        if cell_config.is_some() && cell_detection.is_none() {
            tracing::error!(
                "enabled table cell detection is missing its model artifacts"
            );
            return Err(TsrError::InvalidModel {
                reason: "enabled cell detection requires its own artifacts"
                    .to_owned(),
            });
        }
        tracing::info!(
            "loading TSR {:?} from {} bytes",
            model,
            artifacts.model.len()
        );
        let (artifacts, dictionary) = docparse_common::run_cpu(move || {
            let contract = kind.contract();
            artifacts.verify_against(&contract)?;
            // The pinned TATR preprocessor JSON is valid YAML but has no Paddle token dictionary.
            if model == docparse_config::TsrModel::Tatr {
                return Ok((artifacts, Vec::new()));
            }
            let config: InferenceConfig =
                serde_yml::from_slice(&artifacts.config).map_err(|error| {
                    TsrError::InvalidModel {
                        reason: error.to_string(),
                    }
                })?;
            if format!("PaddlePaddle/{}_onnx", config.global.model_name)
                != contract.repository
            {
                return Err(TsrError::InvalidModel {
                    reason: "TSR YAML does not match the selected model"
                        .to_owned(),
                });
            }
            let mut dictionary = config.postprocess.character_dict;
            dictionary.retain(|token| token != "<td>");
            dictionary.insert(0, "sos".to_owned());
            dictionary.extend(["<td></td>".to_owned(), "eos".to_owned()]);
            if dictionary.len() != 50 {
                return Err(TsrError::InvalidModel {
                    reason: "SLANet_plus needs 50 token classes".to_owned(),
                });
            }
            Ok::<_, TsrError>((artifacts, dictionary))
        })
        .await??;
        let backend =
            docparse_layout::wasm_compat::OnnxBackend::from(config.as_ref());
        let provider = backend.execution_provider();
        tracing::info!("initializing TSR ONNX with provider {}", provider);
        let runner = SessionRunner::load(
            artifacts,
            backend,
            kind,
            config.tsr().batch_size,
            config.tsr().session_size,
            config.tsr().queue_size,
        )
        .await
        .map_err(|error| {
            tracing::error!(
                "TSR provider {} initialization failed: {}",
                provider,
                error
            );
            error
        })?;
        tracing::info!("loaded TSR ONNX with provider {}", provider);
        let model_label = match model {
            docparse_config::TsrModel::Tatr => "tatr-v1.1-all",
            docparse_config::TsrModel::SlanetPlus => "slanet-plus",
            docparse_config::TsrModel::SlanextWired => "slanext-wired",
            docparse_config::TsrModel::SlanextWireless => "slanext-wireless",
        };
        let name = format!(
            "{model_label}-onnx-{provider}{}",
            cell_config.as_ref().map_or(String::new(), |cells| format!(
                "+rtdetr-{}",
                match cells.model {
                    docparse_config::TableCellModel::Wired => "wired",
                    docparse_config::TableCellModel::Wireless => "wireless",
                }
            ))
        );
        let detector = if let Some(cells) = cell_config {
            let artifacts =
                cell_detection.ok_or_else(|| TsrError::InvalidModel {
                    reason: "enabled cell detection requires its own artifacts"
                        .to_owned(),
                })?;
            let kind = ModelKind::Cells(cells.model);
            let artifacts = docparse_common::run_cpu(move || {
                artifacts.verify_against(&kind.contract())?;
                Ok::<_, TsrError>(artifacts)
            })
            .await??;
            tracing::info!(
                "loading table cell detector {:?} with provider {}",
                cells.model,
                provider
            );
            Some((
                SessionRunner::load(
                    artifacts,
                    docparse_layout::wasm_compat::OnnxBackend::from(
                        config.as_ref(),
                    ),
                    kind,
                    cells.batch_size,
                    cells.session_size,
                    cells.queue_size,
                )
                .await?,
                cells.score_threshold,
            ))
        } else {
            None
        };
        Ok(Self::builder()
            .runner(runner)
            .dictionary(dictionary)
            .provider(provider)
            .model(model)
            .detector(detector)
            .name(name)
            .build())
    }

    /// Reports the registered provider; unsupported graph operators may still execute on CPU.
    pub fn execution_provider(&self) -> docparse_layout::ExecutionProvider {
        self.provider
    }

    /// Reports the exact structure family used for model evidence and benchmark labeling.
    pub fn model(&self) -> docparse_config::TsrModel {
        self.model
    }

    /// Identifies both loaded model families and the actual selected provider in logs and evidence.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Runs independent models concurrently and combines their crop-relative predictions.
    pub async fn predict(
        &self,
        image: Arc<PageImage>,
        timings: Timings,
    ) -> Result<TsrPrediction, TsrError> {
        // Keep orchestration separate from model-specific retries and detection filtering.
        let (mut prediction, detected_cell_bboxes) = tokio::try_join!(
            self.predict_structure(Arc::clone(&image), &timings),
            self.detect_cells(image, &timings),
        )?;
        prediction.detected_cell_bboxes = detected_cell_bboxes;
        Ok(prediction)
    }

    /// Retries incomplete structure predictions on image segments and restores original crop coordinates.
    async fn predict_structure(
        &self,
        image: Arc<PageImage>,
        timings: &Timings,
    ) -> Result<TsrPrediction, TsrError> {
        // TATR has no autoregressive truncation; splitting malformed outputs would hide model errors.
        if self.model == docparse_config::TsrModel::Tatr {
            return self.predict_once(image, timings.clone()).await;
        }
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
                            "TSR could not recover the crop at pixel {}: {}",
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
                    tracing::warn!("TSR inference failed: {}", error);
                    return Err(error);
                }
            }
        }
        TsrPrediction::from_segments(parts)
    }

    /// Detects cells once on the original whole-table crop and filters boxes independently of structure retries.
    async fn detect_cells(
        &self,
        image: Arc<PageImage>,
        timings: &Timings,
    ) -> Result<Vec<Vec<f64>>, TsrError> {
        let Some((detector, threshold)) = &self.detector else {
            return Ok(Vec::new());
        };
        let mut detected_cell_bboxes = Vec::new();
        let timer = timings.start(TimingStage::TableCellPreprocess);
        let crop = Arc::clone(&image);
        let input = docparse_common::run_cpu(move || {
            CellInput::try_from(crop.as_ref())
        })
        .await??;
        drop(timer);
        let output = Arc::clone(detector)
            .run(ModelInput::Cells(input), timings.clone())
            .await?;
        let _postprocess = timings.start(TimingStage::TableCellPostprocess);
        let ModelResult::Cells(boxes) = output else {
            return Err(TsrError::InvalidInput {
                reason: "cell session returned structure outputs".to_owned(),
            });
        };
        for cell in boxes.outer_iter() {
            let Some(&[class, score, left, top, right, bottom]) =
                cell.as_slice()
            else {
                return Err(TsrError::InvalidInput {
                    reason: "non-contiguous detector row".to_owned(),
                });
            };
            if f64::from(score) < *threshold || class != 0.0 {
                continue;
            }
            let bbox = [
                f64::from(left).max(0.0),
                f64::from(top).max(0.0),
                f64::from(right).min(f64::from(image.width())),
                f64::from(bottom).min(f64::from(image.height())),
            ];
            if docparse_layout::Bbox::try_from(bbox).is_ok() {
                detected_cell_bboxes.push(bbox.to_vec());
            }
        }
        tracing::info!(
            "detected {} independent table cells for {:?}",
            detected_cell_bboxes.len(),
            self.model
        );
        if detected_cell_bboxes.is_empty() {
            return Err(TsrError::InvalidInput {
                reason: "cell detector returned no accepted boxes".to_owned(),
            });
        }
        Ok(detected_cell_bboxes)
    }

    /// Executes exactly one bounded decoder run, retaining the actual failure for segmented retries.
    async fn predict_once(
        &self,
        image: Arc<PageImage>,
        timings: Timings,
    ) -> Result<TsrPrediction, TsrError> {
        let (width, height) = (image.width(), image.height());
        let preparing = timings.clone();
        let model = self.model;
        let edge = ModelKind::Structure(model).edge();
        let input = docparse_common::run_cpu(move || {
            let _timer = preparing.start(TimingStage::TsrPreprocess);
            // TATR consumes aspect-preserving RGB tensors and masks instead of Paddle BGR squares.
            if model == docparse_config::TsrModel::Tatr {
                crate::tatr::TatrInput::try_from(image.as_ref())
                    .map(ModelInput::Tatr)
            } else {
                SlanetInput::try_from((image.as_ref(), edge))
                    .map(ModelInput::Structure)
            }
        })
        .await??;
        tracing::debug!("starting TSR inference for {}x{} crop", width, height);
        let output = Arc::clone(&self.runner).run(input, timings.clone()).await;
        let result = output.and_then(|output| {
            let _timer = timings.start(TimingStage::TsrPostprocess);
            if let ModelResult::Tatr(output) = output {
                return output.decode(width, height);
            }
            let ModelResult::Structure(output) = output else {
                return Err(TsrError::InvalidInput {
                    reason: "structure session returned detector outputs"
                        .to_owned(),
                });
            };
            TsrPrediction::decode(
                output,
                self.dictionary.as_slice(),
                width,
                height,
                self.model == docparse_config::TsrModel::SlanetPlus,
            )
        });
        match &result {
            Ok(prediction) => tracing::info!(
                "completed TSR inference with {} cells and confidence {:.4}",
                prediction.cell_bboxes.len(),
                prediction.score
            ),
            Err(error) => tracing::debug!(
                "TSR attempt failed for {}x{} crop: {}",
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
            || parts.iter().any(|(_, part)| {
                !part
                    .structure_tokens
                    .iter()
                    .any(|t| matches!(t.as_str(), "<td" | "<td></td>"))
            })
        {
            return Err(TsrError::InvalidInput {
                reason: "empty TSR segment sequence".to_owned(),
            });
        }
        parts.sort_by_key(|(offset, _)| *offset);
        let mut result = TsrPrediction::builder()
            .structure_tokens(vec![
                "<html>".to_owned(),
                "<body>".to_owned(),
                "<table>".to_owned(),
            ])
            .cell_bboxes(Vec::new())
            .score(0.0)
            .build();
        let mut weight = 0;
        for (offset, part) in parts {
            let cells = part
                .structure_tokens
                .iter()
                .filter(|t| matches!(t.as_str(), "<td" | "<td></td>"))
                .count();
            weight += cells;
            result.score += part.score * cells as f64;
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

    /// Structure-only predictions must retain their independent geometry in captured JSON.
    #[test]
    fn serialized_prediction_preserves_independent_cells() {
        let prediction = TsrPrediction::builder()
            .structure_tokens(vec![
                "<tr>".to_owned(),
                "<td></td>".to_owned(),
                "</tr>".to_owned(),
            ])
            .cell_bboxes(Vec::new())
            .score(0.9)
            .detected_cell_bboxes(vec![vec![1.0, 2.0, 30.0, 40.0]])
            .build();
        let value = serde_json::to_value(&prediction).expect("prediction JSON");
        assert_eq!(value.get("cell_bboxes"), Some(&serde_json::json!([])));
        assert_eq!(
            value.get("detected_cell_bboxes"),
            Some(&serde_json::json!([[1.0, 2.0, 30.0, 40.0]]))
        );
    }

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
        let part = TsrPrediction::builder()
            .structure_tokens(
                [
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
            )
            .cell_bboxes(vec![vec![0.0, 0.0, 20.0, 10.0]])
            .score(0.8)
            .build();
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
