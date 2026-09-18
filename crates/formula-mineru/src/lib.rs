//! Formula recognition through an external MinerU vLLM service.
mod wasm_compat;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use docparse_common::timing::{TimingStage, Timings};
use docparse_common::{WasmBoxedFuture, run_cpu, timeout};
use docparse_config::{FormulaEngineConfig, ValidatedConfig};
use docparse_formula::{FormulaEngine, FormulaError, queue::FormulaQueue};
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

/// The served model name configured by the MinerU deployment.
pub const MODEL_NAME: &str = "MinerU2.5-2509-1.2B";

/// HTTP, image, and protocol failures remain distinguishable in the formula error source chain.
#[derive(Debug, thiserror::Error)]
pub enum MineruError {
    #[error(transparent)]
    Queue(#[from] FormulaError),
    #[error(transparent)]
    Config(#[from] docparse_config::ConfigError),
    #[error("MinerU HTTP request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("MinerU PNG encoding failed: {0}")]
    Image(#[from] image::ImageError),
    #[error(transparent)]
    Task(#[from] docparse_common::TaskError),
    #[error("MinerU response does not match the completion schema")]
    Json(#[from] serde_json::Error),
    #[error(
        "MinerU formula batch timed out after {0} ms, including queue admission"
    )]
    Timeout(u128),
    #[error("invalid MinerU request or response: {0}")]
    Invalid(&'static str),
}

impl From<MineruError> for FormulaError {
    /// Preserves the concrete external failure for shared pipeline diagnostics.
    fn from(error: MineruError) -> Self {
        Self::External(Box::new(error))
    }
}

/// A bounded shared crop queue that continuously replenishes HTTP inference slots.
#[derive(typed_builder::TypedBuilder)]
pub struct MineruEngine {
    queue: FormulaQueue,
    timeout: Duration,
    admission: Arc<Semaphore>,
    // Sender is declared first so it closes before the native worker is joined.
    _worker: docparse_common::ThreadManager,
}

/// The actor owns transport resources independently of producer lifetimes.
struct HttpService {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    permits: Arc<Semaphore>,
}

impl TryFrom<&ValidatedConfig> for MineruEngine {
    type Error = MineruError;

    /// Creates the HTTP pool without loading local weights or requiring a running service at startup.
    fn try_from(config: &ValidatedConfig) -> Result<Self, Self::Error> {
        let result = (|| {
            let FormulaEngineConfig::Mineru(service) = &config.formula().engine
            else {
                return Err(MineruError::Invalid(
                    "formula.engine.type must be mineru",
                ));
            };
            let endpoint = service.endpoint()?;
            let timeout = Duration::from_millis(config.formula().timeout_ms);
            let client = wasm_compat::http_client(timeout)?;
            tracing::info!(
                "configured MinerU formula service {} with concurrency {} and timeout {} ms",
                endpoint,
                service.concurrency,
                timeout.as_millis()
            );
            let transport = Arc::new(HttpService {
                client,
                endpoint,
                permits: Arc::new(Semaphore::new(service.concurrency)),
            });
            let (queue, receiver) = FormulaQueue::new(
                "formula_mineru",
                config.formula().queue_size,
            );
            let concurrency = service.concurrency;
            // One remote execution slot processes one crop at a time.
            let metrics = docparse_common::telemetry::ModelMetrics::new(
                "formula_mineru",
                concurrency,
                1,
            );
            let worker = docparse_common::ThreadManager::spawn_async(
                Box::pin(async move {
                    let _alive = metrics.alive(concurrency);
                    let incoming =
                        stream::unfold(receiver, |receiver| async move {
                            loop {
                                let batch = receiver.recv().await?;
                                if let Some(request) = batch.take_ready(1).pop()
                                {
                                    return Some((request, receiver));
                                }
                            }
                        });
                    incoming
                        .for_each_concurrent(concurrency, |mut request| {
                            let transport = Arc::clone(&transport);
                            let metrics = &metrics;
                            async move {
                                request.end_queue();
                                if request.cancelled() {
                                    return;
                                }
                                let _batch = metrics.batch();
                                let image = Arc::clone(&request.image);
                                let timings = request.context.timings.clone();
                                // Canceling one caller aborts only its request, immediately making room for other queued work.
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
                                    Either::Right(((), work)) => {
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
                        })
                        .await;
                    tracing::debug!("closed MinerU formula request queue");
                }),
            )?;

            Ok(Self::builder()
                .queue(queue)
                .timeout(timeout)
                .admission(Arc::new(Semaphore::new(
                    concurrency + config.formula().queue_size,
                )))
                ._worker(worker)
                .build())
        })();
        result.inspect_err(|error| {
            tracing::error!("MinerU formula initialization failed: {}", error)
        })
    }
}

impl HttpService {
    /// Holds admission through encoding and response decoding; cancellation drops the HTTP future and permit.
    async fn recognize_image(
        &self,
        image: Arc<PageImage>,
        timings: Timings,
    ) -> Result<String, MineruError> {
        let queued = timings.start(TimingStage::FormulaQueue);
        let permit = Arc::clone(&self.permits).acquire_owned().await.map_err(
            |_closed| MineruError::Invalid("request admission is closed"),
        )?;
        drop(queued);
        let preprocessing = timings.start(TimingStage::FormulaPreprocess);
        // The blocking encoder owns the permit until completion, even when its caller is canceled.
        let (data_url, _permit) = run_cpu(move || {
            let _preprocessing = preprocessing;
            if image.width() == 0 || image.height() == 0 {
                return Err(MineruError::Invalid(
                    "formula crop must not be empty",
                ));
            }
            let mut rgb = image::RgbImage::from_raw(
                image.width(),
                image.height(),
                image.data().to_vec(),
            )
            .ok_or(MineruError::Invalid("invalid RGB formula crop"))?;
            // MinerU pads extreme aspect ratios before bicubic upscaling of edges below 28 pixels.
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
                    "prepared MinerU formula crop from {}x{} to {}x{}",
                    image.width(),
                    image.height(),
                    rgb.width(),
                    rgb.height()
                );
            }
            let mut png = Vec::new();
            image::codecs::png::PngEncoder::new(&mut png).write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )?;
            Ok::<_, MineruError>((
                format!("data:image/png;base64,{}", STANDARD.encode(png)),
                permit,
            ))
        })
        .await??;
        let inference = timings.start(TimingStage::FormulaInference);
        let request = self.client.post(self.endpoint.clone()).json(&serde_json::json!({
            "model": MODEL_NAME,
            "messages": [
                {"role": "system", "content": "You are a helpful assistant."},
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": data_url}},
                    {"type": "text", "text": "\nFormula Recognition:"}
                ]}
            ],
            "temperature": 0.0,
            "top_p": 0.01,
            "top_k": 1,
            "presence_penalty": 1.0,
            "frequency_penalty": 0.05,
            "repetition_penalty": 1.0,
            "vllm_xargs": {"no_repeat_ngram_size": 100},
            "skip_special_tokens": false,
            "stream": false
        }));
        let response = wasm_compat::send(request).await?;
        // Bound the external response even if Content-Length is missing or dishonest.
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if bytes.len() + chunk.len() > 1024 * 1024 {
                return Err(MineruError::Invalid("completion exceeds 1 MiB"));
            }
            bytes.extend_from_slice(&chunk);
        }
        drop(inference);
        let _decoding = timings.start(TimingStage::FormulaDecode);
        String::try_from(serde_json::from_slice::<Completion>(&bytes)?)
    }
}

impl FormulaEngine for MineruEngine {
    /// Identifies the external model independently of the local ONNX provider.
    fn name(&self) -> &str {
        "mineru-2.5-vllm"
    }

    /// Limits retained crop pixels globally before requests reach the HTTP queue.
    fn admission(&self) -> Option<Arc<Semaphore>> {
        Some(Arc::clone(&self.admission))
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
                tracing::warn!("invalid MinerU caller batch size {}", count);
                return Err(
                    MineruError::Invalid("batch size must be 1..32").into()
                );
            }
            tracing::debug!("queuing {} MinerU formula crops", count);
            let result = timeout(self.timeout, self.queue.run(images, timings))
                .await
                .unwrap_or_else(|_elapsed| {
                    Err(MineruError::Timeout(self.timeout.as_millis()).into())
                });
            match result {
                Ok(latex) => {
                    tracing::debug!(
                        "completed {} queued MinerU formula crops",
                        count
                    );
                    Ok(latex)
                }
                Err(error) => {
                    tracing::warn!(
                        "MinerU caller with {} crops failed: {}",
                        count,
                        error
                    );
                    Err(error)
                }
            }
        })
    }
}

/// Minimal OpenAI completion envelope; unrelated usage and model metadata are ignored.
#[derive(Deserialize)]
struct Completion {
    choices: Vec<Choice>,
}

/// Both the termination reason and text are required to reject partial generations.
#[derive(Deserialize)]
struct Choice {
    finish_reason: String,
    message: Message,
}

/// Text content returned by the external model.
#[derive(Deserialize)]
struct Message {
    content: String,
}

impl TryFrom<Completion> for String {
    type Error = MineruError;

    /// Removes the protocol end marker before matched outer math delimiters and empty-output validation.
    fn try_from(completion: Completion) -> Result<Self, Self::Error> {
        let [choice]: [Choice; 1] =
            completion.choices.try_into().map_err(|_choices| {
                MineruError::Invalid("expected exactly one completion choice")
            })?;
        if choice.finish_reason != "stop" {
            return Err(MineruError::Invalid(
                "generation did not finish normally; refusing truncated LaTeX",
            ));
        }
        let content = choice.message.content.trim();
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
            return Err(MineruError::Invalid("empty LaTeX completion"));
        }
        Ok(latex.to_owned())
    }
}
