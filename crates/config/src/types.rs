use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

/// The complete unvalidated configuration after all source layers are merged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub layout: LayoutConfig,
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
            .execution_provider(ExecutionProviderConfig::Cpu)
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
    Openvino,
}

impl Default for ExecutionProviderConfig {
    /// Selects the universally available CPU provider.
    fn default() -> Self {
        Self::Cpu
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
}

impl Default for OcrPolicy {
    /// Disables OCR unless the caller explicitly opts in.
    fn default() -> Self {
        Self::Disabled
    }
}

/// OCR configuration exposed even though the first version ships no engine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct OcrConfig {
    pub policy: OcrPolicy,
}

impl Default for OcrConfig {
    /// Builds the default disabled OCR policy.
    fn default() -> Self {
        Self::builder().policy(OcrPolicy::default()).build()
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
