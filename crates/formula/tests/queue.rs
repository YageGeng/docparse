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
    let (queue, receiver) = FormulaQueue::new(1);
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
    let (queue, receiver) = FormulaQueue::new(2);
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
    let (queue, receiver) = FormulaQueue::new(4);
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
