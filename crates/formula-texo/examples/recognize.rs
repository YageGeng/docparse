//! Reproducible native latency probe: MODEL_DIRECTORY REPEATS IMAGE...
use docparse_common::timing::Timings;
use docparse_config::{RawConfig, ValidatedConfig};
use docparse_formula::FormulaEngine;
use docparse_formula_texo::TexoEngine;
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use std::{collections::BTreeMap, path::PathBuf, sync::Arc, time::Instant};

/// Loads once, warms one batch, and reports warm batch latency separately from initialization.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let directory = PathBuf::from(
        args.next()
            .ok_or("usage: recognize MODEL_DIRECTORY REPEATS IMAGE...")?,
    );
    let repeats: usize = args.next().ok_or("missing repeat count")?.parse()?;
    if !(1..=1000).contains(&repeats) {
        return Err("repeat count must be 1..1000".into());
    }
    let mut images = Vec::new();
    for path in args {
        let image = image::open(path)?.to_rgb8();
        images.push(Arc::new(PageImage::try_from(
            PageImageInput::builder()
                .width(image.width())
                .height(image.height())
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(image.into_raw()))
                .build(),
        )?));
    }
    if !(1..=32).contains(&images.len()) {
        return Err("supply 1..32 formula images".into());
    }
    let mut config = RawConfig::default();
    config.formula.engine = docparse_config::FormulaEngineConfig::Texo(
        docparse_config::TexoFormulaConfig::builder()
            .encoder_path(directory.join("encoder_model.onnx"))
            .decoder_path(directory.join("decoder_model_merged.onnx"))
            .tokenizer_path(directory.join("tokenizer.json"))
            .build(),
    );
    let loading = Instant::now();
    let engine =
        TexoEngine::from_config(Arc::new(ValidatedConfig::try_from(config)?))
            .await?;
    let initialization_ms = loading.elapsed().as_secs_f64() * 1000.0;
    let expected = engine.recognize(images.clone(), Timings::default()).await?;
    let mut durations = Vec::with_capacity(repeats);
    let mut stages = BTreeMap::<String, f64>::new();
    for _ in 0..repeats {
        let (timings, mut observations) = Timings::channel();
        let started = Instant::now();
        let result = engine.recognize(images.clone(), timings).await?;
        durations.push(started.elapsed().as_secs_f64() * 1000.0);
        if result != expected {
            return Err("repeated recognition changed output".into());
        }
        while let Ok(timing) = observations.try_recv() {
            *stages.entry(format!("{:?}", timing.stage)).or_default() +=
                timing.duration_ms / repeats as f64;
        }
    }
    durations.sort_by(f64::total_cmp);
    let p50 = durations.get((repeats - 1) / 2).ok_or("missing median")?;
    let p95 = durations
        .get((repeats * 95).div_ceil(100) - 1)
        .ok_or("missing p95")?;
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "engine": engine.name(), "precision": "fp32", "batch_size": images.len(),
            "initialization_ms": initialization_ms, "warmup_batches": 1, "measured_batches": repeats,
            "batch_p50_ms": p50, "batch_p95_ms": p95,
            "mean_formulas_per_second": images.len() as f64 * repeats as f64 * 1000.0 / durations.iter().sum::<f64>(),
            "mean_stages_ms": stages, "latex": expected,
        }))?
    );
    Ok(())
}
