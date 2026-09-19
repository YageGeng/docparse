use crate::{
    ConfigError, DatabaseConfig, FormulaConfig, FusionConfig, LayoutConfig,
    OcrConfig, OutputConfig, RawConfig, RenderConfig, RuntimeConfig,
    ServerConfig, TsrConfig,
};
use typed_builder::TypedBuilder;

const ASSIGNMENT_WEIGHT_TOLERANCE: f64 = 1.0e-6;
const MINIMUM_MODEL_INPUT_EDGE: u32 = 800;

/// Validated parser settings; native deployment configuration is checked separately by its consumers.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct ValidatedConfig {
    // Platform-only state has no serialized configuration key or public builder override.
    #[builder(default, setter(skip))]
    pub(crate) platform: crate::wasm_compat::PlatformOptions,
    layout: LayoutConfig,
    tsr: TsrConfig,
    formula: FormulaConfig,
    runtime: RuntimeConfig,
    render: RenderConfig,
    fusion: FusionConfig,
    ocr: OcrConfig,
    output: OutputConfig,
}

impl ValidatedConfig {
    /// Returns formula artifact paths and the validated batch/deadline limits.
    pub fn formula(&self) -> &FormulaConfig {
        &self.formula
    }
    /// Returns validated layout engine configuration.
    pub fn layout(&self) -> &LayoutConfig {
        &self.layout
    }

    /// Returns validated table model paths and recovery policy.
    pub fn tsr(&self) -> &TsrConfig {
        &self.tsr
    }

    /// Returns validated runtime limits.
    pub fn runtime(&self) -> &RuntimeConfig {
        &self.runtime
    }

    /// Returns validated page rendering settings.
    pub fn render(&self) -> &RenderConfig {
        &self.render
    }

    /// Returns validated page fusion settings.
    pub fn fusion(&self) -> &FusionConfig {
        &self.fusion
    }

    /// Returns validated OCR policy.
    pub fn ocr(&self) -> &OcrConfig {
        &self.ocr
    }

    /// Returns validated output rendering options.
    pub fn output(&self) -> &OutputConfig {
        &self.output
    }

    /// Validates a finite inclusive unit-interval value.
    fn validate_unit_interval(
        value: f64,
        field: &'static str,
    ) -> Result<(), ConfigError> {
        if value.is_finite() && (0.0..=1.0).contains(&value) {
            Ok(())
        } else {
            Err(ConfigError::InvalidValue {
                field,
                reason: "must be finite and within [0, 1]",
            })
        }
    }

    /// Validates a finite value that cannot be negative.
    fn validate_non_negative(
        value: f64,
        field: &'static str,
    ) -> Result<(), ConfigError> {
        if value.is_finite() && value >= 0.0 {
            Ok(())
        } else {
            Err(ConfigError::InvalidValue {
                field,
                reason: "must be finite and non-negative",
            })
        }
    }

    /// Validates a finite value that must be strictly positive.
    fn validate_positive(
        value: f64,
        field: &'static str,
    ) -> Result<(), ConfigError> {
        if value.is_finite() && value > 0.0 {
            Ok(())
        } else {
            Err(ConfigError::InvalidValue {
                field,
                reason: "must be finite and greater than zero",
            })
        }
    }
}

impl TryFrom<RawConfig> for ValidatedConfig {
    type Error = ConfigError;

    /// Validates all lexical and numeric invariants without reading model artifacts.
    fn try_from(config: RawConfig) -> Result<Self, Self::Error> {
        Self::validate_platform(&config)?;
        // Queue capacity is explicit and independent of consumers or batches, including capacities below one batch.
        for (size, field) in
            [
                (config.layout.queue_size, "layout.queue_size"),
                (config.tsr.queue_size, "tsr.queue_size"),
                (config.formula.queue_size, "formula.queue_size"),
                (config.ocr.detection.queue_size, "ocr.detection.queue_size"),
                (
                    config.ocr.recognition.queue_size,
                    "ocr.recognition.queue_size",
                ),
                (
                    config.ocr.orientation.queue_size,
                    "ocr.orientation.queue_size",
                ),
            ]
            .into_iter()
            .chain(config.tsr.cell_detection.as_ref().map(
                |cells| (cells.queue_size, "tsr.cell_detection.queue_size"),
            ))
        {
            // Leave room for up to 1024 active HTTP requests within the 32-bit Tokio semaphore limit.
            if size == 0 || size > (u32::MAX >> 3) as usize - 1024 {
                return Err(ConfigError::InvalidValue {
                    field,
                    reason: "queue_size must be between 1 and 536869887",
                });
            }
        }
        // Hardware capacity determines session counts; require a consumer without imposing a deployment ceiling.
        for (value, field) in [
            (config.layout.session_size, "layout.session_size"),
            (config.tsr.session_size, "tsr.session_size"),
            (
                config.ocr.detection.session_size,
                "ocr.detection.session_size",
            ),
            (
                config.ocr.recognition.session_size,
                "ocr.recognition.session_size",
            ),
            (
                config.ocr.orientation.session_size,
                "ocr.orientation.session_size",
            ),
        ] {
            if value == 0 {
                return Err(ConfigError::InvalidValue {
                    field,
                    reason: "must be greater than zero",
                });
            }
        }
        for (value, field) in [
            (config.layout.batch_size, "layout.batch_size"),
            (config.ocr.detection.batch_size, "ocr.detection.batch_size"),
            (
                config.ocr.recognition.batch_size,
                "ocr.recognition.batch_size",
            ),
            (
                config.ocr.orientation.batch_size,
                "ocr.orientation.batch_size",
            ),
        ] {
            if !(1..=32).contains(&value) {
                return Err(ConfigError::InvalidValue {
                    field,
                    reason: "must be between 1 and 32",
                });
            }
        }
        // Bound OCR tensors, candidate work and deadlines before any model or image is loaded.
        for (value, field) in [
            (config.ocr.detection_threshold, "ocr.detection_threshold"),
            (config.ocr.box_threshold, "ocr.box_threshold"),
            (
                config.ocr.recognition_threshold,
                "ocr.recognition_threshold",
            ),
            (
                config.ocr.orientation_threshold,
                "ocr.orientation_threshold",
            ),
        ] {
            Self::validate_unit_interval(value, field)?;
        }
        if !(32..=4096).contains(&config.ocr.detection_max_side)
            || !(320..=4096).contains(&config.ocr.recognition_max_width)
            || !(1..=10_000).contains(&config.ocr.max_candidates)
            || !(1..=86_400_000).contains(&config.ocr.timeout_ms)
            || !config.ocr.unclip_ratio.is_finite()
            || !(0.1..=5.0).contains(&config.ocr.unclip_ratio)
        {
            return Err(ConfigError::InvalidValue {
                field: "ocr",
                reason: "invalid OCR dimensions, candidate limit, expansion or timeout",
            });
        }
        Self::validate_unit_interval(
            config.layout.score_threshold,
            "layout.score_threshold",
        )?;
        if let Some(cells) = &config.tsr.cell_detection {
            if cells.session_size == 0 {
                return Err(ConfigError::InvalidValue {
                    field: "tsr.cell_detection.session_size",
                    reason: "must be greater than zero",
                });
            }
            // Bound detector batches independently from structure batches and table admission.
            if !(1..=32).contains(&cells.batch_size) {
                return Err(ConfigError::InvalidValue {
                    field: "tsr.cell_detection.batch_size",
                    reason: "must be between 1 and 32",
                });
            }
            Self::validate_unit_interval(
                cells.score_threshold,
                "tsr.cell_detection.score_threshold",
            )?;
        }
        // Only SLANeXt lacks usable structure geometry; TATR can also run without a detector.
        if config.tsr.mode != crate::TableMode::RulesOnly
            && matches!(
                config.tsr.model,
                crate::TsrModel::SlanextWired
                    | crate::TsrModel::SlanextWireless
            )
            && !config
                .tsr
                .cell_detection
                .as_ref()
                .is_some_and(|cells| cells.enabled)
        {
            return Err(ConfigError::InvalidValue {
                field: "tsr.cell_detection",
                reason: "SLANeXt requires independent cell detection because its position output is invalid",
            });
        }
        // Batch size bounds one tensor invocation; bounded model queues own request backpressure.
        if !(1..=32).contains(&config.tsr.batch_size) {
            return Err(ConfigError::InvalidValue {
                field: "tsr.batch_size",
                reason: "must be between 1 and 32",
            });
        }
        if !(1..=86_400_000).contains(&config.tsr.timeout_ms) {
            return Err(ConfigError::InvalidValue {
                field: "tsr.timeout_ms",
                reason: "must be between one and 86400000 milliseconds",
            });
        }
        config.render.validate()?;

        Self::validate_unit_interval(
            config.fusion.minimum_line_coverage,
            "fusion.minimum_line_coverage",
        )?;
        Self::validate_unit_interval(
            config.fusion.center_minimum_line_coverage,
            "fusion.center_minimum_line_coverage",
        )?;

        let assignment_weights = [
            (
                "fusion.assignment_coverage_weight",
                config.fusion.assignment_coverage_weight,
            ),
            (
                "fusion.assignment_center_weight",
                config.fusion.assignment_center_weight,
            ),
            (
                "fusion.assignment_baseline_weight",
                config.fusion.assignment_baseline_weight,
            ),
            (
                "fusion.assignment_confidence_weight",
                config.fusion.assignment_confidence_weight,
            ),
            (
                "fusion.assignment_specificity_weight",
                config.fusion.assignment_specificity_weight,
            ),
        ];
        for (field, weight) in assignment_weights {
            Self::validate_non_negative(weight, field)?;
        }
        let assignment_weight_sum = assignment_weights
            .into_iter()
            .map(|(_, weight)| weight)
            .sum::<f64>();
        if (assignment_weight_sum - 1.0).abs() > ASSIGNMENT_WEIGHT_TOLERANCE {
            return Err(ConfigError::InvalidValue {
                field: "fusion.assignment_weights",
                reason: "must sum to one within an absolute tolerance of 1e-6",
            });
        }

        Self::validate_positive(
            config.fusion.paragraph_gap_multiplier,
            "fusion.paragraph_gap_multiplier",
        )?;
        Self::validate_non_negative(
            config.fusion.indent_tolerance_points,
            "fusion.indent_tolerance_points",
        )?;
        Self::validate_non_negative(
            config.fusion.font_size_tolerance_points,
            "fusion.font_size_tolerance_points",
        )?;
        Self::validate_non_negative(
            config.fusion.estimated_font_size_tolerance_points,
            "fusion.estimated_font_size_tolerance_points",
        )?;

        if let crate::FormulaEngineConfig::Mineru(mineru) =
            &config.formula.engine
            && (config.formula.inline_enabled || config.formula.display_enabled)
        {
            mineru.endpoint()?;
        }
        let (cpu, gpu) = config.formula.engine.session_counts();
        // Check arithmetic/permit representability without imposing a hardware-dependent session ceiling.
        let capacity = cpu
            .checked_add(gpu)
            .filter(|total| *total > 0)
            .and_then(|total| total.checked_mul(config.formula.batch_size))
            .and_then(|active| active.checked_add(config.formula.queue_size));
        if capacity.is_none_or(|capacity| capacity > (usize::MAX >> 3)) {
            return Err(ConfigError::InvalidValue {
                field: "formula.engine",
                reason: "CPU/GPU session counts must have a positive total and fit the admission capacity",
            });
        }
        if !(1..=32).contains(&config.formula.batch_size) {
            return Err(ConfigError::InvalidValue {
                field: "formula.batch_size",
                reason: "must be between 1 and 32",
            });
        }
        if !(1..=86_400_000).contains(&config.formula.timeout_ms) {
            return Err(ConfigError::InvalidValue {
                field: "formula.timeout_ms",
                reason: "must be between 1 and 86400000",
            });
        }
        let RawConfig {
            layout,
            tsr,
            formula,
            runtime,
            render,
            fusion,
            ocr,
            output,
            // Keep native connection settings and credentials out of parser instances and browser validation.
            ..
        } = config;
        Ok(Self::builder()
            .layout(layout)
            .tsr(tsr)
            .formula(formula)
            .runtime(runtime)
            .render(render)
            .fusion(fusion)
            .ocr(ocr)
            .output(output)
            .build())
    }
}

impl ServerConfig {
    /// Validates process admission, the listener, and literal routes before allocating resources.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !(1..=1024).contains(&self.max_uploads) {
            return Err(ConfigError::InvalidValue {
                field: "server.max_uploads",
                reason: "must be between 1 and 1024",
            });
        }
        if self.host.is_empty() || self.host.chars().any(char::is_whitespace) {
            return Err(ConfigError::InvalidValue {
                field: "server.host",
                reason: "must be a non-empty host name or IP address without whitespace",
            });
        }
        // A prefix is a fixed path, never an Axum capture pattern or a URL containing query/fragment data.
        if !matches!(self.api_prefix.as_str(), "" | "/")
            && (!self.api_prefix.starts_with('/')
                || self.api_prefix.split('/').skip(1).any(|segment| {
                    segment.is_empty()
                        || matches!(segment, "." | "..")
                        || !segment.bytes().all(|byte| {
                            byte.is_ascii_alphanumeric()
                                || matches!(byte, b'-' | b'_' | b'.' | b'~')
                        })
                }))
        {
            return Err(ConfigError::InvalidValue {
                field: "server.api_prefix",
                reason: "must be empty, /, or an absolute path of literal ASCII segments without a trailing slash",
            });
        }
        Ok(())
    }
}

impl DatabaseConfig {
    /// Validates a PostgreSQL target, pool budgets, and slow query threshold before allocating connections.
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !["postgresql://", "postgres://"].into_iter().any(|scheme| {
            self.url
                .strip_prefix(scheme)
                .is_some_and(|target| !target.is_empty())
        }) {
            return Err(ConfigError::InvalidValue {
                field: "database.url",
                reason: "must be a non-empty PostgreSQL connection URL",
            });
        }
        if self.max_connections == 0
            || self.min_connections > self.max_connections
        {
            return Err(ConfigError::InvalidValue {
                field: "database.max_connections",
                reason: "must be positive and at least min_connections",
            });
        }
        for (field, value) in [
            ("database.timeout_ms", self.timeout_ms),
            ("database.acquire_timeout_ms", self.acquire_timeout_ms),
            ("database.idle_timeout_ms", self.idle_timeout_ms),
            (
                "database.sqlx_slow_statements_threshold_ms",
                self.sqlx_slow_statements_threshold_ms,
            ),
        ] {
            if !(1..=86_400_000).contains(&value) {
                return Err(ConfigError::InvalidValue {
                    field,
                    reason: "must be between one and 86400000 milliseconds",
                });
            }
        }
        Ok(())
    }
}

impl RenderConfig {
    /// Validates render capacity before service startup as well as standalone parser construction.
    pub fn validate(&self) -> Result<(), ConfigError> {
        // Render queues count unfinished pages independently of the number of PDFium processes.
        for (value, field) in [
            (self.workers, "render.workers"),
            (self.queue_size, "render.queue_size"),
        ] {
            if value == 0 || value > (u32::MAX >> 3) as usize - 1024 {
                return Err(ConfigError::InvalidValue {
                    field,
                    reason: "must be between 1 and 536869887",
                });
            }
        }

        if self.dpi == 0 {
            return Err(ConfigError::InvalidValue {
                field: "render.dpi",
                reason: "must be greater than zero",
            });
        }
        if self.max_long_edge_pixels < MINIMUM_MODEL_INPUT_EDGE {
            return Err(ConfigError::InvalidValue {
                field: "render.max_long_edge_pixels",
                reason: "must be at least 800 pixels",
            });
        }

        Ok(())
    }
}
