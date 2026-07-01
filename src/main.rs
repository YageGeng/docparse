use std::path::PathBuf;
use std::time::Instant;

use anyhow::{Context, Result, bail};
use docparse_layout::{LayoutDetector, LayoutOptions};
use tracing::{Level, event};

/// Runs the docparse command-line interface.
#[tokio::main]
async fn main() -> Result<()> {
    run_cli(std::env::args().skip(1).collect()).await
}

/// Dispatches top-level CLI commands.
async fn run_cli(args: Vec<String>) -> Result<()> {
    match args.as_slice() {
        [command, image] if command == "layout" => {
            run_layout(PathBuf::from(image), false).await
        }
        [command, flag, image]
            if command == "layout" && flag == "--profile" =>
        {
            init_profile_tracing();
            run_layout(PathBuf::from(image), true).await
        }
        [command, image, flag]
            if command == "layout" && flag == "--profile" =>
        {
            init_profile_tracing();
            run_layout(PathBuf::from(image), true).await
        }
        [command] if command == "layout" => {
            bail!("usage: docparse layout [--profile] <image>")
        }
        _ => bail!("usage: docparse layout [--profile] <image>"),
    }
}

/// Enables stderr tracing output for profile events.
fn init_profile_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("docparse=info,docparse_layout=info")
        .with_target(true)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Loads one image and prints PP-DocLayoutV3 layout detections as JSON.
async fn run_layout(image_path: PathBuf, profile: bool) -> Result<()> {
    let total = Instant::now();
    let started = Instant::now();
    let image = image::open(&image_path)
        .with_context(|| format!("failed to open {}", image_path.display()))?;
    event!(
        Level::INFO,
        phase = "cli.open_image",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );

    let started = Instant::now();
    let detector = LayoutDetector::new(LayoutOptions::default()).await?;
    event!(
        Level::INFO,
        phase = "cli.load_detector",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );

    let started = Instant::now();
    let page = detector.detect_image(&image)?;
    event!(
        Level::INFO,
        phase = "cli.detect_image",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        blocks = page.blocks.len()
    );

    let started = Instant::now();
    println!("{}", serde_json::to_string_pretty(&page)?);
    event!(
        Level::INFO,
        phase = "cli.write_json",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );
    if profile {
        event!(
            Level::INFO,
            phase = "cli.total",
            elapsed_ms = total.elapsed().as_secs_f64() * 1000.0
        );
    }
    Ok(())
}
