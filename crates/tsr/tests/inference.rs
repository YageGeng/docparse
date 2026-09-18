use std::path::Path;
use std::sync::Arc;

use docparse_common::timing::Timings;
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use docparse_tsr::{ModelArtifacts, SlanetPlusEngine};

/// Both batched model queues remain usable after their construction runtime is destroyed.
#[test]
#[ignore = "requires downloaded SLANet_plus and wireless cell artifacts"]
fn batched_models_survive_construction_runtime() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut raw =
        docparse_config::ConfigLoader::new(root.join("docparse.toml"))
            .load_raw()
            .expect("configuration");
    // Exercise both shared queues with more than one native consumer.
    raw.tsr.session_size = 2;
    raw.tsr
        .cell_detection
        .as_mut()
        .expect("cell model")
        .session_size = 2;
    raw.tsr.batch_size = 4;
    raw.tsr
        .cell_detection
        .as_mut()
        .expect("cell model")
        .batch_size = 2;
    let raster = image::open(root.join("crates/tsr/tests/fixtures/table.png"))
        .expect("table image")
        .into_rgb8();
    let image = Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(raster.width())
                .height(raster.height())
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(raster.into_raw()))
                .build(),
        )
        .expect("RGB"),
    );
    // Idle model owners must not retain the caller's only blocking thread.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .expect("construction runtime");
    let engine = runtime
        .block_on(SlanetPlusEngine::from_config(Arc::new(
            docparse_config::ValidatedConfig::try_from(raw)
                .expect("configuration"),
        )))
        .expect("models");
    let expected = runtime
        .block_on(engine.predict(Arc::clone(&image), Timings::default()))
        .expect("initial prediction");
    drop(runtime);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .expect("replacement runtime");
    let actual = runtime
        .block_on(engine.predict(image, Timings::default()))
        .expect("prediction after construction runtime shutdown");
    assert_eq!(actual.structure_tokens, expected.structure_tokens);
    assert_eq!(actual.cell_bboxes, expected.cell_bboxes);
    assert_eq!(actual.detected_cell_bboxes, expected.detected_cell_bboxes);
}

/// Independently configured structure/detector batches preserve each crop's topology and original coordinates.
#[tokio::test]
#[ignore = "requires downloaded SLANet_plus and wireless cell artifacts"]
async fn configurable_batches_match_singleton_predictions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let raster = image::open(root.join("crates/tsr/tests/fixtures/table.png"))
        .expect("table image")
        .into_rgb8();
    let small = image::imageops::resize(
        &raster,
        raster.width() / 2,
        raster.height() / 2,
        image::imageops::FilterType::Triangle,
    );
    let images: Vec<_> = [raster, small]
        .into_iter()
        .map(|raster| {
            Arc::new(
                PageImage::try_from(
                    PageImageInput::builder()
                        .width(raster.width())
                        .height(raster.height())
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(raster.into_raw()))
                        .build(),
                )
                .expect("RGB"),
            )
        })
        .collect();
    let mut references = Vec::new();
    for (structure_batch, cell_batch) in [(1, 1), (4, 2), (2, 4)] {
        let mut raw =
            docparse_config::ConfigLoader::new(root.join("docparse.toml"))
                .load_raw()
                .expect("configuration");
        raw.tsr.session_size = if structure_batch == 1 { 1 } else { 2 };
        raw.tsr.cell_detection.as_mut().expect("cells").session_size =
            if cell_batch == 1 { 1 } else { 2 };
        raw.tsr.batch_size = structure_batch;
        raw.tsr
            .cell_detection
            .as_mut()
            .expect("cell configuration")
            .batch_size = cell_batch;
        let engine = Arc::new(
            SlanetPlusEngine::from_config(Arc::new(
                docparse_config::ValidatedConfig::try_from(raw)
                    .expect("config"),
            ))
            .await
            .expect("model"),
        );
        if structure_batch == 1 {
            for image in &images {
                references.push(
                    engine
                        .predict(Arc::clone(image), Timings::default())
                        .await
                        .expect("singleton prediction"),
                );
            }
            continue;
        }
        let mut calls = tokio::task::JoinSet::new();
        // Five requests exercise both full and partial batches, with distinct coordinate scales.
        for index in 0..5 {
            let engine = Arc::clone(&engine);
            let image = Arc::clone(images.get(index % 2).expect("crop"));
            calls.spawn(async move {
                (index, engine.predict(image, Timings::default()).await)
            });
        }
        while let Some(call) = calls.join_next().await {
            let (index, prediction) = call.expect("task");
            let prediction = prediction.expect("batch prediction");
            let expected =
                references.get(index % 2).expect("singleton reference");
            assert_eq!(prediction.structure_tokens, expected.structure_tokens);
            for (actual, expected) in [
                (&prediction.cell_bboxes, &expected.cell_bboxes),
                (
                    &prediction.detected_cell_bboxes,
                    &expected.detected_cell_bboxes,
                ),
            ] {
                assert_eq!(actual.len(), expected.len());
                for bbox in actual {
                    assert!(
                        expected.iter().any(|reference| bbox
                            .iter()
                            .zip(reference)
                            .all(|(a, b)| (a - b).abs() <= 2.0)),
                        "batch {structure_batch}/{cell_batch} crop {index} mixed or changed box {bbox:?}"
                    );
                }
            }
        }
    }
}

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
