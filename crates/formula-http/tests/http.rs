//! Exercise the actual HTTP adapter without downloading local inference models.
use axum::{
    Json, Router,
    extract::{Multipart, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use docparse_common::timing::Timings;
use docparse_config::{
    FormulaEngineConfig, HttpFormulaConfig, RawConfig, ValidatedConfig,
};
use docparse_formula::FormulaEngine;
use docparse_formula_http::HttpEngine;
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
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
    assert_eq!(body.pointer("/model"), Some(&json!("custom-formula-model")));
    assert_eq!(
        body.pointer("/messages/0/content"),
        Some(&json!("You are a helpful assistant."))
    );
    assert_eq!(
        body.pointer("/messages/1/content/1/text"),
        Some(&json!("Read the formula exactly."))
    );
    assert_eq!(body.pointer("/temperature"), Some(&json!(0.0)));
    assert!(
        body.get("vllm_xargs").is_none(),
        "generic chat must not require MinerU's custom logits processor"
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

/// Checks that image-only requests preserve crop pixels and carry no text prompt.
async fn upload(
    State(counts): State<Arc<Counts>>,
    mut form: Multipart,
) -> Response {
    let mut id = None;
    while let Some(field) = form.next_field().await.expect("multipart field") {
        match field.name().expect("field name") {
            "image" => {
                assert_eq!(field.content_type(), Some("image/png"));
                let bytes = field.bytes().await.expect("image bytes");
                let image =
                    image::load_from_memory(&bytes).expect("PNG").to_rgb8();
                assert_eq!(
                    image.dimensions(),
                    (16, 16),
                    "image-only crops are not resized by the client"
                );
                id = Some(image.get_pixel(0, 0).0[0]);
            }
            "task" => assert_eq!(field.text().await.expect("task"), "formula"),
            other => {
                return (
                    StatusCode::BAD_REQUEST,
                    format!("unexpected field {other}"),
                )
                    .into_response();
            }
        }
    }
    let id = id.expect("image field");
    let active = counts.active.fetch_add(1, Ordering::SeqCst) + 1;
    counts.peak.fetch_max(active, Ordering::SeqCst);
    counts.calls.fetch_add(1, Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(
        10 + u64::from(8 - id.min(8)) * 4,
    ))
    .await;
    counts.active.fetch_sub(1, Ordering::SeqCst);
    match id {
        250 => (StatusCode::UNPROCESSABLE_ENTITY, "generation incomplete")
            .into_response(),
        251 => Json(json!({"text": "   "})).into_response(),
        252 => (StatusCode::SERVICE_UNAVAILABLE, "private upstream details")
            .into_response(),
        253 => Json(json!({"wrong_field": "x"})).into_response(),
        255 => {
            Json(json!({"text": "x".repeat(1024 * 1024 + 1)})).into_response()
        }
        _ => {
            Json(json!({"text": format!("$$x_{{{id}}}$$"), "output_tokens": 5}))
                .into_response()
        }
    }
}

/// Binds a local OpenAI-compatible service with a reverse-proxy prefix.
async fn service() -> (String, Arc<Counts>, tokio::task::JoinHandle<()>) {
    let counts = Arc::new(Counts::default());
    let router = Router::new()
        .route("/mineru/v1/chat/completions", post(completion))
        .route("/mineru/v1/predictions/upload", post(upload))
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
fn engine(url: String, worker_size: usize, timeout_ms: u64) -> HttpEngine {
    let mut raw = RawConfig::default();
    raw.formula.engine = vec![FormulaEngineConfig::Http(
        HttpFormulaConfig::builder()
            .server_url(url)
            .worker_size(worker_size)
            .prompt(Some("Read the formula exactly.".into()))
            .model("custom-formula-model".into())
            .build(),
    )];
    raw.formula.timeout_ms = timeout_ms;
    HttpEngine::try_from(&ValidatedConfig::try_from(raw).expect("config"))
        .expect("engine")
}

/// Two independently configured HTTP groups compete for one queue without multiplying either group's worker limit.
#[tokio::test]
async fn multiple_http_groups_share_one_formula_queue() {
    let (first_url, first_counts, first_server) = service().await;
    let (second_url, second_counts, second_server) = service().await;
    let mut raw = RawConfig::default();
    raw.formula.engine = vec![
        FormulaEngineConfig::Http(
            HttpFormulaConfig::builder()
                .server_url(first_url)
                .worker_size(1)
                .build(),
        ),
        FormulaEngineConfig::Http(
            HttpFormulaConfig::builder()
                .server_url(second_url)
                .worker_size(2)
                .prompt(Some("Read the formula exactly.".into()))
                .model("custom-formula-model".into())
                .build(),
        ),
    ];
    let config = ValidatedConfig::try_from(raw).expect("mixed configuration");
    let mut pool =
        docparse_formula::queue::FormulaPool::new(&config).expect("pool");
    assert_eq!(
        pool.admission().expect("admission").available_permits(),
        config.formula().queue_size + 3
    );
    for index in 0..2 {
        let engine = HttpEngine::spawn(
            &config.for_formula_engine(index).expect("group"),
            pool.receiver(),
        )
        .expect("consumer");
        assert_eq!(engine.name(), "formula-http");
        pool.add(engine);
    }
    let output = pool
        .recognize_named(
            (0..24).map(|index| crop(index % 8)).collect(),
            Timings::default(),
        )
        .await
        .expect("mixed results");
    for (index, result) in output.into_iter().enumerate() {
        assert_eq!(result.latex, format!("x_{{{}}}", index % 8));
        assert_eq!(result.engine, "formula-http");
    }
    assert!(first_counts.calls.load(Ordering::SeqCst) > 0);
    assert!(second_counts.calls.load(Ordering::SeqCst) > 0);
    assert!(first_counts.peak.load(Ordering::SeqCst) <= 1);
    assert!(second_counts.peak.load(Ordering::SeqCst) <= 2);
    first_server.abort();
    second_server.abort();
}

/// Image-only consumers share the queue limit across callers, preserve order, and recover after upstream errors.
#[tokio::test]
async fn image_only_uploads_share_worker_size_and_recover() {
    let (url, counts, server) = service().await;
    let mut raw = RawConfig::default();
    raw.formula.engine = vec![FormulaEngineConfig::Http(
        HttpFormulaConfig::builder()
            .server_url(url)
            .worker_size(2)
            .build(),
    )];
    let engine =
        HttpEngine::try_from(&ValidatedConfig::try_from(raw).expect("config"))
            .expect("engine");
    let (first, second) = tokio::join!(
        engine.recognize((0..4).map(crop).collect(), Timings::default()),
        engine.recognize((4..8).map(crop).collect(), Timings::default()),
    );
    assert_eq!(
        first.expect("first"),
        (0..4).map(|id| format!("x_{{{id}}}")).collect::<Vec<_>>()
    );
    assert_eq!(
        second.expect("second"),
        (4..8).map(|id| format!("x_{{{id}}}")).collect::<Vec<_>>()
    );
    assert_eq!(counts.peak.load(Ordering::SeqCst), 2);
    assert_eq!(counts.calls.load(Ordering::SeqCst), 8);
    for id in [250, 251, 252, 253, 255] {
        let error = engine
            .recognize(vec![crop(id)], Timings::default())
            .await
            .expect_err("invalid output");
        assert!(!error.to_string().contains("private upstream details"));
    }
    assert_eq!(
        engine
            .recognize(vec![crop(6)], Timings::default())
            .await
            .expect("recovered"),
        ["x_{6}"]
    );
    server.abort();
}

/// Engine-owned consumers must survive destruction of the runtime that constructed them.
#[test]
fn engine_survives_construction_runtime() {
    let construction = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let server_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .expect("server runtime");
    let (url, _, server) = server_runtime.block_on(service());
    let recognizer = construction.block_on(async { engine(url, 2, 2000) });
    drop(construction);
    let output = server_runtime
        .block_on(recognizer.recognize(vec![crop(2)], Timings::default()));
    server.abort();
    assert_eq!(
        output.expect("engine survives construction runtime"),
        ["x_{2}"]
    );
}

/// Out-of-order HTTP completions preserve crop order and share one worker_size budget across pages.
#[tokio::test]
async fn bounds_worker_size_across_batches_and_preserves_order() {
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

/// An available HTTP slot takes another caller's crop while an earlier slow crop is still running.
#[tokio::test]
async fn shared_http_queue_refills_before_slow_caller_finishes() {
    let (url, counts, server) = service().await;
    let engine = Arc::new(engine(url, 2, 5000));
    let slow_engine = Arc::clone(&engine);
    let slow = tokio::spawn(async move {
        slow_engine
            .recognize(vec![crop(249), crop(1)], Timings::default())
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while counts.calls.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("first caller admitted");
    let output = tokio::time::timeout(
        Duration::from_millis(500),
        engine.recognize(vec![crop(2)], Timings::default()),
    )
    .await
    .expect("refill without waiting for the slow crop")
    .expect("second caller");
    assert_eq!(output, ["x_{2}"]);
    assert!(!slow.is_finished());
    slow.abort();
    let _ = slow.await;
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
