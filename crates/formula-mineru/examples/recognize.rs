//! Recognize formula crops using SERVER_URL CONCURRENCY IMAGE... without local model files.
use docparse_common::timing::Timings;
use docparse_config::{
    FormulaEngineConfig, MineruFormulaConfig, RawConfig, ValidatedConfig,
};
use docparse_formula::FormulaEngine;
use docparse_formula_mineru::MineruEngine;
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use std::sync::Arc;

/// Reads cropped formula images and prints ordered LaTeX results from the external service.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let server_url = args
        .next()
        .ok_or("usage: recognize SERVER_URL CONCURRENCY IMAGE...")?;
    let concurrency = args.next().ok_or("missing concurrency")?.parse()?;
    let mut raw = RawConfig::default();
    raw.formula.engine = FormulaEngineConfig::Mineru(MineruFormulaConfig {
        server_url,
        concurrency,
    });
    let engine = MineruEngine::try_from(&ValidatedConfig::try_from(raw)?)?;
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
    let latex = engine.recognize(images, Timings::default()).await?;
    println!("{}", serde_json::to_string_pretty(&latex)?);
    Ok(())
}
