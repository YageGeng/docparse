//! Shared execution-provider registration for layout, OCR and table structure models.
use super::tensorrt::{TensorRtCache, TensorRtProfile};
use crate::LayoutError;
use docparse_config::{OnnxThreadPool, OptimizationLevel, ValidatedConfig};
use ort::session::{
    Session,
    builder::{GraphOptimizationLevel, SessionBuilder},
};
use serde::Serialize;

/// Identifies the ONNX backend selected by build features or the browser runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionProvider {
    Cpu,
    Cuda,
    #[serde(rename = "coreml")]
    CoreMl,
    /// Apple GPU through CoreML with CPUAndGPU compute units.
    Metal,
    Openvino,
    /// Browser WebGPU execution, validated at the platform boundary.
    WebGpu,
}

impl ExecutionProvider {
    /// Returns the stable diagnostic name of a backend.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Cuda => "cuda",
            Self::CoreMl => "coreml",
            Self::Metal => "metal",
            Self::Openvino => "openvino",
            Self::WebGpu => "webgpu",
        }
    }
}
impl std::fmt::Display for ExecutionProvider {
    /// Formats backend names consistently across model initialization and inference logs.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl ExecutionProvider {
    /// Selects the native provider enabled at build time, preferring accelerators.
    #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
    const fn compiled() -> Self {
        if cfg!(feature = "cuda") {
            Self::Cuda
        } else if cfg!(feature = "metal") {
            Self::Metal
        } else if cfg!(feature = "coreml") {
            Self::CoreMl
        } else if cfg!(feature = "openvino") {
            Self::Openvino
        } else {
            Self::Cpu
        }
    }
}

/// CPU compute-thread policy shared by every native ONNX session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OnnxThreading {
    pool: OnnxThreadPool,
    intra_threads: usize,
    spinning: bool,
}

impl From<&docparse_config::RuntimeConfig> for OnnxThreading {
    /// Copies the three runtime threading keys without interpreting automatic values yet.
    fn from(runtime: &docparse_config::RuntimeConfig) -> Self {
        Self {
            pool: runtime.onnx_thread_pool,
            intra_threads: runtime.onnx_intra_threads,
            spinning: runtime.onnx_spinning,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl OnnxThreading {
    /// Resolves 0 to the cgroup-aware parallelism for the shared pool; sessions keep ORT's own default.
    fn global_threads(self) -> usize {
        match self.intra_threads {
            0 => std::thread::available_parallelism().map_or(1, usize::from),
            threads => threads,
        }
    }

    /// Applies this policy to one builder; global sessions inherit the shared pool automatically.
    fn apply(
        self,
        builder: SessionBuilder,
    ) -> Result<SessionBuilder, LayoutError> {
        match self.pool {
            OnnxThreadPool::Global => Ok(builder),
            OnnxThreadPool::Session => {
                // rc.15 inherits the environment pool unless explicitly opting out for this session.
                let builder = builder
                    .with_independent_thread_pool()
                    .map_err(ort::Error::from)?;
                let builder = match self.intra_threads {
                    0 => builder,
                    threads => builder
                        .with_intra_threads(threads)
                        .map_err(ort::Error::from)?,
                };
                Ok(builder
                    .with_intra_op_spinning(self.spinning)
                    .map_err(ort::Error::from)?)
            }
        }
    }
}

/// The shared environment may belong to the host, whose global policy must remain untouched.
struct SharedEnvironment {
    environment: ort::environment::Environment,
    threading: Option<OnnxThreading>,
}

// Both host registration and lazy CLI/server initialization use the same synchronized owner.
static ENVIRONMENT: std::sync::Mutex<Option<SharedEnvironment>> =
    std::sync::Mutex::new(None);

#[cfg(not(target_arch = "wasm32"))]
impl TryFrom<OnnxThreading> for ort::environment::Environment {
    type Error = ort::Error;

    /// Builds the default native environment without owning process-wide registration.
    fn try_from(threading: OnnxThreading) -> Result<Self, Self::Error> {
        let builder = ort::init();
        let builder = if threading.pool == OnnxThreadPool::Global {
            let options = ort::environment::GlobalThreadPoolOptions::default()
                .with_intra_threads(threading.global_threads())?
                .with_inter_threads(1)?
                .with_spin_control(threading.spinning)?;
            builder.with_global_thread_pool(options)
        } else {
            builder
        };
        builder.build()
    }
}

impl SharedEnvironment {
    /// Reuses the registered host environment or lazily installs the existing standalone policy.
    fn get_or_init(
        threading: OnnxThreading,
    ) -> Result<ort::environment::Environment, LayoutError> {
        let mut shared =
            ENVIRONMENT.lock().map_err(|error| LayoutError::Engine {
                message: format!(
                    "ONNX environment initialization lock failed: {error}"
                ),
            })?;
        #[cfg(not(target_arch = "wasm32"))]
        let requested = OnnxThreading {
            intra_threads: threading.global_threads(),
            ..threading
        };
        #[cfg(target_arch = "wasm32")]
        let requested = threading;
        if let Some(installed) = shared.as_ref() {
            if threading.pool == OnnxThreadPool::Global
                && let Some(policy) = installed.threading
                && policy != requested
            {
                tracing::warn!(
                    "ONNX environment already uses {:?}; ignoring requested global policy {:?}",
                    policy,
                    requested
                );
            }
            return Ok(installed.environment.clone());
        }
        #[cfg(not(target_arch = "wasm32"))]
        let environment = ort::environment::Environment::try_from(requested)?;
        #[cfg(target_arch = "wasm32")]
        let environment = ort::init().with_telemetry(false).build()?;
        tracing::info!(
            "initialized shared ONNX environment with {:?}",
            requested
        );
        *shared = Some(Self {
            environment: environment.clone(),
            threading: Some(requested),
        });
        Ok(environment)
    }
}

/// Carries the provider and parser-wide graph, memory and threading policies to every session constructor.
#[derive(Debug, Clone, Copy, typed_builder::TypedBuilder)]
pub struct OnnxBackend {
    provider: ExecutionProvider,
    optimization_level: OptimizationLevel,
    memory_pattern: bool,
    #[builder(default)]
    threading: OnnxThreading,
    // Matches the runtime configuration default so ad-hoc builders share GPU memory the same way.
    #[cfg_attr(target_arch = "wasm32", allow(dead_code))]
    #[builder(default = true)]
    arena_shrinkage: bool,
    // Per-model provider tuning; the shared backend keeps defaults until a model applies its own.
    // Only CUDA reads it, so other builds record it without using it.
    #[cfg_attr(not(feature = "cuda"), allow(dead_code))]
    #[builder(default)]
    tuning: docparse_config::OnnxTuning,
    // Sessions with a profile run TensorRT in `tensorrt` builds; others stay on CUDA.
    #[builder(default)]
    tensorrt_profile: Option<TensorRtProfile>,
    // This configuration's engine cache; only `tensorrt` builds read it.
    #[cfg_attr(not(feature = "tensorrt"), allow(dead_code))]
    #[builder(default)]
    tensorrt_cache: Option<TensorRtCache>,
}

impl OnnxBackend {
    /// Registers a host-created ORT environment before any DocParse sessions are initialized.
    ///
    /// The host retains control of global thread pools and telemetry. Re-registering the same
    /// environment is harmless; an active environment is never replaced. DocParse retains a
    /// shared handle, so dropping the host's handle does not invalidate later parser creation.
    pub fn use_environment(
        environment: ort::environment::Environment,
    ) -> Result<(), LayoutError> {
        use ort::AsPointer;
        let mut shared =
            ENVIRONMENT.lock().map_err(|error| LayoutError::Engine {
                message: format!(
                    "ONNX environment initialization lock failed: {error}"
                ),
            })?;
        if let Some(installed) = shared.as_ref() {
            if !std::ptr::eq(installed.environment.ptr(), environment.ptr()) {
                return Err(LayoutError::Engine {
                    message: "register the host ONNX environment before initializing DocParse sessions".into(),
                });
            }
        } else {
            *shared = Some(SharedEnvironment {
                environment,
                threading: None,
            });
        }
        Ok(())
    }

    /// Reports the selected backend without exposing a runtime mutation mechanism.
    pub const fn execution_provider(self) -> ExecutionProvider {
        self.provider
    }

    /// Names the provider that actually runs this backend's sessions for logs and output
    /// provenance: TensorRT for a profiled CUDA session in `tensorrt` builds, otherwise the
    /// selected provider.
    pub fn provider_label(self) -> &'static str {
        if cfg!(feature = "tensorrt")
            && self.tensorrt_profile.is_some()
            && self.provider == ExecutionProvider::Cuda
        {
            "tensorrt"
        } else {
            self.provider.as_str()
        }
    }

    /// Chooses the compiled native provider with the shared configuration's default graph level.
    #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
    pub fn compiled() -> Self {
        Self::from_runtime(
            ExecutionProvider::compiled(),
            &docparse_config::RuntimeConfig::default(),
        )
    }

    /// Returns a copy that lets TensorRT build one engine covering this model's batch range;
    /// `None` keeps a model whose graph TensorRT rejects on the regular provider.
    pub fn with_tensorrt_profile(
        self,
        profile: impl Into<Option<TensorRtProfile>>,
    ) -> Self {
        Self {
            tensorrt_profile: profile.into(),
            ..self
        }
    }

    /// Returns a copy carrying one model's provider tuning, leaving the shared backend unchanged.
    pub fn tuned(self, tuning: docparse_config::OnnxTuning) -> Self {
        Self { tuning, ..self }
    }

    /// In `tensorrt` builds, every session that declares a shape profile runs TensorRT first
    /// (CUDA remains the fallback); sessions without one, such as the merged Texo decoder whose
    /// `If` graph TensorRT rejects, keep plain CUDA.
    fn tensorrt_provider(
        self,
    ) -> Result<Option<ort::ep::ExecutionProviderDispatch>, LayoutError> {
        #[cfg(all(not(target_arch = "wasm32"), feature = "tensorrt"))]
        if let Some(profile) = self.tensorrt_profile {
            if self.provider != ExecutionProvider::Cuda {
                tracing::error!(
                    "ONNX execution provider tensorrt is unavailable for provider {}",
                    self.provider
                );
                return Err(LayoutError::ExecutionProviderUnavailable {
                    provider: "tensorrt",
                });
            }
            return profile.execution_provider(self.tensorrt_cache).map(Some);
        }
        Ok(None)
    }

    /// The single mapping from runtime keys to session policy, shared by every constructor.
    fn from_runtime(
        provider: ExecutionProvider,
        runtime: &docparse_config::RuntimeConfig,
    ) -> Self {
        Self::builder()
            .provider(provider)
            .optimization_level(runtime.optimization_level)
            .memory_pattern(runtime.memory_pattern)
            .threading(OnnxThreading::from(runtime))
            .arena_shrinkage(runtime.onnx_arena_shrinkage)
            .tensorrt_cache(
                runtime
                    .tensorrt_cache_dir
                    .as_deref()
                    .map(TensorRtCache::new),
            )
            .build()
    }

    /// Builds per-run options; enabled CUDA runs return idle arena chunks so variable-shape
    /// models (OCR, table structure) do not keep their peak and starve other sessions.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn run_options(self) -> Result<ort::session::RunOptions, LayoutError> {
        let mut options = ort::session::RunOptions::new()?;
        if self.arena_shrinkage && self.provider == ExecutionProvider::Cuda {
            // The CUDA provider is registered on its default device.
            options.set("memory.enable_memory_arena_shrinkage", "gpu:0")?;
        }
        Ok(options)
    }

    /// Applies global graph and memory settings to inspection, compatibility sessions, and accelerated builders.
    pub fn cpu_builder(self) -> Result<SessionBuilder, LayoutError> {
        let level = match self.optimization_level {
            OptimizationLevel::Level1 => GraphOptimizationLevel::Level1,
            OptimizationLevel::Level2 => GraphOptimizationLevel::Level2,
            OptimizationLevel::Level3 => GraphOptimizationLevel::Level3,
            OptimizationLevel::All => GraphOptimizationLevel::All,
        };
        // rc.15 requires an explicit environment; its shared handle outlives all parser instances.
        let environment = SharedEnvironment::get_or_init(self.threading)?;
        // Host execution-provider defaults must not override the parser's compiled backend.
        let builder = Session::builder(&environment)?
            .with_no_environment_execution_providers()
            .map_err(ort::Error::from)?
            .with_optimization_level(level)
            .map_err(ort::Error::from)?
            .with_memory_pattern(self.memory_pattern)
            .map_err(ort::Error::from)?;
        #[cfg(not(target_arch = "wasm32"))]
        let builder = self.threading.apply(builder)?;
        Ok(builder)
    }
}

impl From<&ValidatedConfig> for OnnxBackend {
    /// Combines parser-wide runtime settings with the compiled native or host-selected browser provider.
    fn from(config: &ValidatedConfig) -> Self {
        #[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
        let provider = ExecutionProvider::compiled();
        #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
        let provider = if config.webgpu_enabled() {
            ExecutionProvider::WebGpu
        } else {
            ExecutionProvider::Cpu
        };
        Self::from_runtime(provider, config.runtime())
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl OnnxBackend {
    /// Serializes browser sessions through output readback, including work whose caller timed out.
    pub async fn inference_guard() -> tokio::sync::MutexGuard<'static, ()> {
        // ORT WebGPU shares download buffers across sessions. Overlapping layout and
        // TSR runs can unmap a buffer while the other session is still reading it.
        static INFERENCE: tokio::sync::Mutex<()> =
            tokio::sync::Mutex::const_new(());
        INFERENCE.lock().await
    }
}

impl TryFrom<OnnxBackend> for SessionBuilder {
    type Error = LayoutError;

    /// Registers requested accelerators strictly; unsupported builds never silently select CPU.
    fn try_from(backend: OnnxBackend) -> Result<Self, Self::Error> {
        // TensorRT runs first and falls back to the regular provider for unsupported nodes.
        let tensorrt = backend.tensorrt_provider()?;
        // Register the selected provider only after applying the shared per-parser session options.
        let builder = backend.cpu_builder()?;
        let provider = backend.provider;
        let dispatch = match provider {
            ExecutionProvider::Cpu => return Ok(builder),
            ExecutionProvider::Cuda => {
                #[cfg(all(not(target_arch = "wasm32"), feature = "cuda"))]
                {
                    // OCR widths and partial batches keep changing after warmup; select
                    // convolution kernels heuristically instead of benchmarking every new shape.
                    let cuda = ort::ep::CUDA::default()
                        .with_conv_algorithm_search(
                            ort::ep::cuda::ConvAlgorithmSearch::Heuristic,
                        );
                    // Always explicit: ONNX Runtime enables TF32 when the option is absent, so
                    // only setting `true` would leave `tf32 = false` unable to disable it.
                    Some(cuda.with_tf32(backend.tuning.tf32).build())
                }
                #[cfg(not(all(
                    not(target_arch = "wasm32"),
                    feature = "cuda"
                )))]
                {
                    None
                }
            }
            ExecutionProvider::CoreMl | ExecutionProvider::Metal => {
                #[cfg(all(not(target_arch = "wasm32"), feature = "coreml"))]
                {
                    let units = if provider == ExecutionProvider::Metal {
                        ort::ep::coreml::ComputeUnits::CPUAndGPU
                    } else {
                        ort::ep::coreml::ComputeUnits::All
                    };
                    tracing::info!(
                        "configuring CoreML with {:?} compute units and FastPrediction specialization",
                        units
                    );
                    Some(
                        ort::ep::CoreML::default()
                            .with_compute_units(units)
                            // Sessions are reused across pages; favor steady-state prediction over compilation latency.
                            .with_specialization_strategy(
                                ort::ep::coreml::SpecializationStrategy::FastPrediction,
                            )
                            .build(),
                    )
                }
                #[cfg(not(all(
                    not(target_arch = "wasm32"),
                    feature = "coreml"
                )))]
                {
                    None
                }
            }
            ExecutionProvider::Openvino => {
                #[cfg(all(not(target_arch = "wasm32"), feature = "openvino"))]
                {
                    Some(ort::ep::OpenVINO::default().build())
                }
                #[cfg(not(all(
                    not(target_arch = "wasm32"),
                    feature = "openvino"
                )))]
                {
                    None
                }
            }
            ExecutionProvider::WebGpu => {
                #[cfg(all(target_arch = "wasm32", feature = "wasm"))]
                {
                    Some(ort::ep::WebGPU::default().build())
                }
                #[cfg(not(all(target_arch = "wasm32", feature = "wasm")))]
                {
                    None
                }
            }
        };
        let dispatch: ort::ep::ExecutionProviderDispatch = dispatch
            .ok_or_else(|| {
                tracing::error!(
                    "ONNX execution provider {} is unavailable in this build",
                    provider
                );
                LayoutError::ExecutionProviderUnavailable {
                    provider: provider.as_str(),
                }
            })?;
        tracing::info!("registering ONNX execution provider {}", provider);
        let with_tensorrt = tensorrt.is_some();
        builder
            .with_execution_providers(
                tensorrt
                    .into_iter()
                    .chain([dispatch.error_on_failure()])
                    .collect::<Vec<_>>(),
            )
            .map_err(|error| {
                tracing::error!(
                    "ONNX execution provider {} (tensorrt first: {}) initialization failed: {}",
                    provider,
                    with_tensorrt,
                    error
                );
                LayoutError::from(ort::Error::from(error))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wasm_compat::ProfileDim;
    /// Well-formed stand-in for a pinned model digest.
    const DIGEST: &str =
        "45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba";

    /// Recreating parsers after idle gaps and concurrent loads must reuse ORT's sole environment.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn environment_survives_idle_gaps_and_concurrent_builders() {
        use ort::AsPointer;

        let policy = OnnxThreading::default();
        let environment =
            SharedEnvironment::get_or_init(policy).expect("first environment");
        let address = environment.ptr() as usize;
        drop(environment);
        let workers = (0..4)
            .map(|_| {
                std::thread::spawn(move || {
                    let environment = SharedEnvironment::get_or_init(policy)
                        .expect("shared environment");
                    let builder = Session::builder(&environment)
                        .expect("session builder");
                    drop(builder);
                    environment.ptr() as usize
                })
            })
            .collect::<Vec<_>>();
        for worker in workers {
            assert_eq!(worker.join().expect("builder thread"), address);
        }
    }
    /// Every native model receives the compiled backend even when built from code-default configuration.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn native_models_share_the_compiled_backend() {
        let config =
            ValidatedConfig::try_from(docparse_config::RawConfig::default())
                .expect("config");
        let selected = OnnxBackend::from(&config).execution_provider();
        assert_eq!(selected, OnnxBackend::compiled().execution_provider());
        for (provider, enabled) in [
            (ExecutionProvider::Cuda, cfg!(feature = "cuda")),
            (ExecutionProvider::Metal, cfg!(feature = "metal")),
            (
                ExecutionProvider::CoreMl,
                cfg!(feature = "coreml") && !cfg!(feature = "metal"),
            ),
            (ExecutionProvider::Openvino, cfg!(feature = "openvino")),
            (
                ExecutionProvider::Cpu,
                !cfg!(any(
                    feature = "cuda",
                    feature = "coreml",
                    feature = "openvino"
                )),
            ),
        ] {
            assert_eq!(selected == provider, enabled, "{provider}");
        }
    }
    /// A missing accelerator feature must fail instead of silently creating a CPU session.
    #[test]
    fn unsupported_providers_are_rejected() {
        for (provider, enabled) in [
            (ExecutionProvider::Cuda, cfg!(feature = "cuda")),
            (ExecutionProvider::CoreMl, cfg!(feature = "coreml")),
            (ExecutionProvider::Metal, cfg!(feature = "coreml")),
            (ExecutionProvider::Openvino, cfg!(feature = "openvino")),
            (
                ExecutionProvider::WebGpu,
                cfg!(all(feature = "wasm", target_arch = "wasm32")),
            ),
        ] {
            if !enabled {
                assert!(
                    matches!(SessionBuilder::try_from(OnnxBackend::builder().provider(provider).optimization_level(OptimizationLevel::default()).memory_pattern(false).build()),
                    Err(LayoutError::ExecutionProviderUnavailable { provider: name }) if name == provider.as_str())
                );
            }
        }
    }
    /// Automatic global sizing follows cgroup-aware parallelism, and explicit values win.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn global_threads_resolve_auto_and_explicit_values() {
        let auto = OnnxThreading::default();
        assert_eq!(
            auto.global_threads(),
            std::thread::available_parallelism().map_or(1, usize::from)
        );
        let explicit = OnnxThreading {
            intra_threads: 3,
            ..OnnxThreading::default()
        };
        assert_eq!(explicit.global_threads(), 3);
    }
    /// Runtime keys map directly, and the default policy is the shared, non-spinning pool.
    #[test]
    fn threading_follows_runtime_config() {
        let mut runtime = docparse_config::RuntimeConfig::default();
        assert_eq!(
            OnnxThreading::from(&runtime),
            OnnxThreading {
                pool: OnnxThreadPool::Global,
                intra_threads: 0,
                spinning: false,
            }
        );
        runtime.onnx_thread_pool = OnnxThreadPool::Session;
        runtime.onnx_intra_threads = 2;
        runtime.onnx_spinning = true;
        assert_eq!(
            OnnxThreading::from(&runtime),
            OnnxThreading {
                pool: OnnxThreadPool::Session,
                intra_threads: 2,
                spinning: true,
            }
        );
    }
    /// CUDA runs return idle arena chunks only when enabled; other providers keep plain options.
    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn arena_shrinkage_applies_only_to_enabled_cuda() {
        let backend = |provider, arena_shrinkage| {
            OnnxBackend::builder()
                .provider(provider)
                .optimization_level(OptimizationLevel::default())
                .memory_pattern(false)
                .arena_shrinkage(arena_shrinkage)
                .build()
        };
        let key = "memory.enable_memory_arena_shrinkage";
        let mut enabled = backend(ExecutionProvider::Cuda, true)
            .run_options()
            .expect("options");
        assert_eq!(enabled.get(key).as_deref(), Some("gpu:0"));
        for (provider, shrink) in [
            (ExecutionProvider::Cuda, false),
            (ExecutionProvider::Cpu, true),
        ] {
            let mut options =
                backend(provider, shrink).run_options().expect("options");
            assert_eq!(options.get(key), None);
        }
    }
    /// Tuning a copy leaves the shared backend, and so every other model, untouched.
    #[test]
    fn tuned_changes_only_this_backend() {
        let tuning = docparse_config::OnnxTuning { tf32: false };
        let shared = OnnxBackend::builder()
            .provider(ExecutionProvider::Cuda)
            .optimization_level(OptimizationLevel::default())
            .memory_pattern(false)
            .build();
        let tuned = shared.tuned(tuning);
        assert_eq!(shared.tuning, docparse_config::OnnxTuning::default());
        assert_eq!(tuned.tuning, tuning);
    }
    /// A profiled session on a non-CUDA provider is refused instead of silently skipping TensorRT.
    #[cfg(all(not(target_arch = "wasm32"), feature = "tensorrt"))]
    #[test]
    fn tensorrt_requires_cuda_provider() {
        static INPUTS: [(&str, &[ProfileDim]); 1] =
            [("x", &[ProfileDim::Fixed(3)])];
        let backend = OnnxBackend::builder()
            .provider(ExecutionProvider::Cpu)
            .optimization_level(OptimizationLevel::default())
            .memory_pattern(false)
            .build()
            .with_tensorrt_profile(
                TensorRtProfile::builder()
                    .inputs(&INPUTS)
                    .max_batch(1)
                    .model(DIGEST)
                    .build(),
            );
        assert!(matches!(
            SessionBuilder::try_from(backend),
            Err(LayoutError::ExecutionProviderUnavailable {
                provider: "tensorrt"
            })
        ));
    }
    /// Without the feature a declared profile is inert: CPU builds still create sessions.
    #[cfg(not(feature = "tensorrt"))]
    #[test]
    fn tensorrt_profile_is_inert_without_feature() {
        static INPUTS: [(&str, &[ProfileDim]); 1] =
            [("x", &[ProfileDim::Fixed(3)])];
        let backend = OnnxBackend::builder()
            .provider(ExecutionProvider::Cpu)
            .optimization_level(OptimizationLevel::default())
            .memory_pattern(false)
            .build()
            .with_tensorrt_profile(
                TensorRtProfile::builder()
                    .inputs(&INPUTS)
                    .max_batch(1)
                    .model(DIGEST)
                    .build(),
            );
        SessionBuilder::try_from(backend)
            .expect("CPU builder without tensorrt");
    }
    /// Without a cache directory TensorRT would rebuild every engine on each start, so profiled
    /// sessions are refused before any provider loads, even after another configuration in the
    /// same process registered its own directory.
    #[cfg(all(not(target_arch = "wasm32"), feature = "tensorrt"))]
    #[test]
    fn tensorrt_requires_cache_directory() {
        static INPUTS: [(&str, &[ProfileDim]); 1] =
            [("x", &[ProfileDim::Fixed(3)])];
        let directory = tempfile::tempdir().expect("directory");
        let other = docparse_config::RuntimeConfig {
            tensorrt_cache_dir: Some(directory.path().to_path_buf()),
            ..Default::default()
        };
        let _ = OnnxBackend::from_runtime(ExecutionProvider::Cuda, &other);
        let backend = OnnxBackend::from_runtime(
            ExecutionProvider::Cuda,
            &docparse_config::RuntimeConfig::default(),
        )
        .with_tensorrt_profile(
            TensorRtProfile::builder()
                .inputs(&INPUTS)
                .max_batch(1)
                .model(DIGEST)
                .build(),
        );
        assert!(matches!(
            SessionBuilder::try_from(backend),
            Err(LayoutError::TensorRtCacheUnset)
        ));
    }
    /// Each configuration carries its own TensorRT cache: one without a directory, or with a
    /// different one, is never served by a directory another configuration registered first.
    #[test]
    fn tensorrt_cache_follows_each_configuration() {
        let first = tempfile::tempdir().expect("directory");
        let second = tempfile::tempdir().expect("directory");
        let cache_of = |dir: Option<&std::path::Path>| {
            let runtime = docparse_config::RuntimeConfig {
                tensorrt_cache_dir: dir.map(std::path::Path::to_path_buf),
                ..Default::default()
            };
            OnnxBackend::from_runtime(ExecutionProvider::Cuda, &runtime)
                .tensorrt_cache
                .map(TensorRtCache::path)
        };
        assert_eq!(cache_of(Some(first.path())), Some(first.path()));
        assert_eq!(cache_of(None), None);
        assert_eq!(cache_of(Some(second.path())), Some(second.path()));
        assert_eq!(cache_of(Some(first.path())), Some(first.path()));
    }
    /// Labels name TensorRT only for sessions it actually runs: a profiled CUDA session in a
    /// `tensorrt` build; an unprofiled session, or any session in other builds, keeps its provider.
    #[test]
    fn provider_label_names_tensorrt_only_for_profiled_sessions() {
        static INPUTS: [(&str, &[ProfileDim]); 1] =
            [("x", &[ProfileDim::Fixed(3)])];
        let cuda = OnnxBackend::builder()
            .provider(ExecutionProvider::Cuda)
            .optimization_level(OptimizationLevel::default())
            .memory_pattern(false)
            .build();
        let profiled = cuda.with_tensorrt_profile(
            TensorRtProfile::builder()
                .inputs(&INPUTS)
                .max_batch(1)
                .model(DIGEST)
                .build(),
        );
        assert_eq!(cuda.provider_label(), "cuda");
        assert_eq!(
            profiled.provider_label(),
            if cfg!(feature = "tensorrt") {
                "tensorrt"
            } else {
                "cuda"
            }
        );
    }
}
