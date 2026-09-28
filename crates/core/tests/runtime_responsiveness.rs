//! Measures how long a CPU-heavy parser phase keeps a single async executor thread busy.
//!
//! A phase that misses its blocking hop shows up as a large `max_executor_gap`: while the
//! parser runs it inline, the canary task on the same single-worker runtime cannot be polled.
//! These are measurement probes rather than assertions, so they are ignored by default and
//! print one `MEASURE` line per run. Run them with:
//! `cargo test -p docparse-core --test runtime_responsiveness -- --ignored --nocapture --test-threads=1`
//!
//! `/tmp/bench-heavy.pdf` (20 pages x 120 translucent lines) is an adversarial document
//! generated with reportlab; without it the probe falls back to `table_layout.pdf`.
//!
//! The engines and page fixtures below mirror `table_external.rs` through the same public
//! constructors; keep both in sync, or move them to `tests/common/` if more probes share them.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use docparse_config::{RawConfig, TableMode, ValidatedConfig};
use docparse_core::{
    Baseline, DocParser, ExtractedPage, PageInput, ParseObserver, ParseOptions,
    ParseProgress, TableEvidence, TableOptions, TableStructureEngine,
    TableStructureError, TableWord, TextItem, TextItemId, TextSource,
    TextStyle, Timing, TsrTableInput, TsrTableRequest,
};
use docparse_layout::{
    AffineTransform, Bbox, GeometrySource, LayoutDetection, LayoutEngine,
    LayoutError, LayoutRequest, PageImage, PageImageInput, PageRotation,
    PageTransform, PageTransformInput, PixelFormat, Point,
};

/// One page split into `tables` horizontal bands, each reported as a table region.
struct WholeTables {
    tables: usize,
}

impl LayoutEngine for WholeTables {
    /// Identifies this measurement layout provider.
    fn name(&self) -> &str {
        "measurement-table"
    }

    /// Keeps the measurement context deterministic.
    fn model_revision(&self) -> &str {
        "1"
    }

    /// Reports every band as a table so the external structure path runs per band.
    fn detect(
        &self,
        request: LayoutRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    > {
        let tables = self.tables;
        Box::pin(async move {
            let (width, height) = request.transform.viewport_size();
            let band = height / tables as f64;
            Ok((0..tables)
                .map(|index| {
                    LayoutDetection::builder()
                        .source_detection_index(index as u32)
                        .raw_label("table".to_owned())
                        .class_id(21)
                        .label(docparse_layout::LayoutLabel::Table)
                        .confidence(0.99)
                        .bbox(
                            Bbox::try_from([
                                0.0,
                                index as f64 * band,
                                width,
                                (index + 1) as f64 * band,
                            ])
                            .expect("region bounds"),
                        )
                        .geometry_source(GeometrySource::DerivedFromBbox)
                        .model_order(index as i64)
                        .metadata(BTreeMap::new())
                        .build()
                })
                .collect())
        })
    }
}

/// Answers every table request with a complete grid of the requested shape.
struct GridEngine {
    rows: usize,
    columns: usize,
}

impl TableStructureEngine for GridEngine {
    /// Identifies this measurement structure provider.
    fn name(&self) -> &str {
        "measurement-grid"
    }

    /// Exercises the predicted-topology binding path.
    fn geometry_policy(&self) -> docparse_core::TsrGeometryPolicy {
        docparse_core::TsrGeometryPolicy::Predicted
    }

    /// Returns one full grid so binding walks every cell.
    fn recognize(
        &self,
        request: TsrTableRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<TsrTableInput, TableStructureError>,
    > {
        let rows = self.rows;
        let columns = self.columns;
        Box::pin(async move {
            let width = f64::from(request.image.width());
            let height = f64::from(request.image.height());
            let mut tokens = vec!["<table>".to_owned()];
            let mut boxes = Vec::new();
            for row in 0..rows {
                tokens.push("<tr>".to_owned());
                for column in 0..columns {
                    tokens.push("<td></td>".to_owned());
                    boxes.push(vec![
                        column as f64 * width / columns as f64,
                        row as f64 * height / rows as f64,
                        (column + 1) as f64 * width / columns as f64,
                        (row + 1) as f64 * height / rows as f64,
                    ]);
                }
                tokens.push("</tr>".to_owned());
            }
            tokens.push("</table>".to_owned());
            Ok(TsrTableInput::builder()
                .request_id(request.request_id)
                .structure_tokens(tokens)
                .cell_bboxes(boxes)
                .build())
        })
    }
}

/// Returns no layout regions so only the document-wide phases dominate.
struct NoLayout;

impl LayoutEngine for NoLayout {
    /// Identifies this measurement layout provider.
    fn name(&self) -> &str {
        "measurement-empty"
    }

    /// Keeps the measurement context deterministic.
    fn model_revision(&self) -> &str {
        "1"
    }

    /// Reports no regions so the parser uses its native fallback.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    > {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

/// Records the longest duration observed for each parse stage.
#[derive(Default)]
struct StageStats(Mutex<BTreeMap<String, f64>>);

impl ParseObserver for StageStats {
    /// Ignores progress for this measurement.
    fn on_progress(&self, _progress: ParseProgress) {}

    /// Keeps the slowest interval per stage.
    fn on_timing(&self, timing: Timing) {
        let mut stats = self.0.lock().expect("stage stats");
        let entry = stats.entry(format!("{:?}", timing.stage)).or_insert(0.0);
        *entry = entry.max(timing.duration_ms);
    }
}

impl StageStats {
    /// Reports the slowest stages in descending order.
    fn summary(&self) -> String {
        let stats = self.0.lock().expect("stage stats");
        let mut ranked: Vec<_> = stats.iter().collect();
        ranked.sort_by(|left, right| right.1.total_cmp(left.1));
        ranked
            .into_iter()
            .take(6)
            .map(|(stage, milliseconds)| format!("{stage}={milliseconds:.3}ms"))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Builds validated configuration with models disabled and table rules local.
fn config() -> ValidatedConfig {
    let mut raw = RawConfig::default();
    raw.formula.inline_enabled = false;
    raw.formula.display_enabled = false;
    raw.tsr.mode = TableMode::RulesOnly;
    raw.layout.model_path = PathBuf::from("/tmp/runtime-responsiveness.onnx");
    raw.layout.model_config_path =
        PathBuf::from("/tmp/runtime-responsiveness.yml");
    raw.layout.model_manifest_path =
        PathBuf::from("/tmp/runtime-responsiveness.json");
    ValidatedConfig::try_from(raw).expect("measurement config")
}

/// Builds one page with `tables` bands, each a `rows x columns` measured text table.
fn table_page(tables: usize, rows: usize, columns: usize) -> PageInput {
    let columns_px = 40usize * columns;
    let band_px = 20usize * rows;
    let height_px = band_px * tables;
    let width = columns_px as f64;
    let band_height = band_px as f64;
    let height = height_px as f64;
    let mut items = Vec::new();
    let mut evidence = TableEvidence::default();
    for table in 0..tables {
        for row in 0..rows {
            for column in 0..columns {
                let index = ((table * rows + row) * columns + column) as u32;
                let x = column as f64 * 40.0 + 2.0;
                let y = table as f64 * band_height + row as f64 * 20.0 + 2.0;
                let right = x + 34.0;
                let bbox =
                    Bbox::try_from([x, y, right, y + 10.0]).expect("item bbox");
                items.push(
                    TextItem::builder()
                        .id(TextItemId::native(1, index))
                        .raw_text(format!("c{index}"))
                        .bbox(bbox)
                        .baseline(Some(Baseline {
                            start: Point::new(x, y + 8.0),
                            end: Point::new(right, y + 8.0),
                        }))
                        .source(TextSource::Native)
                        .extraction_order(index)
                        .style(Some(
                            TextStyle::builder()
                                .font_size(Some(10.0))
                                .bold(true)
                                .build(),
                        ))
                        .build(),
                );
                evidence.words.insert(
                    TextItemId::native(1, index),
                    vec![
                        TableWord::builder()
                            .byte_range(0..2)
                            .bbox(bbox)
                            .build(),
                    ],
                );
            }
        }
    }
    PageInput::builder()
        .extracted(
            ExtractedPage::builder()
                .page_number(1)
                .width(width)
                .height(height)
                .rotation(0)
                .text_items(items)
                .table_evidence(evidence)
                .build(),
        )
        .image(Arc::new(
            PageImage::try_from(
                PageImageInput::builder()
                    .width(u32::try_from(columns_px).expect("page width"))
                    .height(u32::try_from(height_px).expect("page height"))
                    .pixel_format(PixelFormat::Rgb8)
                    .data(Arc::from(vec![255u8; columns_px * height_px * 3]))
                    .build(),
            )
            .expect("page image"),
        ))
        .transform(
            PageTransform::try_from(
                PageTransformInput::builder()
                    .page_to_viewport(AffineTransform::identity())
                    .viewport_width(width)
                    .viewport_height(height)
                    .render_width(
                        u32::try_from(columns_px).expect("render width"),
                    )
                    .render_height(
                        u32::try_from(height_px).expect("render height"),
                    )
                    .model_width(800)
                    .model_height(800)
                    .rotation(PageRotation::Degrees0)
                    .build(),
            )
            .expect("page transform"),
        )
        .build()
}

/// Prefers the generated adversarial document and falls back to the repository fixture.
fn document_path() -> PathBuf {
    let generated = PathBuf::from("/tmp/bench-heavy.pdf");
    if generated.is_file() {
        return generated;
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pdf/table_layout.pdf")
}

/// Canary shared state measuring executor availability.
struct Canary {
    polls: Arc<AtomicU64>,
    max_gap: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
}

impl Canary {
    /// Starts a yield loop on the runtime being measured.
    fn start() -> Self {
        let polls = Arc::new(AtomicU64::new(0));
        let max_gap = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        tokio::spawn({
            let polls = Arc::clone(&polls);
            let max_gap = Arc::clone(&max_gap);
            let stop = Arc::clone(&stop);
            async move {
                let mut last = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    polls.fetch_add(1, Ordering::Relaxed);
                    let now = Instant::now();
                    let gap = now.duration_since(last).as_micros() as u64;
                    max_gap.fetch_max(gap, Ordering::Relaxed);
                    last = now;
                    tokio::task::yield_now().await;
                }
            }
        });
        Self {
            polls,
            max_gap,
            stop,
        }
    }

    /// Clears counters after the canary has settled, just before the measured window.
    fn reset(&self) {
        self.polls.store(0, Ordering::Relaxed);
        self.max_gap.store(0, Ordering::Relaxed);
    }

    /// Stops the canary loop.
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Reports one measured run.
    fn report(
        &self,
        label: &str,
        run: usize,
        runs: usize,
        work: Duration,
        extra: &str,
    ) {
        println!(
            "MEASURE {label} run={run}/{runs}: work={work:?} canary_polls={} max_executor_gap={:?} {extra}",
            self.polls.load(Ordering::Relaxed),
            Duration::from_micros(self.max_gap.load(Ordering::Relaxed)),
        );
    }
}

/// Runs everything on one thread so the canary and the parser share one executor thread.
///
/// `multi_thread` would not work here: `block_on` drives its future on the calling thread
/// while `tokio::spawn` uses a worker, so an inline phase would never delay the canary.
fn single_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("single-thread runtime")
}

/// Settles the canary, then clears its counters.
async fn settle(canary: &Canary) {
    tokio::time::sleep(Duration::from_millis(30)).await;
    canary.reset();
}

/// External table topology: crop copying and grid binding per band.
#[test]
#[ignore = "measurement probe; run with --ignored --nocapture"]
fn table_topology_keeps_the_executor_available() {
    const TABLES: usize = 4;
    const ROWS: usize = 40;
    const COLUMNS: usize = 20;
    const RUNS: usize = 3;

    single_thread_runtime().block_on(async {
        let parser = Arc::new(
            DocParser::builder()
                .config(Arc::new(config()))
                .layout_engine(Arc::new(WholeTables { tables: TABLES }))
                .build()
                .await
                .expect("parser"),
        );
        let engine: Arc<dyn TableStructureEngine> = Arc::new(GridEngine {
            rows: ROWS,
            columns: COLUMNS,
        });
        for run in 1..=RUNS {
            let canary = Canary::start();
            settle(&canary).await;
            let started = Instant::now();
            let parsing = tokio::spawn({
                let parser = Arc::clone(&parser);
                let engine = Arc::clone(&engine);
                async move {
                    parser
                        .parse_page_with_options(
                            table_page(TABLES, ROWS, COLUMNS),
                            ParseOptions::builder()
                                .table(
                                    TableOptions::builder()
                                        .mode(TableMode::TsrOnly)
                                        .build(),
                                )
                                .table_engine(Some(engine))
                                .build(),
                        )
                        .await
                }
            });
            parsing.await.expect("join").expect("page parse");
            let work = started.elapsed();
            canary.report(
                "external-table",
                run,
                RUNS,
                work,
                &format!("tables={TABLES} rows={ROWS} columns={COLUMNS}"),
            );
            canary.stop();
        }
    });
}

/// Document-wide phases: watermark classification, context building, linking and validation.
#[test]
#[ignore = "measurement probe; run with --ignored --nocapture"]
fn document_phases_keep_the_executor_available() {
    const RUNS: usize = 3;

    single_thread_runtime().block_on(async {
        let parser = Arc::new(
            DocParser::builder()
                .config(Arc::new(config()))
                .layout_engine(Arc::new(NoLayout))
                .build()
                .await
                .expect("parser"),
        );
        let path = document_path();
        let bytes: Arc<[u8]> =
            std::fs::read(&path).expect("fixture bytes").into();
        let observer = Arc::new(StageStats::default());
        for run in 1..=RUNS {
            let canary = Canary::start();
            settle(&canary).await;
            let started = Instant::now();
            let parsing = tokio::spawn({
                let parser = Arc::clone(&parser);
                let bytes = Arc::clone(&bytes);
                let observer = Arc::clone(&observer);
                async move {
                    parser.parse_bytes_with_observer(bytes, &*observer).await
                }
            });
            let result = parsing.await.expect("join").expect("document parse");
            let work = started.elapsed();
            canary.report(
                "document-phases",
                run,
                RUNS,
                work,
                &format!(
                    "document={} pages={} slowest: {}",
                    path.display(),
                    result.pages.len(),
                    observer.summary()
                ),
            );
            canary.stop();
        }
    });
}

/// Same document work with no canary, to separate hop cost from canary CPU competition.
#[test]
#[ignore = "measurement probe; run with --ignored --nocapture"]
fn document_phase_cost_without_canary() {
    single_thread_runtime().block_on(async {
        let parser = Arc::new(
            DocParser::builder()
                .config(Arc::new(config()))
                .layout_engine(Arc::new(NoLayout))
                .build()
                .await
                .expect("parser"),
        );
        let path = document_path();
        let bytes: Arc<[u8]> =
            std::fs::read(&path).expect("fixture bytes").into();
        let observer = StageStats::default();
        for run in 1..=3 {
            let started = Instant::now();
            let result = parser
                .parse_bytes_with_observer(Arc::clone(&bytes), &observer)
                .await
                .expect("document parse");
            println!(
                "MEASURE document-cost run={run}/3: work={:?} pages={} slowest: {}",
                started.elapsed(),
                result.pages.len(),
                observer.summary()
            );
        }
    });
}
