//! External formula configuration must remain independent of local model paths.
use docparse_config::{ConfigLoader, RawConfig, ValidatedConfig};
use serde_json::json;

/// Disabled formula recognition ignores service settings while either enabled kind still validates them.
#[test]
fn disabled_mineru_ignores_unused_service_settings() {
    for (inline, display) in [(false, false), (true, false), (false, true)] {
        let mut raw = RawConfig::default();
        raw.formula.engine = docparse_config::FormulaEngineConfig::Mineru(
            docparse_config::MineruFormulaConfig {
                server_url: String::new(),
                concurrency: 0,
            },
        );
        raw.formula.inline_enabled = inline;
        raw.formula.display_enabled = display;
        let result = ValidatedConfig::try_from(raw);
        if inline || display {
            result.expect_err("enabled service must be valid");
        } else {
            result.expect("disabled service is unused");
        }
    }
}

/// Switching engines preserves the service URL and applies environment concurrency overrides.
#[test]
fn mineru_loads_without_local_artifacts() {
    let directory = tempfile::tempdir().expect("config directory");
    let path = directory.path().join("docparse.toml");
    std::fs::write(&path, "[formula.engine]\ntype = 'mineru'\nserver_url = 'http://localhost:8000/v1'\nconcurrency = 8\n").expect("config");
    let raw = ConfigLoader::new(path)
        .with_env_provider(figment::Figment::from(
            figment::providers::Serialized::defaults(
                json!({"render":{"workers":1,"queue_size":16},"layout":{"queue_size":1},"tsr":{"queue_size":1,"cell_detection":{"queue_size":1}},"ocr":{"detection":{"queue_size":1},"recognition":{"queue_size":16},"orientation":{"queue_size":16}},"formula": {"queue_size":4,"engine": {"concurrency": 3}}}),
            ),
        ))
        .load_raw()
        .expect("MinerU config");
    ValidatedConfig::try_from(raw.clone()).expect("valid MinerU config");
    let engine = serde_json::to_value(raw.formula.engine).expect("engine JSON");
    assert_eq!(
        engine,
        json!({"type": "mineru", "server_url": "http://localhost:8000/v1", "concurrency": 3})
    );
}

/// Malformed addresses and unbounded concurrency fail before any network request.
#[test]
fn mineru_rejects_invalid_service_settings() {
    for mut settings in [
        json!({"server_url": "", "concurrency": 4}),
        json!({"server_url": "file:///tmp/model", "concurrency": 4}),
        json!({"server_url": "http://user:secret@localhost:8000", "concurrency": 4}),
        json!({"server_url": "http://localhost:8000?token=secret", "concurrency": 4}),
        json!({"server_url": "http://localhost:8000", "concurrency": 0}),
        json!({"server_url": "http://localhost:8000", "concurrency": 1025}),
    ] {
        let mut value =
            serde_json::to_value(RawConfig::default()).expect("defaults");
        settings
            .as_object_mut()
            .expect("settings object")
            .insert("type".into(), json!("mineru"));
        *value
            .pointer_mut("/formula/engine")
            .expect("engine settings") = settings;
        let raw: RawConfig =
            serde_json::from_value(value).expect("typed config");
        ValidatedConfig::try_from(raw).expect_err("invalid MinerU settings");
    }
}
