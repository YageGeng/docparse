use crate::{
    ConfigError, FusionConfig, LayoutConfig, OcrConfig, OutputConfig,
    RawConfig, RenderConfig, RuntimeConfig,
};
use typed_builder::TypedBuilder;

const ASSIGNMENT_WEIGHT_TOLERANCE: f64 = 1.0e-6;
const MINIMUM_MODEL_INPUT_EDGE: u32 = 800;

/// Configuration whose paths and numeric invariants have been validated once.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct ValidatedConfig {
    layout: LayoutConfig,
    runtime: RuntimeConfig,
    render: RenderConfig,
    fusion: FusionConfig,
    ocr: OcrConfig,
    output: OutputConfig,
}

impl ValidatedConfig {
    /// Returns validated layout engine configuration.
    pub fn layout(&self) -> &LayoutConfig {
        &self.layout
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
        for (field, path) in [
            ("layout.model_path", &config.layout.model_path),
            ("layout.model_config_path", &config.layout.model_config_path),
            (
                "layout.model_manifest_path",
                &config.layout.model_manifest_path,
            ),
        ] {
            if !path.is_absolute() {
                return Err(ConfigError::InvalidValue {
                    field,
                    reason: "must be an absolute path resolved by ConfigLoader",
                });
            }
        }

        Self::validate_unit_interval(
            config.layout.score_threshold,
            "layout.score_threshold",
        )?;
        if config.layout.session_pool_size == 0 {
            return Err(ConfigError::InvalidValue {
                field: "layout.session_pool_size",
                reason: "must be greater than zero",
            });
        }

        if config.runtime.page_concurrency == 0 {
            return Err(ConfigError::InvalidValue {
                field: "runtime.page_concurrency",
                reason: "must be greater than zero",
            });
        }
        if config.runtime.render_queue_capacity == 0
            || config.runtime.render_queue_capacity
                > config.runtime.page_concurrency
        {
            return Err(ConfigError::InvalidValue {
                field: "runtime.render_queue_capacity",
                reason: "must be between one and page_concurrency",
            });
        }
        if config.runtime.blocking_task_limit == 0 {
            return Err(ConfigError::InvalidValue {
                field: "runtime.blocking_task_limit",
                reason: "must be greater than zero",
            });
        }

        if config.render.dpi == 0 {
            return Err(ConfigError::InvalidValue {
                field: "render.dpi",
                reason: "must be greater than zero",
            });
        }
        if config.render.max_long_edge_pixels < MINIMUM_MODEL_INPUT_EDGE {
            return Err(ConfigError::InvalidValue {
                field: "render.max_long_edge_pixels",
                reason: "must be at least 800 pixels",
            });
        }

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

        let RawConfig {
            layout,
            runtime,
            render,
            fusion,
            ocr,
            output,
        } = config;
        Ok(Self::builder()
            .layout(layout)
            .runtime(runtime)
            .render(render)
            .fusion(fusion)
            .ocr(ocr)
            .output(output)
            .build())
    }
}
