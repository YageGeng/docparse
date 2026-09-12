use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

/// The complete unvalidated configuration after all source layers are merged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub layout: LayoutConfig,
    #[builder(default)]
    #[serde(default)]
    pub tsr: TsrConfig,
    pub runtime: RuntimeConfig,
    pub render: RenderConfig,
    pub fusion: FusionConfig,
    pub ocr: OcrConfig,
    pub output: OutputConfig,
}

impl Default for RawConfig {
    /// Builds the complete code-default configuration.
    fn default() -> Self {
        Self::builder()
            .layout(LayoutConfig::default())
            .tsr(TsrConfig::default())
            .runtime(RuntimeConfig::default())
            .render(RenderConfig::default())
            .fusion(FusionConfig::default())
            .ocr(OcrConfig::default())
            .output(OutputConfig::default())
            .build()
    }
}

/// Configuration for the default PP-DocLayoutV3 engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct LayoutConfig {
    pub model_path: PathBuf,
    pub model_config_path: PathBuf,
    pub model_manifest_path: PathBuf,
    pub score_threshold: f64,
    pub execution_provider: ExecutionProviderConfig,
    pub session_pool_size: usize,
}

impl Default for LayoutConfig {
    /// Builds the default layout model paths and execution settings.
    fn default() -> Self {
        Self::builder()
            .model_path(PathBuf::from("models/pp-doclayout-v3/inference.onnx"))
            .model_config_path(PathBuf::from(
                "models/pp-doclayout-v3/inference.yml",
            ))
            .model_manifest_path(PathBuf::from(
                "models/pp-doclayout-v3/model-manifest.json",
            ))
            .score_threshold(0.5)
            .execution_provider(ExecutionProviderConfig::default())
            .session_pool_size(1)
            .build()
    }
}

/// ONNX Runtime execution provider requested by configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionProviderConfig {
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

impl ExecutionProviderConfig {
    /// Returns the stable configuration and diagnostic name of a backend.
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
impl std::fmt::Display for ExecutionProviderConfig {
    /// Formats backend names consistently across model initialization and inference logs.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Bounds for document-level asynchronous and blocking work.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    pub page_concurrency: usize,
    pub render_queue_capacity: usize,
    pub blocking_task_limit: usize,
    pub continue_on_page_error: bool,
}

impl Default for RuntimeConfig {
    /// Builds conservative default concurrency limits.
    fn default() -> Self {
        Self::builder()
            .page_concurrency(4)
            .render_queue_capacity(2)
            .blocking_task_limit(4)
            .continue_on_page_error(true)
            .build()
    }
}

/// Page rasterization settings shared by extraction and layout inference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct RenderConfig {
    pub dpi: u32,
    pub max_long_edge_pixels: u32,
}

impl Default for RenderConfig {
    /// Builds the default rasterization quality limits.
    fn default() -> Self {
        Self::builder().dpi(144).max_long_edge_pixels(2400).build()
    }
}

/// Thresholds and weights used by page-local layout/text fusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct FusionConfig {
    pub minimum_line_coverage: f64,
    pub center_minimum_line_coverage: f64,
    pub assignment_coverage_weight: f64,
    pub assignment_center_weight: f64,
    pub assignment_baseline_weight: f64,
    pub assignment_confidence_weight: f64,
    pub assignment_specificity_weight: f64,
    pub paragraph_gap_multiplier: f64,
    pub indent_tolerance_points: f64,
    pub font_size_tolerance_points: f64,
    pub estimated_font_size_tolerance_points: f64,
}

impl Default for FusionConfig {
    /// Builds the first-version fusion policy defaults.
    fn default() -> Self {
        Self::builder()
            .minimum_line_coverage(0.30)
            .center_minimum_line_coverage(0.10)
            .assignment_coverage_weight(0.55)
            .assignment_center_weight(0.20)
            .assignment_baseline_weight(0.10)
            .assignment_confidence_weight(0.10)
            .assignment_specificity_weight(0.05)
            .paragraph_gap_multiplier(1.5)
            .indent_tolerance_points(6.0)
            .font_size_tolerance_points(0.5)
            .estimated_font_size_tolerance_points(1.5)
            .build()
    }
}

/// OCR invocation policy for a parser instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OcrPolicy {
    Disabled,
    MissingRegions,
    /// Runs OCR over the whole page while retaining healthy native text during fusion.
    Always,
}

impl Default for OcrPolicy {
    /// Disables OCR unless the caller explicitly opts in.
    fn default() -> Self {
        Self::Disabled
    }
}

/// Built-in PaddleOCR artifacts, inference limits and native-text enrichment policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct OcrConfig {
    pub policy: OcrPolicy,
    pub execution_provider: ExecutionProviderConfig,
    pub detection_model_dir: PathBuf,
    pub recognition_model_dir: PathBuf,
    pub orientation_model_dir: PathBuf,
    pub detection_max_side: u32,
    pub detection_threshold: f64,
    pub box_threshold: f64,
    pub unclip_ratio: f64,
    pub max_candidates: usize,
    pub recognition_max_width: u32,
    pub recognition_threshold: f64,
    pub classify_orientation: bool,
    pub orientation_threshold: f64,
    pub timeout_ms: u64,
}

impl Default for OcrConfig {
    /// Builds the default disabled OCR policy.
    fn default() -> Self {
        Self::builder()
            .policy(OcrPolicy::default())
            .execution_provider(ExecutionProviderConfig::default())
            .detection_model_dir(PathBuf::from("models/pp-ocrv6-medium-det"))
            .recognition_model_dir(PathBuf::from("models/pp-ocrv6-medium-rec"))
            .orientation_model_dir(PathBuf::from(
                "models/pp-lcnet-textline-ori",
            ))
            .detection_max_side(2048)
            .detection_threshold(0.2)
            .box_threshold(0.45)
            .unclip_ratio(1.4)
            .max_candidates(3000)
            .recognition_max_width(3200)
            .recognition_threshold(0.5)
            .classify_orientation(true)
            .orientation_threshold(0.9)
            .timeout_ms(120_000)
            .build()
    }
}

/// Rendering options that do not mutate the canonical parse result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct OutputConfig {
    pub formula_placeholder: String,
    pub include_evidence: bool,
    pub include_diagnostics: bool,
}

impl Default for OutputConfig {
    /// Builds the default output visibility policy.
    fn default() -> Self {
        Self::builder()
            .formula_placeholder("[formula]".to_owned())
            .include_evidence(true)
            .include_diagnostics(false)
            .build()
    }
}

/// Selects structure recovery for the table regions already owned by layout.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TableMode {
    RulesOnly,
    #[default]
    Fallback,
    ExternalOnly,
}

/// Independent SLANet_plus artifacts, execution backend, and per-document table policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct TsrConfig {
    #[builder(default)]
    #[serde(default)]
    pub execution_provider: ExecutionProviderConfig,
    pub model_path: PathBuf,
    pub model_config_path: PathBuf,
    pub model_manifest_path: PathBuf,
    pub mode: TableMode,
    pub max_in_flight: usize,
    pub timeout_ms: u64,
}

impl Default for TsrConfig {
    /// Uses local reconstruction first and the pinned TSR model for unresolved tables.
    fn default() -> Self {
        Self::builder()
            .model_path(PathBuf::from("models/slanet-plus/inference.onnx"))
            .model_config_path(PathBuf::from(
                "models/slanet-plus/inference.yml",
            ))
            .model_manifest_path(PathBuf::from(
                "models/slanet-plus/model-manifest.json",
            ))
            .mode(TableMode::default())
            .max_in_flight(2)
            .timeout_ms(60_000)
            .build()
    }
}
