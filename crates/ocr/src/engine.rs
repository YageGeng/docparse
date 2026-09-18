//! Complete Paddle detection, rectification, orientation and recognition over shared page pixels.
use crate::{
    OcrError,
    decode::Dictionary,
    model::{ModelKind, ModelOutput},
    preprocess::{ImageTensor, TextCrop},
    wasm_compat::SessionRunner,
};
use docparse_common::timing::{TimingStage, Timings};
use docparse_config::{OcrConfig, ValidatedConfig};
use docparse_layout::{Bbox, ModelArtifacts, PageImage, Polygon, Quad};
use futures_util::{StreamExt, stream};
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
}

impl PaddleOcrEngine {
    /// Verifies all model identities and creates strict provider sessions before accepting images.
    pub async fn from_artifacts(
        config: Arc<ValidatedConfig>,
        artifacts: OcrArtifacts,
    ) -> Result<Self, OcrError> {
        let options = config.ocr().clone();
        let orientation = options.classify_orientation;
        let (artifacts, dictionary) = docparse_common::run_cpu(move || {
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
            let config: RecognitionConfig = serde_yml::from_slice(
                &artifacts.recognition.config,
            )
            .map_err(|error| OcrError::InvalidModel(error.to_string()))?;
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
        // Every model consumer uses the shared compiled backend or the browser Worker's selected capability.
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
            options.detection.session_size,
            options.detection.batch_size,
            options.detection.queue_size,
        )
        .await?;
        let recognizer = SessionRunner::load(
            artifacts.recognition.model,
            backend,
            ModelKind::Recognition,
            options.recognition.session_size,
            options.recognition.batch_size,
            options.recognition.queue_size,
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
                    options.orientation.session_size,
                    options.orientation.batch_size,
                    options.orientation.queue_size,
                )
                .await?,
            )
        } else {
            None
        };
        Ok(Self::builder()
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
        // Pages submit independently; each model queue applies backpressure and its sessions own inference concurrency.
        let source = Arc::clone(&image);
        let limit = self.config.detection_max_side;
        let timer = timings.clone();
        let input = docparse_common::run_cpu(move || {
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
        let quads = docparse_common::run_cpu(move || {
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

    /// Publishes individual lines through bounded admission and restores detection order after shared-queue inference.
    async fn recognize_lines(
        &self,
        image: Arc<PageImage>,
        quads: Vec<Quad>,
        regions: Vec<Bbox>,
        timings: Timings,
    ) -> Result<Vec<RecognizedText>, OcrError> {
        let max_width = self.config.recognition_max_width;
        // Only active models contribute to admission; disabled orientation must not retain extra crops or tensors.
        let window = (self.config.recognition.session_size
            * self.config.recognition.batch_size
            + self.config.recognition.queue_size)
            .max(if self.classifier.is_some() {
                self.config.orientation.session_size
                    * self.config.orientation.batch_size
                    + self.config.orientation.queue_size
            } else {
                0
            });
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
        // Width ordering improves physical batches without coupling admission to a page-local chunk.
        selected.sort_by_key(|line| line.1);
        let requests =
            stream::iter(selected.into_iter().map(|(index, _, quad)| {
                let source = Arc::clone(&image);
                let timings = timings.clone();
                async move {
                    let timer = timings.clone();
                    let classify = self.classifier.is_some();
                    let (mut crop, orientation) =
                        docparse_common::run_cpu(move || {
                            let _timer = timer
                                .start(TimingStage::OcrRecognitionPreprocess);
                            let crop = TextCrop::try_from((&*source, &quad))?;
                            let input = classify
                                .then(|| {
                                    ImageTensor::orientation(&[&crop.image])
                                })
                                .transpose()?;
                            Ok::<_, OcrError>((crop, input))
                        })
                        .await??;
                    if let (Some(classifier), Some(input)) =
                        (&self.classifier, orientation)
                    {
                        let ModelOutput::Orientation(mut results) =
                            Arc::clone(classifier)
                                .run(input, timings.clone())
                                .await?
                        else {
                            return Err(OcrError::InvalidData(
                                "unexpected classifier output".into(),
                            ));
                        };
                        let result = results.pop().ok_or_else(|| {
                            OcrError::InvalidData(
                                "missing orientation result".into(),
                            )
                        })?;
                        if result.rotated
                            && result.confidence
                                >= self.config.orientation_threshold
                        {
                            crop.rotate_half_turn()?;
                        }
                    }
                    let timer = timings.clone();
                    let (quad, input) = docparse_common::run_cpu(move || {
                        let _timer =
                            timer.start(TimingStage::OcrRecognitionPreprocess);
                        let input = ImageTensor::recognition(
                            &[&crop.image],
                            max_width,
                        )?;
                        Ok::<_, OcrError>((crop.quad, input))
                    })
                    .await??;
                    let ModelOutput::Recognition(mut results) =
                        Arc::clone(&self.recognizer)
                            .run(input, timings.clone())
                            .await?
                    else {
                        return Err(OcrError::InvalidData(
                            "unexpected recognizer output".into(),
                        ));
                    };
                    let steps = results.pop().ok_or_else(|| {
                        OcrError::InvalidData(
                            "missing recognition result".into(),
                        )
                    })?;
                    let _timer = timings.start(TimingStage::OcrDecode);
                    let (text, confidence) = self.dictionary.decode(steps)?;
                    let text = text.trim().to_owned();
                    Ok::<_, OcrError>(
                        (!text.is_empty()
                            && confidence >= self.config.recognition_threshold)
                            .then_some((
                                index,
                                RecognizedText {
                                    text,
                                    confidence,
                                    quad,
                                },
                            )),
                    )
                }
            }))
            .buffer_unordered(window);
        tokio::pin!(requests);
        let mut recognized = Vec::new();
        while let Some(result) = requests.next().await {
            if let Some(line) = result? {
                recognized.push(line);
            }
        }
        recognized.sort_unstable_by_key(|(index, _)| *index);
        Ok(recognized.into_iter().map(|(_, line)| line).collect())
    }
}
