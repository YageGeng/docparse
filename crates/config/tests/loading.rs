use std::fs;
use std::path::{Path, PathBuf};

use docparse_config::{ConfigError, ConfigLoader};
use figment::Figment;
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde_json::json;

/// Writes one configuration file and returns its path.
fn write_config(directory: &Path, name: &str, contents: &str) -> PathBuf {
    let path = directory.join(name);
    fs::write(&path, contents)
        .expect("the test configuration must be writable");
    path
}

/// Creates an isolated Figment provider that behaves like parsed environment data.
fn environment_provider(value: serde_json::Value) -> Figment {
    Figment::from(Serialized::defaults(value))
}

/// Creates an explicit override for the layout score threshold.
fn score_override(score_threshold: f64) -> Dict {
    let mut layout = Dict::new();
    layout.insert("score_threshold".into(), Value::from(score_threshold));

    let mut root = Dict::new();
    root.insert("layout".into(), Value::from(layout));
    root
}

/// Verifies that profile values override the main file and paths use the main directory.
#[test]
fn profile_overrides_main_file_and_resolves_model_paths() {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let main_path = write_config(
        directory.path(),
        "docparse.toml",
        r#"
[layout]
model_path = "artifacts/model.onnx"
model_config_path = "artifacts/model.yml"
model_manifest_path = "artifacts/manifest.json"
score_threshold = 0.4

[runtime]
page_concurrency = 2
"#,
    );
    write_config(
        directory.path(),
        "docparse.dev.toml",
        r#"
[layout]
score_threshold = 0.6

[runtime]
page_concurrency = 3
"#,
    );

    let config = ConfigLoader::new(&main_path)
        .with_profile("dev")
        .load_raw()
        .expect("the layered configuration must load");

    assert!((config.layout.score_threshold - 0.6).abs() < f64::EPSILON);
    assert_eq!(config.runtime.page_concurrency, 3);
    assert_eq!(
        config.layout.model_path,
        directory
            .path()
            .canonicalize()
            .expect("canonical temporary directory")
            .join("artifacts/model.onnx")
    );
    assert_eq!(
        config.layout.model_config_path,
        directory
            .path()
            .canonicalize()
            .expect("canonical temporary directory")
            .join("artifacts/model.yml")
    );
    assert_eq!(
        config.layout.model_manifest_path,
        directory
            .path()
            .canonicalize()
            .expect("canonical temporary directory")
            .join("artifacts/manifest.json")
    );
}

/// Verifies that an unselected profile file is never loaded.
#[test]
fn absent_profile_does_not_load_profile_file() {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let main_path = write_config(
        directory.path(),
        "docparse.toml",
        "[layout]\nscore_threshold = 0.4\n",
    );
    write_config(
        directory.path(),
        "docparse.dev.toml",
        "unknown_profile_key = true\n",
    );

    let config = ConfigLoader::new(main_path)
        .load_raw()
        .expect("the unprofiled configuration must load");

    assert!((config.layout.score_threshold - 0.4).abs() < f64::EPSILON);
}

/// Verifies explicit profile selection and the complete value precedence chain.
#[test]
fn explicit_profile_and_overrides_have_expected_precedence() {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let main_path = write_config(
        directory.path(),
        "docparse.toml",
        "[runtime]\npage_concurrency = 2\n",
    );
    write_config(
        directory.path(),
        "docparse.dev.toml",
        "[runtime]\npage_concurrency = 6\n",
    );
    write_config(
        directory.path(),
        "docparse.prod.toml",
        "[runtime]\npage_concurrency = 7\n",
    );
    let environment = environment_provider(json!({
        "profile": "dev",
        "layout": { "score_threshold": 0.8 }
    }));

    let config = ConfigLoader::new(main_path)
        .with_profile("prod")
        .with_env_provider(environment)
        .with_overrides(score_override(0.9))
        .load_raw()
        .expect("all configuration layers must merge");

    assert_eq!(config.runtime.page_concurrency, 7);
    assert!((config.layout.score_threshold - 0.9).abs() < f64::EPSILON);
}

/// Verifies that the injected environment profile selects the profile file only.
#[test]
fn environment_profile_selects_file_without_entering_raw_config() {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let main_path = write_config(directory.path(), "docparse.toml", "");
    write_config(
        directory.path(),
        "docparse.dev.toml",
        "[runtime]\npage_concurrency = 6\n",
    );
    let environment = environment_provider(json!({ "profile": "dev" }));

    let config = ConfigLoader::new(main_path)
        .with_env_provider(environment)
        .load_raw()
        .expect("the environment-selected profile must load");

    assert_eq!(config.runtime.page_concurrency, 6);
}

/// Verifies that a missing main configuration file has a dedicated error.
#[test]
fn missing_main_file_is_rejected() {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let missing = directory.path().join("docparse.toml");

    let error = ConfigLoader::new(&missing)
        .load_raw()
        .expect_err("a missing main file must fail");

    assert!(matches!(
        error,
        ConfigError::ConfigFileNotFound { path } if path == missing
    ));
}

/// Verifies that a selected but missing profile file has a dedicated error.
#[test]
fn missing_profile_file_is_rejected() {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let main_path = write_config(directory.path(), "docparse.toml", "");
    let expected_path = directory
        .path()
        .canonicalize()
        .expect("canonical temporary directory")
        .join("docparse.dev.toml");

    let error = ConfigLoader::new(main_path)
        .with_profile("dev")
        .load_raw()
        .expect_err("a missing selected profile must fail");

    assert!(matches!(
        error,
        ConfigError::ProfileFileNotFound { profile, path }
            if profile == "dev" && path == expected_path
    ));
}

/// Verifies that profile names cannot escape the configuration directory.
#[test]
fn invalid_profile_names_are_rejected() {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let main_path = write_config(directory.path(), "docparse.toml", "");

    for invalid in ["", "../dev", "nested/dev", r"dev\prod", "dev.profile"] {
        let error = ConfigLoader::new(&main_path)
            .with_profile(invalid)
            .load_raw()
            .expect_err("an invalid profile name must fail");
        assert!(matches!(
            error,
            ConfigError::InvalidProfileName { name } if name == invalid
        ));
    }
}

/// Verifies that unknown keys report their complete configuration path.
#[test]
fn unknown_field_error_contains_full_key_path() {
    let directory =
        tempfile::tempdir().expect("the test directory must be created");
    let main_path = write_config(
        directory.path(),
        "docparse.toml",
        "[layout]\nscore_thresold = 0.9\n",
    );

    let error = ConfigLoader::new(main_path)
        .load_raw()
        .expect_err("an unknown nested key must fail");

    assert!(error.to_string().contains("layout.score_thresold"));
}

/// Verifies the repository default config remains loadable by the strict schema.
#[test]
fn repository_default_config_matches_documented_defaults() {
    let config_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docparse.toml");

    let config = ConfigLoader::new(config_path)
        .load_raw()
        .expect("the repository default config must remain valid");

    assert!((config.layout.score_threshold - 0.5).abs() < f64::EPSILON);
    assert_eq!(config.runtime.page_concurrency, 4);
    assert_eq!(config.render.dpi, 144);
    assert_eq!(config.output.formula_placeholder, "[formula]");
}
