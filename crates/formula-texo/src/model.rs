//! Shared model contract and autoregressive state for native and browser execution.
use crate::{TexoArtifacts, wasm_compat::SessionRunner};
use docparse_common::WasmBoxedFuture;
use docparse_common::timing::Timings;
use docparse_config::ValidatedConfig;
use docparse_formula::{FormulaEngine, FormulaError};
use docparse_layout::PageImage;
use ort::{
    session::{Session, SessionInputValue, SessionOutputs},
    value::{DynValue, Tensor},
};
use std::{borrow::Cow, sync::Arc};
use typed_builder::TypedBuilder;

/// Maximum sequence length, including BOS, from the pinned generation configuration.
pub const MAX_LENGTH: usize = 1024;
const VOCAB_SIZE: usize = 687;
const CACHE_NAMES: [&str; 8] = [
    "past_key_values.0.decoder.key",
    "past_key_values.0.decoder.value",
    "past_key_values.0.encoder.key",
    "past_key_values.0.encoder.value",
    "past_key_values.1.decoder.key",
    "past_key_values.1.decoder.value",
    "past_key_values.1.encoder.key",
    "past_key_values.1.encoder.value",
];
pub(crate) const PRESENT_NAMES: [&str; 8] = [
    "present.0.decoder.key",
    "present.0.decoder.value",
    "present.0.encoder.key",
    "present.0.encoder.value",
    "present.1.decoder.key",
    "present.1.decoder.value",
    "present.1.encoder.key",
    "present.1.encoder.value",
];

/// Detached output ownership ends the asynchronous input borrow without copying tensors.
pub(crate) struct StepOutput {
    pub(crate) logits: DynValue,
    cache: [Option<DynValue>; 8],
}

impl TryFrom<SessionOutputs<'_>> for StepOutput {
    type Error = FormulaError;
    /// Moves runtime handles out before either session or generation state is reused.
    fn try_from(mut outputs: SessionOutputs<'_>) -> Result<Self, Self::Error> {
        let logits = outputs.remove("logits").ok_or_else(|| {
            FormulaError::Invalid("missing Texo logits".into())
        })?;
        let cache = PRESENT_NAMES.map(|name| outputs.remove(name));
        Ok(Self { logits, cache })
    }
}

/// Reusable Texo sessions with bounded ownership and one result per input crop.
pub struct TexoEngine {
    pool: docparse_formula::queue::FormulaPool,
    name: String,
}

impl TexoEngine {
    /// Verifies both graphs and tokenizer before initializing the selected execution provider.
    pub async fn from_artifacts(
        config: Arc<ValidatedConfig>,
        artifacts: TexoArtifacts,
    ) -> Result<Self, FormulaError> {
        // Standalone engines own the same producer pool used by mixed configurations.
        let mut pool = docparse_formula::queue::FormulaPool::new(&config)?;
        let workers =
            Self::spawn_from_artifacts(config, artifacts, pool.receiver())
                .await?;
        let name = workers.name().to_owned();
        pool.add(workers);
        Ok(Self { pool, name })
    }

    /// Initializes execution owners on the receiving side of an existing pool.
    pub async fn spawn_from_artifacts(
        config: Arc<ValidatedConfig>,
        artifacts: TexoArtifacts,
        receiver: docparse_common::Queue<
            docparse_formula::queue::FormulaRequest,
        >,
    ) -> Result<docparse_formula::queue::FormulaWorkers, FormulaError> {
        let artifacts = docparse_common::run_cpu(move || {
            artifacts.verify()?;
            Ok::<_, FormulaError>(artifacts)
        })
        .await??;
        let backend =
            docparse_layout::wasm_compat::OnnxBackend::from(config.as_ref());
        tracing::info!(
            "loading Texo ONNX encoder and cached decoder with provider {}",
            backend.execution_provider()
        );
        let runner =
            SessionRunner::load(artifacts, backend, config.formula(), receiver)
                .await
                .map_err(|error| {
                    tracing::error!("Texo initialization failed: {}", error);
                    error
                })?;

        let session_size = config.formula().single_engine()?.worker_size();
        let provider = backend.execution_provider();
        tracing::info!(
            "loaded Texo with {} session pairs on {}",
            session_size,
            provider
        );
        Ok(runner)
    }
}

impl FormulaEngine for TexoEngine {
    /// Shares adaptive admission across all documents using this engine.
    fn pressure(&self) -> Option<Arc<docparse_common::queue::QueuePressure>> {
        self.pool.pressure()
    }

    /// Reports the selected model and registered execution provider.
    fn name(&self) -> &str {
        &self.name
    }

    /// Bounds pre-crop admission globally instead of allocating one ready window per page.
    fn admission(&self) -> Option<Arc<tokio::sync::Semaphore>> {
        self.pool.admission()
    }

    /// Preserves input order, propagates cancellation, and refuses truncated output.
    fn recognize(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> WasmBoxedFuture<'_, Result<Vec<String>, FormulaError>> {
        Box::pin(async move {
            if !(1..=32).contains(&images.len()) {
                tracing::warn!("invalid Texo batch size {}", images.len());
                return Err(FormulaError::Invalid(
                    "Texo batch size must be 1..32".into(),
                ));
            }
            self.pool.recognize(images, timings).await.map_err(|error| {
                tracing::warn!("Texo formula batch failed: {}", error);
                error
            })
        })
    }
}

/// The runtime owner retains both graphs and their matching tokenizer together.
pub(crate) struct ModelSessions {
    pub(crate) encoder: Session,
    pub(crate) decoder: Session,
    pub(crate) tokenizer: tokenizers::Tokenizer,
}

impl ModelSessions {
    /// Uses the pinned WordLevel vocabulary, including its explicit BOS/EOS identities.
    pub(crate) fn tokenizer(
        bytes: &[u8],
    ) -> Result<tokenizers::Tokenizer, FormulaError> {
        let tokenizer = tokenizers::Tokenizer::from_bytes(bytes)
            .map_err(|error| FormulaError::Tokenizer(error.to_string()))?;
        if tokenizer.get_vocab_size(true) != VOCAB_SIZE
            || tokenizer.token_to_id("<s>") != Some(0)
            || tokenizer.token_to_id("</s>") != Some(2)
        {
            return Err(FormulaError::Tokenizer(
                "unexpected Texo vocabulary".into(),
            ));
        }
        Ok(tokenizer)
    }
}

/// Values remain runtime-owned between steps; Rust reads logits, never full KV buffers.
#[derive(TypedBuilder)]
pub(crate) struct Generation {
    pub(crate) hidden: DynValue,
    pub(crate) cache: Vec<DynValue>,
    tokens: Vec<Vec<u32>>,
    next: Vec<i64>,
    finished: Vec<bool>,
    step: usize,
}

impl Generation {
    /// Stops canceled rows from extending a mixed batch; their outputs are discarded by the request owner.
    pub(crate) fn cancel(
        &mut self,
        cancelled: impl Iterator<Item = bool>,
    ) -> bool {
        for ((finished, next), cancelled) in
            self.finished.iter_mut().zip(&mut self.next).zip(cancelled)
        {
            if cancelled {
                *finished = true;
                *next = 1;
            }
        }
        self.finished.iter().all(|finished| *finished)
    }

    /// Initializes the decoder's empty past and BOS separately for each crop.
    pub(crate) fn new(
        hidden: DynValue,
        batch: usize,
    ) -> Result<Self, FormulaError> {
        let cache = (0..8)
            .map(|_| {
                Tensor::from_array(([batch, 16, 0, 24], Vec::<f32>::new()))
                    .map(Tensor::into_dyn)
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::builder()
            .hidden(hidden)
            .cache(cache)
            .tokens(vec![vec![0]; batch])
            .next(vec![0; batch])
            .finished(vec![false; batch])
            .step(0)
            .build())
    }

    /// Borrows hidden features and cached tensors without cloning or copying their data.
    pub(crate) fn inputs(
        &self,
    ) -> Result<Vec<(Cow<'static, str>, SessionInputValue<'_>)>, FormulaError>
    {
        let mut inputs = ort::inputs![
            "input_ids" => Tensor::from_array(([self.next.len(), 1], self.next.clone()))?,
            "encoder_hidden_states" => &self.hidden,
            "use_cache_branch" => Tensor::from_array(([1], vec![self.step > 0]))?,
        ];
        for (name, value) in CACHE_NAMES.into_iter().zip(&self.cache) {
            inputs.push((Cow::Borrowed(name), value.into()));
        }
        Ok(inputs)
    }

    /// Updates greedy predictions and replaces only the KV tensors the selected branch produced.
    pub(crate) fn advance(
        &mut self,
        output: StepOutput,
    ) -> Result<bool, FormulaError> {
        let (shape, values) = output.logits.try_extract_tensor::<f32>()?;
        let batch = self.next.len();
        if shape.as_ref() != [batch as i64, 1, VOCAB_SIZE as i64]
            || values.len() != batch * VOCAB_SIZE
        {
            return Err(FormulaError::Invalid(format!(
                "unexpected Texo logits shape {shape:?}"
            )));
        }
        for (((row, tokens), next), finished) in values
            .as_chunks::<VOCAB_SIZE>()
            .0
            .iter()
            .zip(&mut self.tokens)
            .zip(&mut self.next)
            .zip(&mut self.finished)
        {
            if *finished {
                *next = 1;
                continue;
            }
            let mut best = (0_usize, f32::NEG_INFINITY);
            for (id, &value) in row.iter().enumerate() {
                if !value.is_finite() {
                    return Err(FormulaError::Invalid(
                        "non-finite Texo logits".into(),
                    ));
                }
                // Strict comparison preserves the first token on ties, like numpy/torch argmax.
                if value > best.1 {
                    best = (id, value);
                }
            }
            tokens.push(best.0 as u32);
            *next = best.0 as i64;
            *finished = best.0 == 2;
        }
        if self.finished.iter().all(|done| *done) {
            return Ok(true);
        }
        for (index, ((name, value), cached)) in PRESENT_NAMES
            .iter()
            .zip(&mut self.cache)
            .zip(output.cache)
            .enumerate()
        {
            // Cached decoder branches emit empty cross-attention outputs; preserve the first-step values.
            if self.step > 0 && index % 4 >= 2 {
                continue;
            }
            *value = cached.ok_or_else(|| {
                FormulaError::Invalid(format!("missing Texo cache {name}"))
            })?;
        }
        self.step += 1;
        Ok(false)
    }

    /// Keeps complete crops from other callers when one sequence reaches the generation cap.
    pub(crate) fn decode(
        self,
        tokenizer: &tokenizers::Tokenizer,
    ) -> Vec<Result<String, FormulaError>> {
        self.tokens
            .iter()
            .zip(self.finished)
            .map(|(ids, finished)| {
                if !finished {
                    return Err(FormulaError::Invalid("Texo generation reached 1024 tokens without EOS; refusing truncated LaTeX".into()));
                }
                tokenizer
                    .decode(ids, true)
                    .map(|latex| latex.trim().to_owned())
                    .map_err(|error| FormulaError::Tokenizer(error.to_string()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Canceled long rows cannot keep a completed peer waiting for the token limit.
    #[test]
    fn canceled_rows_release_completed_peers() {
        let hidden = Tensor::from_array(([2, 1, 2048], vec![0_f32; 4096]))
            .expect("tensor")
            .into_dyn();
        let mut generation = Generation::new(hidden, 2).expect("generation");
        assert!(!generation.cancel([true, false].into_iter()));
        assert_eq!(generation.finished, vec![true, false]);
        *generation.finished.get_mut(1).expect("second row") = true;
        assert!(generation.cancel([true, false].into_iter()));
    }

    /// One unfinished crop must not discard a completed crop from another page in the same batch.
    #[test]
    fn completed_crops_survive_an_unfinished_batch_peer() {
        let hidden = Tensor::from_array(([2, 1, 2048], vec![0_f32; 4096]))
            .expect("tensor")
            .into_dyn();
        let mut generation = Generation::new(hidden, 2).expect("generation");
        *generation.finished.first_mut().expect("first row") = true;
        generation.tokens.first_mut().expect("first row").clear();
        let tokenizer = tokenizers::Tokenizer::new(
            tokenizers::models::wordlevel::WordLevel::default(),
        );
        let results = generation.decode(&tokenizer);
        assert!(
            results.first().expect("first result").is_ok(),
            "completed crop must survive"
        );
        assert!(
            results.get(1).expect("second result").is_err(),
            "unfinished crop must fail"
        );
    }

    /// Reaching the generation cap cannot turn a partial expression into successful LaTeX.
    #[test]
    fn rejects_unfinished_sequences() {
        let hidden = Tensor::from_array(([1, 1, 2048], vec![0_f32; 2048]))
            .expect("tensor")
            .into_dyn();
        let generation = Generation::new(hidden, 1).expect("generation");
        let tokenizer = tokenizers::Tokenizer::new(
            tokenizers::models::wordlevel::WordLevel::default(),
        );
        assert!(
            matches!(generation.decode(&tokenizer).pop(), Some(Err(FormulaError::Invalid(message))) if message.contains("without EOS"))
        );
    }
}
