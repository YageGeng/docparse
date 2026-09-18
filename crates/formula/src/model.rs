//! Public formula engine and lossless decoding of the pinned model token output.
use crate::{FormulaArtifacts, wasm_compat::SessionRunner};
use docparse_config::ValidatedConfig;
use docparse_layout::{
    PageImage,
    timing::Timings,
    wasm_compat::{WasmBoxedFuture, WasmCompatSend, WasmCompatSync},
};
use std::sync::Arc;

/// Formula failures preserve the cause without pretending that source text is recognized LaTeX.
#[derive(Debug, thiserror::Error)]
pub enum FormulaError {
    /// External recognizers retain transport and protocol causes without coupling local engines to HTTP.
    #[error("external formula recognition failed: {0}")]
    External(#[source] Box<dyn std::error::Error + Send + Sync>),
    #[error("invalid formula artifacts: {0}")]
    Artifacts(String),
    #[error("formula input/output is invalid: {0}")]
    Invalid(String),
    #[error("formula tokenizer failed: {0}")]
    Tokenizer(String),
    #[error(transparent)]
    Onnx(#[from] ort::Error),
    #[error(transparent)]
    Layout(#[from] docparse_layout::LayoutError),
    #[error(transparent)]
    Task(#[from] docparse_layout::wasm_compat::TaskError),
    #[error("formula artifacts require explicit bytes on this platform")]
    ArtifactsRequired,
}

/// One batch must produce exactly one LaTeX result for each input crop in the same order.
pub trait FormulaEngine: WasmCompatSend + WasmCompatSync {
    /// Identifies the actual recognizer in lifecycle logs.
    fn name(&self) -> &str;
    /// Owns crop pixels through completion, including cancellation of the caller.
    fn recognize(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> WasmBoxedFuture<'_, Result<Vec<String>, FormulaError>>;
}

/// A reusable bounded ONNX session shared by documents and page batches.
pub struct PpFormulaNetEngine {
    runner: Arc<SessionRunner>,
    name: String,
}

impl PpFormulaNetEngine {
    /// Validates the pinned pair once and initializes the existing compile-time/browser backend.
    pub async fn from_artifacts(
        config: Arc<ValidatedConfig>,
        artifacts: FormulaArtifacts,
    ) -> Result<Self, FormulaError> {
        let (artifacts, kind) =
            docparse_layout::wasm_compat::run_cpu(move || {
                let kind = artifacts.verify()?;
                Ok::<_, FormulaError>((artifacts, kind))
            })
            .await??;
        let backend =
            docparse_layout::wasm_compat::OnnxBackend::from(config.as_ref());
        tracing::info!(
            "loading {} with provider {} and configured batch size {}",
            kind.as_str(),
            backend.execution_provider(),
            config.formula().batch_size
        );
        let runner = SessionRunner::load(artifacts, backend, kind)
            .await
            .map_err(|error| {
                tracing::error!(
                    "formula model initialization failed: {}",
                    error
                );
                error
            })?;
        let provider = match backend.execution_provider() {
            docparse_layout::ExecutionProvider::CoreMl
            | docparse_layout::ExecutionProvider::Metal => "cpu",
            provider => provider.as_str(),
        };
        tracing::info!(
            "loaded {} with actual executor {} (requested {})",
            kind.as_str(),
            provider,
            backend.execution_provider()
        );
        Ok(Self {
            runner,
            name: format!("{}-onnx-{provider}", kind.as_str()),
        })
    }
}

impl FormulaEngine for PpFormulaNetEngine {
    /// Reports the fixed supported model family.
    fn name(&self) -> &str {
        &self.name
    }

    /// Sends one real batch to the model owner; no per-crop pseudo-batching is used.
    fn recognize(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> WasmBoxedFuture<'_, Result<Vec<String>, FormulaError>> {
        Box::pin(async move {
            if images.is_empty() || images.len() > 32 {
                return Err(FormulaError::Invalid(
                    "batch size must be 1..32".into(),
                ));
            }
            Arc::clone(&self.runner).run(images, timings).await.map_err(
                |error| {
                    tracing::warn!("formula batch failed: {}", error);
                    error
                },
            )
        })
    }
}

/// Decodes generated IDs without lossy regex cleanup or a post-inference token-length truncation.
pub(crate) struct FormulaDecoder {
    tokenizer: tokenizers::Tokenizer,
    vocab_size: i64,
}

impl FormulaDecoder {
    /// Builds only from the verified matching tokenizer and requires explicit BOS/EOS identities.
    pub(crate) fn new(bytes: &[u8]) -> Result<Self, FormulaError> {
        let tokenizer = tokenizers::Tokenizer::from_bytes(bytes)
            .map_err(|error| FormulaError::Tokenizer(error.to_string()))?;
        if tokenizer.token_to_id("<s>") != Some(0)
            || tokenizer.token_to_id("</s>") != Some(2)
        {
            return Err(FormulaError::Tokenizer(
                "unexpected BOS/EOS identities".into(),
            ));
        }
        // With added tokens, this query clones the vocabulary; pay that cost only once per session.
        let vocab_size = tokenizer.get_vocab_size(true) as i64;
        Ok(Self {
            tokenizer,
            vocab_size,
        })
    }

    /// Preserves batch order and fails explicitly when the model omits the end marker.
    pub(crate) fn decode(
        &self,
        outputs: &ort::session::SessionOutputs<'_>,
        batch: usize,
    ) -> Result<Vec<String>, FormulaError> {
        let output = outputs.get("fetch_name_0").ok_or_else(|| {
            FormulaError::Invalid("missing token output".into())
        })?;
        let (shape, values) = output.try_extract_tensor::<i64>()?;
        if shape.len() != 2 || shape.first().copied() != Some(batch as i64) {
            return Err(FormulaError::Invalid(format!(
                "unexpected token shape {shape:?}"
            )));
        }
        let width = usize::try_from(*shape.get(1).ok_or_else(|| {
            FormulaError::Invalid("missing token dimension".into())
        })?)
        .map_err(|error| FormulaError::Invalid(error.to_string()))?;
        if width == 0 || values.len() != batch * width {
            return Err(FormulaError::Invalid(
                "invalid token buffer length".into(),
            ));
        }
        let mut formulas = Vec::with_capacity(batch);
        for row in values.chunks_exact(width) {
            let mut tokens = Vec::new();
            let mut terminated = false;
            for id in row {
                if *id == 2 || *id >= self.vocab_size {
                    terminated = true;
                    break;
                }
                if *id > 2 {
                    tokens.push(u32::try_from(*id).map_err(|error| {
                        FormulaError::Invalid(error.to_string())
                    })?);
                }
            }
            if !terminated {
                return Err(FormulaError::Invalid("formula generation ended without EOS/padding; refusing truncated LaTeX".into()));
            }
            let latex = self
                .tokenizer
                .decode(&tokens, true)
                .map_err(|error| FormulaError::Tokenizer(error.to_string()))?;
            formulas.push(latex.trim().to_owned());
        }
        Ok(formulas)
    }
}
