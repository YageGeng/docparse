//! Formula engine boundary regressions independent of external model files.
use docparse_common::timing::{TimingStage, Timings};
use docparse_formula::{FormulaEngine, FormulaError};
use docparse_formula_texo::{TexoArtifacts, TexoEngine};
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use std::sync::Arc;

/// Truncated and unrelated weights must fail before allocating model tensors.
#[test]
fn rejects_unverified_model_bytes() {
    for bytes in [b"".as_slice(), b"GGUF", b"not a Texo checkpoint"] {
        assert!(matches!(
            TexoArtifacts {
                encoder: Arc::from(bytes),
                decoder: Arc::from(bytes),
                tokenizer: Arc::from(bytes),
            }
            .verify(),
            Err(FormulaError::Artifacts(_))
        ));
    }
}

/// Constructs the same validated RGB crop accepted by core's formula interface.
fn image(name: &str) -> Arc<PageImage> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    let rgb = image::open(path).expect("fixture").to_rgb8();
    Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(rgb.width())
                .height(rgb.height())
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(rgb.into_raw()))
                .build(),
        )
        .expect("RGB crop"),
    )
}

/// Real cached batches must match the independent Python baseline and recover after cancellation.
#[tokio::test]
#[ignore = "requires models/texo; run tests/reference.py or download the pinned assets first"]
async fn real_model_batch_parity_and_cancellation() {
    let dir = std::env::var_os("DOCPARSE_TEXO_MODELS")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../models/texo")
        });
    let mut raw = docparse_config::RawConfig::default();
    raw.formula.engine = docparse_config::FormulaEngineConfig::Texo(
        docparse_config::TexoFormulaConfig::builder()
            .cpu_session_size(1)
            .gpu_session_size(usize::from(
                docparse_layout::OnnxBackend::compiled().execution_provider()
                    != docparse_layout::ExecutionProvider::Cpu,
            ))
            .encoder_path(dir.join("encoder_model.onnx"))
            .decoder_path(dir.join("decoder_model_merged.onnx"))
            .tokenizer_path(dir.join("tokenizer.json"))
            .build(),
    );
    let config = Arc::new(
        docparse_config::ValidatedConfig::try_from(raw).expect("config"),
    );

    let engine = TexoEngine::from_config(config)
        .await
        .expect("real Texo sessions");
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/reference.json"))
            .expect("reference");
    let cases = reference
        .get("cases")
        .expect("cases")
        .as_array()
        .expect("array");
    let images: Vec<_> = cases
        .iter()
        .map(|case| image(case["image"].as_str().expect("image")))
        .collect();
    let expected: Vec<_> = cases
        .iter()
        .map(|case| case["latex"].as_str().expect("latex").to_owned())
        .collect();
    assert_eq!(
        engine
            .recognize(images.clone(), Timings::default())
            .await
            .expect("unequal-length batch"),
        expected
    );
    for (crop, expected) in images.iter().zip(&expected) {
        assert_eq!(
            engine
                .recognize(vec![Arc::clone(crop)], Timings::default())
                .await
                .expect("singleton tail"),
            std::slice::from_ref(expected)
        );
    }
    let (page_a, page_b) = tokio::join!(
        engine.recognize(images.clone(), Timings::default().for_page(1)),
        engine.recognize(images.clone(), Timings::default().for_page(2)),
    );
    assert_eq!(page_a.expect("first page"), expected);
    assert_eq!(page_b.expect("second page"), expected);
    let empty = Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(0)
                .height(0)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(Vec::<u8>::new()))
                .build(),
        )
        .expect("empty raster"),
    );
    let (invalid, valid) = tokio::join!(
        engine.recognize(vec![empty], Timings::default()),
        engine.recognize(images.clone(), Timings::default()),
    );
    invalid.expect_err("malformed crop");
    assert_eq!(valid.expect("unrelated page survives"), expected);
    engine
        .recognize(Vec::new(), Timings::default())
        .await
        .expect_err("empty batch");
    let (timings, mut observations) = Timings::channel();
    {
        let request = engine.recognize(images.clone(), timings);
        tokio::pin!(request);
        loop {
            tokio::select! {
                result = &mut request => panic!("request completed before cancellation: {result:?}"),
                observation = observations.recv() => {
                    if observation.expect("stage").stage == TimingStage::FormulaPreprocess { break; }
                }
            }
        }
        // Dropping the pending future must terminate its run without poisoning either session.
    }
    let recovered = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        engine.recognize(vec![image("formula_single.png")], Timings::default()),
    )
    .await
    .expect("canceled batch released the worker")
    .expect("recovered session");
    assert_eq!(
        recovered,
        [cases
            .iter()
            .find(|case| case["image"] == "formula_single.png")
            .expect("single")["latex"]
            .as_str()
            .expect("latex")]
    );
}
