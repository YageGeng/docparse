//! Shared admission and ready-only batching independent of model artifacts.
use docparse_common::timing::Timings;
use docparse_formula::FormulaError;
use docparse_formula::queue::{FormulaQueue, FormulaRequest};
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use std::sync::Arc;

/// One crop keeps the test independent of local model resources.
fn image() -> Arc<PageImage> {
    Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(1)
                .height(1)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from([0, 0, 0]))
                .build(),
        )
        .expect("image"),
    )
}

/// A model-owned request retains render capacity after its caller is canceled, even for a preexisting crop.
#[tokio::test]
async fn canceled_model_request_retains_page_delivery() {
    let pages = docparse_common::PageQueue::new(1);
    let lease = pages.reserve().await.expect("page slot");
    let (queue, receiver) = FormulaQueue::new("test", 1);
    let crop = image();
    let caller = tokio::spawn(async move {
        lease.scope(queue.run(vec![crop], Timings::default())).await
    });
    let batch = receiver.recv().await.expect("model request").take_ready(1);
    caller.abort();
    caller.await.expect_err("canceled caller");
    tokio::time::timeout(std::time::Duration::from_millis(30), pages.reserve())
        .await
        .expect_err("model request still owns the delivery");
    drop(batch);
    tokio::time::timeout(std::time::Duration::from_secs(1), pages.reserve())
        .await
        .expect("model released resources")
        .expect("queue");
}

/// A bad crop in a merged batch cannot discard another caller's completed formula.
#[tokio::test]
async fn merged_callers_keep_independent_results() {
    let (queue, receiver) = FormulaQueue::new("test", 2);
    let mut first = Box::pin(queue.run(vec![image()], Timings::default()));
    let mut second = Box::pin(queue.run(vec![image()], Timings::default()));
    // Polling once publishes each request before awaiting its response; no scheduler sleep is needed.
    assert!(futures_util::poll!(&mut first).is_pending());
    assert!(futures_util::poll!(&mut second).is_pending());
    let batch = receiver.recv().await.expect("first").take_ready(2);
    FormulaRequest::complete_batch(
        batch,
        Ok(vec![
            Err(FormulaError::Invalid("bad crop".into())),
            Ok("healthy".into()),
        ]),
    );
    first.await.expect_err("first crop failed");
    assert_eq!(second.await.expect("unrelated crop survives"), ["healthy"]);
}

/// A partial ready batch is immediately usable and canceled crops do not consume its limit.
#[tokio::test]
async fn shared_queue_flushes_ready_work_and_routes_results() {
    let (queue, receiver) = FormulaQueue::new("test", 4);
    let mut first = Box::pin(queue.run(vec![image()], Timings::default()));
    assert!(futures_util::poll!(&mut first).is_pending());
    let batch = receiver.recv().await.expect("first").take_ready(4);
    assert_eq!(batch.len(), 1, "do not wait for a full batch");
    FormulaRequest::complete_batch(batch, Ok(vec![Ok("first".into())]));
    assert_eq!(first.await.expect("result"), ["first"]);
    let mut canceled = Box::pin(queue.run(vec![image()], Timings::default()));
    assert!(futures_util::poll!(&mut canceled).is_pending());
    drop(canceled);
    let mut live =
        Box::pin(queue.run(vec![image(), image()], Timings::default()));
    assert!(futures_util::poll!(&mut live).is_pending());
    let batch = receiver.recv().await.expect("ready work").take_ready(2);
    assert_eq!(
        batch.len(),
        2,
        "canceled work must not reduce the ready batch"
    );
    FormulaRequest::complete_batch(
        batch,
        Ok(vec![Ok("a".into()), Ok("b".into())]),
    );
    assert_eq!(live.await.expect("result"), ["a", "b"]);
}

/// A batch-wide failure is retried in halves so only crops that fail alone lose their formula.
#[tokio::test]
async fn failed_batches_retry_in_halves_until_crops_fail_alone() {
    let (queue, receiver) = FormulaQueue::new("test", 4);
    let callers = (0..4)
        .map(|_| Box::pin(queue.run(vec![image()], Timings::default())))
        .collect::<Vec<_>>();
    let mut callers = callers;
    for caller in &mut callers {
        assert!(futures_util::poll!(caller).is_pending());
    }
    let batch = receiver.recv().await.expect("batch").take_ready(4);
    let poisoned = Arc::as_ptr(&batch.get(2).expect("third crop").image);
    let mut calls = 0;
    // Any multi-crop batch fails like a device allocation failure; one crop also fails on its own.
    FormulaRequest::complete_with_retry(batch, &mut |requests| {
        calls += 1;
        if requests.len() > 1 {
            return Err(FormulaError::Onnx(ort::Error::new(
                "allocation failed",
            )));
        }
        if Arc::as_ptr(&requests.first().expect("crop").image) == poisoned {
            return Ok(vec![Err(FormulaError::Invalid("bad crop".into()))]);
        }
        Ok(vec![Ok("latex".into())])
    });
    let mut outcomes = Vec::new();
    for caller in callers {
        outcomes.push(caller.await);
    }
    assert_eq!(outcomes.iter().filter(|outcome| outcome.is_ok()).count(), 3);
    assert!(matches!(
        outcomes.get(2),
        Some(Err(FormulaError::Invalid(message))) if message == "bad crop"
    ));
    // 4 → 2 + 2 → 1 + 1 + 1 + 1 physical attempts.
    assert_eq!(calls, 7);
}

/// Deterministic failures such as result-count mismatches are reported once, never bisected.
#[tokio::test]
async fn deterministic_batch_errors_are_not_retried() {
    let (queue, receiver) = FormulaQueue::new("test", 2);
    let mut first = Box::pin(queue.run(vec![image()], Timings::default()));
    let mut second = Box::pin(queue.run(vec![image()], Timings::default()));
    assert!(futures_util::poll!(&mut first).is_pending());
    assert!(futures_util::poll!(&mut second).is_pending());
    let batch = receiver.recv().await.expect("batch").take_ready(2);
    let mut calls = 0;
    FormulaRequest::complete_with_retry(batch, &mut |_requests| {
        calls += 1;
        Err(FormulaError::Invalid("result count mismatch".into()))
    });
    assert_eq!(calls, 1);
    first.await.expect_err("first caller shares the failure");
    second.await.expect_err("second caller shares the failure");
}

/// A batch whose callers all canceled is reported once instead of being retried.
#[tokio::test]
async fn canceled_batches_are_not_retried() {
    let (queue, receiver) = FormulaQueue::new("test", 2);
    let mut first = Box::pin(queue.run(vec![image()], Timings::default()));
    let mut second = Box::pin(queue.run(vec![image()], Timings::default()));
    assert!(futures_util::poll!(&mut first).is_pending());
    assert!(futures_util::poll!(&mut second).is_pending());
    let batch = receiver.recv().await.expect("batch").take_ready(2);
    drop(first);
    drop(second);
    let mut calls = 0;
    FormulaRequest::complete_with_retry(batch, &mut |_requests| {
        calls += 1;
        Err(FormulaError::Onnx(ort::Error::new("batch canceled")))
    });
    assert_eq!(calls, 1);
}
