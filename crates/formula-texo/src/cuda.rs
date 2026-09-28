//! Device-resident I/O binding acceleration for CUDA ExecutionProvider.
use crate::model::{
    CACHE_NAMES, Generation, MAX_LENGTH, PRESENT_NAMES, StepOutput,
};
use docparse_formula::{FormulaError, queue::FormulaRequest as Request};
use docparse_layout::wasm_compat::OnnxBackend;
use ort::{
    memory::{AllocationDevice, AllocatorType, MemoryInfo, MemoryType},
    session::{RunOptions, Session},
    value::Tensor,
};

/// Holds persistent CUDA device and CPU memory information for zero-copy I/O binding.
#[derive(Debug)]
pub(crate) struct CudaIoContext {
    cuda_mem: MemoryInfo<'static>,
    cpu_mem: MemoryInfo<'static>,
}

impl CudaIoContext {
    /// Detects if the backend is CUDA and initializes the device memory context.
    pub(crate) fn detect(
        backend: &OnnxBackend,
        index: usize,
    ) -> Result<Option<Self>, FormulaError> {
        if backend.execution_provider()
            != docparse_layout::ExecutionProvider::Cuda
        {
            return Ok(None);
        }
        tracing::info!("enabling CUDA IoBinding for Texo consumer {}", index);
        let cuda_mem = MemoryInfo::new(
            AllocationDevice::CUDA,
            0,
            AllocatorType::Device,
            MemoryType::Default,
        )
        .map_err(|error| FormulaError::Invalid(error.to_string()))?;
        let cpu_mem = MemoryInfo::new(
            AllocationDevice::CPU,
            0,
            AllocatorType::Device,
            MemoryType::CPUOutput,
        )
        .map_err(|error| FormulaError::Invalid(error.to_string()))?;
        Ok(Some(Self { cuda_mem, cpu_mem }))
    }

    /// Executes accelerated CUDA inference retaining KV caches on device via ONNX Runtime IoBinding.
    pub(crate) fn recognize(
        &self,
        encoder: &mut Session,
        decoder: &mut Session,
        input: &crate::preprocess::FormulaInput,
        requests: &[Request],
        options: &RunOptions,
    ) -> Result<Generation, FormulaError> {
        let batch = requests.len();
        let pixels = Tensor::from_array(input.0.clone())?;
        let physical = docparse_common::telemetry::Inference::new(
            "formula_texo",
            "encoder",
            batch,
        );

        // Bind encoder inputs and outputs directly on CUDA device memory.
        let mut enc_binding = encoder.create_binding()?;
        enc_binding.bind_input("pixel_values", &pixels)?;
        enc_binding
            .bind_output_to_device("last_hidden_state", &self.cuda_mem)?;
        let enc_outputs =
            encoder.run_binding_with_options(&enc_binding, options);
        physical.finish(enc_outputs.is_ok());
        let hidden =
            enc_outputs?.remove("last_hidden_state").ok_or_else(|| {
                FormulaError::Invalid("missing Texo image features".into())
            })?;

        let mut generation = Generation::new(hidden, batch)?;
        let mut dec_binding = decoder.create_binding()?;
        // The encoder features remain device-bound and unchanging across all autoregressive steps.
        dec_binding.bind_input("encoder_hidden_states", &generation.hidden)?;

        for _ in 1..MAX_LENGTH {
            if requests.iter().all(Request::cancelled) {
                return Err(FormulaError::Invalid(
                    "Texo batch canceled".into(),
                ));
            }
            if generation.cancel(requests.iter().map(Request::cancelled)) {
                break;
            }

            let next_ids =
                Tensor::from_array(([batch, 1], generation.next.clone()))?;
            dec_binding.bind_input("input_ids", &next_ids)?;
            let use_cache =
                Tensor::from_array(([1], vec![generation.step > 0]))?;
            dec_binding.bind_input("use_cache_branch", &use_cache)?;

            if generation.step == 0 {
                for name in CACHE_NAMES {
                    let empty = Tensor::from_array((
                        [batch, 16, 0, 24],
                        Vec::<f32>::new(),
                    ))?;
                    dec_binding.bind_input(name, &empty)?;
                }
            } else {
                for (idx, (past_name, val)) in
                    CACHE_NAMES.iter().zip(&generation.cache).enumerate()
                {
                    if idx % 4 < 2 {
                        // Rebind self-attention KV cache produced by the preceding decoding step.
                        dec_binding.bind_input(*past_name, val)?;
                    } else if generation.step == 1 {
                        // Cross-attention KV cache is constant after step 0; bind once at step 1.
                        dec_binding.bind_input(*past_name, val)?;
                    }
                }
            }

            // Clear previous output handles so dynamic growing KV shapes are reallocated on device.
            dec_binding.clear_outputs();
            dec_binding.bind_output_to_device("logits", &self.cpu_mem)?;
            for name in PRESENT_NAMES {
                dec_binding.bind_output_to_device(name, &self.cuda_mem)?;
            }

            let physical = docparse_common::telemetry::Inference::new(
                "formula_texo",
                "decoder",
                batch,
            );
            let outputs =
                decoder.run_binding_with_options(&dec_binding, options);
            physical.finish(outputs.is_ok());
            let output = StepOutput::try_from(outputs?)?;
            if generation.advance(output)? {
                break;
            }
        }
        Ok(generation)
    }
}
