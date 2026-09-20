//! Native Texo sessions consume the same ready-crop queue as other formula engines.
use super::{Generation, MAX_LENGTH, ModelSessions, StepOutput};
use crate::TexoArtifacts;
use docparse_common::timing::TimingStage;
use docparse_formula::{
    FormulaError,
    queue::{FormulaRequest as Request, FormulaWorkers},
};
use docparse_layout::wasm_compat::{OnnxBackend, SessionWorker};
use ort::{session::builder::SessionBuilder, value::Tensor};
use std::sync::Arc;

type BatchResult = Result<Vec<Result<String, FormulaError>>, FormulaError>;

/// Queue ownership is separate from native session ownership so heterogeneous groups can compete directly.
pub(crate) struct SessionManager;

impl SessionManager {
    /// Loads each pair on its dedicated native owner and attaches it to the selected queue.
    pub(crate) async fn load(
        artifacts: TexoArtifacts,
        backend: OnnxBackend,
        config: &docparse_config::FormulaConfig,
        receiver: docparse_common::Queue<Request>,
    ) -> Result<FormulaWorkers, FormulaError> {
        let settings = config.single_engine()?;
        Self::start(
            settings.worker_size(),
            settings.batch_size(),
            receiver,
            format!("texo-transfer-onnx-{}", backend.execution_provider()),
            move |index| {
                tracing::info!(
                    "initializing Texo consumer {} on {}",
                    index,
                    backend.execution_provider()
                );
                let mut model = ModelSessions {
                    encoder: SessionBuilder::try_from(backend)?
                        .commit_from_memory(&artifacts.encoder)?,
                    decoder: SessionBuilder::try_from(backend)?
                        .commit_from_memory(&artifacts.decoder)?,
                    tokenizer: ModelSessions::tokenizer(&artifacts.tokenizer)?,
                };
                Ok(move |requests: &mut Vec<Request>| model.recognize(requests))
            },
        )
        .await
    }

    /// Initializes all owners before exposing readiness and safely drops partial groups on failure.
    async fn start<F, W>(
        worker_size: usize,
        batch_size: usize,
        receiver: docparse_common::Queue<Request>,
        name: String,
        initialize: F,
    ) -> Result<FormulaWorkers, FormulaError>
    where
        F: Fn(usize) -> Result<W, FormulaError> + Send + Sync + 'static,
        W: FnMut(&mut Vec<Request>) -> BatchResult + 'static,
    {
        let mut workers = FormulaWorkers::new(name.clone());
        let initialize = Arc::new(initialize);
        let metrics = docparse_common::telemetry::ModelMetrics::new(
            "formula_texo",
            worker_size,
            batch_size,
        );
        for index in 0..worker_size {
            let initialize = Arc::clone(&initialize);
            let session = SessionWorker::new(move || initialize(index)).await?;
            let receiver = receiver.clone();
            let name = name.clone();
            let metrics = Arc::clone(&metrics);
            workers.spawn(Box::pin(async move {
                let _alive = metrics.alive(1);
                while let Some(batch) = receiver.recv().await {
                    let mut requests = batch.take_ready(batch_size);
                    if requests.is_empty() {
                        continue;
                    }
                    for request in &mut requests {
                        request.engine.clone_from(&name);
                    }
                    let _batch = metrics.batch();
                    // The native owner retains requests and observes their original response cancellation through every decoder step.
                    if let Err(error) = session
                        .run(move |model| {
                            let result = model(&mut requests);
                            Request::complete_batch(requests, result);
                        })
                        .await
                    {
                        tracing::error!(
                            "Texo consumer execution failed: {}",
                            error
                        );
                    }
                }
            }))?;
        }
        Ok(workers)
    }
}

impl ModelSessions {
    /// Keeps each batch's KV cache local to its owner; canceled peers cannot terminate another page's generation.
    fn recognize(&mut self, requests: &mut Vec<Request>) -> BatchResult {
        let mut values = Vec::new();
        // Validate and prepare each crop separately so malformed input from one PDF cannot fail its batch peers.
        for request in std::mem::take(requests) {
            if request.cancelled() {
                continue;
            }
            let input = Request::measure(
                std::slice::from_ref(&request),
                TimingStage::FormulaPreprocess,
                || crate::preprocess::preprocess(&request.image),
            );
            match input {
                Ok(input) => {
                    values.extend(input);
                    requests.push(request);
                }
                Err(error) => {
                    Request::complete_batch(vec![request], Ok(vec![Err(error)]))
                }
            }
        }
        if requests.is_empty() {
            return Ok(Vec::new());
        }
        if requests.iter().all(Request::cancelled) {
            return Err(FormulaError::Invalid("Texo batch canceled".into()));
        }
        let input = crate::preprocess::FormulaInput(
            ndarray::Array4::from_shape_vec(
                (
                    requests.len(),
                    3,
                    crate::preprocess::IMAGE_SIZE,
                    crate::preprocess::IMAGE_SIZE,
                ),
                values,
            )
            .map_err(|error| FormulaError::Invalid(error.to_string()))?,
        );
        let options = ort::session::RunOptions::new()?;
        let generation =
            Request::measure(requests, TimingStage::FormulaInference, || {
                let pixels = Tensor::from_array(input.0)?;
                // Standard runs return host outputs and let ORT manage transfers and completion.
                let physical = docparse_common::telemetry::Inference::new(
                    "formula_texo",
                    "encoder",
                    requests.len(),
                );
                let outputs = self.encoder.run_with_options(
                    ort::inputs!["pixel_values" => pixels],
                    &options,
                );
                physical.finish(outputs.is_ok());
                let hidden =
                    outputs?.remove("last_hidden_state").ok_or_else(|| {
                        FormulaError::Invalid(
                            "missing Texo image features".into(),
                        )
                    })?;
                let mut generation = Generation::new(hidden, requests.len())?;
                for _ in 1..MAX_LENGTH {
                    if requests.iter().all(Request::cancelled) {
                        return Err(FormulaError::Invalid(
                            "Texo batch canceled".into(),
                        ));
                    }
                    if generation
                        .cancel(requests.iter().map(Request::cancelled))
                    {
                        break;
                    }
                    let inputs = generation.inputs()?;
                    let physical = docparse_common::telemetry::Inference::new(
                        "formula_texo",
                        "decoder",
                        requests.len(),
                    );
                    let outputs =
                        self.decoder.run_with_options(inputs, &options);
                    physical.finish(outputs.is_ok());
                    let output = StepOutput::try_from(outputs?)?;
                    if generation.advance(output)? {
                        break;
                    }
                }
                Ok::<_, FormulaError>(generation)
            })?;
        Ok(Request::measure(
            requests,
            TimingStage::FormulaDecode,
            || generation.decode(&self.tokenizer),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Partial startup drops already-created native owners even while a shared producer remains alive.
    #[tokio::test]
    async fn failed_initialization_releases_existing_sessions() {
        struct Owner(Arc<AtomicUsize>);
        impl Drop for Owner {
            /// Records native model teardown.
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let destroyed = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&destroyed);
        let (_queue, receiver) =
            docparse_formula::queue::FormulaQueue::new("test", 4);
        let result = SessionManager::start(
            2,
            2,
            receiver,
            "test".into(),
            move |index| {
                if index == 1 {
                    return Err(FormulaError::Invalid("load failure".into()));
                }
                let owner = Owner(Arc::clone(&observed));
                Ok(move |_: &mut Vec<Request>| {
                    let _ = &owner;
                    Ok(Vec::new())
                })
            },
        )
        .await;
        assert!(matches!(result, Err(FormulaError::Invalid(_))));
        assert_eq!(destroyed.load(Ordering::SeqCst), 1);
    }
}
