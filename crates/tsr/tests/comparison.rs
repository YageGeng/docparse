use docparse_common::timing::Timings;
use docparse_config::{
    ConfigLoader, ModelFiles, TableCellConfig, TableCellModel, TsrModel,
    ValidatedConfig,
};
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use std::{path::Path, sync::Arc};

/// Both upgraded structure variants use real independent detections instead of invalid location heads.
#[tokio::test]
#[ignore = "requires downloaded official structure and cell models"]
async fn upgraded_models_produce_independent_cell_geometry() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("root");
    let raster = image::open(root.join("crates/tsr/tests/fixtures/table.png"))
        .expect("real crop")
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
        .expect("image"),
    );
    for (model, cell_model, suffix) in [
        (TsrModel::SlanextWired, TableCellModel::Wired, "wired"),
        (
            TsrModel::SlanextWireless,
            TableCellModel::Wireless,
            "wireless",
        ),
    ] {
        let mut raw = ConfigLoader::new(root.join("docparse.toml"))
            .load_raw()
            .expect("config");
        raw.tsr.model = model;
        let directory = root.join(format!("models/slanext-{suffix}"));
        raw.tsr.model_path = directory.join("inference.onnx");
        raw.tsr.model_config_path = directory.join("inference.yml");
        raw.tsr.model_manifest_path = directory.join("model-manifest.json");
        let directory = root.join(format!("models/rtdetr-table-cell-{suffix}"));
        raw.tsr.cell_detection = Some(
            TableCellConfig::builder()
                .queue_size(1)
                .model(cell_model)
                .score_threshold(0.3)
                .files(
                    ModelFiles::builder()
                        .model_path(directory.join("inference.onnx"))
                        .model_config_path(directory.join("inference.yml"))
                        .model_manifest_path(
                            directory.join("model-manifest.json"),
                        )
                        .build(),
                )
                .build(),
        );
        let engine = docparse_tsr::PaddleTsrEngine::from_config(Arc::new(
            ValidatedConfig::try_from(raw).expect("valid config"),
        ))
        .await
        .expect("models");
        let prediction = engine
            .predict(Arc::clone(&image), Timings::default())
            .await
            .expect("real prediction");
        assert!(
            prediction.cell_bboxes.is_empty(),
            "SLANeXt position head is invalid"
        );
        assert!(
            prediction
                .structure_tokens
                .iter()
                .any(|token| token == "<tr>")
        );
        assert!(
            prediction.detected_cell_bboxes.len() > 10,
            "table cells must come from real detection"
        );
        eprintln!(
            "{model:?}: {} tokens, {} detected cells",
            prediction.structure_tokens.len(),
            prediction.detected_cell_bboxes.len()
        );
    }
}
