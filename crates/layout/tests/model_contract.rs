use std::fs;
use std::path::{Path, PathBuf};

use docparse_layout::{LayoutError, ModelSchema, inspect_model};

/// Resolves a repository path from the layout crate directory.
fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

/// Verifies the fixed ONNX artifact exactly matches the tracked neutral schema.
#[test]
#[ignore = "requires fixed PP-DocLayoutV3 model"]
fn fixed_model_schema_matches_fixture() {
    let model_path = repository_path("models/pp-doclayout-v3/inference.onnx");
    let fixture_path = repository_path(
        "crates/layout/tests/fixtures/model/pp_doclayout_v3_schema.json",
    );
    let expected: ModelSchema = serde_json::from_slice(
        &fs::read(fixture_path).expect("the schema fixture must be readable"),
    )
    .expect("the schema fixture must deserialize");

    let actual =
        inspect_model(model_path).expect("the fixed model must inspect");

    assert_eq!(actual, expected);
}

/// Verifies missing models fail before ONNX Runtime initialization.
#[test]
fn missing_model_has_a_structured_error() {
    let path = repository_path("models/pp-doclayout-v3/missing.onnx");

    let error = inspect_model(&path).expect_err("a missing model must fail");

    assert!(
        matches!(error, LayoutError::ModelNotFound { path: actual } if actual == path)
    );
}
