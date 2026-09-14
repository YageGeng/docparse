//! Warm-model native parsing benchmark. Build with release and the selected accelerator features.
use docparse_config::{
    ConfigLoader, OcrPolicy, OutputConfig, TableMode, ValidatedConfig,
};
use docparse_core::{
    DocParser, JsonRenderer, ParseObserver, ParseOptions, ParseProgress, Timing,
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
struct Observer(Mutex<BTreeMap<String, (u64, f64, f64)>>);

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
        value.2 = value.2.max(timing.duration_ms);
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

/// Measures one document and compact JSON persistence, returning metrics instead of retaining its canonical data.
async fn measure(
    parser: Arc<DocParser>,
    path: PathBuf,
    output: OutputConfig,
    scratch: PathBuf,
) -> Value {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned();
    let observer = Observer::default();
    let started = Instant::now();
    let result = parser
        .parse_path_with_options(
            &path,
            ParseOptions::builder().observer(Some(&observer)).build(),
        )
        .await;
    let parse_seconds = started.elapsed().as_secs_f64();
    let document = match result {
        Ok(document) => document,
        Err(error) => {
            return json!({"kind":"failed", "file":name, "seconds":parse_seconds, "error":error.to_string()});
        }
    };
    let pages = document.pages.len();
    let mut warnings = BTreeMap::<String, usize>::new();
    for warning in document.pages.iter().flat_map(|page| &page.warnings) {
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
    let errors: Vec<_> = document.pages.iter().flat_map(|page| page.warnings.iter().map(move |warning| (page.page_number, warning)))
        .filter(|(_, warning)| matches!(warning.code.as_str(), "LayoutUnavailable" | "OcrUnavailable" | "OcrFailed" | "TableExternalFailed" | "TableExternalTimeout"))
        .take(5).map(|(page, warning)| json!({"page":page,"code":warning.code,"message":warning.message})).collect();
    let page_errors = document.errors.len();
    let writing = Instant::now();
    // Match the server's blocking compact serializer and buffered writes, using the report filesystem rather than a RAM-backed sink.
    let written =
        tokio::task::spawn_blocking(move || -> Result<_, std::io::Error> {
            let queued_seconds = writing.elapsed().as_secs_f64();
            let serializing = Instant::now();
            let file = tempfile::tempfile_in(scratch)?;
            let mut writer = BufWriter::new(file);
            serde_json::to_writer(
                &mut writer,
                &JsonRenderer::view_with_config(&document, &output),
            )
            .map_err(std::io::Error::other)?;
            writer.flush()?;
            let serialize_seconds = serializing.elapsed().as_secs_f64();
            let syncing = Instant::now();
            writer.get_ref().sync_all()?;
            Ok((
                queued_seconds,
                serialize_seconds,
                syncing.elapsed().as_secs_f64(),
                writer.get_ref().metadata()?.len(),
            ))
        })
        .await;
    let (write_queue_seconds, serialize_seconds, sync_seconds, json_bytes) =
        match written {
            Ok(Ok(value)) => value,
            other => {
                return json!({"kind":"failed", "file":name, "seconds":started.elapsed().as_secs_f64(), "error":format!("result persistence failed: {other:?}")});
            }
        };
    let seconds = started.elapsed().as_secs_f64();
    eprintln!(
        "{name}: {pages} pages, parse {parse_seconds:.3}s, JSON {serialize_seconds:.3}s, sync {sync_seconds:.3}s, total {seconds:.3}s, degraded={degraded}"
    );
    json!({"kind":"document", "file":name, "pages":pages, "seconds":seconds, "parse_seconds":parse_seconds,
        "write_queue_seconds":write_queue_seconds, "serialize_seconds":serialize_seconds, "sync_seconds":sync_seconds, "json_bytes":json_bytes,
        "page_errors":page_errors, "degraded":degraded, "warnings":warnings, "inference_errors":errors,
        "stages":observer.0.into_inner().unwrap_or_else(|error| error.into_inner())})
}

/// Loads and warms shared engines once, then measures the whole corpus at a bounded document concurrency.
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
            .ok_or("usage: benchmark CONFIG PDF_DIRECTORY OUTPUT_JSONL [CONCURRENCY] [REPEATS]")?,
    );
    let directory = PathBuf::from(args.next().ok_or("missing PDF directory")?);
    let output_path = PathBuf::from(args.next().ok_or("missing output JSONL")?);
    let concurrency = args
        .next()
        .map(|value| value.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(1);
    let repeats = args
        .next()
        .map(|value| value.to_string_lossy().parse::<usize>())
        .transpose()?
        .unwrap_or(1);
    if !(1..=32).contains(&concurrency) || !(1..=10).contains(&repeats) {
        return Err("concurrency must be 1..32 and repeats 1..10".into());
    }
    if args.next().is_some() {
        return Err("unexpected extra argument".into());
    }
    let raw = ConfigLoader::new(&config_path).load_raw()?;
    // Only parser settings belong in benchmark artifacts; database credentials must never be serialized here.
    let config_json = json!({"layout":raw.layout,"tsr":raw.tsr,"ocr":raw.ocr,"runtime":raw.runtime,"render":raw.render,"fusion":raw.fusion,"output":raw.output});
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
    let scratch = output_path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut output = BufWriter::new(File::create(&output_path)?);
    record(
        &mut output,
        json!({"kind":"configuration", "config":config_json, "documents":paths.len(), "concurrency":concurrency, "repeats":repeats,
            "backend":docparse_layout::wasm_compat::OnnxBackend::from(config.as_ref()).execution_provider().to_string(), "stage_values":["count","total_ms","max_ms"]}),
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
    let parser = Arc::new(builder.build().await?);
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
    let mut invalid = false;
    for round in 1..=repeats {
        let mut total_pages = 0_u64;
        let mut document_seconds = 0.0;
        let mut failed = 0_usize;
        let mut degraded = 0_usize;
        let corpus = Instant::now();
        let mut pending = paths.iter();
        let mut tasks = tokio::task::JoinSet::new();
        record(
            &mut output,
            json!({"kind":"round_start", "round":round, "concurrency":concurrency}),
        )?;
        loop {
            while tasks.len() < concurrency {
                let Some(path) = pending.next() else {
                    break;
                };
                record(
                    &mut output,
                    json!({"kind":"document_start", "round":round, "file":path.file_name().map(|name| name.to_string_lossy()), "bytes":std::fs::metadata(path)?.len()}),
                )?;
                tasks.spawn(measure(
                    Arc::clone(&parser),
                    path.clone(),
                    config.output().clone(),
                    scratch.clone(),
                ));
            }
            let Some(result) = tasks.join_next().await else {
                break;
            };
            let mut value = result?;
            total_pages +=
                value.get("pages").and_then(Value::as_u64).unwrap_or(0);
            document_seconds +=
                value.get("seconds").and_then(Value::as_f64).unwrap_or(0.0);
            failed += usize::from(
                value.get("kind").and_then(Value::as_str) == Some("failed"),
            );
            degraded += usize::from(
                value.get("degraded").and_then(Value::as_bool) == Some(true),
            );
            value
                .as_object_mut()
                .ok_or("invalid document metric")?
                .insert("round".into(), json!(round));
            record(&mut output, value)?;
        }
        let corpus_seconds = corpus.elapsed().as_secs_f64();
        record(
            &mut output,
            json!({"kind":"summary", "round":round, "concurrency":concurrency,
            "pages":total_pages, "failed_documents":failed, "degraded_documents":degraded,
            "valid":failed == 0 && degraded == 0, "document_seconds":document_seconds,
            "corpus_seconds":corpus_seconds, "pages_per_second":total_pages as f64 / corpus_seconds}),
        )?;
        eprintln!(
            "round {round}, concurrency {concurrency}: {total_pages} pages in {corpus_seconds:.3}s; failed={failed}, degraded={degraded}"
        );
        invalid |= failed > 0 || degraded > 0;
    }
    if invalid {
        return Err("benchmark contains failed or degraded documents; do not treat their throughput as valid inference performance".into());
    }
    Ok(())
}
