use std::path::Path;
use std::sync::Arc;

use docparse_layout::{
    PageImage, PageImageInput, PixelFormat, timing::Timings,
};
use docparse_tsr::{ModelArtifacts, SlanetPlusEngine};

/// The pinned real model must decode cell topology and geometry from a rendered table image.
#[tokio::test]
#[ignore = "requires downloaded SLANet_plus artifacts"]
async fn pinned_model_recognizes_a_real_table() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("root");
    let artifacts = ModelArtifacts::from_paths(
        &root.join("models/slanet-plus/inference.onnx"),
        &root.join("models/slanet-plus/inference.yml"),
        &root.join("models/slanet-plus/model-manifest.json"),
    )
    .expect("artifacts");
    let mut raw = docparse_config::RawConfig::default();
    raw.tsr.cell_detection = None;
    // The real crop exercises the same compile-time backend used by all model families.
    let provider =
        docparse_layout::OnnxBackend::compiled().execution_provider();
    let config = Arc::new(
        docparse_config::ValidatedConfig::try_from(raw).expect("config"),
    );
    let engine = SlanetPlusEngine::from_artifacts(config, artifacts)
        .await
        .expect("real model");
    assert_eq!(engine.execution_provider(), provider);
    let raster = image::open(root.join("crates/tsr/tests/fixtures/table.png"))
        .expect("table image")
        .into_rgb8();
    let image = PageImage::try_from(
        PageImageInput::builder()
            .width(raster.width())
            .height(raster.height())
            .pixel_format(PixelFormat::Rgb8)
            .data(Arc::from(raster.into_raw()))
            .build(),
    )
    .expect("RGB");
    let prediction = engine
        .predict(Arc::new(image), Timings::default())
        .await
        .expect("prediction");
    assert_eq!(
        prediction
            .structure_tokens
            .iter()
            .filter(|t| t.as_str() == "<tr>")
            .count(),
        20
    );
    assert_eq!(prediction.cell_bboxes.len(), 110);
    assert!(prediction.score > 0.9);
    let oracle: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/table-reference.json"))
            .expect("oracle");
    let expected_tokens: Vec<String> = serde_json::from_value(
        oracle.get("structure_tokens").expect("tokens").clone(),
    )
    .expect("tokens");
    let expected_boxes: Vec<Vec<f64>> = serde_json::from_value(
        oracle.get("cell_bboxes").expect("boxes").clone(),
    )
    .expect("boxes");
    assert_eq!(prediction.structure_tokens, expected_tokens);
    for (actual, expected) in prediction
        .cell_bboxes
        .iter()
        .flatten()
        .zip(expected_boxes.iter().flatten())
    {
        assert!(
            (actual - expected).abs() <= 2.0,
            "coordinate {actual} differs from Python {expected}"
        );
    }
}
