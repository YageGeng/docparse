//! Measures derived-artifact admission: how much memory concurrent builds peak at.
//!
//! The signal is wall-clock wave scaling: with `ARTIFACT_PERMITS` slots, a burst of distinct
//! requests runs in `ceil(requests / permits)` waves. Resident memory is reported but is NOT a
//! concurrency signal: the whole-document read buffer stays in the allocator heap, so measured
//! peak RSS grows by roughly one source size per build even when only one build runs at a time
//! (measured: 1/3/12 permits gave 177/145/193 MiB at 1140/411/185 ms). Run with:
//! `cargo test -p docparse-server --test artifact_admission -- --ignored --nocapture --test-threads=1`

use docparse_server::storage::SharedStorage;
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

/// Concurrent artifact requests in one measurement.
const REQUESTS: usize = 12;
/// Canonical result size per request; the index path reads all of it.
const SOURCE_BYTES: usize = 16 * 1024 * 1024;

/// Reads the number of live threads in this process.
fn thread_count() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("status");
    let line = status
        .lines()
        .find(|line| line.starts_with("Threads:"))
        .expect("Threads line");
    line.split_whitespace()
        .nth(1)
        .expect("value")
        .parse()
        .expect("number")
}

/// Reads the resident set size of this process in bytes.
fn resident_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("status");
    let line = status
        .lines()
        .find(|line| line.starts_with("VmRSS:"))
        .expect("VmRSS line");
    let kilobytes: u64 = line
        .split_whitespace()
        .nth(1)
        .expect("value")
        .parse()
        .expect("number");
    kilobytes * 1024
}

/// Writes one canonical success envelope without holding the whole document in memory.
async fn write_canonical_result(path: &Path) {
    let mut file = tokio::fs::File::create(path).await.expect("create");
    file.write_all(
        br#"{"success":true,"data":{"context":{"page_count":1},"pages":[{"page_number":1,"text":""#,
    )
    .await
    .expect("prefix");
    let filler = vec![b'x'; 1024 * 1024];
    let mut written = 0;
    while written < SOURCE_BYTES {
        file.write_all(&filler).await.expect("filler");
        written += filler.len();
    }
    file.write_all(br#""}],"errors":[]}}"#)
        .await
        .expect("suffix");
    file.flush().await.expect("flush");
}

/// Reports wave scaling while every request builds a distinct artifact.
#[test]
#[ignore = "measurement probe; run with --ignored --nocapture"]
fn concurrent_artifact_builds_bound_peak_memory() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let directory = tempfile::tempdir().expect("directory");
        let storage = SharedStorage::new(directory.path()).await.expect("storage");
        let mut names = Vec::new();
        for index in 0..REQUESTS {
            let name = format!("bench-{index:02}.pdf");
            let path = storage.path(&name).expect("canonical path");
            write_canonical_result(&path).await;
            names.push(name);
        }

        let baseline = resident_bytes();
        let baseline_threads = thread_count();
        let peak = Arc::new(AtomicU64::new(baseline));
        let peak_threads = Arc::new(AtomicU64::new(baseline_threads));
        let stop = Arc::new(AtomicBool::new(false));
        let monitor = tokio::spawn({
            let peak = Arc::clone(&peak);
            let peak_threads = Arc::clone(&peak_threads);
            let stop = Arc::clone(&stop);
            async move {
                while !stop.load(Ordering::Relaxed) {
                    peak.fetch_max(resident_bytes(), Ordering::Relaxed);
                    peak_threads.fetch_max(thread_count(), Ordering::Relaxed);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
            }
        });

        let started = Instant::now();
        let mut builds = tokio::task::JoinSet::new();
        for name in names {
            let storage = storage.clone();
            builds.spawn(async move { storage.result_artifact(&name, None).await });
        }
        let mut failures = 0;
        while let Some(result) = builds.join_next().await {
            if result.expect("join").is_err() {
                failures += 1;
            }
        }
        let elapsed = started.elapsed();
        stop.store(true, Ordering::Relaxed);
        let _ = monitor.await;

        let peak = peak.load(Ordering::Relaxed);
        let peak_threads = peak_threads.load(Ordering::Relaxed);
        let permits = docparse_server::storage::ARTIFACT_PERMITS;
        let waves = REQUESTS.div_ceil(permits);
        println!(
            "MEASURE artifact: requests={REQUESTS} source={}MiB permits={permits} waves={waves} elapsed={elapsed:?} per_wave={:?} threads={baseline_threads}->{peak_threads} rss_baseline={}MiB rss_peak={}MiB rss_delta={}MiB failures={failures}",
            SOURCE_BYTES / (1024 * 1024),
            elapsed / u32::try_from(waves).expect("waves"),
            baseline / (1024 * 1024),
            peak / (1024 * 1024),
            peak.saturating_sub(baseline) / (1024 * 1024),
        );
    });
}
