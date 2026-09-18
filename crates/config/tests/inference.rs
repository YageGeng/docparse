use docparse_config::{RawConfig, ValidatedConfig};
use serde_json::json;

/// Every model queue requires an explicit positive capacity, independently of sessions and batches.
#[test]
fn queue_sizes_are_required_and_independent() {
    let paths = [
        "/layout",
        "/tsr",
        "/tsr/cell_detection",
        "/ocr/detection",
        "/ocr/recognition",
        "/ocr/orientation",
        "/formula",
    ];
    let defaults =
        serde_json::to_value(RawConfig::default()).expect("defaults");
    for path in paths {
        let mut missing = defaults.clone();
        missing
            .pointer_mut(path)
            .expect("model")
            .as_object_mut()
            .expect("object")
            .remove("queue_size");
        assert!(
            serde_json::from_value::<RawConfig>(missing).is_err(),
            "accepted missing {path}/queue_size"
        );
    }
    for path in paths {
        for size in [0, 1, 7, 1024, 536869888] {
            let mut value = defaults.clone();
            value.pointer_mut(path).expect("model")["queue_size"] = json!(size);
            let raw: RawConfig =
                serde_json::from_value(value).expect("queue schema");
            let result = ValidatedConfig::try_from(raw);
            assert_eq!(
                result.is_ok(),
                (1..=536869887).contains(&size),
                "{path}/queue_size={size}"
            );
        }
        for invalid in [json!(-1), json!(1.5), json!("8"), json!(null)] {
            let mut value = defaults.clone();
            value.pointer_mut(path).expect("model")["queue_size"] = invalid;
            assert!(
                serde_json::from_value::<RawConfig>(value).is_err(),
                "invalid queue type at {path}"
            );
        }
    }
}

/// Layered native loading must not silently fill required queue capacities from code defaults.
#[test]
fn native_loader_requires_queue_sizes_after_merging() {
    use docparse_config::ConfigLoader;
    use figment::{Figment, providers::Serialized};
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("docparse.toml");
    std::fs::write(&path, "").expect("config");
    let queues = json!({
        "render": {"workers": 1, "queue_size": 16},
        "layout": {"queue_size": 3},
        "tsr": {"queue_size": 5, "cell_detection": {"queue_size": 7}},
        "ocr": {"detection": {"queue_size": 2}, "recognition": {"queue_size": 1}, "orientation": {"queue_size": 9}},
        "formula": {"queue_size": 11}
    });
    assert!(
        ConfigLoader::new(&path)
            .with_env_provider(Figment::new())
            .load_raw()
            .is_err(),
        "empty configuration must not inherit queue sizes"
    );
    let raw = ConfigLoader::new(&path)
        .with_env_provider(Figment::from(Serialized::defaults(queues.clone())))
        .load_raw()
        .expect("explicit queue sizes");
    ValidatedConfig::try_from(raw).expect("positive capacities");
    for section in [
        "/layout",
        "/tsr",
        "/tsr/cell_detection",
        "/ocr/detection",
        "/ocr/recognition",
        "/ocr/orientation",
        "/formula",
    ] {
        let mut missing = queues.clone();
        missing
            .pointer_mut(section)
            .expect("section")
            .as_object_mut()
            .expect("object")
            .remove("queue_size");
        let error = ConfigLoader::new(&path)
            .with_env_provider(Figment::from(Serialized::defaults(missing)))
            .load_raw()
            .expect_err("required capacity");
        assert!(error.to_string().contains("queue_size"), "{error}");
    }
}

/// Each model must accept independent session and batch limits through the public configuration.
#[test]
fn every_model_has_independent_inference_limits() {
    let mut value =
        serde_json::to_value(RawConfig::default()).expect("defaults");
    for (path, sessions, batch) in [
        ("/layout", 2, 3),
        ("/tsr", 3, 4),
        ("/tsr/cell_detection", 2, 5),
        ("/ocr/detection", 2, 2),
        ("/ocr/recognition", 3, 8),
        ("/ocr/orientation", 4, 16),
    ] {
        let model = value
            .pointer_mut(path)
            .expect("model")
            .as_object_mut()
            .expect("object");
        model.remove("sessions");
        model.insert("session_size".into(), json!(sessions));
        model.insert("batch_size".into(), json!(batch));
    }
    value
        .pointer_mut("/formula/engine")
        .expect("engine")
        .as_object_mut()
        .expect("engine")
        .remove("sessions");
    *value
        .pointer_mut("/formula/engine/session_size")
        .expect("session size") = json!(2);
    let raw: RawConfig =
        serde_json::from_value(value.clone()).expect("new inference settings");
    ValidatedConfig::try_from(raw.clone()).expect("valid independent settings");
    let round_trip = serde_json::to_value(raw).expect("round trip");
    for path in [
        "/layout",
        "/tsr",
        "/tsr/cell_detection",
        "/ocr/detection",
        "/ocr/recognition",
        "/ocr/orientation",
        "/formula/engine",
    ] {
        assert_eq!(round_trip.pointer(path), value.pointer(path));
    }
}

/// Invalid consumer counts and obsolete names must fail before allocating any model resources.
#[test]
fn inference_limits_reject_invalid_counts_and_old_names() {
    let defaults =
        serde_json::to_value(RawConfig::default()).expect("defaults");
    for path in [
        "/layout",
        "/tsr",
        "/tsr/cell_detection",
        "/ocr/detection",
        "/ocr/recognition",
        "/ocr/orientation",
        "/formula/engine",
    ] {
        for count in [0, 9] {
            let mut value = defaults.clone();
            value.pointer_mut(path).expect("model")["session_size"] =
                json!(count);
            let raw: RawConfig = serde_json::from_value(value).expect("schema");
            assert!(
                ValidatedConfig::try_from(raw).is_err(),
                "accepted {path} session_size={count}"
            );
        }
    }
    for path in ["/layout", "/formula/engine"] {
        let mut value = defaults.clone();
        value.pointer_mut(path).expect("model")["sessions"] = json!(2);
        assert!(
            serde_json::from_value::<RawConfig>(value).is_err(),
            "accepted obsolete {path}/sessions"
        );
    }
    let mut value = defaults;
    value
        .get_mut("ocr")
        .expect("OCR")
        .as_object_mut()
        .expect("object")
        .insert("batch_size".into(), json!(16));
    serde_json::from_value::<RawConfig>(value)
        .expect_err("retired OCR batch size");
}
