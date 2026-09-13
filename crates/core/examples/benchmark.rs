//! Warm-model native parsing benchmark. Build with release and the selected accelerator features.
use docparse_config::{ConfigLoader, OcrPolicy, TableMode, ValidatedConfig};
use docparse_core::{
    DocParser, ParseObserver, ParseOptions, ParseProgress, Timing,
};
use docparse_layout::{
    PageImage, PageImageInput, PixelFormat, PpDocLayoutV3Engine,
    timing::Timings,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

/// Aggregates stage intervals without retaining PDF content or individual OCR-line events.
#[derive(Default)]
struct Observer(Mutex<BTreeMap<String, (u64, f64)>>);

impl ParseObserver for Observer {
    /// Emits coarse progress so a long document remains observable without timing every log call.
    fn on_progress(&self, progress: ParseProgress) {
        if let ParseProgress::Analyzing { completed, total } = progress
            && completed > 0
            && completed % 32 == 0
        {
            eprintln!("analyzed {completed}/{total} pages");
        }
    }
    /// Totals elapsed stage intervals; overlapping or nested stages must not be summed as wall time.
    fn on_timing(&self, timing: Timing) {
        let mut stages =
            self.0.lock().unwrap_or_else(|error| error.into_inner());
        let value = stages.entry(format!("{:?}", timing.stage)).or_default();
        value.0 += 1;
        value.1 += timing.duration_ms;
    }
}

/// Flushes machine-readable checkpoints immediately so a failed run still leaves usable evidence.
fn record(
    output: &mut BufWriter<File>,
    mut value: Value,
) -> Result<(), Box<dyn std::error::Error>> {
    value
        .as_object_mut()
        .ok_or("benchmark record must be an object")?
        .insert(
            "unix_seconds".into(),
            json!(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64()),
        );
    serde_json::to_writer(&mut *output, &value)?;
    writeln!(output)?;
    output.flush()?;
    Ok(())
}

/// Loads engines once, explicitly warms every enabled model, then measures every real PDF once in the same process.
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "warn,ort=error".into()),
        )
        .init();
    let mut args = std::env::args_os().skip(1);
    let config_path = PathBuf::from(
        args.next()
            .ok_or("usage: benchmark CONFIG PDF_DIRECTORY OUTPUT_JSONL")?,
    );
    let directory = PathBuf::from(args.next().ok_or("missing PDF directory")?);
    let output_path = PathBuf::from(args.next().ok_or("missing output JSONL")?);
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let raw = ConfigLoader::new(&config_path).load_raw()?;
    let config_json = serde_json::to_value(&raw)?;
    let config = Arc::new(ValidatedConfig::try_from(raw)?);
    let mut paths: Vec<_> = std::fs::read_dir(&directory)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<_, _>>()?;
    paths.retain(|path| {
        path.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
    });
    paths.sort();
    if paths.is_empty() {
        return Err("PDF directory contains no documents".into());
    }
    let mut output = BufWriter::new(File::create(output_path)?);
    record(
        &mut output,
        json!({"kind":"configuration", "config":config_json, "documents":paths.len()}),
    )?;
    let loading = Instant::now();
    let layout =
        Arc::new(PpDocLayoutV3Engine::from_config(Arc::clone(&config)).await?);
    let ocr = if config.ocr().policy != OcrPolicy::Disabled {
        Some(Arc::new(
            docparse_ocr::PaddleOcrEngine::from_config(Arc::clone(&config))
                .await?,
        ))
    } else {
        None
    };
    let tsr = if config.tsr().mode != TableMode::RulesOnly {
        Some(Arc::new(
            docparse_tsr::SlanetPlusEngine::from_config(Arc::clone(&config))
                .await?,
        ))
    } else {
        None
    };
    let mut builder = DocParser::builder()
        .config(Arc::clone(&config))
        .layout_engine(layout);
    if let Some(engine) = &ocr {
        builder =
            builder.ocr_engine(
                Arc::clone(engine) as Arc<dyn docparse_core::OcrEngine>
            );
    }
    if let Some(engine) = &tsr {
        builder = builder
            .table_engine(Arc::clone(engine)
                as Arc<dyn docparse_core::TableStructureEngine>);
    }
    let parser = builder.build().await?;
    record(
        &mut output,
        json!({"kind":"loaded", "seconds":loading.elapsed().as_secs_f64()}),
    )?;
    let warming = Instant::now();
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let image =
        image::open(workspace.join("crates/ocr/tests/fixtures/printed.png"))?
            .into_rgb8();
    let page = Arc::new(PageImage::try_from(
        PageImageInput::builder()
            .width(image.width())
            .height(image.height())
            .pixel_format(PixelFormat::Rgb8)
            .data(Arc::from(image.into_raw()))
            .build(),
    )?);
    for round in 1..=2 {
        // A native-text PDF may skip OCR/TSR entirely, so warm their actual sessions explicitly as well.
        if let Some(engine) = &ocr {
            let recognized = engine
                .recognize(Arc::clone(&page), Vec::new(), Timings::default())
                .await?;
            if recognized.is_empty() {
                return Err("OCR warmup returned no text".into());
            }
        }
        if let Some(engine) = &tsr {
            engine
                .predict(Arc::clone(&page), Timings::default())
                .await?;
        }
        let result = parser
            .parse_path(
                workspace
                    .join("crates/core/tests/fixtures/pdf/table_layout.pdf"),
            )
            .await?;
        if !result.errors.is_empty()
            || result.pages.iter().flat_map(|page| &page.warnings).any(
                |warning| {
                    matches!(
                        warning.code.as_str(),
                        "LayoutUnavailable"
                            | "OcrUnavailable"
                            | "OcrFailed"
                            | "TableExternalFailed"
                            | "TableExternalTimeout"
                    )
                },
            )
        {
            return Err("PDF warmup had page errors".into());
        }
        eprintln!("model warmup round {round} complete");
    }
    record(
        &mut output,
        json!({"kind":"warmed", "rounds":2, "seconds":warming.elapsed().as_secs_f64()}),
    )?;
    let mut total_pages = 0usize;
    let mut parse_seconds = 0.0;
    let mut failed = 0usize;
    let corpus = Instant::now();
    for path in paths {
        let name = path
            .file_name()
            .ok_or("missing basename")?
            .to_string_lossy()
            .into_owned();
        record(
            &mut output,
            json!({"kind":"document_start", "file":name, "bytes":std::fs::metadata(&path)?.len()}),
        )?;
        let observer = Observer::default();
        let started = Instant::now();
        let result = parser
            .parse_path_with_options(
                &path,
                ParseOptions::builder().observer(Some(&observer)).build(),
            )
            .await;
        let seconds = started.elapsed().as_secs_f64();
        parse_seconds += seconds;
        match result {
            Ok(document) => {
                let pages = document.pages.len();
                total_pages += pages;
                let mut warnings = BTreeMap::<String, usize>::new();
                for warning in
                    document.pages.iter().flat_map(|page| &page.warnings)
                {
                    *warnings.entry(warning.code.clone()).or_default() += 1;
                }
                let degraded = !document.errors.is_empty()
                    || warnings.keys().any(|code| {
                        matches!(
                            code.as_str(),
                            "LayoutUnavailable"
                                | "OcrUnavailable"
                                | "OcrFailed"
                                | "TableExternalFailed"
                                | "TableExternalTimeout"
                        )
                    });
                let inference_errors: Vec<_> = document.pages.iter().flat_map(|page| page.warnings.iter().map(move |warning| (page.page_number, warning)))
                    .filter(|(_, warning)| matches!(warning.code.as_str(), "LayoutUnavailable" | "OcrUnavailable" | "OcrFailed" | "TableExternalFailed" | "TableExternalTimeout"))
                    .take(5).map(|(page, warning)| json!({"page":page,"code":warning.code,"message":warning.message})).collect();
                record(
                    &mut output,
                    json!({"kind":"document", "file":name, "seconds":seconds,
                    "pages":pages, "pages_per_second":pages as f64 / seconds,
                    "page_errors":document.errors.len(), "degraded":degraded, "warnings":warnings, "inference_errors":inference_errors,
                    "stages":observer.0.into_inner().unwrap_or_else(|error| error.into_inner())}),
                )?;
                eprintln!(
                    "{name}: {pages} pages in {seconds:.3}s ({:.3} pages/s)",
                    pages as f64 / seconds
                );
                if degraded {
                    return Err("inference degraded; timings are not a valid accelerator benchmark".into());
                }
            }
            Err(error) => {
                failed += 1;
                record(
                    &mut output,
                    json!({"kind":"failed", "file":name, "seconds":seconds, "error":error.to_string()}),
                )?;
                eprintln!("{name}: parse failed: {error}");
            }
        }
    }
    record(
        &mut output,
        json!({"kind":"summary", "pages":total_pages, "failed_documents":failed,
        "parse_seconds":parse_seconds, "corpus_seconds":corpus.elapsed().as_secs_f64(),
        "pages_per_second":total_pages as f64 / parse_seconds}),
    )?;
    if failed > 0 {
        return Err("one or more benchmark documents failed".into());
    }
    Ok(())
}
