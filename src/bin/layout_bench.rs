use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use docparse_layout::{LayoutDetector, LayoutOptions};
use serde_json::json;

/// Runs the layout benchmark command.
#[tokio::main]
async fn main() -> Result<()> {
    run_benchmark(std::env::args().skip(1).collect()).await
}

/// Parses benchmark arguments and executes the detector loop.
async fn run_benchmark(args: Vec<String>) -> Result<()> {
    let options = BenchOptions::parse(args)?;
    if options.profile {
        init_profile_tracing();
    }
    let opened = Instant::now();
    let image = image::open(&options.image_path).with_context(|| {
        format!("failed to open {}", options.image_path.display())
    })?;
    let open_ms = elapsed_ms(opened);

    let loaded = Instant::now();
    let detector = LayoutDetector::new(LayoutOptions::default()).await?;
    let load_ms = elapsed_ms(loaded);

    let started = Instant::now();
    let first_page = detector.detect_image(&image)?;
    let first_run_ms = elapsed_ms(started);

    for _ in 0..options.warmup {
        let _page = detector.detect_image(&image)?;
    }

    let mut timings = Vec::with_capacity(options.runs);
    for _ in 0..options.runs {
        let started = Instant::now();
        let _page = detector.detect_image(&image)?;
        timings.push(elapsed_ms(started));
    }

    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "backend": backend_name(),
            "model": "pp-doclayout-v3",
            "image": options.image_path,
            "runs": options.runs,
            "warmup": options.warmup,
            "open_image_ms": open_ms,
            "load_detector_ms": load_ms,
            "first_run_ms": first_run_ms,
            "detect_ms": summarize(&timings),
            "blocks": first_page.blocks.len(),
        }))?
    );

    Ok(())
}

/// Enables stderr tracing output for model profile events.
fn init_profile_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("docparse_layout=info")
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Returns the fixed native backend name.
fn backend_name() -> &'static str {
    "burn-wgpu"
}

/// Converts an [`Instant`] start into elapsed milliseconds.
fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Computes simple latency statistics from measured milliseconds.
fn summarize(values: &[f64]) -> serde_json::Value {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    json!({
        "min": sorted[0],
        "p50": percentile(&sorted, 0.50),
        "p90": percentile(&sorted, 0.90),
        "max": sorted[sorted.len() - 1],
        "mean": sorted.iter().sum::<f64>() / sorted.len() as f64,
    })
}

/// Returns a nearest-rank percentile from sorted milliseconds.
fn percentile(sorted: &[f64], ratio: f64) -> f64 {
    let index = ((sorted.len() - 1) as f64 * ratio).round() as usize;
    sorted[index.min(sorted.len() - 1)]
}

/// Options for one layout benchmark run.
struct BenchOptions {
    image_path: PathBuf,
    runs: usize,
    warmup: usize,
    profile: bool,
}

impl BenchOptions {
    /// Parses `layout_bench` command-line arguments.
    fn parse(args: Vec<String>) -> Result<Self> {
        let mut image_path = None;
        let mut runs = 50usize;
        let mut warmup = 10usize;
        let mut profile = false;
        let mut index = 0usize;
        while index < args.len() {
            match args[index].as_str() {
                "--image" => {
                    index += 1;
                    image_path = args.get(index).map(PathBuf::from);
                }
                "--runs" => {
                    index += 1;
                    runs = args
                        .get(index)
                        .context("missing --runs value")?
                        .parse()
                        .context("invalid --runs value")?;
                }
                "--warmup" => {
                    index += 1;
                    warmup = args
                        .get(index)
                        .context("missing --warmup value")?
                        .parse()
                        .context("invalid --warmup value")?;
                }
                "--profile" => {
                    profile = true;
                }
                _ => bail!(
                    "usage: layout_bench --image <path> [--runs N] [--warmup N] [--profile]"
                ),
            }
            index += 1;
        }
        let image_path = image_path.context(
            "usage: layout_bench --image <path> [--runs N] [--warmup N] [--profile]",
        )?;
        if runs == 0 {
            bail!("--runs must be greater than zero");
        }

        Ok(Self {
            image_path,
            runs,
            warmup,
            profile,
        })
    }
}
