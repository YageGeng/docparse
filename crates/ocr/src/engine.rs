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
        let mut recognized = Vec::new();
        for quad in quads {
            let bbox = Polygon::from(quad.clone()).bbox();
            if !regions.is_empty()
                && !regions.iter().any(|region| {
                    // Share the same scalar overlap contract as OCR planning and text fusion.
                    region.intersection_area(bbox)
                        / region.area().min(bbox.area()).max(f64::EPSILON)
                        >= 0.5
                })
            {
                continue;
            }
            let source = Arc::clone(&image);
            let timer = timings.clone();
            let mut crop = docparse_layout::wasm_compat::run_cpu(move || {
                let _timer = timer.start(TimingStage::OcrRecognitionPreprocess);
                TextCrop::try_from((&*source, &quad))
            })
            .await??;
            if let Some(classifier) = &self.classifier {
                let input = ImageTensor::orientation(&crop.image)?;
                if let ModelOutput::Orientation {
                    rotated,
                    confidence,
                } =
                    Arc::clone(classifier).run(input, timings.clone()).await?
                    && rotated
                    && confidence >= self.config.orientation_threshold
                {
                    crop.rotate_half_turn()?;
                }
            }
            let quad = crop.quad;
            let limit = self.config.recognition_max_width;
            let timer = timings.clone();
            let input = docparse_layout::wasm_compat::run_cpu(move || {
                let _timer = timer.start(TimingStage::OcrRecognitionPreprocess);
                ImageTensor::recognition(&crop.image, limit)
            })
            .await??;
            let ModelOutput::Recognition(steps) = Arc::clone(&self.recognizer)
                .run(input, timings.clone())
                .await?
            else {
                return Err(OcrError::InvalidData(
                    "unexpected recognizer output".into(),
                ));
            };
            let (text, confidence) = {
                let _timer = timings.start(TimingStage::OcrDecode);
                self.dictionary.decode(steps)?
            };
            let text = text.trim().to_owned();
            if !text.is_empty()
                && confidence >= self.config.recognition_threshold
            {
                recognized.push(RecognizedText {
                    text,
                    confidence,
                    quad,
                });
            }
        }
        tracing::debug!(
            "PaddleOCR retained {} text lines from {} detected regions",
            recognized.len(),
            detected
        );
        Ok(recognized)
    }
}
