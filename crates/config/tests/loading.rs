use std::fs;
use std::path::{Path, PathBuf};

use docparse_config::{ConfigError, ConfigLoader, RawConfig};
use figment::Figment;
use figment::providers::Serialized;
use figment::value::{Dict, Value};
use serde_json::json;

/// Structure and cell batch sizes load independently and reject unbounded tensor batches.
#[test]
fn table_batch_sizes_are_independent_and_bounded() {
    let directory = tempfile::tempdir().expect("configuration directory");
    let path = write_config(
        directory.path(),
        "docparse.toml",
        "[tsr]\nbatch_size = 4\n[tsr.cell_detection]\nbatch_size = 2\n",
    );
    let raw = ConfigLoader::new(&path)
        .load_raw()
        .expect("table batch configuration");
    let value = serde_json::to_value(&raw).expect("configuration JSON");
    assert_eq!(value.pointer("/tsr/batch_size"), Some(&json!(4)));
    assert_eq!(
        value.pointer("/tsr/cell_detection/batch_size"),
        Some(&json!(2))
    );
    for (pointer, field) in [
        ("/tsr/batch_size", "tsr.batch_size"),
        (
            "/tsr/cell_detection/batch_size",
            "tsr.cell_detection.batch_size",
        ),
    ] {
        for size in [0, 33] {
            let mut invalid = value.clone();
            *invalid.pointer_mut(pointer).expect("batch size") = json!(size);
            let config = serde_json::from_value::<RawConfig>(invalid)
                .expect("numeric configuration");
            assert!(
                matches!(docparse_config::ValidatedConfig::try_from(config), Err(ConfigError::InvalidValue { field: actual, .. }) if actual == field)
            );
        }
    }
}

/// Short concurrency names preserve file/environment precedence and serialize with their actual units.
#[test]
fn short_concurrency_names_load_and_override() {
    let directory = tempfile::tempdir().expect("configuration directory");
    let path = write_config(
        directory.path(),
        "docparse.toml",
        "[render]\nworkers = 5\nqueue_size = 4\n[layout]\nsession_size = 4\n[tsr]\nsession_size = 8\n[formula]\nbatch_size = 4\n",
    );
    let raw = ConfigLoader::new(&path)
        .with_env_provider(environment_provider(json!({
            "render": {"workers": 3},
            "layout": {"session_size": 2},
            "tsr": {"session_size": 6}
        })))
        .load_raw()
        .expect("short concurrency names must load");
    let values = serde_json::to_value(raw).expect("serialized configuration");
    for (pointer, expected) in [
        ("/render/workers", 3),
        ("/layout/session_size", 2),
        ("/render/queue_size", 4),
        ("/tsr/session_size", 6),
        ("/formula/batch_size", 4),
    ] {
        assert_eq!(
            values.pointer(pointer),
            Some(&json!(expected)),
            "{pointer}"
        );
    }
    // Reject retired names explicitly instead of silently applying a default with a different meaning.
    for (section, field) in [
        ("server", "worker_concurrency"),
        ("server", "pdfium_max_workers"),
        ("layout", "session_pool_size"),
        ("runtime", "page_concurrency"),
        ("tsr", "max_in_flight"),
        ("tsr", "table_jobs"),
        ("ocr", "max_in_flight"),
    ] {
        write_config(
            directory.path(),
            "docparse.toml",
            &format!("[{section}]\n{field} = 2\n"),
        );
        assert!(
            matches!(
                ConfigLoader::new(&path).load_raw(),
                Err(ConfigError::Load { .. })
            ),
            "retired {section}.{field} must be rejected"
        );
    }
}

/// Engine selection is explicit and variant paths resolve beside the primary config.
#[test]
fn formula_engine_paths_are_variant_specific() {
    let directory = tempfile::tempdir().expect("directory");
    let path = write_config(
        directory.path(),
        "docparse.toml",
        r#"
[formula.engine]
type = "texo"
encoder_path = "texo/encoder.onnx"
decoder_path = "texo/decoder.onnx"
tokenizer_path = "texo/tokenizer.json"
"#,
    );
    let raw = ConfigLoader::new(path).load_raw().expect("Texo config");
    let json = serde_json::to_value(raw.formula).expect("formula JSON");
    let engine = json.get("engine").expect("engine");
    assert_eq!(engine.get("type"), Some(&json!("texo")));
    assert_eq!(
        engine.get("encoder_path"),
        Some(&json!(
            directory
                .path()
                .canonicalize()
                .expect("path")
                .join("texo/encoder.onnx")
        ))
    );
    assert!(engine.get("model_manifest_path").is_none());
    assert!(json.get("model_path").is_none());
}

/// Switching variants discards stale paths, while same-variant environment overrides merge normally.
#[test]
fn formula_engine_switches_across_profile_and_environment() {
    let dir = tempfile::tempdir().expect("directory");
    let path = write_config(
        dir.path(),
        "docparse.toml",
        r#"
[formula.engine]
type = "pp"
model_path = "pp/custom.onnx"
model_manifest_path = "pp/custom.json"
"#,
    );
    write_config(
        dir.path(),
        "docparse.texo.toml",
        r#"
[formula.engine]
type = "texo"
encoder_path = "texo/renamed-encoder.onnx"
decoder_path = "texo/renamed-decoder.onnx"
"#,
    );
    let raw = ConfigLoader::new(&path)
        .with_profile("texo")
        .with_env_provider(Figment::from(Serialized::defaults(
            json!({"formula":{"engine":{"tokenizer_path":"texo/env.json"}}}),
        )))
        .load_raw()
        .expect("switched engine");
    let paths = match raw.formula.engine {
        docparse_config::FormulaEngineConfig::Texo(paths) => Some(paths),
        _ => None,
    }
    .expect("expected Texo");
    let base = dir.path().canonicalize().expect("base");
    assert_eq!(paths.encoder_path, base.join("texo/renamed-encoder.onnx"));
    assert_eq!(paths.decoder_path, base.join("texo/renamed-decoder.onnx"));
    assert_eq!(paths.tokenizer_path, base.join("texo/env.json"));
    let raw = ConfigLoader::new(&path).with_profile("texo")
        .with_env_provider(Figment::from(Serialized::defaults(json!({"formula":{"engine":{"type":"pp","model_path":"new/model.onnx"}}}))))
        .load_raw().expect("switch back to PP");
    let paths = match raw.formula.engine {
        docparse_config::FormulaEngineConfig::Pp(paths) => Some(paths),
        _ => None,
    }
    .expect("expected PP");
    assert_eq!(paths.model_path, base.join("new/model.onnx"));
    assert_eq!(
        paths.model_manifest_path,
        base.join("models/pp-formulanet-plus-s/model-manifest.json")
    );
}

/// Variant schemas reject each other's fields and the removed flat path layout.
#[test]
fn formula_engine_rejects_wrong_variant_fields() {
    for value in [
        json!({"type":"texo", "model_manifest_path":"pp.json"}),
        json!({"type":"pp", "encoder_path":"encoder.onnx"}),
        json!({"type":"other"}),
    ] {
        serde_json::from_value::<docparse_config::FormulaEngineConfig>(value)
            .expect_err("invalid variant");
    }
    serde_json::from_value::<docparse_config::FormulaConfig>(
        json!({"model_path":"old.onnx"}),
    )
    .expect_err("legacy paths must not be silently ignored");
}

/// TSR-only experiments resolve independently selected structure and cell artifacts.
#[test]
fn tsr_only_loads_independent_cell_model_paths() {
    let directory = tempfile::tempdir().expect("directory");
    let path = write_config(
        directory.path(),
        "docparse.toml",
        r#"
[tsr]
mode = "tsr_only"
model = "slanext_wired"
model_path = "structure/inference.onnx"
[tsr.cell_detection]
model = "wired"
score_threshold = 0.3
model_path = "cells/inference.onnx"
model_config_path = "cells/inference.yml"
model_manifest_path = "cells/model-manifest.json"
"#,
    );
    let raw = ConfigLoader::new(path).load_raw().expect("TSR-only config");
    let value = serde_json::to_value(&raw.tsr).expect("config JSON");
    assert_eq!(value.get("mode").expect("mode"), "tsr_only");
    assert_eq!(value.get("model").expect("model"), "slanext_wired");
    assert_eq!(
        value
            .get("cell_detection")
            .expect("detector")
            .get("model_path")
            .expect("model path"),
        directory
            .path()
            .canonicalize()
            .expect("directory")
            .join("cells/inference.onnx")
            .to_str()
            .expect("path")
    );
    serde_json::from_value::<docparse_config::TableMode>(json!(
        "external_only"
    ))
    .expect_err("the renamed mode must reject the old spelling");
}

/// Defaults enable wireless cells while the baseline profile can disable them after merging.
#[test]
fn default_wireless_cells_can_be_disabled_by_profile() {
    let defaults = RawConfig::default();
    let cells = defaults.tsr.cell_detection.as_ref().expect("default cells");
    assert!(cells.enabled);
    assert_eq!(cells.model, docparse_config::TableCellModel::Wireless);
    let config_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docparse.toml");
    let main = ConfigLoader::new(&config_path)
        .load_raw()
        .expect("main config");
    let cells = main.tsr.cell_detection.as_ref().expect("main cells");
    assert!(cells.enabled);
    assert_eq!(cells.model, docparse_config::TableCellModel::Wireless);
    // Profile merging must not depend on optional benchmark profiles being shipped with the checkout.
    let directory = tempfile::tempdir().expect("profile directory");
    let profile_base = write_config(
        directory.path(),
        "docparse.toml",
        "[tsr]\nmode = \"tsr_only\"\n",
    );
    write_config(
        directory.path(),
        "docparse.without-cells.toml",
        "[tsr.cell_detection]\nenabled = false\n",
    );
    let baseline = ConfigLoader::new(profile_base)
        .with_profile("without-cells")
        .load_raw()
        .expect("baseline config");
    assert!(
        !baseline
            .tsr
            .cell_detection
            .as_ref()
            .expect("baseline cells")
            .enabled
    );
    assert_eq!(baseline.tsr.model, docparse_config::TsrModel::SlanetPlus);
    assert_eq!(baseline.tsr.mode, docparse_config::TableMode::TsrOnly);
    let mut raw = defaults.clone();
    raw.tsr.model = docparse_config::TsrModel::SlanextWireless;
    raw.tsr
        .cell_detection
        .as_mut()
        .expect("default cells")
        .enabled = false;
    docparse_config::ValidatedConfig::try_from(raw)
        .expect_err("SLANeXt requires an enabled detector");

    // Direct JSON callers must explicitly configure or disable the detector instead of inheriting a queue capacity.
    let mut value = serde_json::to_value(&defaults).expect("defaults");
    value
        .get_mut("tsr")
        .expect("TSR")
        .as_object_mut()
        .expect("TSR object")
        .remove("cell_detection");
    serde_json::from_value::<RawConfig>(value.clone())
        .expect_err("missing detector queue configuration");
    value
        .get_mut("tsr")
        .expect("TSR")
        .as_object_mut()
        .expect("object")
        .insert("cell_detection".into(), serde_json::Value::Null);
    let decoded: RawConfig =
        serde_json::from_value(value).expect("explicitly disabled detector");
    assert!(decoded.tsr.cell_detection.is_none());
}

/// Batch controls must load from old files and reject unbounded tensor allocations.
#[test]
fn ocr_batch_size_loads_and_validates() {
    let directory = tempfile::tempdir().expect("directory");
    for size in [1, 8, 32] {
        let path = write_config(
            directory.path(),
            "docparse.toml",
            &format!("[ocr.recognition]\nbatch_size = {size}\n"),
        );
        ConfigLoader::new(path)
            .load_raw()
            .and_then(docparse_config::ValidatedConfig::try_from)
            .expect("valid OCR batch size");
    }
    for size in [0, 33] {
        let path = write_config(
            directory.path(),
            "docparse.toml",
            &format!("[ocr.recognition]\nbatch_size = {size}\n"),
        );
        assert!(matches!(
            ConfigLoader::new(path)
                .load_raw()
                .and_then(docparse_config::ValidatedConfig::try_from),
            Err(ConfigError::InvalidValue {
                field: "ocr.recognition.batch_size",
                ..
            })
        ));
    }
}

/// Writes one configuration file and returns its path.
fn write_config(directory: &Path, name: &str, contents: &str) -> PathBuf {
    let path = directory.join(name);
    let mut contents = contents.to_owned();
    // Keep fixture tables valid while explicitly supplying capacities unrelated to each loader test.
    if name == "docparse.toml" {
        for (section, size) in [
            ("render", 16),
            ("layout", 1),
            ("tsr", 1),
            ("tsr.cell_detection", 1),
            ("ocr.detection", 1),
            ("ocr.recognition", 16),
            ("ocr.orientation", 16),
            ("formula", 4),
        ] {
            let header = format!("[{section}]");
            let configured = format!("{header}\nqueue_size = {size}");
            if contents.contains(&header) {
                let existing = contents
                    .split_once(&header)
                    .expect("header")
                    .1
                    .split("\n[")
                    .next()
                    .expect("section");
                if !existing.contains("queue_size") {
                    contents = contents.replace(&header, &configured);
                }
            } else {
                contents.push_str(&format!("\n{configured}\n"));
            }
        }
        let render = contents
            .split_once("[render]")
            .expect("render")
            .1
            .split("\n[")
            .next()
            .expect("section");
        if !render.contains("workers") {
            contents = contents.replace("[render]", "[render]\nworkers = 1");
        }
    }
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

[tsr]
model_path = "tables/model.onnx"
mode = "tsr_only"

[render]
queue_size = 2
"#,
    );
    write_config(
        directory.path(),
        "docparse.dev.toml",
        r#"
[layout]
score_threshold = 0.6

[tsr]
mode = "fallback"

[render]
queue_size = 3
"#,
    );

    let config = ConfigLoader::new(&main_path)
        .with_profile("dev")
        .load_raw()
        .expect("the layered configuration must load");

    assert!((config.layout.score_threshold - 0.6).abs() < f64::EPSILON);
    assert_eq!(config.render.queue_size, 3);
    assert_eq!(config.tsr.mode, docparse_config::TableMode::Fallback);
    assert_eq!(
        config.tsr.model_path,
        directory
            .path()
            .canonicalize()
            .expect("directory")
            .join("tables/model.onnx")
    );
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

/// Model backends cannot be overridden through any serialized model configuration group.
#[test]
fn execution_provider_configuration_is_rejected() {
    let directory = tempfile::tempdir().expect("directory");
    for group in ["layout", "tsr", "ocr"] {
        let path = write_config(
            directory.path(),
            "docparse.toml",
            &format!("[{group}]\nexecution_provider = \"cuda\"\n"),
        );
        let error = ConfigLoader::new(path)
            .load_raw()
            .expect_err("backend is selected at compile time");
        assert!(
            error
                .to_string()
                .contains(&format!("{group}.execution_provider"))
        );
        // An environment override must not revive a field removed from TOML.
        let path = write_config(directory.path(), "docparse.toml", "");
        let error = ConfigLoader::new(path)
            .with_env_provider(environment_provider(
                json!({group: {"execution_provider": "cuda"}}),
            ))
            .load_raw()
            .expect_err("environment cannot select a backend");
        assert!(
            error
                .to_string()
                .contains(&format!("{group}.execution_provider"))
        );
    }
}

/// OCR paths are independent files, including filenames that differ from the conventional model directory layout.
#[test]
fn nested_ocr_files_merge_and_resolve_against_the_main_config() {
    let directory = tempfile::tempdir().expect("directory");
    let path = write_config(
        directory.path(),
        "docparse.toml",
        r#"
[ocr.detection]
model_path = "custom/detect.onnx"
model_config_path = "custom/detect.yml"
model_manifest_path = "custom/detect.json"
[ocr.recognition]
model_path = "custom/recognize.onnx"
"#,
    );
    let raw = ConfigLoader::new(path)
        .load_raw()
        .expect("nested OCR files");
    let value = serde_json::to_value(raw).expect("configuration");
    let base = directory.path().canonicalize().expect("base");
    for (key, relative) in [
        ("/ocr/detection/model_path", "custom/detect.onnx"),
        ("/ocr/detection/model_config_path", "custom/detect.yml"),
        ("/ocr/detection/model_manifest_path", "custom/detect.json"),
        ("/ocr/recognition/model_path", "custom/recognize.onnx"),
        (
            "/ocr/recognition/model_config_path",
            "models/pp-ocrv6-medium-rec/inference.yml",
        ),
        (
            "/ocr/recognition/model_manifest_path",
            "models/pp-ocrv6-medium-rec/model-manifest.json",
        ),
        (
            "/ocr/orientation/model_path",
            "models/pp-lcnet-textline-ori/inference.onnx",
        ),
        (
            "/ocr/orientation/model_config_path",
            "models/pp-lcnet-textline-ori/inference.yml",
        ),
        (
            "/ocr/orientation/model_manifest_path",
            "models/pp-lcnet-textline-ori/model-manifest.json",
        ),
    ] {
        assert_eq!(value.pointer(key), Some(&json!(base.join(relative))));
    }
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
        "[render]\nqueue_size = 2\n",
    );
    write_config(
        directory.path(),
        "docparse.dev.toml",
        "[render]\nqueue_size = 6\n",
    );
    write_config(
        directory.path(),
        "docparse.prod.toml",
        "[render]\nqueue_size = 7\n",
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

    assert_eq!(config.render.queue_size, 7);
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
        "[render]\nqueue_size = 6\n",
    );
    let environment = environment_provider(json!({ "profile": "dev" }));

    let config = ConfigLoader::new(main_path)
        .with_env_provider(environment)
        .load_raw()
        .expect("the environment-selected profile must load");

    assert_eq!(config.render.queue_size, 6);
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

    assert!(matches!(
        RawConfig::default().formula.engine,
        docparse_config::FormulaEngineConfig::Texo(_)
    ));
    assert!(matches!(
        config.formula.engine,
        docparse_config::FormulaEngineConfig::Texo(_)
    ));

    assert!((config.layout.score_threshold - 0.5).abs() < f64::EPSILON);
    // Per-host stage tuning must not invalidate the documented library default.
    assert_eq!(RawConfig::default().render.queue_size, 16);
    assert!(config.render.queue_size > 0);
    assert_eq!(config.render.dpi, 144);
    assert_eq!(config.output.formula_placeholder, "[formula]");
    assert_eq!(config.tsr.mode, docparse_config::TableMode::TsrOnly);
    assert_eq!(
        RawConfig::default().tsr.mode,
        docparse_config::TableMode::Fallback
    );
    assert_eq!(
        docparse_config::TableMode::default(),
        docparse_config::TableMode::Fallback
    );
}

/// Serialized configuration contains only model settings and cannot reintroduce per-model backend selection.
#[test]
fn serialized_defaults_have_no_execution_provider() {
    let value = serde_json::to_value(RawConfig::default()).expect("defaults");
    for group in ["layout", "tsr", "ocr"] {
        assert!(
            value
                .get(group)
                .expect("model group")
                .get("execution_provider")
                .is_none()
        );
    }
}

/// Log filters retain code defaults and support the shared file and environment override layers.
#[test]
fn log_directives_follow_configuration_precedence() {
    let directory = tempfile::tempdir().expect("configuration directory");
    let path = write_config(
        directory.path(),
        "docparse.toml",
        "[log]\ndirectives = \"debug,ort=error\"\nfile = \"logs/server.log\"\n",
    );
    let configured = ConfigLoader::new(&path)
        .load_raw()
        .expect("log configuration");
    assert_eq!(configured.log.directives, "debug,ort=error");
    // File destinations use the same config-relative resolution as model artifacts.
    assert_eq!(
        configured.log.file,
        Some(
            directory
                .path()
                .canonicalize()
                .expect("canonical config directory")
                .join("logs/server.log")
        )
    );
    let overridden = ConfigLoader::new(&path)
        .with_env_provider(environment_provider(
            json!({"log": {"directives": "warn,docparse_server=debug", "file": "logs/override.log"}}),
        ))
        .load_raw()
        .expect("environment override");
    assert_eq!(overridden.log.directives, "warn,docparse_server=debug");
    assert_eq!(
        overridden.log.file,
        Some(
            directory
                .path()
                .canonicalize()
                .expect("canonical config directory")
                .join("logs/override.log")
        )
    );
    write_config(directory.path(), "docparse.toml", "");
    let defaults = ConfigLoader::new(path)
        .load_raw()
        .expect("older configuration");
    assert_eq!(defaults.log.directives, "info,ort=warn,sqlx=warn");
    assert_eq!(defaults.log.file, None);
}

/// API prefixes follow the same file and environment precedence as other server settings.
#[test]
fn server_api_prefix_uses_shared_configuration_layers() {
    let directory = tempfile::tempdir().expect("configuration directory");
    let path = write_config(
        directory.path(),
        "docparse.toml",
        "[server]\napi_prefix = \"/api/v2\"\n",
    );
    let configured = ConfigLoader::new(&path)
        .load_raw()
        .expect("prefix configuration");
    assert_eq!(configured.server.api_prefix, "/api/v2");
    let overridden = ConfigLoader::new(path)
        .with_env_provider(environment_provider(
            json!({"server": {"api_prefix": "/gateway/v3"}}),
        ))
        .load_raw()
        .expect("environment override");
    assert_eq!(overridden.server.api_prefix, "/gateway/v3");
}

/// Native deployment sections participate in the existing file, environment, and explicit override precedence.
#[test]
fn server_and_database_use_the_shared_configuration_layers() {
    let directory = tempfile::tempdir().expect("configuration directory");
    let path = write_config(
        directory.path(),
        "docparse.toml",
        r#"
[server]
host = "0.0.0.0"
port = 9090
[database]
url = "postgresql://localhost/docparse"
max_connections = 4
idle_timeout_ms = 12000
"#,
    );
    let environment = environment_provider(json!({
        "server": {"port":9091}, "database":{"max_connections":7}
    }));
    let overrides = environment_provider(json!({"server":{"port":9092}}))
        .extract()
        .expect("overrides");
    let raw = ConfigLoader::new(path)
        .with_env_provider(environment)
        .with_overrides(overrides)
        .load_raw()
        .expect("service configuration");
    let values = serde_json::to_value(raw).expect("configuration values");
    for (path, expected) in [
        ("/server/host", json!("0.0.0.0")),
        ("/server/port", json!(9092)),
        ("/database/url", json!("postgresql://localhost/docparse")),
        ("/database/max_connections", json!(7)),
        ("/database/min_connections", json!(1)),
        ("/database/timeout_ms", json!(5000)),
        ("/database/acquire_timeout_ms", json!(5000)),
        ("/database/idle_timeout_ms", json!(12000)),
        ("/database/sqlx_logging_level", json!("DEBUG")),
        (
            "/database/sqlx_slow_statements_logging_level",
            json!("WARN"),
        ),
        ("/database/sqlx_slow_statements_threshold_ms", json!(1000)),
    ] {
        assert_eq!(values.pointer(path), Some(&expected), "{path}");
    }
}

/// SQL logging accepts typed levels from files and environment overrides, rejecting misspelled levels.
#[test]
fn database_sql_logging_uses_shared_configuration_layers() {
    let directory = tempfile::tempdir().expect("configuration directory");
    let path = write_config(
        directory.path(),
        "docparse.toml",
        r#"
[database]
sqlx_logging_level = "info"
sqlx_slow_statements_logging_level = "error"
sqlx_slow_statements_threshold_ms = 250
"#,
    );
    let configured = ConfigLoader::new(&path).load_raw().expect("SQL logging");
    assert_eq!(
        configured.database.sqlx_logging_level,
        log::LevelFilter::Info
    );
    assert_eq!(
        configured.database.sqlx_slow_statements_logging_level,
        log::LevelFilter::Error
    );
    assert_eq!(configured.database.sqlx_slow_statements_threshold_ms, 250);

    let overridden = ConfigLoader::new(&path)
        .with_env_provider(environment_provider(json!({"database": {
            "sqlx_logging_level": "off",
            "sqlx_slow_statements_logging_level": "debug",
            "sqlx_slow_statements_threshold_ms": 500
        }})))
        .load_raw()
        .expect("environment override");
    assert_eq!(
        overridden.database.sqlx_logging_level,
        log::LevelFilter::Off
    );
    assert_eq!(
        overridden.database.sqlx_slow_statements_logging_level,
        log::LevelFilter::Debug
    );
    assert_eq!(overridden.database.sqlx_slow_statements_threshold_ms, 500);

    for field in ["sqlx_logging_level", "sqlx_slow_statements_logging_level"] {
        write_config(
            directory.path(),
            "docparse.toml",
            &format!("[database]\n{field} = \"verbose\"\n"),
        );
        assert!(ConfigLoader::new(&path).load_raw().is_err(), "{field}");
    }
}
