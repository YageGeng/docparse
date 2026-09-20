//! Mixed consumers retain one pending queue, independent batch limits, and original caller ownership.
use docparse_common::timing::Timings;
use docparse_formula::{
    FormulaError,
    queue::{FormulaQueue, FormulaRequest},
};
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use std::sync::Arc;

/// Builds a valid distinguishable crop without loading inference models.
fn crop(value: u8) -> Arc<PageImage> {
    Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(1)
                .height(1)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(vec![value; 3]))
                .build(),
        )
        .expect("crop"),
    )
}

/// Different consumers drain their own batch limits from one queue and may complete out of order.
#[tokio::test]
async fn consumers_share_pending_work_and_preserve_provenance() {
    let (queue, _) = FormulaQueue::new("mixed-test", 12);
    let first = queue.consumer();
    let second = queue.consumer();
    assert!(Arc::ptr_eq(&first.pressure(), &second.pressure()));
    let work = queue.run_named((0..12).map(crop).collect(), Timings::default());
    tokio::pin!(work);
    assert!(futures_util::poll!(&mut work).is_pending());
    let a = first.receiver();
    let b = second.receiver();
    let mut batches = Vec::new();
    for (receiver, limit, name, expected) in [
        (&a, 2, "texo", 2),
        (&b, 4, "pp", 4),
        (&b, 4, "pp", 4),
        (&a, 2, "texo", 2),
    ] {
        let mut requests =
            receiver.recv().await.expect("ready").take_ready(limit);
        assert_eq!(requests.len(), expected);
        for request in &mut requests {
            request.engine = name.into();
        }
        batches.push(requests);
    }
    for requests in batches.into_iter().rev() {
        let results = requests
            .iter()
            .map(|request| {
                Ok(request.image.data().first().expect("pixel").to_string())
            })
            .collect();
        FormulaRequest::complete_batch(requests, Ok(results));
    }
    let results = work.await.expect("ordered results");
    for (index, output) in results.into_iter().enumerate() {
        assert_eq!(output.latex, index.to_string());
        assert_eq!(
            output.engine,
            if !(2..10).contains(&index) {
                "texo"
            } else {
                "pp"
            }
        );
    }
}

/// Canceling one producer discards its queued crop while a failing peer cannot erase another caller's result.
#[tokio::test]
async fn cancellation_and_per_crop_failures_are_isolated() {
    let (queue, receiver) = FormulaQueue::new("cancel-test", 4);
    {
        let canceled = queue.run(vec![crop(0)], Timings::default());
        tokio::pin!(canceled);
        assert!(futures_util::poll!(&mut canceled).is_pending());
    }
    let good = queue.run(vec![crop(1)], Timings::default());
    let bad = queue.run(vec![crop(2)], Timings::default());
    tokio::pin!(good, bad);
    assert!(futures_util::poll!(&mut good).is_pending());
    assert!(futures_util::poll!(&mut bad).is_pending());
    let requests = receiver.recv().await.expect("ready").take_ready(4);
    assert_eq!(requests.len(), 2);
    FormulaRequest::complete_batch(
        requests,
        Ok(vec![
            Ok("x".into()),
            Err(FormulaError::Invalid("unfinished".into())),
        ]),
    );
    assert_eq!(good.await.expect("unaffected peer"), ["x"]);
    assert!(
        bad.await
            .expect_err("per-crop failure")
            .to_string()
            .contains("unfinished")
    );
}

/// A model-wide count mismatch fails each original response instead of dropping the extra crop.
#[tokio::test]
async fn malformed_batch_counts_fail_all_affected_crops() {
    let (queue, receiver) = FormulaQueue::new("count-test", 2);
    let work = queue.run(vec![crop(1), crop(2)], Timings::default());
    tokio::pin!(work);
    assert!(futures_util::poll!(&mut work).is_pending());
    let requests = receiver.recv().await.expect("ready").take_ready(2);
    FormulaRequest::complete_batch(
        requests,
        Ok(vec![Ok("missing peer".into())]),
    );
    assert!(
        work.await
            .expect_err("count mismatch")
            .to_string()
            .contains("count mismatch")
    );
}
