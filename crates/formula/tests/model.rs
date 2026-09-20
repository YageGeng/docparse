use docparse_common::timing::Timings;
use docparse_formula::{FormulaArtifacts, FormulaEngine, PpFormulaNetEngine};
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use std::sync::Arc;

/// Two real PP sessions must keep serving ordered batches after their construction runtime shuts down.
#[test]
#[ignore = "requires downloaded PP-FormulaNet Plus-S artifacts"]
fn shared_pp_sessions_survive_construction_runtime() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let directory = root.join("models/pp-formulanet-plus-s");
    let mut raw = docparse_config::RawConfig::default();
    raw.formula.batch_size = 3;
    raw.formula.engine = docparse_config::FormulaEngineConfig::Pp(
        docparse_config::PpFormulaConfig::builder()
            .session_size(2)
            .model_path(directory.join("inference.onnx"))
            .tokenizer_path(directory.join("tokenizer.json"))
            .model_manifest_path(directory.join("model-manifest.json"))
            .build(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let engine = runtime
        .block_on(PpFormulaNetEngine::from_config(Arc::new(
            docparse_config::ValidatedConfig::try_from(raw).expect("config"),
        )))
        .expect("sessions");
    let image = image::open(
        root.join("crates/formula-texo/tests/fixtures/formula_single.png"),
    )
    .expect("fixture")
    .into_rgb8();
    let image = Arc::new(
        docparse_layout::PageImage::try_from(
            docparse_layout::PageImageInput::builder()
                .width(image.width())
                .height(image.height())
                .pixel_format(docparse_layout::PixelFormat::Rgb8)
                .data(Arc::from(image.into_raw()))
                .build(),
        )
        .expect("crop"),
    );
    let expected = runtime
        .block_on(engine.recognize(
            vec![Arc::clone(&image)],
            docparse_common::timing::Timings::default(),
        ))
        .expect("baseline");
    drop(runtime);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("replacement runtime");
    let (first, second) = runtime.block_on(async {
        tokio::join!(
            engine.recognize(
                vec![Arc::clone(&image); 4],
                docparse_common::timing::Timings::default()
            ),
            engine.recognize(
                vec![Arc::clone(&image); 2],
                docparse_common::timing::Timings::default()
            ),
        )
    });
    assert_eq!(
        first.expect("first caller"),
        vec![expected.first().expect("baseline formula").clone(); 4]
    );
    assert_eq!(
        second.expect("second caller"),
        vec![expected.first().expect("baseline formula").clone(); 2]
    );
}

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
#[ignore = "requires provisioned formula artifacts and FORMULA_TEST_CROP PNG"]
async fn real_formula_batches_preserve_cardinality_and_content() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut raw =
        docparse_config::ConfigLoader::new(root.join("docparse.toml"))
            .load_raw()
            .expect("config");
    {
        let model = std::env::var("FORMULA_TEST_MODEL")
            .unwrap_or_else(|_| "pp-formulanet-plus-s".into());
        let directory = root.join("models").join(model);
        raw.formula.engine = docparse_config::FormulaEngineConfig::Pp(
            docparse_config::PpFormulaConfig::builder()
                .model_path(directory.join("inference.onnx"))
                .tokenizer_path(directory.join("tokenizer.json"))
                .model_manifest_path(directory.join("model-manifest.json"))
                .build(),
        );
    }
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
    let (timings, mut observations) = Timings::channel();
    let single = engine
        .recognize(vec![Arc::clone(&page)], timings.clone())
        .await
        .expect("single inference");
    eprintln!("single formula: {single:?}");
    assert_eq!(single.len(), 1);
    assert!(!single.first().expect("formula").is_empty());
    for count in [2, 3] {
        eprintln!("running formula batch {count}");
        let output = engine
            .recognize(vec![Arc::clone(&page); count], timings.clone())
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
        .recognize(vec![Arc::clone(&page)], timings.clone())
        .await
        .expect("recognition after cancellation");
    assert_eq!(resumed, single);
    let mut first_run = Vec::new();
    while let Ok(timing) = observations.try_recv() {
        first_run.push(timing);
    }
    // Measure repeated single-crop calls separately from initial kernel setup and batching.
    for _ in 0..3 {
        assert_eq!(
            engine
                .recognize(vec![Arc::clone(&page)], timings.clone())
                .await
                .expect("warm inference"),
            single
        );
    }
    let mut warm_runs = Vec::new();
    while let Ok(timing) = observations.try_recv() {
        warm_runs.push(timing);
    }
    if let Ok(output) = std::env::var("FORMULA_BENCH_OUTPUT") {
        std::fs::write(output, serde_json::to_vec_pretty(&serde_json::json!({
            "engine": engine.name(), "latex": single, "first_and_batched": first_run, "warm_single_crop": warm_runs,
        })).expect("benchmark JSON")).expect("benchmark output");
    }
}
