//! Shared admission and ready-only batching independent of model artifacts.
use docparse_formula::FormulaError;
use docparse_formula::queue::{FormulaQueue, FormulaRequest};
use docparse_layout::{
    PageImage, PageImageInput, PixelFormat, timing::Timings,
};
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

/// A bad crop in a merged batch cannot discard another caller's completed formula.
#[tokio::test]
async fn merged_callers_keep_independent_results() {
    let (queue, mut receiver) = FormulaQueue::new(2);
    let queue = Arc::new(queue);
    let first = Arc::clone(&queue);
    let first = tokio::spawn(async move {
        first.run(vec![image()], Timings::default()).await
    });
    while receiver.is_empty() {
        tokio::task::yield_now().await;
    }
    let second = Arc::clone(&queue);
    let second = tokio::spawn(async move {
        second.run(vec![image()], Timings::default()).await
    });
    while receiver.len() < 2 {
        tokio::task::yield_now().await;
    }
    let batch = FormulaRequest::ready(
        receiver.recv().await.expect("first"),
        &mut receiver,
        2,
    );
    FormulaRequest::complete_batch(
        batch,
        Ok(vec![
            Err(FormulaError::Invalid("bad crop".into())),
            Ok("healthy".into()),
        ]),
    );
    first
        .await
        .expect("first task")
        .expect_err("first crop failed");
    assert_eq!(
        second
            .await
            .expect("second task")
            .expect("unrelated crop survives"),
        ["healthy"]
    );
}

/// A partial ready batch is immediately usable and canceled crops do not consume its limit.
#[tokio::test]
async fn shared_queue_flushes_ready_work_and_routes_results() {
    let (queue, mut receiver) = FormulaQueue::new(4);
    let queue = Arc::new(queue);
    let first = Arc::clone(&queue);
    let first = tokio::spawn(async move {
        first.run(vec![image()], Timings::default()).await
    });
    let request = receiver.recv().await.expect("request");
    let batch = FormulaRequest::ready(request, &mut receiver, 4);
    assert_eq!(batch.len(), 1, "do not wait for a full batch");
    FormulaRequest::complete_batch(batch, Ok(vec![Ok("first".into())]));
    assert_eq!(first.await.expect("task").expect("result"), ["first"]);
    let other = Arc::clone(&queue);
    let canceled = tokio::spawn(async move {
        other.run(vec![image()], Timings::default()).await
    });
    let request = receiver.recv().await.expect("canceled request");
    canceled.abort();
    let _ = canceled.await;
    let other = Arc::clone(&queue);
    let live = tokio::spawn(async move {
        other.run(vec![image(), image()], Timings::default()).await
    });
    while receiver.len() < 2 {
        tokio::task::yield_now().await;
    }
    let batch = FormulaRequest::ready(request, &mut receiver, 2);
    assert_eq!(
        batch.len(),
        2,
        "canceled work must not reduce the ready batch"
    );
    FormulaRequest::complete_batch(
        batch,
        Ok(vec![Ok("a".into()), Ok("b".into())]),
    );
    assert_eq!(live.await.expect("task").expect("result"), ["a", "b"]);
}
