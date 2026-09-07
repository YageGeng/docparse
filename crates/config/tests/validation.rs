use std::fs;

use docparse_config::{ConfigError, ConfigLoader, RawConfig, ValidatedConfig};

/// Loads code defaults through the real loader so model paths become absolute.
fn loaded_defaults() -> RawConfig {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let config_path = directory.path().join("docparse.toml");
    fs::write(&config_path, "")
        .expect("the test configuration must be writable");
    ConfigLoader::new(config_path)
        .load_raw()
        .expect("the default configuration must load")
}

/// Asserts that one invalid value reports the expected stable field name.
fn assert_invalid_value(config: RawConfig, expected_field: &'static str) {
    let error = ValidatedConfig::try_from(config)
        .expect_err("the invalid configuration must be rejected");
    assert!(matches!(
        error,
        ConfigError::InvalidValue { field, .. } if field == expected_field
    ));
}

/// Verifies all finite ranges and non-zero runtime limits.
#[test]
fn invalid_ranges_are_rejected() {
    let defaults = loaded_defaults();

    let mut config = defaults.clone();
    config.layout.score_threshold = f64::NAN;
    assert_invalid_value(config, "layout.score_threshold");

    let mut config = defaults.clone();
    config.layout.score_threshold = f64::INFINITY;
    assert_invalid_value(config, "layout.score_threshold");

    let mut config = defaults.clone();
    config.layout.score_threshold = 1.1;
    assert_invalid_value(config, "layout.score_threshold");

    let mut config = defaults.clone();
    config.layout.session_pool_size = 0;
    assert_invalid_value(config, "layout.session_pool_size");

    let mut config = defaults.clone();
    config.runtime.page_concurrency = 0;
    assert_invalid_value(config, "runtime.page_concurrency");

    let mut config = defaults.clone();
    config.runtime.render_queue_capacity = 0;
    assert_invalid_value(config, "runtime.render_queue_capacity");

    let mut config = defaults.clone();
    config.runtime.blocking_task_limit = 0;
    assert_invalid_value(config, "runtime.blocking_task_limit");

    let mut config = defaults.clone();
    config.runtime.render_queue_capacity = config.runtime.page_concurrency + 1;
    assert_invalid_value(config, "runtime.render_queue_capacity");

    let mut config = defaults.clone();
    config.render.dpi = 0;
    assert_invalid_value(config, "render.dpi");

    let mut config = defaults.clone();
    config.render.max_long_edge_pixels = 799;
    assert_invalid_value(config, "render.max_long_edge_pixels");

    let mut config = defaults.clone();
    config.fusion.minimum_line_coverage = -0.1;
    assert_invalid_value(config, "fusion.minimum_line_coverage");

    let mut config = defaults.clone();
    config.fusion.center_minimum_line_coverage = 1.1;
    assert_invalid_value(config, "fusion.center_minimum_line_coverage");

    let mut config = defaults.clone();
    config.fusion.paragraph_gap_multiplier = f64::NAN;
    assert_invalid_value(config, "fusion.paragraph_gap_multiplier");

    let mut config = defaults;
    config.fusion.indent_tolerance_points = -0.1;
    assert_invalid_value(config, "fusion.indent_tolerance_points");
}

/// Verifies assignment weights are finite, non-negative, and sum to one.
#[test]
fn assignment_weight_contract_is_enforced() {
    let defaults = loaded_defaults();

    let mut config = defaults.clone();
    config.fusion.assignment_coverage_weight = -0.1;
    assert_invalid_value(config, "fusion.assignment_coverage_weight");

    let mut config = defaults.clone();
    config.fusion.assignment_center_weight = f64::INFINITY;
    assert_invalid_value(config, "fusion.assignment_center_weight");

    let mut accepted = defaults.clone();
    accepted.fusion.assignment_specificity_weight += 0.000_000_5;
    ValidatedConfig::try_from(accepted)
        .expect("a weight sum within the tolerance must be accepted");

    let mut rejected = defaults;
    rejected.fusion.assignment_specificity_weight += 0.000_002;
    assert_invalid_value(rejected, "fusion.assignment_weights");
}

/// Verifies validation does not require model artifacts to exist.
#[test]
fn nonexistent_absolute_model_paths_are_accepted() {
    let raw = loaded_defaults();
    assert!(!raw.layout.model_path.exists());
    assert!(!raw.layout.model_config_path.exists());
    assert!(!raw.layout.model_manifest_path.exists());

    let validated = ValidatedConfig::try_from(raw)
        .expect("configuration validation must not access model artifacts");

    assert!(validated.layout().model_path.is_absolute());
    assert!(validated.layout().model_config_path.is_absolute());
    assert!(validated.layout().model_manifest_path.is_absolute());
}

/// Verifies byte-backed or injected engines do not require filesystem locations.
#[test]
fn parameter_validation_does_not_require_model_paths() {
    let raw = RawConfig::default();
    ValidatedConfig::try_from(raw)
        .expect("model paths belong to native artifact loading, not parameter validation");
}
