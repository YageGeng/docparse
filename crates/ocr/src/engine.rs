//! Complete Paddle detection, rectification, orientation and recognition over shared page pixels.
use crate::{
    OcrError,
    decode::Dictionary,
    model::{ModelKind, ModelOutput},
    preprocess::{ImageTensor, TextCrop},
    wasm_compat::SessionRunner,
};
use docparse_config::{OcrConfig, ValidatedConfig};
use docparse_layout::{
    Bbox, ModelArtifacts, PageImage, Polygon, Quad,
    timing::{TimingStage, Timings},
};
use serde::Deserialize;
use std::sync::Arc;
use typed_builder::TypedBuilder;

/// Independent model/config/manifest triples; recognition configuration contains its matching dictionary.
#[derive(Debug, Clone)]
pub struct OcrArtifacts {
    pub detection: ModelArtifacts,
    pub recognition: ModelArtifacts,
    pub orientation: Option<ModelArtifacts>,
}

/// A recognized line in original-image pixels, with corners ordered along the text's reading direction.
#[derive(Debug, Clone)]
pub struct RecognizedText {
    pub text: String,
    pub confidence: f64,
    pub quad: Quad,
}

#[derive(Deserialize)]
struct RecognitionConfig {
    #[serde(rename = "PostProcess")]
    postprocess: RecognitionPostprocess,
}
#[derive(Deserialize)]
struct RecognitionPostprocess {
    name: String,
    character_dict: Vec<String>,
}

/// Reusable OCR pipeline; model sessions and their accelerator resources are initialized once.
#[derive(TypedBuilder)]
#[builder(builder_method(vis = "pub(crate)"))]
pub struct PaddleOcrEngine {
    config: OcrConfig,
    detector: Arc<SessionRunner>,
    recognizer: Arc<SessionRunner>,
    #[builder(default)]
    classifier: Option<Arc<SessionRunner>>,
    dictionary: Dictionary,
    permit: tokio::sync::Semaphore,
}

impl PaddleOcrEngine {
    /// Verifies all model identities and creates strict provider sessions before accepting images.
    pub async fn from_artifacts(
        config: Arc<ValidatedConfig>,
        artifacts: OcrArtifacts,
    ) -> Result<Self, OcrError> {
        let options = config.ocr().clone();
        let orientation = options.classify_orientation;
        let (artifacts, dictionary) =
            docparse_layout::wasm_compat::run_cpu(move || {
                artifacts
                    .detection
                    .verify_against(&ModelKind::Detection.contract())?;
                artifacts
                    .recognition
                    .verify_against(&ModelKind::Recognition.contract())?;
                if orientation {
                    artifacts
                        .orientation
                        .as_ref()
                        .ok_or_else(|| {
                            OcrError::InvalidModel(
                                "orientation artifacts are required".into(),
                            )
                        })?
                        .verify_against(&ModelKind::Orientation.contract())?;
                }
                let config: RecognitionConfig =
                    serde_yml::from_slice(&artifacts.recognition.config)
                        .map_err(|error| {
                            OcrError::InvalidModel(error.to_string())
                        })?;
                if config.postprocess.name != "CTCLabelDecode"
                    || config.postprocess.character_dict.is_empty()
                    || config.postprocess.character_dict.iter().any(|token| {
                        token.is_empty() || token.chars().any(char::is_control)
                    })
                {
                    return Err(OcrError::InvalidModel(
                        "expected a nonempty CTC character dictionary".into(),
                    ));
                }
                let mut dictionary = vec![String::new()];
                dictionary.extend(config.postprocess.character_dict);
                dictionary.push(" ".into());
                Ok::<_, OcrError>((artifacts, Dictionary(dictionary)))
            })
            .await??;
        // All three sessions use the shared compiled backend, or the browser Worker's selected capability.
        let backend =
            docparse_layout::wasm_compat::OnnxBackend::from(config.as_ref());
        tracing::info!(
            "initializing PaddleOCR ONNX with provider {} and {} recognition classes",
            backend.execution_provider(),
            dictionary.0.len()
        );
        let detector = SessionRunner::load(
            artifacts.detection.model,
            backend,
            ModelKind::Detection,
        )
        .await?;
        let recognizer = SessionRunner::load(
            artifacts.recognition.model,
            backend,
            ModelKind::Recognition,
        )
        .await?;
        let classifier = if orientation {
            let model = artifacts.orientation.ok_or_else(|| {
                OcrError::InvalidModel("missing orientation model".into())
            })?;
            Some(
                SessionRunner::load(
                    model.model,
                    backend,
                    ModelKind::Orientation,
                )
                .await?,
            )
        } else {
            None
        };
        Ok(Self::builder()
            // Let different model stages overlap across bounded native pages without duplicating sessions.
            .permit(tokio::sync::Semaphore::new(
                options
                    .max_in_flight
                    .min(crate::wasm_compat::MAX_PAGE_CONCURRENCY),
            ))
            .config(options)
            .detector(detector)
            .recognizer(recognizer)
            .classifier(classifier)
            .dictionary(dictionary)
            .build())
    }

    /// Loads configured native artifact directories; browser callers use from_artifacts.
    pub async fn from_config(
        config: Arc<ValidatedConfig>,
    ) -> Result<Self, OcrError> {
        let artifacts = OcrArtifacts::from_config(config.ocr()).await?;
        Self::from_artifacts(config, artifacts).await
    }

    /// Detects a whole image and recognizes only requested areas; an empty area list selects the entire image.
    pub async fn recognize(
        &self,
        image: Arc<PageImage>,
        regions: Vec<Bbox>,
        timings: Timings,
    ) -> Result<Vec<RecognizedText>, OcrError> {
        // Limit page tensors while independent detector/recognizer sessions overlap across pages.
        let queued = timings.start(TimingStage::OcrQueue);
        let _permit = self.permit.acquire().await.map_err(|_closed| {
            OcrError::InvalidData("OCR engine is closed".into())
        })?;
        drop(queued);
        let source = Arc::clone(&image);
        let limit = self.config.detection_max_side;
        let timer = timings.clone();
        let input = docparse_layout::wasm_compat::run_cpu(move || {
            let _timer = timer.start(TimingStage::OcrDetectionPreprocess);
            ImageTensor::detection(&source, limit)
        })
        .await??;
        let ModelOutput::Detection(map) = Arc::clone(&self.detector)
            .run(input, timings.clone())
            .await?
        else {
            return Err(OcrError::InvalidData(
                "unexpected detector output".into(),
            ));
        };
        let options = self.config.clone();
        let timer = timings.clone();
        let (width, height) = (image.width(), image.height());
        let quads = docparse_layout::wasm_compat::run_cpu(move || {
            let _timer = timer.start(TimingStage::OcrDetectionPostprocess);
            map.decode(width, height, &options)
        })
        .await??;
        let detected = quads.len();
        let recognized =
            self.recognize_lines(image, quads, regions, timings).await?;
        tracing::debug!(
            "PaddleOCR retained {} text lines from {} detected regions",
            recognized.len(),
            detected
        );
        Ok(recognized)
    }

    /// Batches equal-width lines with bounded crop ownership, then restores original detection order.
    async fn recognize_lines(
        &self,
        image: Arc<PageImage>,
        quads: Vec<Quad>,
        regions: Vec<Bbox>,
        timings: Timings,
    ) -> Result<Vec<RecognizedText>, OcrError> {
        let max_width = self.config.recognition_max_width;
        let batch_size = self
            .config
            .batch_size
            .min(crate::wasm_compat::MAX_LINE_BATCH);
        let mut selected = Vec::new();
        for (index, quad) in quads.into_iter().enumerate() {
            let bbox = Polygon::from(quad.clone()).bbox();
            if !regions.is_empty()
                && !regions.iter().any(|region| {
                    region.intersection_area(bbox)
                        / region.area().min(bbox.area()).max(f64::EPSILON)
                        >= 0.5
                })
            {
                continue;
            }
            selected.push((
                index,
                TextCrop::tensor_width(&quad, max_width)?,
                quad,
            ));
        }
        // Equal tensor widths retain the old padding and attention context. Only metadata is pooled.
        // ponytail: exact widths limit batch fill; wider buckets need OCR output comparisons first.
        // Batch one preserves the previous scheduling order for reproducible baseline measurements.
        if batch_size > 1 {
            selected.sort_by_key(|line| line.1);
        }
        let mut recognized = Vec::new();
        // Orientation has a fixed shape, so fill its batches across recognition-width boundaries.
        // Only this bounded crop window is materialized; recognition retains exact-width padding.
        for lines in selected.chunks(batch_size) {
            let lines = lines.to_vec();
            let source = Arc::clone(&image);
            let timer = timings.clone();
            let classify = self.classifier.is_some();
            let (crops, orientation_input) =
                docparse_layout::wasm_compat::run_cpu(move || {
                    let _timer =
                        timer.start(TimingStage::OcrRecognitionPreprocess);
                    let crops = lines
                        .into_iter()
                        .map(|(index, width, quad)| {
                            TextCrop::try_from((&*source, &quad))
                                .map(|crop| (index, width, crop))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    let input = if classify {
                        Some(ImageTensor::orientation(
                            &crops
                                .iter()
                                .map(|(_, _, crop)| &crop.image)
                                .collect::<Vec<_>>(),
                        )?)
                    } else {
                        None
                    };
                    Ok::<_, OcrError>((crops, input))
                })
                .await??;
            let orientations = if let (Some(classifier), Some(input)) =
                (&self.classifier, orientation_input)
            {
                let ModelOutput::Orientation(results) =
                    Arc::clone(classifier).run(input, timings.clone()).await?
                else {
                    return Err(OcrError::InvalidData(
                        "unexpected classifier output".into(),
                    ));
                };
                Some(results)
            } else {
                None
            };
            let threshold = self.config.orientation_threshold;
            let timer = timings.clone();
            let groups = docparse_layout::wasm_compat::run_cpu(move || {
                let _timer = timer.start(TimingStage::OcrRecognitionPreprocess);
                let mut crops = crops;
                if let Some(orientations) = orientations {
                    for ((_, _, crop), result) in
                        crops.iter_mut().zip(orientations)
                    {
                        if result.rotated && result.confidence >= threshold {
                            crop.rotate_half_turn()?;
                        }
                    }
                }
                crops
                    .chunk_by(|a, b| a.1 == b.1)
                    .map(|group| {
                        let input = ImageTensor::recognition(
                            &group
                                .iter()
                                .map(|(_, _, crop)| &crop.image)
                                .collect::<Vec<_>>(),
                            max_width,
                        )?;
                        let positions = group
                            .iter()
                            .map(|(index, _, crop)| (*index, crop.quad.clone()))
                            .collect::<Vec<_>>();
                        Ok::<_, OcrError>((positions, input))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .await??;
            // The crop pixels have been released; only bounded tensors and position metadata remain.
            for (positions, input) in groups {
                let ModelOutput::Recognition(results) =
                    Arc::clone(&self.recognizer)
                        .run(input, timings.clone())
                        .await?
                else {
                    return Err(OcrError::InvalidData(
                        "unexpected recognizer output".into(),
                    ));
                };
                for ((index, quad), steps) in positions.into_iter().zip(results)
                {
                    let _timer = timings.start(TimingStage::OcrDecode);
                    let (text, confidence) = self.dictionary.decode(steps)?;
                    let text = text.trim().to_owned();
                    if !text.is_empty()
                        && confidence >= self.config.recognition_threshold
                    {
                        recognized.push((
                            index,
                            RecognizedText {
                                text,
                                confidence,
                                quad,
                            },
                        ));
                    }
                }
            }
        }
        recognized.sort_unstable_by_key(|(index, _)| *index);
        Ok(recognized.into_iter().map(|(_, line)| line).collect())
    }
}
