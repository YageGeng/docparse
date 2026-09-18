//! Exercise the actual HTTP adapter without downloading local inference models.
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use docparse_config::{
    FormulaEngineConfig, MineruFormulaConfig, RawConfig, ValidatedConfig,
};
use docparse_formula::FormulaEngine;
use docparse_formula_mineru::MineruEngine;
use docparse_layout::{
    PageImage, PageImageInput, PixelFormat, timing::Timings,
};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

/// Counts overlapping requests across all batches sharing the same HTTP engine.
#[derive(Default)]
struct Counts {
    active: AtomicUsize,
    peak: AtomicUsize,
    calls: AtomicUsize,
}

/// Validates the wire contract and returns delayed, failing, or malformed completions.
async fn completion(
    State(counts): State<Arc<Counts>>,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(body.pointer("/model"), Some(&json!("MinerU2.5-2509-1.2B")));
    assert_eq!(
        body.pointer("/messages/0/content"),
        Some(&json!("You are a helpful assistant."))
    );
    assert_eq!(
        body.pointer("/messages/1/content/1/text"),
        Some(&json!("\nFormula Recognition:"))
    );
    assert_eq!(body.pointer("/temperature"), Some(&json!(0.0)));
    assert_eq!(
        body.pointer("/vllm_xargs/no_repeat_ngram_size"),
        Some(&json!(100))
    );
    let data_url = body
        .pointer("/messages/1/content/0/image_url/url")
        .and_then(Value::as_str)
        .expect("data URL");
    let bytes = STANDARD
        .decode(
            data_url
                .strip_prefix("data:image/png;base64,")
                .expect("PNG"),
        )
        .expect("base64");
    let image = image::load_from_memory(&bytes)
        .expect("valid image")
        .to_rgb8();
    let id = image.get_pixel(image.width() / 2, image.height() / 2).0[0];
    // Read the content center because aspect-ratio padding adds a white border.
    match id {
        240 => {
            assert_eq!(image.dimensions(), (1800, 36));
            assert_eq!(image.get_pixel(0, 0).0, [255; 3]);
        }
        241 => {
            assert_eq!(image.dimensions(), (36, 1800));
            assert_eq!(image.get_pixel(0, 0).0, [255; 3]);
        }
        242 => assert_eq!(image.dimensions(), (175, 28)),
        243 => assert_eq!(image.dimensions(), (80, 40)),
        _ => {}
    }
    let active = counts.active.fetch_add(1, Ordering::SeqCst) + 1;
    counts.peak.fetch_max(active, Ordering::SeqCst);
    counts.calls.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(if id == 249 {
        1000
    } else {
        10 + u64::from(8 - id.min(8)) * 4
    }))
    .await;
    counts.active.fetch_sub(1, Ordering::SeqCst);
    match id {
        250 => Json(json!({"choices": [{"finish_reason": "length", "message": {"content": "partial"}}]})).into_response(),
        251 => Json(json!({"choices": [{"finish_reason": "stop", "message": {"content": "\\[ \\]"}}]})).into_response(),
        252 => (StatusCode::SERVICE_UNAVAILABLE, "private upstream details").into_response(),
        253 => Json(json!({"choices": []})).into_response(),
        254 => Json(json!({"choices": [{"finish_reason": "stop", "message": {"content": "\\[x\\]<|im_end|>\n"}}]})).into_response(),
        255 => Json(json!({"choices": [{"finish_reason": "stop", "message": {"content": " <|im_end|> "}}]})).into_response(),
        _ => Json(json!({"choices": [{"finish_reason": "stop", "message": {"content": format!("\\[\nx_{{{id}}}\n\\]")}}]})).into_response(),
    }
}

/// Binds a local OpenAI-compatible service with a reverse-proxy prefix.
async fn service() -> (String, Arc<Counts>, tokio::task::JoinHandle<()>) {
    let counts = Arc::new(Counts::default());
    let router = Router::new()
        .route("/mineru/v1/chat/completions", post(completion))
        .with_state(Arc::clone(&counts));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("listener");
    let url = format!(
        "http://{}/mineru/v1/",
        listener.local_addr().expect("address")
    );
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.expect("server");
    });
    (url, counts, task)
}

/// Encodes an identifier in valid RGB pixels without sharing mutable image storage.
fn crop(id: u8) -> Arc<PageImage> {
    Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(16)
                .height(16)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(vec![id; 16 * 16 * 3]))
                .build(),
        )
        .expect("crop"),
    )
}

/// Creates a native engine using exactly the application's validated configuration path.
fn engine(url: String, concurrency: usize, timeout_ms: u64) -> MineruEngine {
    let mut raw = RawConfig::default();
    raw.formula.engine = FormulaEngineConfig::Mineru(MineruFormulaConfig {
        server_url: url,
        concurrency,
    });
    raw.formula.timeout_ms = timeout_ms;
    MineruEngine::try_from(&ValidatedConfig::try_from(raw).expect("config"))
        .expect("engine")
}

/// Out-of-order HTTP completions preserve crop order and share one concurrency budget across pages.
#[tokio::test]
async fn bounds_concurrency_across_batches_and_preserves_order() {
    let (url, counts, server) = service().await;
    let engine = engine(url, 2, 5000);
    let (first, second) = tokio::join!(
        engine.recognize((0..4).map(crop).collect(), Timings::default()),
        engine.recognize((4..8).map(crop).collect(), Timings::default()),
    );
    assert_eq!(
        first.expect("first batch"),
        (0..4).map(|id| format!("x_{{{id}}}")).collect::<Vec<_>>()
    );
    assert_eq!(
        second.expect("second batch"),
        (4..8).map(|id| format!("x_{{{id}}}")).collect::<Vec<_>>()
    );
    assert_eq!(counts.calls.load(Ordering::SeqCst), 8);
    assert_eq!(counts.peak.load(Ordering::SeqCst), 2);
    server.abort();
}

/// Empty, truncated, malformed, and HTTP failures cannot become recognized LaTeX.
#[tokio::test]
async fn rejects_invalid_responses_and_recovers() {
    let (url, _, server) = service().await;
    let engine = engine(url, 1, 5000);
    for id in [250, 251, 252, 253, 255] {
        let error = engine
            .recognize(vec![crop(id)], Timings::default())
            .await
            .expect_err("invalid response");
        assert!(!error.to_string().contains("private upstream details"));
    }
    assert_eq!(
        engine
            .recognize(vec![crop(5)], Timings::default())
            .await
            .expect("recovered"),
        ["x_{5}"]
    );
    server.abort();
}

/// Mixed wide, tall, tiny, and ordinary crops must meet MinerU's image contract without changing order.
#[tokio::test]
async fn prepares_formula_images_before_sending() {
    let (url, _, server) = service().await;
    let engine = engine(url, 2, 5000);
    let images = [
        (240, 1800, 8),
        (241, 8, 1800),
        (242, 100, 16),
        (243, 80, 40),
    ]
    .into_iter()
    .map(|(id, width, height)| {
        Arc::new(
            PageImage::try_from(
                PageImageInput::builder()
                    .width(width)
                    .height(height)
                    .pixel_format(PixelFormat::Rgb8)
                    .data(Arc::from(vec![
                        id;
                        width as usize * height as usize * 3
                    ]))
                    .build(),
            )
            .expect("crop"),
        )
    })
    .collect();
    assert_eq!(
        engine
            .recognize(images, Timings::default())
            .await
            .expect("preprocessed batch"),
        (240..=243)
            .map(|id| format!("x_{{{id}}}"))
            .collect::<Vec<_>>()
    );
    server.abort();
}

/// The end marker is removed before math delimiters and is never recognized content itself.
#[tokio::test]
async fn strips_completion_end_marker_before_math_delimiters() {
    let (url, _, server) = service().await;
    let engine = engine(url, 1, 5000);
    assert_eq!(
        engine
            .recognize(vec![crop(254)], Timings::default())
            .await
            .expect("LaTeX"),
        ["x"]
    );
    engine
        .recognize(vec![crop(255)], Timings::default())
        .await
        .expect_err("end marker is not LaTeX");
    server.abort();
}

/// Dropped HTTP calls and deadlines release permits without waiting for the upstream response.
#[tokio::test]
async fn cancellation_and_timeout_release_admission() {
    let (url, counts, server) = service().await;
    let engine = engine(url, 1, 200);
    let request = engine.recognize(vec![crop(249)], Timings::default());
    tokio::pin!(request);
    let early = tokio::select! {
        result = &mut request => Some(result),
        _ = async { while counts.calls.load(Ordering::SeqCst) == 0 { tokio::time::sleep(Duration::from_millis(2)).await; } } => None,
    };
    assert!(
        early.is_none(),
        "request completed before admission: {early:?}"
    );
    // The timeout covers queue admission as well as the HTTP round trip.
    assert!(
        request
            .await
            .expect_err("deadline")
            .to_string()
            .contains("timed out")
    );
    assert_eq!(
        engine
            .recognize(vec![crop(6)], Timings::default())
            .await
            .expect("permit recovered"),
        ["x_{6}"]
    );
    {
        let request = engine.recognize(vec![crop(249)], Timings::default());
        tokio::pin!(request);
        let early = tokio::select! {
            result = &mut request => Some(result),
            _ = async { while counts.calls.load(Ordering::SeqCst) < 3 { tokio::time::sleep(Duration::from_millis(2)).await; } } => None,
        };
        assert!(
            early.is_none(),
            "request completed before cancellation: {early:?}"
        );
    }
    assert_eq!(
        engine
            .recognize(vec![crop(7)], Timings::default())
            .await
            .expect("canceled permit recovered"),
        ["x_{7}"]
    );
    server.abort();
}
