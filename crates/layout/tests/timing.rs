use docparse_layout::timing::{TimingStage, Timings};

/// Concurrent page scopes retain attribution and report elapsed time even on early returns.
#[test]
fn concurrent_scopes_keep_page_identity_and_drain_once() {
    let (timings, mut receiver) = Timings::channel();
    std::thread::scope(|scope| {
        for page in 1..=4 {
            let timings = timings.for_page(page);
            scope.spawn(move || {
                let _timer = timings.start(TimingStage::LayoutInference);
                std::thread::sleep(std::time::Duration::from_millis(1));
            });
        }
    });
    let mut pages = Vec::new();
    while let Ok(timing) = receiver.try_recv() {
        assert!(timing.duration_ms.is_finite() && timing.duration_ms > 0.0);
        assert_eq!(timing.stage, TimingStage::LayoutInference);
        assert_eq!(
            serde_json::to_value(&timing)
                .expect("timing serialization")
                .get("stage")
                .and_then(serde_json::Value::as_str),
            Some("layout_inference")
        );
        pages.push(timing.page_number.expect("page-scoped timer"));
    }
    pages.sort_unstable();
    assert_eq!(pages, [1, 2, 3, 4]);
    receiver
        .try_recv()
        .expect_err("drained records must not repeat");

    // A cancelled observer must not turn a completed inference into a new failure.
    drop(receiver);
    drop(timings.start(TimingStage::ParseTotal));
}
