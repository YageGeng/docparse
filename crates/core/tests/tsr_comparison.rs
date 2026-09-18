//! Real PDF experiments execute the production page/TSR path and retain inspectable failures.
use docparse_common::timing::{Timing, Timings};
use docparse_config::{
    ConfigLoader, ModelFiles, TableCellConfig, TableCellModel, TsrModel,
    ValidatedConfig,
};
use docparse_core::{
    DocParser, LocalPdfiumProvider, PageInput, ParseObserver, ParseOptions,
    PdfInput, PdfiumProvider, TableStructureEngine, TableStructureError,
    TsrGeometryPolicy, TsrTableInput, TsrTableRequest,
};
use docparse_layout::{PageImage, PageImageInput, PixelFormat};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};

/// Records immutable crop pixels and predictions around the real configured engine.
struct CaptureEngine {
    engine: Arc<docparse_tsr::PaddleTsrEngine>,
    output: PathBuf,
}

impl TableStructureEngine for CaptureEngine {
    /// Keeps model identity identical to the production engine.
    fn name(&self) -> &str {
        self.engine.name()
    }

    /// Runs the same detector matching and source calibration as normal parsing.
    fn geometry_policy(&self) -> TsrGeometryPolicy {
        TsrGeometryPolicy::Predicted
    }

    /// Captures diagnostics without replacing either inference or the production table assembler.
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
                Err(error) => json!({"error":error.to_string()}),
            };
            std::fs::write(self.output.join(format!("{stem}.json")), serde_json::to_vec_pretty(&json!({"engine":self.name(),"request_id":request.request_id,"page":request.page_number,"bbox":request.crop_bbox,"transform":request.crop_to_viewport,"prediction":value})).expect("JSON")).expect("capture");
            prediction
                .map(|prediction| TsrTableInput::from((&request, prediction)))
                .map_err(|error| TableStructureError::Engine {
                    message: error.to_string(),
                })
        })
    }
}

/// Owns per-page observations independently of deterministic result serialization.
#[derive(Default)]
struct Observations(Mutex<Vec<Timing>>);
impl ParseObserver for Observations {
    /// Page progress is printed by the experiment driver; this collector retains timings only.
    fn on_progress(&self, _progress: docparse_core::ParseProgress) {}
    /// Keeps all actual model and source-filling timings, including failed attempts.
    fn on_timing(&self, timing: Timing) {
        self.0.lock().expect("timings").push(timing);
    }
}

/// Applies the requested comparison variant while preserving unrelated parser limits.
fn configure_comparison_models(
    tsr: &mut docparse_config::TsrConfig,
    variant: &str,
    root: &Path,
) -> Result<(), String> {
    tsr.mode = docparse_config::TableMode::TsrOnly;
    tsr.cell_detection = None;
    // Every variant starts from the same pinned baseline, independently of the application's current model.
    tsr.model = TsrModel::SlanetPlus;
    let baseline = root.join("models/slanet-plus");
    tsr.model_path = baseline.join("inference.onnx");
    tsr.model_config_path = baseline.join("inference.yml");
    tsr.model_manifest_path = baseline.join("model-manifest.json");
    let selection = match variant {
        "baseline" => None,
        "cells-wired" => {
            Some((TsrModel::SlanetPlus, TableCellModel::Wired, "wired"))
        }
        "cells-wireless" => {
            Some((TsrModel::SlanetPlus, TableCellModel::Wireless, "wireless"))
        }
        "upgraded-wired" => {
            Some((TsrModel::SlanextWired, TableCellModel::Wired, "wired"))
        }
        "upgraded-wireless" => Some((
            TsrModel::SlanextWireless,
            TableCellModel::Wireless,
            "wireless",
        )),
        _ => return Err(format!("unknown experiment variant: {variant}")),
    };
    if let Some((model, cell_model, suffix)) = selection {
        tsr.model = model;
        if model != TsrModel::SlanetPlus {
            let directory = root.join(format!("models/slanext-{suffix}"));
            tsr.model_path = directory.join("inference.onnx");
            tsr.model_config_path = directory.join("inference.yml");
            tsr.model_manifest_path = directory.join("model-manifest.json");
        }
        let directory = root.join(format!("models/rtdetr-table-cell-{suffix}"));
        tsr.cell_detection = Some(
            TableCellConfig::builder()
                .queue_size(1)
                .model(cell_model)
                .score_threshold(0.3)
                .files(
                    ModelFiles::builder()
                        .model_path(directory.join("inference.onnx"))
                        .model_config_path(directory.join("inference.yml"))
                        .model_manifest_path(
                            directory.join("model-manifest.json"),
                        )
                        .build(),
                )
                .build(),
        );
    }
    Ok(())
}

/// Fixed real pages compare model changes through the same production parser and source validator.
#[tokio::test]
#[ignore = "requires TSR_COMPARE_VARIANT and real PDFs under TSR_COMPARE_PDFS"]
async fn real_pdf_table_model_comparison() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("root");
    let variant = std::env::var("TSR_COMPARE_VARIANT").expect("variant");
    let corpus = PathBuf::from(
        std::env::var("TSR_COMPARE_PDFS")
            .unwrap_or_else(|_| "/Volumes/Yage/Downloads/docs".to_owned()),
    );
    let output = root
        .join("packages/wasm-web/test-results/tsr-comparison")
        .join(&variant);
    std::fs::create_dir_all(&output).expect("output");
    let mut raw = ConfigLoader::new(root.join("docparse.toml"))
        .load_raw()
        .expect("config");
    configure_comparison_models(&mut raw.tsr, &variant, &root)
        .expect("comparison model selection");
    std::fs::write(
        output.join("config.json"),
        serde_json::to_vec_pretty(&json!({"layout": raw.layout, "tsr": raw.tsr, "render": raw.render, "runtime": raw.runtime, "ocr": raw.ocr, "fusion": raw.fusion})).expect("config JSON"),
    )
    .expect("config snapshot");
    let config =
        Arc::new(ValidatedConfig::try_from(raw).expect("validated config"));
    let loading = Instant::now();
    let engine = Arc::new(
        docparse_tsr::PaddleTsrEngine::from_config(Arc::clone(&config))
            .await
            .expect("models"),
    );
    let parser = DocParser::builder()
        .config(Arc::clone(&config))
        .table_engine(Arc::clone(&engine) as Arc<dyn TableStructureEngine>)
        .build()
        .await
        .expect("parser");
    let initialization_ms = loading.elapsed().as_secs_f64() * 1000.0;
    let raster = image::open(root.join("crates/tsr/tests/fixtures/table.png"))
        .expect("warmup table")
        .into_rgb8();
    let image = Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(raster.width())
                .height(raster.height())
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(raster.into_raw()))
                .build(),
        )
        .expect("warmup image"),
    );
    let warming = Instant::now();
    for _ in 0..2 {
        engine
            .predict(Arc::clone(&image), Timings::default())
            .await
            .expect("warmup");
    }
    let warmup_ms = warming.elapsed().as_secs_f64() * 1000.0;
    let cases: &[(&str, &[u32])] = &[
        ("2303.18223v16.pdf", &[8, 24, 33, 47, 57, 68, 82]),
        ("2603.01919v2.pdf", &[5, 14, 16, 17]),
        (
            "Terminal-Universe_ Turning Agent Trajectories into Scalable Terminal Environments.pdf",
            &[3, 32],
        ),
        (
            "SQLMorph_ Query Mutation and Fine-Grained Metrics for Text-to-SQL Evaluation.pdf",
            &[5, 10],
        ),
        ("2609.13141v1.pdf", &[6, 10]),
        ("2609.13146v1.pdf", &[6, 7]),
    ];
    let mut records = Vec::new();
    for (file_index, (file, pages)) in cases.iter().enumerate() {
        let session = LocalPdfiumProvider
            .open(
                PdfInput::Path(corpus.join(file)),
                config.runtime(),
                Timings::default(),
            )
            .await
            .expect("real PDF");
        for &page_number in *pages {
            let directory = output.join(format!("{file_index}-p{page_number}"));
            std::fs::create_dir_all(&directory).expect("case directory");
            let scan = session
                .pre_scan_page(page_number, None)
                .await
                .expect("source text");
            let rendered = session
                .render_page(page_number, config.render())
                .await
                .expect("page raster");
            image::save_buffer(
                directory.join("page.png"),
                rendered.image.data(),
                rendered.image.width(),
                rendered.image.height(),
                image::ColorType::Rgb8,
            )
            .expect("page image");
            let input = PageInput::builder()
                .extracted(scan.extracted)
                .image(rendered.image)
                .transform(rendered.transform)
                .build();
            let observer = Observations::default();
            let started = Instant::now();
            let result = parser
                .parse_page_with_options(
                    input,
                    ParseOptions::builder()
                        .observer(Some(&observer))
                        .table_engine(Some(Arc::new(CaptureEngine {
                            engine: Arc::clone(&engine),
                            output: directory.clone(),
                        })))
                        .build(),
                )
                .await
                .expect("production page parse");
            let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;
            let tables: Vec<_> = result
                .blocks
                .iter()
                .filter(|b| b.label == docparse_layout::LayoutLabel::Table)
                .collect();
            let record = json!({"file":file,"page":page_number,"directory":directory.file_name().and_then(|name| name.to_str()),"tables":tables.len(),"structured":tables.iter().filter(|b|b.table.is_some()).count(),"elapsed_ms":elapsed_ms,"warnings":result.warnings,"timings":*observer.0.lock().expect("timings")});
            eprintln!(
                "{variant}: {file} page {page_number}: {}/{} tables",
                record.get("structured").expect("structured count"),
                tables.len()
            );
            std::fs::write(
                directory.join("result.json"),
                serde_json::to_vec_pretty(&result).expect("result JSON"),
            )
            .expect("result");
            records.push(record);
            std::fs::write(output.join("report.json"), serde_json::to_vec_pretty(&json!({"variant":variant,"engine":engine.name(),"initialization_ms":initialization_ms,"warmup_ms":warmup_ms,"warmup_rounds":2,"pages":records})).expect("report JSON")).expect("report");
        }
        session.close().await.expect("close PDF");
    }
    assert_eq!(
        records.len(),
        cases.iter().map(|(_, pages)| pages.len()).sum::<usize>()
    );
}

/// Switching the application's default model cannot alter any named experiment's model identity.
#[test]
fn comparison_variants_replace_inherited_model_artifacts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (variant, model, directory, detector) in [
        ("baseline", TsrModel::SlanetPlus, "slanet-plus", None),
        (
            "cells-wired",
            TsrModel::SlanetPlus,
            "slanet-plus",
            Some("wired"),
        ),
        (
            "cells-wireless",
            TsrModel::SlanetPlus,
            "slanet-plus",
            Some("wireless"),
        ),
        (
            "upgraded-wired",
            TsrModel::SlanextWired,
            "slanext-wired",
            Some("wired"),
        ),
        (
            "upgraded-wireless",
            TsrModel::SlanextWireless,
            "slanext-wireless",
            Some("wireless"),
        ),
    ] {
        let mut tsr = docparse_config::TsrConfig::builder()
            .queue_size(1)
            .model(TsrModel::SlanextWireless)
            .model_path(PathBuf::from("inherited/model.onnx"))
            .model_config_path(PathBuf::from("inherited/model.yml"))
            .model_manifest_path(PathBuf::from("inherited/manifest.json"))
            .cell_detection(Some(
                TableCellConfig::builder()
                    .queue_size(1)
                    .model(TableCellModel::Wired)
                    .score_threshold(0.7)
                    .files(
                        ModelFiles::builder()
                            .model_path(PathBuf::from("inherited/cells.onnx"))
                            .model_config_path(PathBuf::from(
                                "inherited/cells.yml",
                            ))
                            .model_manifest_path(PathBuf::from(
                                "inherited/cells.json",
                            ))
                            .build(),
                    )
                    .build(),
            ))
            .mode(docparse_config::TableMode::Fallback)
            .timeout_ms(1234)
            .build();
        configure_comparison_models(&mut tsr, variant, &root).expect("variant");
        assert_eq!(tsr.model, model, "{variant}");
        let directory = root.join("models").join(directory);
        assert_eq!(
            tsr.model_path,
            directory.join("inference.onnx"),
            "{variant}"
        );
        assert_eq!(
            tsr.model_config_path,
            directory.join("inference.yml"),
            "{variant}"
        );
        assert_eq!(
            tsr.model_manifest_path,
            directory.join("model-manifest.json"),
            "{variant}"
        );
        assert_eq!(tsr.timeout_ms, 1234);
        assert_eq!(
            tsr.cell_detection
                .as_ref()
                .map(|cells| cells.files.model_path.clone()),
            detector.map(|suffix| root.join(format!(
                "models/rtdetr-table-cell-{suffix}/inference.onnx"
            )))
        );
        if let Some(cells) = &tsr.cell_detection {
            let suffix = detector.expect("detector variant");
            let directory =
                root.join(format!("models/rtdetr-table-cell-{suffix}"));
            assert_eq!(
                cells.files.model_config_path,
                directory.join("inference.yml")
            );
            assert_eq!(
                cells.files.model_manifest_path,
                directory.join("model-manifest.json")
            );
            assert_eq!(
                cells.model,
                if suffix == "wired" {
                    TableCellModel::Wired
                } else {
                    TableCellModel::Wireless
                }
            );
        }
    }
}
