//! Recognize formula crops using SERVER_URL WORKER_SIZE IMAGE... without local model files.
use docparse_common::timing::Timings;
use docparse_config::{
    FormulaEngineConfig, HttpFormulaConfig, RawConfig, ValidatedConfig,
};
use docparse_formula::FormulaEngine;
use docparse_formula_http::HttpEngine;
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use std::sync::Arc;

/// Reads cropped formula images and prints ordered LaTeX results from the external service.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let server_url = args
        .next()
        .ok_or("usage: recognize SERVER_URL WORKER_SIZE IMAGE...")?;
    let worker_size = args.next().ok_or("missing worker_size")?.parse()?;
    let mut raw = RawConfig::default();
    raw.formula.engine = vec![FormulaEngineConfig::Http(
        HttpFormulaConfig::builder()
            .server_url(server_url)
            .worker_size(worker_size)
            .prompt(std::env::var("FORMULA_PROMPT").ok())
            .model(
                std::env::var("FORMULA_MODEL")
                    .unwrap_or_else(|_| HttpFormulaConfig::default().model),
            )
            .build(),
    )];
    let engine = HttpEngine::try_from(&ValidatedConfig::try_from(raw)?)?;
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
