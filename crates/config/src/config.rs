//! Configuration definitions and defaults shared by parser and server consumers.
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

/// The complete unvalidated configuration after all source layers are merged.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct RawConfig {
    pub layout: LayoutConfig,
    #[builder(default)]
    pub tsr: TsrConfig,
    /// Optional formula recognition over existing layout detections.
    #[builder(default)]
    pub formula: FormulaConfig,
    pub runtime: RuntimeConfig,
    pub render: RenderConfig,
    pub fusion: FusionConfig,
    pub ocr: OcrConfig,
    pub output: OutputConfig,
    /// Server logging is configured before connections and model initialization.
    #[builder(default)]
    #[serde(default)]
    pub log: LogConfig,
    /// Native HTTP settings share the loader but are not part of parser validation.
    #[builder(default)]
    #[serde(default)]
    pub server: ServerConfig,
    /// Native database consumers validate these settings before opening a connection pool.
    #[builder(default)]
    #[serde(default)]
    pub database: DatabaseConfig,
    /// Optional historical query upstream; local metrics are always available on the server.
    #[builder(default)]
    #[serde(default)]
    pub monitoring: MonitoringConfig,
}

/// Server-side historical queries never accept a browser-supplied upstream URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub struct MonitoringConfig {
    pub prometheus_url: Option<String>,
}

/// Server event filters and optional file destination; native setup validates logging resources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(default, deny_unknown_fields)]
pub struct LogConfig {
    #[builder(default = "info,ort=warn,sqlx=warn".to_owned(), setter(into))]
    pub directives: String,
    /// Appends plain-text logs here; relative paths resolve beside the primary configuration file.
    #[builder(default, setter(strip_option))]
    pub file: Option<PathBuf>,
}

impl Default for LogConfig {
    /// Preserves the existing server filter for configurations that omit the log section.
    fn default() -> Self {
        Self::builder().build()
    }
}

/// Native listener and admission limits; host names and IP addresses are resolved by Tokio at bind time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    /// Maximum concurrent upload requests accepted by one server.
    #[builder(default = 4)]
    pub max_uploads: usize,
    #[builder(default = "127.0.0.1".to_owned(), setter(into))]
    pub host: String,
    #[builder(default = 8080)]
    pub port: u16,
    /// Prefix shared by all HTTP endpoints and their generated OpenAPI paths.
    #[builder(default = "/api".to_owned(), setter(into))]
    pub api_prefix: String,
}

impl Default for ServerConfig {
    /// Retains the existing loopback listener while allowing a zero port for OS-assigned test listeners.
    fn default() -> Self {
        Self::builder().build()
    }
}

/// PostgreSQL pool settings use the same field names and millisecond units as WisLand.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(default, deny_unknown_fields)]
pub struct DatabaseConfig {
    #[builder(default, setter(into))]
    pub url: String,
    #[builder(default = 5000)]
    pub timeout_ms: u64,
    #[builder(default = 5000)]
    pub acquire_timeout_ms: u64,
    #[builder(default = 600_000)]
    pub idle_timeout_ms: u64,
    #[builder(default = 10)]
    pub max_connections: u32,
    #[builder(default = 1)]
    pub min_connections: u32,
    /// Ordinary query events stay at debug by default; off disables them independently of slow queries.
    #[builder(default = log::LevelFilter::Debug)]
    pub sqlx_logging_level: log::LevelFilter,
    /// Slow query events use a separate level and still obey the application tracing filter.
    #[builder(default = log::LevelFilter::Warn)]
    pub sqlx_slow_statements_logging_level: log::LevelFilter,
    /// Queries taking at least this many milliseconds use the slow query level.
    #[builder(default = 1000)]
    pub sqlx_slow_statements_threshold_ms: u64,
}

impl Default for DatabaseConfig {
    /// Leaves credentials unconfigured so standalone parsing never requires a database URL.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl std::fmt::Debug for DatabaseConfig {
    /// Keeps connection credentials out of parent configuration diagnostics.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DatabaseConfig")
            .field("url", &"<redacted>")
            .field("timeout_ms", &self.timeout_ms)
            .field("acquire_timeout_ms", &self.acquire_timeout_ms)
            .field("idle_timeout_ms", &self.idle_timeout_ms)
            .field("max_connections", &self.max_connections)
            .field("min_connections", &self.min_connections)
            .field("sqlx_logging_level", &self.sqlx_logging_level)
            .field(
                "sqlx_slow_statements_logging_level",
                &self.sqlx_slow_statements_logging_level,
            )
            .field(
                "sqlx_slow_statements_threshold_ms",
                &self.sqlx_slow_statements_threshold_ms,
            )
            .finish()
    }
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

/// Formula engine selection and shared bounded inference policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct FormulaConfig {
    /// Required pending-crop capacity, independent of model sessions and batch size.
    pub queue_size: usize,
    /// Recognize detected inline formulas; false preserves their native text and layout.
    #[serde(default = "FormulaConfig::default_enabled")]
    #[builder(default = true)]
    pub inline_enabled: bool,
    /// Recognize detected display formulas independently of inline recognition.
    #[serde(default = "FormulaConfig::default_enabled")]
    #[builder(default = true)]
    pub display_enabled: bool,
    /// Explicit recognizer selection; model paths or service settings belong to its variant.
    #[serde(default)]
    #[builder(default)]
    pub engine: FormulaEngineConfig,
    /// Maximum ready crops per local model invocation; MinerU uses its HTTP concurrency limit.
    #[serde(default = "FormulaConfig::default_batch_size")]
    #[builder(default = 4)]
    pub batch_size: usize,
    /// Per-crop parser deadline including pre-crop admission and shared model queue time.
    #[serde(default = "FormulaConfig::default_timeout_ms")]
    #[builder(default = 120_000)]
    pub timeout_ms: u64,
}

impl FormulaConfig {
    /// Preserves optional recognition switches while keeping queue_size required during deserialization.
    const fn default_enabled() -> bool {
        true
    }
    /// Preserves the existing batch default independently of the required queue capacity.
    const fn default_batch_size() -> usize {
        4
    }
    /// Preserves the existing deadline for partial serialized formula options.
    const fn default_timeout_ms() -> u64 {
        120_000
    }
}

impl Default for FormulaConfig {
    /// Enables both formula kinds unless the caller explicitly disables either one.
    fn default() -> Self {
        Self::builder().queue_size(4).build()
    }
}

/// Explicit formula recognizer and its variant-specific artifacts or service settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FormulaEngineConfig {
    /// PP-FormulaNet Plus-S, Plus-M, or Plus-L, identified by its pinned manifest.
    Pp(PpFormulaConfig),
    /// Texo's encoder and cached decoder with the matching WordLevel tokenizer.
    Texo(TexoFormulaConfig),
    /// External MinerU vLLM service; no local formula model files are required.
    Mineru(MineruFormulaConfig),
}

/// Native MinerU service address and the shared limit on in-flight crop requests.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MineruFormulaConfig {
    /// HTTP(S) service root or OpenAI API base ending in `/v1`.
    pub server_url: String,
    /// Maximum concurrent HTTP requests across all pages using one engine.
    pub concurrency: usize,
}

impl Default for MineruFormulaConfig {
    /// Matches the separately deployed MinerU vLLM service without selecting it by default.
    fn default() -> Self {
        Self {
            server_url: "http://127.0.0.1:8000".into(),
            concurrency: 8,
        }
    }
}

impl MineruFormulaConfig {
    /// Validates service settings and preserves reverse-proxy prefixes when building the endpoint.
    pub fn endpoint(&self) -> Result<url::Url, crate::ConfigError> {
        if !(1..=1024).contains(&self.concurrency) {
            return Err(crate::ConfigError::InvalidValue {
                field: "formula.engine.concurrency",
                reason: "must be between 1 and 1024",
            });
        }
        let invalid_url = || crate::ConfigError::InvalidValue {
            field: "formula.engine.server_url",
            reason: "must be an HTTP(S) base URL without credentials, query, or fragment",
        };
        let mut endpoint = url::Url::parse(&self.server_url)
            .map_err(|_parse_error| invalid_url())?;
        if !matches!(endpoint.scheme(), "http" | "https")
            || endpoint.host_str().is_none()
            || !endpoint.username().is_empty()
            || endpoint.password().is_some()
            || endpoint.query().is_some()
            || endpoint.fragment().is_some()
        {
            return Err(invalid_url());
        }
        let base = endpoint.path().trim_end_matches('/');
        let path = if base.ends_with("/v1") {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/v1/chat/completions")
        };
        endpoint.set_path(&path);
        Ok(endpoint)
    }
}

impl Default for FormulaEngineConfig {
    /// Uses Texo unless configuration explicitly selects PP-FormulaNet.
    fn default() -> Self {
        Self::Texo(TexoFormulaConfig::default())
    }
}

/// Files and session count used only by the PP-FormulaNet recognizer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(default, deny_unknown_fields)]
pub struct PpFormulaConfig {
    /// Independent model consumers sharing one crop queue, matching Texo's session policy.
    #[serde(default = "default_session_size")]
    #[builder(default = 1)]
    pub session_size: usize,
    /// ONNX graph for the selected PP-FormulaNet variant.
    pub model_path: PathBuf,
    /// Matching ByteLevel BPE tokenizer.
    pub tokenizer_path: PathBuf,
    /// Immutable model identity and SHA-256 digests.
    pub model_manifest_path: PathBuf,
}

impl Default for PpFormulaConfig {
    /// Uses the existing pinned PP-FormulaNet Plus-S artifact set.
    fn default() -> Self {
        Self::builder()
            .model_path("models/pp-formulanet-plus-s/inference.onnx".into())
            .tokenizer_path("models/pp-formulanet-plus-s/tokenizer.json".into())
            .model_manifest_path(
                "models/pp-formulanet-plus-s/model-manifest.json".into(),
            )
            .build()
    }
}

/// Artifacts and independent encoder/decoder owners used only by the Texo recognizer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(default, deny_unknown_fields)]
pub struct TexoFormulaConfig {
    /// Independent encoder/decoder owners consuming the same crop queue.
    #[builder(default = 1)]
    pub session_size: usize,
    /// Image encoder ONNX graph.
    pub encoder_path: PathBuf,
    /// Merged first-step/cached decoder ONNX graph.
    pub decoder_path: PathBuf,
    /// Matching WordLevel tokenizer.
    pub tokenizer_path: PathBuf,
}

impl Default for TexoFormulaConfig {
    /// Uses the three author-published artifacts installed by the Texo downloader.
    fn default() -> Self {
        Self::builder()
            .encoder_path("models/texo/encoder_model.onnx".into())
            .decoder_path("models/texo/decoder_model_merged.onnx".into())
            .tokenizer_path("models/texo/tokenizer.json".into())
            .build()
    }
}

/// Configuration for the default PP-DocLayoutV3 engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct LayoutConfig {
    /// Required pending-page capacity shared by all layout sessions.
    pub queue_size: usize,
    pub model_path: PathBuf,
    pub model_config_path: PathBuf,
    pub model_manifest_path: PathBuf,
    pub score_threshold: f64,
    /// Independent ONNX sessions shared by all documents; each executes one layout inference at a time.
    pub session_size: usize,
    /// Maximum ready pages per physical layout inference; short batches run immediately.
    #[serde(default = "default_session_size")]
    #[builder(default = 1)]
    pub batch_size: usize,
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
            .session_size(1)
            .queue_size(1)
            .build()
    }
}

/// Graph optimization policy shared by all ONNX sessions, independent of execution provider.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum OptimizationLevel {
    #[default]
    Level1,
    Level2,
    Level3,
    All,
}

/// Shared inference and page failure policies; render capacity is configured under RenderConfig.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct RuntimeConfig {
    /// One setting governs every model; omitted values use basic graph rewrites.
    #[serde(default)]
    #[builder(default)]
    pub optimization_level: OptimizationLevel,
    /// Shared memory-plan switch; disabled by default for variable batches and decoder caches.
    #[serde(default)]
    #[builder(default)]
    pub memory_pattern: bool,
    /// The shorter key retains continuation after recoverable page failures, not fatal document errors.
    pub continue_on_error: bool,
}

impl Default for RuntimeConfig {
    /// Defaults every session to level1 and preserves fallback after recoverable page errors.
    fn default() -> Self {
        Self::builder().continue_on_error(true).build()
    }
}

/// Page rasterization settings shared by extraction and layout inference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct RenderConfig {
    /// Required process count; idle PDFium workers alone admit new documents.
    pub workers: usize,
    /// Required shared capacity for pages reserved through actual processing completion.
    pub queue_size: usize,
    pub dpi: u32,
    pub max_long_edge_pixels: u32,
}

impl Default for RenderConfig {
    /// Builds the default rasterization quality limits.
    fn default() -> Self {
        Self::builder()
            .workers(1)
            .queue_size(16)
            .dpi(144)
            .max_long_edge_pixels(2400)
            .build()
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

/// Independently configurable files for one verified ONNX model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct ModelFiles {
    pub model_path: PathBuf,
    pub model_config_path: PathBuf,
    pub model_manifest_path: PathBuf,
}

impl ModelFiles {
    /// Supplies conventional default filenames while allowing each path to be overridden independently.
    fn in_directory(directory: &str) -> Self {
        let directory = PathBuf::from(directory);
        Self::builder()
            .model_path(directory.join("inference.onnx"))
            .model_config_path(directory.join("inference.yml"))
            .model_manifest_path(directory.join("model-manifest.json"))
            .build()
    }
}

/// Preserves a single session and singleton batch for newly introduced inference settings.
const fn default_session_size() -> usize {
    1
}

/// One OCR model owns its own sessions, ready queue, and model batch limit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct OcrModelConfig {
    /// Required pending-input capacity shared by this model's consumers.
    pub queue_size: usize,
    #[serde(flatten)]
    pub files: ModelFiles,
    /// Number of independently owned ONNX sessions consuming the same queue.
    #[serde(default = "default_session_size")]
    #[builder(default = 1)]
    pub session_size: usize,
    /// Maximum ready inputs; incompatible tensor shapes remain separate physical batches.
    #[serde(default = "default_session_size")]
    #[builder(default = 1)]
    pub batch_size: usize,
}

/// Built-in PaddleOCR artifacts, inference limits and native-text enrichment policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct OcrConfig {
    pub policy: OcrPolicy,
    /// Model-specific limits replace the ambiguous shared OCR batch size.
    pub detection: OcrModelConfig,
    pub recognition: OcrModelConfig,
    pub orientation: OcrModelConfig,
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
            .detection(
                OcrModelConfig::builder()
                    .queue_size(1)
                    .files(ModelFiles::in_directory(
                        "models/pp-ocrv6-medium-det",
                    ))
                    .build(),
            )
            .recognition(
                OcrModelConfig::builder()
                    .queue_size(16)
                    .files(ModelFiles::in_directory(
                        "models/pp-ocrv6-medium-rec",
                    ))
                    .batch_size(16)
                    .build(),
            )
            .orientation(
                OcrModelConfig::builder()
                    .queue_size(16)
                    .files(ModelFiles::in_directory(
                        "models/pp-lcnet-textline-ori",
                    ))
                    .batch_size(16)
                    .build(),
            )
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
    TsrOnly,
}

/// Selects a fixed table structure model independently of its artifact paths.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum TsrModel {
    #[default]
    SlanetPlus,
    SlanextWired,
    SlanextWireless,
    /// Microsoft TATR v1.1 recognizes structure alongside the shared cell detector.
    Tatr,
}

/// Selects the dedicated detector for the table image family under evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableCellModel {
    Wired,
    Wireless,
}

/// Independently verified detection artifacts and the acceptance threshold for cells.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct TableCellConfig {
    /// Required pending-crop capacity independent of structure-model admission.
    pub queue_size: usize,
    /// Independent detector sessions consume the same ready crop queue.
    #[serde(default = "default_session_size")]
    #[builder(default = 1)]
    pub session_size: usize,
    /// Allows layered profiles to disable the default detector without removing its paths.
    #[builder(default = true)]
    pub enabled: bool,
    pub model: TableCellModel,
    /// Maximum ready crops combined into one detector call; one preserves existing memory usage.
    #[builder(default = 1)]
    pub batch_size: usize,
    #[serde(flatten)]
    pub files: ModelFiles,
    pub score_threshold: f64,
}

impl Default for TableCellConfig {
    /// Uses the wireless RT-DETR model alongside SLANet+ structure recognition.
    fn default() -> Self {
        Self::builder()
            .model(TableCellModel::Wireless)
            .queue_size(1)
            .files(ModelFiles::in_directory(
                "models/rtdetr-table-cell-wireless",
            ))
            .score_threshold(0.3)
            .build()
    }
}

/// Structure artifacts, optional cell detection and per-document policy; the build selects the backend.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
#[serde(deny_unknown_fields)]
pub struct TsrConfig {
    /// Required pending-crop capacity shared by all structure sessions.
    pub queue_size: usize,
    /// Independent structure sessions consume the same ready crop queue.
    #[serde(default = "default_session_size")]
    #[builder(default = 1)]
    pub session_size: usize,
    #[serde(default)]
    #[builder(default)]
    pub model: TsrModel,
    // A missing section must not silently enable a detector with an implicit capacity; explicit null still disables it.
    #[serde(deserialize_with = "Option::deserialize")]
    #[builder(default = TsrConfig::default_cell_detection())]
    pub cell_detection: Option<TableCellConfig>,
    pub model_path: PathBuf,
    pub model_config_path: PathBuf,
    pub model_manifest_path: PathBuf,
    pub mode: TableMode,
    /// Maximum ready crops combined into one structure call; the model queue provides backpressure.
    #[serde(default = "TsrConfig::default_batch_size")]
    #[builder(default = Self::default_batch_size())]
    pub batch_size: usize,
    pub timeout_ms: u64,
}

impl TsrConfig {
    /// Keeps old configurations at singleton inference until batching is explicitly selected.
    const fn default_batch_size() -> usize {
        1
    }

    /// Keeps serialized and builder defaults aligned for the recommended model combination.
    fn default_cell_detection() -> Option<TableCellConfig> {
        Some(TableCellConfig::default())
    }
}

impl Default for TsrConfig {
    /// Uses local reconstruction first and the pinned TSR model for unresolved tables.
    fn default() -> Self {
        Self::builder()
            .model_path(PathBuf::from("models/slanet-plus/inference.onnx"))
            .queue_size(1)
            .model_config_path(PathBuf::from(
                "models/slanet-plus/inference.yml",
            ))
            .model_manifest_path(PathBuf::from(
                "models/slanet-plus/model-manifest.json",
            ))
            .mode(TableMode::default())
            .timeout_ms(60_000)
            .build()
    }
}
