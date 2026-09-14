use docparse_config::{RawConfig, ValidatedConfig};
use docparse_layout::{
    PageImage, PageImageInput, PixelFormat, timing::Timings,
};
use docparse_ocr::PaddleOcrEngine;
use std::{path::PathBuf, sync::Arc};

/// Real batches must preserve line order, geometry and text while reducing model invocations.
#[tokio::test]
#[ignore = "requires downloaded PaddleOCR artifacts; use --features cuda for CUDA"]
async fn batches_preserve_printed_document_results() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace");
    let image = image::load_from_memory(include_bytes!("fixtures/printed.png"))
        .expect("fixture")
        .into_rgb8();
    let page = Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(image.width())
                .height(image.height())
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(image.into_raw()))
                .build(),
        )
        .expect("page"),
    );
    let mut baseline = None;
    let mut single_calls = 0;
    for batch_size in [1, 4, 8, 16] {
        let mut raw = RawConfig::default();
        raw.ocr.batch_size = batch_size;
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
        .expect("engine");
        let (timings, mut events) = Timings::channel();
        let lines = engine
            .recognize(Arc::clone(&page), Vec::new(), timings)
            .await
            .expect("OCR");
        // Release all native sessions before loading the next engine on an 8 GiB GPU.
        drop(engine);
        let mut calls = 0;
        let mut orientation_calls = 0;
        while let Ok(event) = events.try_recv() {
            if event.stage
                == docparse_layout::timing::TimingStage::OcrOrientationInference
            {
                orientation_calls += 1;
            }
            if event.stage
                == docparse_layout::timing::TimingStage::OcrRecognitionInference
            {
                calls += 1;
            }
        }
        if batch_size == 16 {
            assert!(
                orientation_calls < calls,
                "fixed-size orientation batches must not inherit recognition width boundaries"
            );
        }
        let signature: Vec<_> = lines
            .iter()
            .map(|line| (line.text.clone(), line.quad.clone()))
            .collect();
        if let Some(expected) = &baseline {
            assert_eq!(
                &signature, expected,
                "batch {batch_size} changed text or order"
            );
            assert!(
                calls < single_calls,
                "batch {batch_size} did not reduce calls"
            );
        } else {
            assert!(signature.len() >= 10);
            baseline = Some(signature);
            single_calls = calls;
        }
    }
}

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
