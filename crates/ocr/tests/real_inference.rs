use docparse_common::timing::Timings;
use docparse_config::{RawConfig, ValidatedConfig};
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use docparse_ocr::PaddleOcrEngine;
use std::{path::PathBuf, sync::Arc};

/// Disabled orientation settings must not admit more prepared lines ahead of singleton recognition.
#[tokio::test]
#[ignore = "requires downloaded PaddleOCR detector and recognizer artifacts"]
async fn disabled_orientation_does_not_expand_recognition_admission() {
    use docparse_common::timing::TimingStage;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace");
    let mut raw = RawConfig::default();
    raw.ocr.classify_orientation = false;
    raw.ocr.recognition.session_size = 1;
    raw.ocr.recognition.batch_size = 1;
    raw.ocr.recognition.queue_size = 1;
    raw.ocr.orientation.queue_size = 256;
    raw.ocr.orientation.session_size = 8;
    raw.ocr.orientation.batch_size = 32;
    for files in [&mut raw.ocr.detection.files, &mut raw.ocr.recognition.files]
    {
        files.model_path = root.join(&files.model_path);
        files.model_config_path = root.join(&files.model_config_path);
        files.model_manifest_path = root.join(&files.model_manifest_path);
    }
    let engine = PaddleOcrEngine::from_config(Arc::new(
        ValidatedConfig::try_from(raw).expect("config"),
    ))
    .await
    .expect("models");
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
    let (timings, mut events) = Timings::channel();
    let lines = engine
        .recognize(page, Vec::new(), timings)
        .await
        .expect("OCR");
    assert!(
        lines.len() >= 10,
        "the fixture must exceed the intended admission window"
    );
    // Read the ordered records after completion so test scheduling cannot race the first inference event.
    let mut prepared = 0;
    let mut before_first_inference = None;
    while let Ok(event) = events.try_recv() {
        if event.stage == TimingStage::OcrRecognitionPreprocess {
            prepared += 1;
        }
        if event.stage == TimingStage::OcrRecognitionInference {
            before_first_inference = Some(prepared);
            break;
        }
    }
    // Two admitted lines each have crop and tensor preparation; neither can admit a third before inference completes.
    assert!(
        before_first_inference.expect("inference timing") <= 4,
        "disabled orientation enlarged the ready window: {before_first_inference:?} preprocessing records"
    );
}

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
        raw.ocr.recognition.batch_size = batch_size;
        raw.ocr.orientation.batch_size = batch_size;
        // The largest case also verifies that all three independent multi-session queues preserve outputs.
        for model in [
            &mut raw.ocr.detection,
            &mut raw.ocr.recognition,
            &mut raw.ocr.orientation,
        ] {
            model.session_size = if batch_size == 16 { 2 } else { 1 };
        }
        raw.ocr.detection.batch_size = 2;
        for files in [
            &mut raw.ocr.detection.files,
            &mut raw.ocr.recognition.files,
            &mut raw.ocr.orientation.files,
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
                == docparse_common::timing::TimingStage::OcrOrientationInference
            {
                orientation_calls += 1;
            }
            if event.stage
                == docparse_common::timing::TimingStage::OcrRecognitionInference
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

/// Concurrent pages reach shared model queues without a page gate and preserve real OCR results.
#[tokio::test]
#[ignore = "requires downloaded PaddleOCR artifacts; use --features cuda for CUDA"]
async fn concurrent_pages_run_complete_ocr_without_page_gate() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace");
    let mut raw = RawConfig::default();
    // Resolve code-default files independently of the user's deployment configuration.
    for files in [
        &mut raw.ocr.detection.files,
        &mut raw.ocr.recognition.files,
        &mut raw.ocr.orientation.files,
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
    let page = Arc::new(page);
    let (timings, mut events) = Timings::channel();
    let mut requests = Vec::new();
    for page_number in 1..=3 {
        let mut request = Box::pin(engine.recognize(
            Arc::clone(&page),
            Vec::new(),
            timings.for_page(page_number),
        ));
        assert!(futures_util::poll!(request.as_mut()).is_pending());
        requests.push(request);
    }
    // Hold every page future pending so a hidden gate cannot recycle a completed page's permit.
    let preprocessed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let mut pages = std::collections::BTreeSet::new();
        while pages.len() < 3 {
            let event = events.recv().await.expect("preprocessing event");
            if event.stage == docparse_common::timing::TimingStage::OcrDetectionPreprocess {
                pages.insert(event.page_number.expect("page attribution"));
            }
        }
        pages
    }).await.expect("all three pages must enter preprocessing before any page completes");
    assert_eq!(preprocessed, std::collections::BTreeSet::from([1, 2, 3]));
    let mut results = futures_util::future::try_join_all(requests)
        .await
        .expect("real concurrent OCR")
        .into_iter();
    let lines = results.next().expect("first page");
    for peer in results {
        assert_eq!(
            peer.iter()
                .map(|line| (&line.text, &line.quad))
                .collect::<Vec<_>>(),
            lines
                .iter()
                .map(|line| (&line.text, &line.quad))
                .collect::<Vec<_>>()
        );
    }
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
