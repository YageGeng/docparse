use docparse_config::{RawConfig, ValidatedConfig};
use docparse_layout::{
    PageImage, PageImageInput, PixelFormat, timing::Timings,
};
use docparse_ocr::PaddleOcrEngine;
use std::{path::PathBuf, sync::Arc};

/// Runs pinned detector, classifier and recognizer models against a rendered document, never mocked outputs.
#[tokio::test]
#[ignore = "requires downloaded PaddleOCR artifacts; use --features cuda for CUDA"]
async fn printed_document_runs_complete_ocr() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace");
    let mut raw = RawConfig::default();
    // Resolve code-default files independently of the user's deployment configuration.
    for files in [
        &mut raw.ocr.detection,
        &mut raw.ocr.recognition,
        &mut raw.ocr.orientation,
    ] {
        files.model_path = root.join(&files.model_path);
        files.model_config_path = root.join(&files.model_config_path);
        files.model_manifest_path = root.join(&files.model_manifest_path);
    }
    let engine = PaddleOcrEngine::from_config(Arc::new(
        ValidatedConfig::try_from(raw).expect("config"),
    ))
    .await
    .expect("real OCR engine");
    let image = image::load_from_memory(include_bytes!("fixtures/printed.png"))
        .expect("fixture")
        .into_rgb8();
    let page = PageImage::try_from(
        PageImageInput::builder()
            .width(image.width())
            .height(image.height())
            .pixel_format(PixelFormat::Rgb8)
            .data(Arc::from(image.into_raw()))
            .build(),
    )
    .expect("page");
    let lines = engine
        .recognize(Arc::new(page), Vec::new(), Timings::default())
        .await
        .expect("real OCR inference");
    let text = lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    println!("OCR lines: {}\n{text}", lines.len());
    assert!(lines.len() >= 10, "expected printed document lines");
    assert!(
        text.contains("Embedded font document"),
        "recognized title: {text}"
    );
    assert!(text.contains("Native and Web"), "recognized body: {text}");
    assert!(
        lines
            .iter()
            .all(|line| line.confidence.is_finite() && line.confidence >= 0.5)
    );
}
