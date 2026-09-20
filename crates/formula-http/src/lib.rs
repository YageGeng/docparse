//! Formula recognition through image-only uploads or prompted HTTP chat completions.
mod wasm_compat;

use base64::{Engine as _, engine::general_purpose::STANDARD};

use docparse_common::timing::{TimingStage, Timings};
use docparse_common::{WasmBoxedFuture, run_cpu};
use docparse_config::{FormulaEngineConfig, ValidatedConfig};
use docparse_formula::{
    FormulaEngine, FormulaError,
    queue::{FormulaPool, FormulaRequest, FormulaWorkers},
};
use docparse_layout::PageImage;
use futures_util::{
    StreamExt,
    future::{Either, select},
    stream,
};
use image::ImageEncoder;
use serde::Deserialize;
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

/// HTTP, image, and protocol failures remain distinguishable in the formula error source chain.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error(transparent)]
    Queue(#[from] FormulaError),
    #[error(transparent)]
    Config(#[from] docparse_config::ConfigError),
    #[error("formula HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("HTTP PNG encoding failed: {0}")]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Task(#[from] docparse_common::TaskError),
    #[error("HTTP formula response does not match the expected schema")]
    Json(#[from] serde_json::Error),
    #[error("invalid HTTP request or response: {0}")]
    Invalid(&'static str),
}

impl From<HttpError> for FormulaError {
    /// Preserves the concrete external failure for shared pipeline diagnostics.
    fn from(error: HttpError) -> Self {
        Self::External(Box::new(error))
    }
}

/// A bounded shared crop queue that continuously replenishes HTTP inference slots.
pub struct HttpEngine {
    pool: FormulaPool,
}

/// The actor owns transport resources independently of producer lifetimes.
#[derive(typed_builder::TypedBuilder)]
struct HttpService {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    #[builder(default)]
    prompt: Option<String>,
    model: String,
    permits: Arc<Semaphore>,
}

impl TryFrom<&ValidatedConfig> for HttpEngine {
    type Error = HttpError;
    /// Creates a standalone HTTP consumer group using its configured worker limit.
    fn try_from(config: &ValidatedConfig) -> Result<Self, Self::Error> {
        // Standalone callers retain a real producer; shared initialization returns workers only.
        let mut pool = FormulaPool::new(config)?;
        let workers = Self::spawn(config, pool.receiver())?;
        pool.add(workers);
        Ok(Self { pool })
    }
}

impl HttpEngine {
    /// Registers HTTP workers directly on the shared formula queue without creating a second pending queue.
    pub fn spawn(
        config: &ValidatedConfig,
        receiver: docparse_common::Queue<FormulaRequest>,
    ) -> Result<FormulaWorkers, HttpError> {
        let result = (|| {
            let FormulaEngineConfig::Http(service) =
                config.formula().single_engine()?
            else {
                return Err(HttpError::Invalid(
                    "HTTP constructor requires an HTTP consumer group",
                ));
            };
            let endpoint = service.endpoint()?;
            let timeout = Duration::from_millis(config.formula().timeout_ms);
            let client = wasm_compat::http_client(timeout)?;
            let transport = Arc::new(
                HttpService::builder()
                    .client(client)
                    .endpoint(endpoint)
                    .prompt(service.prompt.clone())
                    .model(service.model.clone())
                    .permits(Arc::new(Semaphore::new(service.worker_size)))
                    .build(),
            );
            let mut workers = FormulaWorkers::new("formula-http".into());
            let metrics = docparse_common::telemetry::ModelMetrics::new(
                "formula_http",
                service.worker_size,
                1,
            );
            let endpoint = transport.endpoint.clone();
            let worker_size = service.worker_size;
            workers.spawn(Box::pin(async move {
                stream::iter(0..worker_size)
                    .for_each_concurrent(worker_size, |_| {
                        let receiver = receiver.clone();
                        let transport = Arc::clone(&transport);
                        let metrics = Arc::clone(&metrics);
                        async move {
                            let _alive = metrics.alive(1);
                            while let Some(batch) = receiver.recv().await {
                                let Some(mut request) =
                                    batch.take_ready(1).pop()
                                else {
                                    continue;
                                };
                                request.engine = "formula-http".into();
                                let _batch = metrics.batch();
                                let image = Arc::clone(&request.image);
                                let timings = request.context.timings.clone();
                                let result = match select(
                                    Box::pin(
                                        transport
                                            .recognize_image(image, timings),
                                    ),
                                    Box::pin(request.closed()),
                                )
                                .await
                                {
                                    Either::Left((result, cancellation)) => {
                                        drop(cancellation);
                                        Some(result)
                                    }
                                    Either::Right((_, work)) => {
                                        drop(work);
                                        None
                                    }
                                };
                                if let Some(result) = result {
                                    request.complete(
                                        result.map_err(FormulaError::from),
                                    );
                                }
                            }
                        }
                    })
                    .await;
            }))?;
            tracing::info!(
                "configured HTTP formula consumers at {} with {} workers",
                endpoint,
                service.worker_size
            );
            Ok(workers)
        })();
        result.inspect_err(|error| {
            tracing::error!("HTTP formula initialization failed: {}", error)
        })
    }
}

impl HttpService {
    /// Holds admission through encoding and response decoding; cancellation drops the HTTP future and permit.
    async fn recognize_image(
        &self,
        image: Arc<PageImage>,
        timings: Timings,
    ) -> Result<String, HttpError> {
        let queued = timings.start(TimingStage::FormulaQueue);
        let permit = Arc::clone(&self.permits).acquire_owned().await.map_err(
            |_closed| HttpError::Invalid("request admission is closed"),
        )?;
        drop(queued);
        let preprocessing = timings.start(TimingStage::FormulaPreprocess);
        // The blocking encoder owns the permit until completion, even when its caller is canceled.
        let prompted = self.prompt.is_some();
        let (png, _permit) = run_cpu(move || {
            let _preprocessing = preprocessing;
            if image.width() == 0 || image.height() == 0 {
                return Err(HttpError::Invalid(
                    "formula crop must not be empty",
                ));
            }
            let mut rgb = image::RgbImage::from_raw(
                image.width(),
                image.height(),
                image.data().to_vec(),
            )
            .ok_or(HttpError::Invalid("invalid RGB formula crop"))?;
            // Prompted vision models need minimum image dimensions; image-only services own their preprocessing.
            if prompted {
                let width = rgb.width().max(rgb.height().div_ceil(50));
                let height = rgb.height().max(rgb.width().div_ceil(50));
                if (width, height) != rgb.dimensions() {
                    let mut padded = image::RgbImage::from_pixel(
                        width,
                        height,
                        image::Rgb([255; 3]),
                    );
                    image::imageops::replace(
                        &mut padded,
                        &rgb,
                        i64::from((width - rgb.width()) / 2),
                        i64::from((height - rgb.height()) / 2),
                    );
                    rgb = padded;
                }
                let shortest = rgb.width().min(rgb.height());
                if shortest < 28 {
                    // The padded aspect ratio is at most 50, so these products fit in u32.
                    rgb = image::imageops::resize(
                        &rgb,
                        (rgb.width() * 28).div_ceil(shortest),
                        (rgb.height() * 28).div_ceil(shortest),
                        image::imageops::FilterType::CatmullRom,
                    );
                }
                if rgb.dimensions() != (image.width(), image.height()) {
                    tracing::debug!(
                        "prepared HTTP formula crop from {}x{} to {}x{}",
                        image.width(),
                        image.height(),
                        rgb.width(),
                        rgb.height()
                    );
                }
            }
            let mut png = Vec::new();
            image::codecs::png::PngEncoder::new(&mut png).write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )?;
            Ok::<_, HttpError>((png, permit))
        })
        .await??;
        let inference = timings.start(TimingStage::FormulaInference);
        let request = self.client.post(self.endpoint.clone());
        let request = if let Some(prompt) = &self.prompt {
            request.json(&serde_json::json!({
                "model": self.model,
                "messages": [
                    {"role": "system", "content": "You are a helpful assistant."},
                    {"role": "user", "content": [
                        {"type": "image_url", "image_url": {"url": format!("data:image/png;base64,{}", STANDARD.encode(png))}},
                        {"type": "text", "text": prompt}
                    ]}
                ],
                "temperature": 0.0,
                "stream": false
            }))
        } else {
            request.multipart(
                reqwest::multipart::Form::new()
                    .part(
                        "image",
                        reqwest::multipart::Part::bytes(png)
                            .file_name("formula.png")
                            .mime_str("image/png")?,
                    )
                    .text("task", "formula"),
            )
        };
        tracing::debug!("sending HTTP formula request to {}", self.endpoint);
        let response = wasm_compat::send(request).await?;
        // Bound the external response even if Content-Length is missing or dishonest.
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err(HttpError::Invalid("completion exceeds 1 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        drop(inference);
        let _decoding = timings.start(TimingStage::FormulaDecode);
        let result = if prompted {
            String::try_from(serde_json::from_slice::<Completion>(&bytes)?)
        } else {
            String::try_from(serde_json::from_slice::<Prediction>(&bytes)?)
        };
        if result.is_ok() {
            tracing::debug!(
                "completed HTTP formula request to {}",
                self.endpoint
            );
        }
        result
    }
}

impl FormulaEngine for HttpEngine {
    /// Applies the same pending-queue policy to HTTP formula consumers.
    fn pressure(&self) -> Option<Arc<docparse_common::queue::QueuePressure>> {
        self.pool.pressure()
    }

    /// Identifies the external model independently of the local ONNX provider.
    fn name(&self) -> &str {
        "formula-http"
    }

    /// Limits retained crop pixels globally before requests reach the HTTP queue.
    fn admission(&self) -> Option<Arc<Semaphore>> {
        self.pool.admission()
    }

    /// Runs bounded crop requests concurrently while preserving order and a single batch deadline.
    fn recognize(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> WasmBoxedFuture<'_, Result<Vec<String>, FormulaError>> {
        Box::pin(async move {
            let count = images.len();
            if !(1..=32).contains(&count) {
                tracing::warn!("invalid HTTP caller batch size {}", count);
                return Err(
                    HttpError::Invalid("batch size must be 1..32").into()
                );
            }
            tracing::debug!("queuing {} HTTP formula crops", count);
            let result = self.pool.recognize(images, timings).await;
            match result {
                Ok(latex) => {
                    tracing::debug!(
                        "completed {} queued HTTP formula crops",
                        count
                    );
                    Ok(latex)
                }
                Err(error) => {
                    tracing::warn!(
                        "HTTP caller with {} crops failed: {}",
                        count,
                        error
                    );
                    Err(error)
                }
            }
        })
    }
}

/// Minimal chat completion envelope; unrelated usage and model metadata are ignored.
#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
}
/// Completion termination must be explicit before accepting its text.
#[derive(Deserialize)]
struct Choice {
    finish_reason: String,
    message: Message,
}
/// Text returned by the prompted model.
#[derive(Deserialize)]
struct Message {
    content: String,
}
impl TryFrom<Completion> for String {
    type Error = HttpError;
    /// Rejects partial chat generations before shared text normalization.
    fn try_from(completion: Completion) -> Result<Self, Self::Error> {
        let [choice]: [Choice; 1] =
            completion.choices.try_into().map_err(|_choices| {
                HttpError::Invalid("expected exactly one completion choice")
            })?;
        if choice.finish_reason != "stop" {
            return Err(HttpError::Invalid(
                "generation did not finish normally; refusing truncated LaTeX",
            ));
        }
        Self::try_from(Prediction {
            text: choice.message.content,
        })
    }
}

/// JSON text returned by an image-only formula service.
#[derive(Deserialize)]
struct Prediction {
    text: String,
}

impl TryFrom<Prediction> for String {
    type Error = HttpError;

    /// Normalizes outer math delimiters and rejects empty output for either HTTP protocol.
    fn try_from(prediction: Prediction) -> Result<Self, Self::Error> {
        let content = prediction.text.trim();
        let content = content
            .strip_suffix("<|im_end|>")
            .unwrap_or(content)
            .trim_end();
        let latex = [(r"\[", r"\]"), (r"\(", r"\)"), ("$$", "$$"), ("$", "$")]
            .into_iter()
            .find_map(|(start, end)| {
                content.strip_prefix(start)?.strip_suffix(end)
            })
            .unwrap_or(content)
            .trim();
        if latex.is_empty() {
            return Err(HttpError::Invalid("empty LaTeX completion"));
        }
        Ok(latex.to_owned())
    }
}
