use std::fs;

use docparse_config::{
    ConfigError, ConfigLoader, DatabaseConfig, RawConfig, ServerConfig,
    ValidatedConfig,
};

/// Texo defaults to one consumer and rejects empty pools without imposing a hardware ceiling.
#[test]
fn texo_sessions_are_positive_and_default_to_one() {
    let mut value = serde_json::to_value(RawConfig::default()).expect("config");
    assert_eq!(
        value.pointer("/formula/engine/0/worker_size"),
        Some(&1.into())
    );
    for count in [0, 9] {
        *value
            .pointer_mut("/formula/engine/0/worker_size")
            .expect("sessions") = count.into();
        let raw = serde_json::from_value(value.clone()).expect("shape");
        // Preserve zero rejection while allowing counts beyond the former eight-session limit.
        if count == 0 {
            assert_invalid_value(raw, "formula.engine.worker_size");
        } else {
            ValidatedConfig::try_from(raw).expect("positive consumer count");
        }
    }
}

/// Loads code defaults through the real loader so model paths become absolute.
fn loaded_defaults() -> RawConfig {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let config_path = directory.path().join("docparse.toml");
    fs::write(&config_path, "render.workers = 1\nrender.queue_size = 16\nlayout.queue_size = 1\ntsr.queue_size = 1\ntsr.cell_detection.queue_size = 1\nocr.detection.queue_size = 1\nocr.recognition.queue_size = 16\nocr.orientation.queue_size = 16\nformula.queue_size = 4\n")
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
    config.layout.session_size = 0;
    assert_invalid_value(config, "layout.session_size");

    let mut config = defaults.clone();
    config.render.workers = 0;
    assert_invalid_value(config, "render.workers");
    let mut config = defaults.clone();
    config.render.queue_size = 0;
    assert_invalid_value(config, "render.queue_size");

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

/// The retired page gate must not silently override per-model consumers and queue backpressure.
#[test]
fn ocr_in_flight_limit_is_rejected() {
    let mut value = serde_json::to_value(RawConfig::default()).expect("config");
    value
        .get_mut("ocr")
        .expect("OCR")
        .as_object_mut()
        .expect("object")
        .insert("max_in_flight".into(), serde_json::json!(2));
    serde_json::from_value::<RawConfig>(value).expect_err("retired page gate");
}

/// Upload concurrency retains its existing admission range without a document job gate.
#[test]
fn server_concurrency_limits_are_loaded_and_validated() {
    for (field, cases) in [(
        "max_uploads",
        [
            (0, false),
            (1, true),
            (4, true),
            (1024, true),
            (1025, false),
        ],
    )] {
        for (limit, valid) in cases {
            let config: ServerConfig =
                serde_json::from_value(serde_json::json!({
                    (field): limit
                }))
                .expect("concurrency must deserialize");
            let result = config.validate();
            if valid {
                result.expect("valid concurrency");
            } else {
                assert!(
                    matches!(result, Err(ConfigError::InvalidValue { field: actual, .. })
                    if actual == format!("server.{field}"))
                );
            }
        }
    }
}

/// A configurable prefix cannot introduce captures, malformed URLs, or ambiguous route separators.
#[test]
fn server_api_prefix_is_a_literal_absolute_path() {
    assert_eq!(ServerConfig::default().api_prefix, "/api");
    for prefix in ["", "/", "/api", "/gateway/api-v2", "/v1.0/~jobs"] {
        ServerConfig::builder()
            .api_prefix(prefix)
            .build()
            .validate()
            .expect("literal prefix");
    }
    for prefix in [
        "api",
        "/api/",
        "//",
        "/api//v1",
        "/{id}",
        "/api/*path",
        "/:id",
        "/../api",
        "/api?x=1",
        "/api#v1",
        "/api%2Fv1",
        "/api v1",
        "/api\r\n",
    ] {
        assert!(
            matches!(
                ServerConfig::builder()
                    .api_prefix(prefix)
                    .build()
                    .validate(),
                Err(ConfigError::InvalidValue {
                    field: "server.api_prefix",
                    ..
                })
            ),
            "accepted invalid prefix {prefix:?}"
        );
    }
}

/// Service consumers reject invalid pool budgets while parser-only callers remain independent of database settings.
#[test]
fn native_deployment_settings_are_validated_separately() {
    for host in ["", " ", "127.0.0.1 "] {
        assert!(matches!(
            ServerConfig::builder().host(host).build().validate(),
            Err(ConfigError::InvalidValue {
                field: "server.host",
                ..
            })
        ));
    }
    ServerConfig::builder()
        .host("localhost")
        .port(0)
        .build()
        .validate()
        .expect("DNS and ephemeral port");
    ServerConfig::builder()
        .host("::1")
        .build()
        .validate()
        .expect("IPv6");
    let defaults = DatabaseConfig::builder()
        .url("postgresql://user:private-password@localhost/docparse")
        .build();
    defaults.validate().expect("database defaults");
    assert!(!format!("{defaults:?}").contains("private-password"));
    for (field, value, expected) in [
        ("url", serde_json::json!(""), "database.url"),
        (
            "url",
            serde_json::json!("mysql://localhost/docparse"),
            "database.url",
        ),
        (
            "max_connections",
            serde_json::json!(0),
            "database.max_connections",
        ),
        (
            "min_connections",
            serde_json::json!(11),
            "database.max_connections",
        ),
        ("timeout_ms", serde_json::json!(0), "database.timeout_ms"),
        (
            "acquire_timeout_ms",
            serde_json::json!(0),
            "database.acquire_timeout_ms",
        ),
        (
            "idle_timeout_ms",
            serde_json::json!(86_400_001),
            "database.idle_timeout_ms",
        ),
        (
            "sqlx_slow_statements_threshold_ms",
            serde_json::json!(0),
            "database.sqlx_slow_statements_threshold_ms",
        ),
        (
            "sqlx_slow_statements_threshold_ms",
            serde_json::json!(86_400_001),
            "database.sqlx_slow_statements_threshold_ms",
        ),
    ] {
        let mut json = serde_json::to_value(&defaults).expect("defaults");
        *json.get_mut(field).expect("configuration field") = value;
        let config: DatabaseConfig =
            serde_json::from_value(json).expect("configuration");
        assert!(
            matches!(config.validate(), Err(ConfigError::InvalidValue { field, .. }) if field == expected)
        );
    }
    let mut parser = RawConfig::default();
    parser.database.max_connections = 0;
    ValidatedConfig::try_from(parser).expect("parser does not open a database");
}
/// Formula kinds default on independently, retain bounded batches, and reject the removed master switch.
#[test]
fn formula_configuration_accepts_bounded_batches() {
    let mut value = serde_json::to_value(docparse_config::RawConfig::default())
        .expect("default config");
    value.as_object_mut().expect("object").insert(
        "formula".into(),
        serde_json::json!({"queue_size": 4, "engine":[{"type":"texo","batch_size":4}], "timeout_ms": 120000}),
    );
    let raw =
        serde_json::from_value::<docparse_config::RawConfig>(value.clone());
    let valid = docparse_config::ValidatedConfig::try_from(
        raw.expect("formula config"),
    )
    .expect("valid formula config");
    assert_eq!(
        serde_json::to_value(valid.formula())
            .expect("formula JSON")
            .get("inline_enabled"),
        Some(&serde_json::Value::Bool(true))
    );
    value
        .get_mut("formula")
        .expect("formula")
        .as_object_mut()
        .expect("object")
        .insert("inline_enabled".into(), false.into());
    let disabled: docparse_config::RawConfig =
        serde_json::from_value(value.clone()).expect("inline option");
    assert_eq!(
        serde_json::to_value(disabled.formula)
            .expect("formula JSON")
            .get("inline_enabled"),
        Some(&serde_json::Value::Bool(false))
    );
    assert_eq!(
        serde_json::to_value(valid.formula())
            .expect("formula JSON")
            .get("display_enabled"),
        Some(&serde_json::Value::Bool(true))
    );
    let mut legacy = value.clone();
    legacy
        .get_mut("formula")
        .expect("formula")
        .as_object_mut()
        .expect("object")
        .insert("enabled".into(), true.into());
    assert!(
        serde_json::from_value::<docparse_config::RawConfig>(legacy).is_err(),
        "the master switch must not remain configurable"
    );
    for invalid in [0, 33] {
        *value
            .pointer_mut("/formula/engine/0/batch_size")
            .expect("batch_size") = invalid.into();
        let raw =
            serde_json::from_value::<docparse_config::RawConfig>(value.clone())
                .expect("shape");
        docparse_config::ValidatedConfig::try_from(raw)
            .expect_err("invalid formula batch must fail");
    }
}

/// Figure delivery is inline unless a directory is named for file output.
#[test]
fn figure_file_delivery_requires_a_directory() {
    let mut inline = RawConfig::default();
    assert_eq!(
        inline.figures.delivery,
        docparse_config::FigureDelivery::Inline
    );
    ValidatedConfig::try_from(inline.clone()).expect("inline figures");
    inline.figures.delivery = docparse_config::FigureDelivery::File;
    assert!(matches!(
        ValidatedConfig::try_from(inline.clone()),
        Err(ConfigError::InvalidValue {
            field: "figures.directory",
            ..
        })
    ));
    inline.figures.directory = Some(std::path::PathBuf::from("figures"));
    ValidatedConfig::try_from(inline).expect("file figures");
}
