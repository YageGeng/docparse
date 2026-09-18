use docparse_config::{ConfigLoader, RawConfig, ValidatedConfig};
use figment::{Figment, providers::Serialized};
use serde_json::json;

/// Render capacities are explicit, independent values and obsolete scheduling knobs are rejected.
#[test]
fn render_configuration_is_required_and_independent() {
    let defaults =
        serde_json::to_value(RawConfig::default()).expect("defaults");
    for field in ["workers", "queue_size"] {
        let mut value = defaults.clone();
        value
            .get_mut("render")
            .expect("render")
            .as_object_mut()
            .expect("object")
            .remove(field);
        serde_json::from_value::<RawConfig>(value)
            .expect_err("required render field");
    }
    let mut value = defaults;
    let render = value
        .get_mut("render")
        .expect("render")
        .as_object_mut()
        .expect("object");
    render.insert("workers".into(), json!(5));
    render.insert("queue_size".into(), json!(1));
    ValidatedConfig::try_from(
        serde_json::from_value::<RawConfig>(value.clone()).expect("schema"),
    )
    .expect("one slot with five workers");
    for (section, field) in [
        ("server", "jobs"),
        ("server", "pdfium_workers"),
        ("runtime", "stage_pages"),
        ("runtime", "render_queue_capacity"),
        ("runtime", "blocking_task_limit"),
        ("runtime", "page_limit"),
    ] {
        let mut obsolete = value.clone();
        obsolete
            .get_mut(section)
            .expect("section")
            .as_object_mut()
            .expect("object")
            .insert(field.into(), json!(2));
        serde_json::from_value::<RawConfig>(obsolete)
            .expect_err("retired configuration");
    }
}

/// Native layered defaults cannot conceal a missing render worker count or queue capacity.
#[test]
fn render_loader_requires_both_fields() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("docparse.toml");
    std::fs::write(&path, "").expect("config");
    let mut values = json!({"layout":{"queue_size":1},"tsr":{"queue_size":1,"cell_detection":{"queue_size":1}},"ocr":{"detection":{"queue_size":1},"recognition":{"queue_size":1},"orientation":{"queue_size":1}},"formula":{"queue_size":1}});
    assert!(
        ConfigLoader::new(&path)
            .with_env_provider(Figment::from(Serialized::defaults(
                values.clone()
            )))
            .load_raw()
            .is_err(),
        "missing render settings"
    );
    values
        .as_object_mut()
        .expect("object")
        .insert("render".into(), json!({"workers":2,"queue_size":1}));
    let raw = ConfigLoader::new(&path)
        .with_env_provider(Figment::from(Serialized::defaults(values.clone())))
        .load_raw()
        .expect("explicit render settings");
    ValidatedConfig::try_from(raw).expect("valid render settings");
    for field in ["workers", "queue_size"] {
        let mut missing = values.clone();
        missing
            .get_mut("render")
            .expect("render")
            .as_object_mut()
            .expect("object")
            .remove(field);
        ConfigLoader::new(&path)
            .with_env_provider(Figment::from(Serialized::defaults(missing)))
            .load_raw()
            .expect_err("missing render field");
    }
}
