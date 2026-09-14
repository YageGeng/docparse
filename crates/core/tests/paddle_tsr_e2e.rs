//! Real-model acceptance driven by explicit local PDF paths, never a fixture provider.
use docparse_config::{ConfigLoader, TableMode, ValidatedConfig};
use docparse_core::{
    DocParser, ParseObserver, ParseProgress, TableStructureEngine,
    TableStructureError, TableStructureSource, Timing, TsrGeometryPolicy,
    TsrTableInput, TsrTableRequest,
};
use docparse_layout::timing::TimingStage;
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

/// Requires complete recovery by default and labels explicitly allowed omissions as partial.
fn table_coverage(
    total: usize,
    recovered: usize,
    allow_unresolved: bool,
) -> Result<&'static str, String> {
    match (total == recovered, allow_unresolved) {
        (true, _) => Ok("passed"),
        (false, true) => Ok("partial"),
        (false, false) => {
            Err(format!("only {recovered}/{total} tables recovered"))
        }
    }
}

/// The completeness gate is independent of the chosen table mode.
#[test]
fn incomplete_table_runs_cannot_report_success() {
    table_coverage(20, 19, false).expect_err("partial default run");
    assert_eq!(
        table_coverage(20, 19, true).expect("explicit allowance"),
        "partial"
    );
    assert_eq!(
        table_coverage(20, 20, false).expect("complete run"),
        "passed"
    );
}

/// Captures actual model crops and responses so failed postprocessing can be replayed independently.
struct RecordingEngine {
    engine: docparse_tsr::SlanetPlusEngine,
    output: PathBuf,
}
impl TableStructureEngine for RecordingEngine {
    /// Preserves the production engine identity in acceptance evidence.
    fn name(&self) -> &str {
        self.engine.name()
    }
    /// Uses the same learned geometry policy as the production model.
    fn geometry_policy(&self) -> TsrGeometryPolicy {
        self.engine.geometry_policy()
    }
    /// Saves owned pixels and model predictions before the real adapter validates them.
    fn recognize(
        &self,
        request: TsrTableRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<TsrTableInput, TableStructureError>,
    > {
        Box::pin(async move {
            let stem = request.block_id.as_str().replace(':', "-");
            image::save_buffer(
                self.output.join(format!("{stem}.png")),
                request.image.data(),
                request.image.width(),
                request.image.height(),
                image::ColorType::Rgb8,
            )
            .expect("crop");
            let prediction = self
                .engine
                .predict(Arc::clone(&request.image), request.timings.clone())
                .await;
            let value = match &prediction {
                Ok(p) => json!(p),
                Err(e) => json!({"error":e.to_string()}),
            };
            std::fs::write(self.output.join(format!("{stem}.json")), serde_json::to_vec(&json!({"engine":self.engine.name(),"request_id":request.request_id,"page":request.page_number,"bbox":request.crop_bbox,"transform":request.crop_to_viewport,"prediction":value})).expect("prediction JSON")).expect("prediction");
            Ok(TsrTableInput::from((
                &request,
                prediction.map_err(|e| TableStructureError::Engine {
                    message: e.to_string(),
                })?,
            )))
        })
    }
}

/// Captures serial progress and stage measurements from the real parser.
#[derive(Default)]
struct Observations(Mutex<Vec<Timing>>);

/// Refreshes real crop predictions without rerunning PDF extraction or layout.
#[tokio::test]
#[ignore = "requires TSR_CAPTURE_DIR and installed official model artifacts"]
#[allow(
    clippy::indexing_slicing,
    reason = "the capture format requires these model fields"
)]
async fn refresh_tsr_captured_predictions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let raw = ConfigLoader::new(root.join("docparse.toml"))
        .load_raw()
        .expect("config");
    let engine = docparse_tsr::SlanetPlusEngine::from_config(Arc::new(
        ValidatedConfig::try_from(raw).expect("config"),
    ))
    .await
    .expect("model");
    let directory =
        PathBuf::from(std::env::var("TSR_CAPTURE_DIR").expect("directory"))
            .join("crops");
    let mut failures = Vec::new();
    for entry in std::fs::read_dir(&directory).expect("directory") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let mut value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).expect("metadata"))
                .expect("JSON");
        if value.get("request_id").is_none() {
            continue;
        }
        let raster = image::open(path.with_extension("png"))
            .expect("crop")
            .into_rgb8();
        let input = docparse_layout::PageImage::try_from(
            docparse_layout::PageImageInput::builder()
                .width(raster.width())
                .height(raster.height())
                .pixel_format(docparse_layout::PixelFormat::Rgb8)
                .data(Arc::from(raster.into_raw()))
                .build(),
        )
        .expect("pixels");
        value["prediction"] = match engine
            .predict(
                Arc::new(input),
                docparse_layout::timing::Timings::default(),
            )
            .await
        {
            Ok(p) => json!(p),
            Err(e) => {
                failures.push(format!("{}: {e}", path.display()));
                json!({"error":e.to_string()})
            }
        };
        value["engine"] = json!(engine.name());
        eprintln!(
            "{}: {} structure boxes, {} detected cells",
            path.file_stem().expect("stem").to_string_lossy(),
            value["prediction"]["cell_bboxes"]
                .as_array()
                .map_or(0, Vec::len),
            value["prediction"]["detected_cell_bboxes"]
                .as_array()
                .map_or(0, Vec::len)
        );
        std::fs::write(
            &path,
            serde_json::to_vec(&value).expect("metadata JSON"),
        )
        .expect("metadata");
    }
    assert!(failures.is_empty(), "{failures:?}");
}
impl ParseObserver for Observations {
    /// Reports actual page completion without storing image buffers.
    fn on_progress(&self, progress: ParseProgress) {
        if let ParseProgress::Analyzing { completed, total } = progress
            && (completed % 20 == 0 || completed == total)
        {
            eprintln!("pages {completed}/{total}");
        }
    }
    /// Retains model inference evidence separately from canonical PDF results.
    fn on_timing(&self, timing: Timing) {
        self.0.lock().expect("timings").push(timing);
    }
}

/// Configured table modes must preserve real model provenance, with unresolved cases reported separately.
#[tokio::test]
#[ignore = "requires installed models and TSR_E2E_PDFS JSON paths"]
async fn real_pdfs_use_configured_table_model() {
    let paths: Vec<PathBuf> = serde_json::from_str(
        &std::env::var("TSR_E2E_PDFS").expect("TSR_E2E_PDFS JSON array"),
    )
    .expect("PDF paths");
    assert!(!paths.is_empty());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("root");
    let mut raw = ConfigLoader::new(root.join("docparse.toml"))
        .load_raw()
        .expect("production config");
    if let Ok(mode) = std::env::var("TSR_E2E_MODE") {
        raw.tsr.mode = serde_json::from_value(json!(mode)).expect("table mode");
    }
    let mode = raw.tsr.mode;
    // Keep native and browser acceptance artifacts under the renamed WASM package.
    let output =
        root.join(std::env::var("TSR_E2E_OUTPUT").unwrap_or_else(|_| {
            "packages/wasm-web/test-results/paddle-tsr/native".to_owned()
        }));
    std::fs::create_dir_all(&output).expect("output directory");
    let initialization = Instant::now();
    let config =
        Arc::new(ValidatedConfig::try_from(raw).expect("validated config"));
    let model_manifest: Option<docparse_layout::ModelManifest> =
        if mode == TableMode::RulesOnly {
            None
        } else {
            Some(
                serde_json::from_slice(
                    &std::fs::read(&config.tsr().model_manifest_path)
                        .expect("model manifest"),
                )
                .expect("model identity"),
            )
        };
    let mut builder = DocParser::builder().config(Arc::clone(&config));
    if std::env::var_os("TSR_E2E_CAPTURE").is_some() {
        let captures = output.join("crops");
        std::fs::create_dir_all(&captures).expect("capture directory");
        builder = builder.table_engine(Arc::new(RecordingEngine {
            engine: docparse_tsr::SlanetPlusEngine::from_config(Arc::clone(
                &config,
            ))
            .await
            .expect("real model"),
            output: captures,
        }));
    }
    let parser = builder.build().await.expect("real model initialization");
    let initialized_ms = initialization.elapsed().as_secs_f64() * 1000.0;
    let mut runs = Vec::new();
    let mut overall_status = "passed";
    let allow_unresolved =
        std::env::var_os("TSR_E2E_ALLOW_UNRESOLVED").is_some();
    for (index, path) in paths.iter().enumerate() {
        let observations = Observations::default();
        let bytes: Arc<[u8]> =
            Arc::from(std::fs::read(path).expect("actual PDF"));
        let started = Instant::now();
        let document = parser
            .parse_bytes_with_observer(bytes, &observations)
            .await
            .expect("complete production parse");
        let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
        assert!(document.errors.is_empty());
        if std::env::var_os("TSR_E2E_CAPTURE").is_some() {
            std::fs::write(
                output.join(format!("document-{index}-full.json")),
                serde_json::to_vec(&document).expect("document JSON"),
            )
            .expect("full document");
        }
        let tables = document
            .pages
            .iter()
            .flat_map(|p| &p.blocks)
            .filter(|b| b.label == docparse_layout::LayoutLabel::Table)
            .collect::<Vec<_>>();
        let structured = tables
            .iter()
            .filter_map(|b| b.table.as_ref())
            .collect::<Vec<_>>();
        let model_tables = structured
            .iter()
            .filter(|t| t.source == TableStructureSource::ExternalTsr)
            .count();
        let timings = observations.0.lock().expect("timings");
        let model_calls = timings
            .iter()
            .filter(|t| t.stage == TimingStage::TsrInference)
            .count();
        if mode == TableMode::TsrOnly {
            assert_eq!(
                model_tables,
                structured.len(),
                "TSR-only mode must not substitute local rules"
            );
            assert!(
                model_calls > 0 && model_calls >= model_tables,
                "real model inference must be attempted"
            );
        }
        let coverage =
            table_coverage(tables.len(), structured.len(), allow_unresolved);
        let status = coverage.as_deref().unwrap_or("failed");
        if mode == TableMode::RulesOnly {
            assert_eq!(model_calls, 0);
        }
        let record = json!({"status":status,"file":path.file_name().expect("name").to_string_lossy(), "pages":document.pages.len(), "tables":tables.len(),
            "text_tables":tables.iter().filter(|b| b.lines.iter().any(|l| l.text_items.iter().any(|i| !i.raw_text.trim().is_empty()))).count(),
            "structured":structured.len(), "model_tables":model_tables, "model_calls":model_calls,
            "table_requests":timings.iter().filter(|t|t.stage==TimingStage::TableExternal).count(), "elapsed_ms":elapsed_ms});
        let pages = document.pages.iter().map(|p| json!({"page":p.page_number,"warnings":p.warnings,
            "tables":p.blocks.iter().filter(|b| b.label == docparse_layout::LayoutLabel::Table).map(|b| json!({"id":b.id,"bbox":b.bbox,"table":b.table,"evidence":b.evidence})).collect::<Vec<_>>()
        })).collect::<Vec<_>>();
        std::fs::write(
            output.join(format!("document-{index}.json")),
            serde_json::to_vec(&json!({"pages":pages,"timings":*timings}))
                .expect("table report"),
        )
        .expect("write report");
        eprintln!("{record}");
        runs.push(record);
        std::fs::write(output.join("report.json"), serde_json::to_vec_pretty(&json!({"status":if coverage.is_err() {"failed"} else {"running"},"mode":mode,"model":model_manifest.as_ref().map(|manifest| manifest.repository.as_str()),"revision":model_manifest.as_ref().map(|manifest| manifest.revision.as_str()),"initialization_ms":initialized_ms,"runs":runs})).expect("report")).expect("write report");
        if coverage.expect("table recovery acceptance failed") == "partial" {
            overall_status = "partial";
        }
    }
    std::fs::write(output.join("report.json"), serde_json::to_vec_pretty(&json!({"status":overall_status,"mode":mode,"model":model_manifest.as_ref().map(|manifest| manifest.repository.as_str()),"revision":model_manifest.as_ref().map(|manifest| manifest.revision.as_str()),"initialization_ms":initialized_ms,"runs":runs})).expect("report")).expect("write report");
}

/// All default models must initialize from owned bytes even when every configured path is absent.
#[tokio::test]
#[ignore = "requires installed pinned layout and TSR artifacts"]
async fn explicit_parser_artifacts_never_load_configured_paths() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let layout = docparse_layout::ModelArtifacts::from_paths(
        &root.join("models/pp-doclayout-v3/inference.onnx"),
        &root.join("models/pp-doclayout-v3/inference.yml"),
        &root.join("models/pp-doclayout-v3/model-manifest.json"),
    )
    .expect("layout bytes");
    let tsr = docparse_layout::ModelArtifacts::from_paths(
        &root.join("models/slanet-plus/inference.onnx"),
        &root.join("models/slanet-plus/inference.yml"),
        &root.join("models/slanet-plus/model-manifest.json"),
    )
    .expect("TSR bytes");
    let cells = docparse_layout::ModelArtifacts::from_paths(
        &root.join("models/rtdetr-table-cell-wireless/inference.onnx"),
        &root.join("models/rtdetr-table-cell-wireless/inference.yml"),
        &root.join("models/rtdetr-table-cell-wireless/model-manifest.json"),
    )
    .expect("cell bytes");
    let absent = tempfile::tempdir().expect("empty model directory");
    let mut raw = docparse_config::RawConfig::default();
    let cell_files = &mut raw
        .tsr
        .cell_detection
        .as_mut()
        .expect("default cells")
        .files;
    for path in [
        &mut cell_files.model_path,
        &mut cell_files.model_config_path,
        &mut cell_files.model_manifest_path,
    ] {
        *path = absent.path().join("missing-artifact");
    }
    for path in [
        &mut raw.layout.model_path,
        &mut raw.layout.model_config_path,
        &mut raw.layout.model_manifest_path,
        &mut raw.tsr.model_path,
        &mut raw.tsr.model_config_path,
        &mut raw.tsr.model_manifest_path,
    ] {
        *path = absent.path().join("missing-artifact");
    }
    let parser = DocParser::from_artifacts(
        ValidatedConfig::try_from(raw).expect("config"),
        docparse_core::ParserArtifacts {
            layout,
            tsr: Some(docparse_tsr::TsrArtifacts {
                structure: tsr,
                cell_detection: Some(cells),
            }),
            ocr: None,
        },
    )
    .await
    .expect("byte-only construction");
    let bytes = std::fs::read(
        root.join("crates/core/tests/fixtures/pdf/extraction_metadata.pdf"),
    )
    .expect("real PDF");
    let document = parser
        .parse_bytes(Arc::from(bytes))
        .await
        .expect("real inference");
    assert!(document.errors.is_empty());
    assert_eq!(document.pages.len(), 1);
}
