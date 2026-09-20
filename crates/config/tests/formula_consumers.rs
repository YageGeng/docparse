//! Formula consumers share policy while retaining independent worker and batch limits.
use docparse_config::{RawConfig, ValidatedConfig};
use serde_json::json;

/// A mixed list retains each group's independent limits without a global batch setting.
#[test]
fn mixed_formula_consumers_load() {
    let mut value =
        serde_json::to_value(RawConfig::default()).expect("defaults");
    *value.pointer_mut("/formula/engine").expect("engine") = json!([
        {"type":"texo", "worker_size":2, "batch_size":4},
        {"type":"http", "worker_size":4, "server_url":"http://localhost:6008"},
        {"type":"http", "worker_size":1, "server_url":"http://localhost:8000", "prompt":"Read formula"}
    ]);
    let raw: RawConfig =
        serde_json::from_value(value).expect("mixed consumers");
    ValidatedConfig::try_from(raw.clone()).expect("validated consumers");
    let encoded = serde_json::to_value(raw.formula).expect("formula");
    assert_eq!(
        encoded
            .get("engine")
            .and_then(serde_json::Value::as_array)
            .expect("engines")
            .len(),
        3
    );
    assert!(encoded.pointer("/engine/1/batch_size").is_none());
    assert!(encoded.get("batch_size").is_none());
}

/// HTTP has no batch option, retired global batching is rejected, and enabled pools need a consumer.
#[test]
fn rejects_obsolete_or_empty_formula_pools() {
    for formula in [
        json!({"queue_size":4,"engine":[{"type":"http","batch_size":1}]}),
        json!({"queue_size":4,"engine":[{"type":"http","concurrency":2}]}),
        json!({"queue_size":4,"batch_size":4,"engine":[{"type":"texo"}]}),
        json!({"queue_size":4,"engine":{"type":"texo"}}),
    ] {
        serde_json::from_value::<docparse_config::FormulaConfig>(formula)
            .expect_err("obsolete settings");
    }
    let mut raw = RawConfig::default();
    raw.formula.engine.clear();
    ValidatedConfig::try_from(raw.clone())
        .expect_err("enabled pool needs consumers");
    raw.formula.inline_enabled = false;
    raw.formula.display_enabled = false;
    ValidatedConfig::try_from(raw).expect("disabled formula pool");
}
