//! External formula configuration must remain independent of local model paths.
use docparse_config::{
    ConfigLoader, FormulaEngineConfig, RawConfig, ValidatedConfig,
};
use serde_json::json;

/// Disabled formula recognition ignores service settings while either enabled kind still validates them.
#[test]
fn disabled_http_ignores_unused_service_settings() {
    for (inline, display) in [(false, false), (true, false), (false, true)] {
        let mut raw = RawConfig::default();
        raw.formula.engine = vec![docparse_config::FormulaEngineConfig::Http(
            docparse_config::HttpFormulaConfig::builder()
                .server_url(String::new())
                .worker_size(0)
                .build(),
        )];
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

/// Switching engines preserves the service URL and applies environment worker_size overrides.
#[test]
fn http_loads_without_local_artifacts() {
    let directory = tempfile::tempdir().expect("config directory");
    let path = directory.path().join("docparse.toml");
    std::fs::write(&path, "[[formula.engine]]\ntype = 'http'\nserver_url = 'http://localhost:8000/v1'\nworker_size = 8\n").expect("config");
    let raw = ConfigLoader::new(path)
        .with_env_provider(figment::Figment::from(
            figment::providers::Serialized::defaults(
                json!({"render":{"workers":1,"queue_size":16},"layout":{"queue_size":1},"tsr":{"queue_size":1,"cell_detection":{"queue_size":1}},"ocr":{"detection":{"queue_size":1},"recognition":{"queue_size":16},"orientation":{"queue_size":16}},"formula": {"queue_size":4,"engine": [{"type":"http", "server_url":"http://localhost:8000/v1", "worker_size": 3}]}}),
            ),
        ))
        .load_raw()
        .expect("HTTP config");
    ValidatedConfig::try_from(raw.clone()).expect("valid HTTP config");
    let engine = serde_json::to_value(raw.formula.engine).expect("engine JSON");
    assert_eq!(
        engine,
        json!([{ "type": "http", "server_url": "http://localhost:8000/v1", "worker_size": 3, "model": "MinerU2.5-2509-1.2B" }])
    );
}

/// Malformed addresses and unbounded worker_size fail before any network request.
#[test]
fn http_rejects_invalid_service_settings() {
    for mut settings in [
        json!({"server_url": "", "worker_size": 4}),
        json!({"server_url": "file:///tmp/model", "worker_size": 4}),
        json!({"server_url": "http://user:secret@localhost:8000", "worker_size": 4}),
        json!({"server_url": "http://localhost:8000?token=secret", "worker_size": 4}),
        json!({"server_url": "http://localhost:8000", "worker_size": 0}),
        json!({"server_url": "http://localhost:8000", "worker_size": 1025}),
        json!({"server_url": "http://localhost:8000", "prompt": "  "}),
        json!({"server_url": "http://localhost:8000", "prompt": "Recognize", "model": ""}),
    ] {
        let mut value =
            serde_json::to_value(RawConfig::default()).expect("defaults");
        settings
            .as_object_mut()
            .expect("settings object")
            .insert("type".into(), json!("http"));
        *value
            .pointer_mut("/formula/engine")
            .expect("engine settings") = json!([settings]);
        let raw: RawConfig =
            serde_json::from_value(value).expect("typed config");
        ValidatedConfig::try_from(raw).expect_err("invalid HTTP settings");
    }
}

/// Both protocols retain reverse-proxy prefixes and normalize a trailing slash on API bases.
#[test]
fn http_endpoints_follow_prompt_presence() {
    for (base, prefix) in [
        ("", ""),
        ("/v1/", ""),
        ("/proxy", "/proxy"),
        ("/proxy/v1/", "/proxy"),
    ] {
        for prompt in [None, Some("Recognize the formula".to_owned())] {
            let operation = if prompt.is_some() {
                "chat/completions"
            } else {
                "predictions/upload"
            };
            let config = docparse_config::HttpFormulaConfig::builder()
                .server_url(format!("https://example.com{base}"))
                .prompt(prompt)
                .build();
            assert_eq!(
                config.endpoint().expect("endpoint").as_str(),
                format!("https://example.com{prefix}/v1/{operation}")
            );
        }
    }
}

/// Optional prompts and model names survive configuration loading at realistic HTTP worker_size.
#[test]
fn http_config_supports_image_only_and_prompted_models() {
    for extra in [
        json!({}),
        json!({"prompt": "Recognize this formula", "model": "custom-model"}),
    ] {
        let mut settings = json!({"type": "http", "server_url": "http://localhost:6008/v1", "worker_size": 32});
        settings
            .as_object_mut()
            .expect("settings")
            .extend(extra.as_object().expect("extra").clone());
        let engine: FormulaEngineConfig =
            serde_json::from_value(settings.clone()).expect("HTTP engine");
        let mut raw = RawConfig::default();
        raw.formula.engine = vec![engine.clone()];
        ValidatedConfig::try_from(raw)
            .expect("valid HTTP settings without local weights");
        let encoded = serde_json::to_value(engine).expect("serialize");
        for (key, value) in settings.as_object().expect("settings") {
            assert_eq!(encoded.get(key), Some(value), "{key}");
        }
    }
}
