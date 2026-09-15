use docparse_formula::{FormulaArtifacts, FormulaEngine, PpFormulaNetEngine};
use docparse_layout::{
    PageImage, PageImageInput, PixelFormat, timing::Timings,
};
use std::sync::Arc;

/// Altered model bytes must be rejected before ONNX allocates a session.
#[tokio::test]
async fn rejects_unverified_formula_artifacts() {
    let config = Arc::new(
        docparse_config::ValidatedConfig::try_from(
            docparse_config::RawConfig::default(),
        )
        .expect("config"),
    );
    let artifacts = FormulaArtifacts {
        model: Arc::from(&b"changed"[..]),
        tokenizer: Arc::from(&b"{}"[..]),
        manifest: Arc::from(&b"{}"[..]),
    };
    assert!(
        PpFormulaNetEngine::from_artifacts(config, artifacts)
            .await
            .is_err()
    );
}

/// The real model must decode every batch member, including a smaller final batch.
#[tokio::test]
#[ignore = "requires provisioned Plus-L artifacts and FORMULA_TEST_CROP PNG"]
async fn real_formula_batches_preserve_cardinality_and_content() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let raw = docparse_config::ConfigLoader::new(root.join("docparse.toml"))
        .load_raw()
        .expect("config");
    eprintln!("loading real formula engine");
    let engine = PpFormulaNetEngine::from_config(Arc::new(
        docparse_config::ValidatedConfig::try_from(raw)
            .expect("validated config"),
    ))
    .await
    .expect("formula model");
    eprintln!("loaded real formula engine");
    let image =
        image::open(std::env::var("FORMULA_TEST_CROP").expect("crop path"))
            .expect("crop")
            .to_rgb8();
    let page = Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(image.width())
                .height(image.height())
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(image.into_raw()))
                .build(),
        )
        .expect("page image"),
    );
    let single = engine
        .recognize(vec![Arc::clone(&page)], Timings::default())
        .await
        .expect("single inference");
    eprintln!("single formula: {single:?}");
    assert_eq!(single.len(), 1);
    assert!(!single.first().expect("formula").is_empty());
    for count in [2, 3] {
        eprintln!("running formula batch {count}");
        let output = engine
            .recognize(vec![Arc::clone(&page); count], Timings::default())
            .await
            .expect("batch inference");
        assert_eq!(
            output,
            vec![single.first().expect("formula").clone(); count]
        );
    }
    // Cancellation must leave the shared recognizer available for a later request.
    tokio::time::timeout(
        std::time::Duration::from_millis(1),
        engine.recognize(vec![Arc::clone(&page)], Timings::default()),
    )
    .await
    .expect_err("the bounded request must time out");
    let resumed = engine
        .recognize(vec![page], Timings::default())
        .await
        .expect("recognition after cancellation");
    assert_eq!(resumed, single);
}
