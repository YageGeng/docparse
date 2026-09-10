//! External structures exercise the public parser with native facts and a controlled provider.
use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

use docparse_config::{RawConfig, ValidatedConfig};
use docparse_core::{
    Baseline, DocParser, ExtractedPage, PageInput, ParseOptions, TableEvidence,
    TableMode, TableOptions, TableRule, TableStructureEngine,
    TableStructureError, TableStructureSource, TableWord, TextItem, TextItemId,
    TextSource, TextStyle, TsrRequestReason, TsrTableInput, TsrTableRequest,
};
use docparse_layout::{
    AffineTransform, Bbox, GeometrySource, LayoutDetection, LayoutEngine,
    LayoutError, LayoutLabel, LayoutRequest, PageImage, PageImageInput,
    PageRotation, PageTransform, PageTransformInput, PixelFormat, Point,
};

/// The page's known table region; this provider performs no row/column reconstruction.
struct WholeTable;
impl LayoutEngine for WholeTable {
    /// Identifies this native integration-test layout provider.
    fn name(&self) -> &str {
        "whole-table-test"
    }
    /// Keeps test context deterministic.
    fn model_revision(&self) -> &str {
        "1"
    }
    /// Gives the real parser one table region in canonical viewport coordinates.
    fn detect(
        &self,
        request: LayoutRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    > {
        Box::pin(async move {
            let (w, h) = request.transform.viewport_size();
            Ok(vec![
                LayoutDetection::builder()
                    .source_detection_index(0)
                    .raw_label("table".to_owned())
                    .class_id(21)
                    .label(LayoutLabel::Table)
                    .confidence(0.99)
                    .bbox(
                        Bbox::try_from([0.0, 0.0, w, h]).expect("page bounds"),
                    )
                    .geometry_source(GeometrySource::DerivedFromBbox)
                    .model_order(0)
                    .metadata(BTreeMap::new())
                    .build(),
            ])
        })
    }
}

/// Two valid table regions whose partial overlap would become containment after raster rounding.
struct PartialTables;
impl LayoutEngine for PartialTables {
    /// Identifies the deterministic layout boundary regression.
    fn name(&self) -> &str {
        "partial-tables"
    }

    /// Keeps table geometry stable independently of external structure policy.
    fn model_revision(&self) -> &str {
        "1"
    }

    /// Leaves the narrow table protruding 0.7 points beyond the wider table.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    > {
        Box::pin(async {
            Ok([[0.2, 0.2, 150.1, 90.2], [130.0, 20.0, 150.8, 35.0]]
                .into_iter()
                .enumerate()
                .map(|(index, bounds)| {
                    LayoutDetection::builder()
                        .source_detection_index(index as u32)
                        .raw_label("table".to_owned())
                        .class_id(21)
                        .label(LayoutLabel::Table)
                        .confidence(0.99)
                        .bbox(Bbox::try_from(bounds).expect("table region"))
                        .geometry_source(GeometrySource::DerivedFromBbox)
                        .model_order(index as i64)
                        .metadata(BTreeMap::new())
                        .build()
                })
                .collect())
        })
    }
}

/// Controlled external outcomes cover policy and rollback independently of any network service.
enum Reply {
    Grid(usize, usize),
    Invalid,
    Failure,
    Pending(Arc<AtomicBool>),
}
struct Engine {
    calls: AtomicUsize,
    reply: Reply,
    requests: Mutex<Vec<TsrTableRequest>>,
}
impl Engine {
    /// Records each request while supplying the selected deterministic response mode.
    fn new(reply: Reply) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            reply,
            requests: Mutex::new(Vec::new()),
        }
    }
}
/// Confirms dropping a pending provider future releases its owned resources.
struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    /// Marks release when an external future completes or is canceled.
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl TableStructureEngine for Engine {
    /// Labels controlled topology input rather than claiming model inference.
    fn name(&self) -> &str {
        "controlled-structure-input"
    }
    /// Returns a valid, rejected, failed, or cancelable pending structure response.
    fn recognize(
        &self,
        request: TsrTableRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<TsrTableInput, TableStructureError>,
    > {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests
            .lock()
            .expect("requests")
            .push(request.clone());
        Box::pin(async move {
            match &self.reply {
                Reply::Failure => Err(TableStructureError::Engine {
                    message: "test failure".to_owned(),
                }),
                Reply::Pending(dropped) => {
                    let _guard = Dropped(Arc::clone(dropped));
                    std::future::pending().await
                }
                Reply::Invalid => Ok(TsrTableInput::builder()
                    .request_id("stale".to_owned())
                    .structure_tokens(Vec::new())
                    .cell_bboxes(Vec::new())
                    .build()),
                Reply::Grid(rows, columns) => {
                    let mut tokens = vec!["<table>".to_owned()];
                    let mut boxes = Vec::new();
                    for r in 0..*rows {
                        tokens.push("<tr>".to_owned());
                        for c in 0..*columns {
                            tokens.push("<td></td>".to_owned());
                            boxes.push(vec![
                                c as f64 * f64::from(request.image.width())
                                    / *columns as f64,
                                r as f64 * f64::from(request.image.height())
                                    / *rows as f64,
                                (c + 1) as f64
                                    * f64::from(request.image.width())
                                    / *columns as f64,
                                (r + 1) as f64
                                    * f64::from(request.image.height())
                                    / *rows as f64,
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
                }
            }
        })
    }
}

/// Builds test inputs through the same public constructors used by native callers.
struct Fixture;
impl Fixture {
    /// Creates a parser with real extraction/fusion and a fixed page-local table region.
    async fn parser() -> DocParser {
        DocParser::builder()
            .config(Arc::new(
                ValidatedConfig::try_from(RawConfig::default())
                    .expect("config"),
            ))
            .layout_engine(Arc::new(WholeTable))
            .build()
            .await
            .expect("parser")
    }

    /// Produces either a locally recoverable ruled table or one indivisible row needing external structure.
    fn page(local: bool) -> PageInput {
        let mut items = Vec::new();
        let mut evidence = TableEvidence::default();
        for (index, text) in if local {
            vec!["A", "B", "1", "2"]
        } else {
            vec!["left right"]
        }
        .into_iter()
        .enumerate()
        {
            let x = 10.0 + (index % 2) as f64 * 100.0;
            let y = 10.0 + (index / 2) as f64 * 50.0;
            let right = if local { x + 5.0 } else { 180.0 };
            items.push(
                TextItem::builder()
                    .id(TextItemId::native(1, index as u32))
                    .raw_text(text.to_owned())
                    .bbox(
                        Bbox::try_from([x, y, right, y + 10.0]).expect("item"),
                    )
                    .baseline(Some(Baseline {
                        start: Point::new(x, y + 8.0),
                        end: Point::new(right, y + 8.0),
                    }))
                    .source(TextSource::Native)
                    .extraction_order(index as u32)
                    .style(Some(
                        TextStyle::builder()
                            .font_size(Some(10.0))
                            .bold(true)
                            .build(),
                    ))
                    .build(),
            );
        }
        if local {
            evidence.rules.extend([0.0, 40.0, 100.0].map(|y| {
                TableRule::Horizontal {
                    y,
                    left: 0.0,
                    right: 200.0,
                }
            }));
            evidence.rules.extend([0.0, 100.0, 200.0].map(|x| {
                TableRule::Vertical {
                    x,
                    top: 0.0,
                    bottom: 100.0,
                }
            }));
        } else {
            evidence.words.insert(
                TextItemId::native(1, 0),
                vec![
                    TableWord::builder()
                        .byte_range(0..4)
                        .bbox(
                            Bbox::try_from([10.0, 10.0, 45.0, 20.0])
                                .expect("left"),
                        )
                        .build(),
                    TableWord::builder()
                        .byte_range(5..10)
                        .bbox(
                            Bbox::try_from([110.0, 10.0, 155.0, 20.0])
                                .expect("right"),
                        )
                        .build(),
                ],
            );
        }
        PageInput::builder()
            .extracted(
                ExtractedPage::builder()
                    .page_number(1)
                    .width(200.0)
                    .height(100.0)
                    .rotation(0)
                    .text_items(items)
                    .table_evidence(evidence)
                    .build(),
            )
            .image(Arc::new(
                PageImage::try_from(
                    PageImageInput::builder()
                        .width(200)
                        .height(100)
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(vec![255; 60000]))
                        .build(),
                )
                .expect("image"),
            ))
            .transform(
                PageTransform::try_from(
                    PageTransformInput::builder()
                        .page_to_viewport(AffineTransform::identity())
                        .viewport_width(200.0)
                        .viewport_height(100.0)
                        .render_width(200)
                        .render_height(100)
                        .model_width(800)
                        .model_height(800)
                        .rotation(PageRotation::Degrees0)
                        .build(),
                )
                .expect("transform"),
            )
            .build()
    }
}

/// Fallback is driven by full reconstruction failure; successful local tables do not call the provider.
#[tokio::test]
async fn fallback_only_requests_unresolved_tables() {
    let parser = Fixture::parser().await;
    for (local, expected) in [(true, 0), (false, 1)] {
        let engine = Arc::new(Engine::new(Reply::Grid(1, 2)));
        let page = parser
            .parse_page_with_options(
                Fixture::page(local),
                ParseOptions::builder()
                    .table(
                        TableOptions::builder()
                            .mode(TableMode::Fallback)
                            .build(),
                    )
                    .table_engine(Some(
                        Arc::clone(&engine) as Arc<dyn TableStructureEngine>
                    ))
                    .build(),
            )
            .await
            .expect("parse");
        assert_eq!(engine.calls.load(Ordering::SeqCst), expected);
        let table = page
            .blocks
            .iter()
            .find_map(|b| b.table.as_ref())
            .expect("table");
        if !local {
            assert_eq!(table.source, TableStructureSource::ExternalTsr);
            assert_eq!(
                table
                    .cells
                    .iter()
                    .map(|c| c.text.as_str())
                    .collect::<Vec<_>>(),
                ["left", "right"]
            );
            assert!(table.cells.iter().all(|c| !c.is_header));
            assert!(
                !page
                    .warnings
                    .iter()
                    .any(|w| w.code == "TableStructureUnavailable")
            );
            assert!(matches!(
                engine
                    .requests
                    .lock()
                    .expect("requests")
                    .first()
                    .expect("request")
                    .reason,
                TsrRequestReason::RulesFailed { .. }
            ));
        }
    }
}

/// External-only mode preserves declared data cells even when local evidence looks like a bold header.
#[tokio::test]
async fn external_only_preserves_provider_topology_and_local_only_skips_it() {
    let parser = Fixture::parser().await;
    for mode in [TableMode::RulesOnly, TableMode::ExternalOnly] {
        let engine = Arc::new(Engine::new(Reply::Grid(4, 2)));
        let page = parser
            .parse_page_with_options(
                Fixture::page(true),
                ParseOptions::builder()
                    .table(TableOptions::builder().mode(mode).build())
                    .table_engine(Some(
                        Arc::clone(&engine) as Arc<dyn TableStructureEngine>
                    ))
                    .build(),
            )
            .await
            .expect("parse");
        assert_eq!(
            engine.calls.load(Ordering::SeqCst),
            usize::from(mode == TableMode::ExternalOnly)
        );
        let table = page
            .blocks
            .iter()
            .find_map(|b| b.table.as_ref())
            .expect("table");
        if mode == TableMode::ExternalOnly {
            assert_eq!(table.source, TableStructureSource::ExternalTsr);
            assert!(table.cells.iter().all(|c| !c.is_header));
            // The original layout crop contains a blank bottom row beyond the source text enclosure.
            assert_eq!((table.row_count, table.column_count), (4, 2));
            assert_eq!(
                table
                    .cells
                    .iter()
                    .filter(|c| c.row == 3 && c.text.is_empty())
                    .count(),
                2
            );
        }
    }
}

/// Failed external attempts preserve source text, do not run a local fallback, and release timed-out futures.
#[tokio::test]
async fn external_failures_are_transactional() {
    let parser = Fixture::parser().await;
    let dropped = Arc::new(AtomicBool::new(false));
    for (reply, code) in [
        (Reply::Invalid, "InvalidTsrInput"),
        (Reply::Failure, "TableExternalFailed"),
        // A legal grid can still cut a source word across a row boundary.
        (Reply::Grid(3, 2), "TableTextAssignmentFailed"),
        (Reply::Pending(Arc::clone(&dropped)), "TableExternalTimeout"),
    ] {
        let engine = Arc::new(Engine::new(reply));
        let input = Fixture::page(true);
        let original = input.extracted.text_items.clone();
        let page = parser
            .parse_page_with_options(
                input,
                ParseOptions::builder()
                    .table(
                        TableOptions::builder()
                            .mode(TableMode::ExternalOnly)
                            .timeout_ms(10)
                            .build(),
                    )
                    .table_engine(Some(
                        Arc::clone(&engine) as Arc<dyn TableStructureEngine>
                    ))
                    .build(),
            )
            .await
            .expect("degraded page");
        assert!(page.blocks.iter().all(|b| b.table.is_none()));
        assert!(page.warnings.iter().any(|w| w.code == code));
        let mut texts = page
            .iter_text_items()
            .map(|i| (i.id.clone(), i.raw_text.clone()))
            .collect::<Vec<_>>();
        texts.sort();
        let mut expected = original
            .into_iter()
            .map(|i| (i.id, i.raw_text))
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(texts, expected);
    }
    assert!(dropped.load(Ordering::SeqCst));
}

/// Missing providers are rejected before any PDF loading or inference can occur.
#[tokio::test]
async fn external_mode_requires_a_provider() {
    let parser = Fixture::parser().await;
    let result = parser
        .parse_bytes_with_options(
            Arc::from(Vec::<u8>::new()),
            ParseOptions::builder()
                .table(
                    TableOptions::builder()
                        .mode(TableMode::ExternalOnly)
                        .build(),
                )
                .build(),
        )
        .await;
    assert!(matches!(
        result,
        Err(docparse_core::DocParseError::TableStructure(
            TableStructureError::InvalidOptions { .. }
        ))
    ));
}

/// Counts live futures across pages sharing the same invocation budget.
#[derive(Default)]
struct ConcurrentEngine {
    active: AtomicUsize,
    maximum: AtomicUsize,
    calls: AtomicUsize,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    /// Releases the active counter on completion or timeout.
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
impl TableStructureEngine for ConcurrentEngine {
    /// Names the controlled concurrency probe.
    fn name(&self) -> &str {
        "concurrency-probe"
    }
    /// Keeps requests alive long enough for independent pages to contend for the shared permits.
    fn recognize(
        &self,
        request: TsrTableRequest,
    ) -> docparse_core::WasmBoxedFuture<
        '_,
        Result<TsrTableInput, TableStructureError>,
    > {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            let _active = Active(&self.active);
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
            Ok(TsrTableInput::builder()
                .request_id(request.request_id)
                .structure_tokens(
                    ["<tr>", "<td></td>", "</tr>"]
                        .into_iter()
                        .map(str::to_owned)
                        .collect(),
                )
                .cell_bboxes(vec![vec![
                    0.0,
                    0.0,
                    f64::from(request.image.width()),
                    f64::from(request.image.height()),
                ]])
                .build())
        })
    }
}

/// One document's tables share one limit even when several pages execute concurrently.
#[tokio::test]
async fn external_budget_is_shared_across_pages() {
    let mut raw = RawConfig::default();
    raw.runtime.page_concurrency = 4;
    raw.render.max_long_edge_pixels = 800;
    let parser = DocParser::builder()
        .config(Arc::new(ValidatedConfig::try_from(raw).expect("config")))
        .layout_engine(Arc::new(WholeTable))
        .build()
        .await
        .expect("parser");
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pdf/multipage_layout.pdf"),
    )
    .expect("fixture");
    for limit in [1, 2] {
        let engine = Arc::new(ConcurrentEngine::default());
        let result = parser
            .parse_bytes_with_options(
                Arc::from(bytes.clone()),
                ParseOptions::builder()
                    .table(
                        TableOptions::builder()
                            .mode(TableMode::ExternalOnly)
                            .max_in_flight(limit)
                            .build(),
                    )
                    .table_engine(Some(
                        Arc::clone(&engine) as Arc<dyn TableStructureEngine>
                    ))
                    .build(),
            )
            .await
            .expect("document");
        assert!(result.pages.len() >= 2);
        assert_eq!(engine.calls.load(Ordering::SeqCst), result.pages.len());
        assert!(engine.maximum.load(Ordering::SeqCst) <= limit);
        assert_eq!(engine.active.load(Ordering::SeqCst), 0);
        assert!(
            result
                .pages
                .iter()
                .flat_map(|p| &p.blocks)
                .filter(|b| b.label == LayoutLabel::Table)
                .all(|b| b.table.as_ref().is_some_and(
                    |t| t.source == TableStructureSource::ExternalTsr
                ))
        );
    }
}

/// Crop rounding cannot alter page ownership, including at low or nonuniform raster scales and rotated origins.
#[tokio::test]
async fn external_crop_rounding_preserves_layout_boundaries() {
    let parser = DocParser::builder()
        .config(Arc::new(
            ValidatedConfig::try_from(RawConfig::default()).expect("config"),
        ))
        .layout_engine(Arc::new(PartialTables))
        .build()
        .await
        .expect("parser");
    for (width, height) in [(200, 100), (50, 25), (401, 201)] {
        for (rotation, degrees, coefficients) in [
            (
                PageRotation::Degrees0,
                0,
                [1.0, 0.0, 0.0, -1.0, -30.0, 120.0],
            ),
            (
                PageRotation::Degrees90,
                90,
                [0.0, 1.0, 1.0, 0.0, -20.0, -30.0],
            ),
            (
                PageRotation::Degrees180,
                180,
                [-1.0, 0.0, 0.0, 1.0, 230.0, -20.0],
            ),
            (
                PageRotation::Degrees270,
                270,
                [0.0, -1.0, -1.0, 0.0, 220.0, 130.0],
            ),
        ] {
            let mut input = Fixture::page(true);
            input.extracted.rotation = degrees;
            input.extracted.text_items.push(
                TextItem::builder()
                    .id(TextItemId::native(1, 10))
                    .raw_text("X".to_owned())
                    .bbox(
                        Bbox::try_from([140.0, 25.0, 150.8, 30.0])
                            .expect("text"),
                    )
                    .source(TextSource::Native)
                    .extraction_order(10)
                    .build(),
            );
            input.image = Arc::new(
                PageImage::try_from(
                    PageImageInput::builder()
                        .width(width)
                        .height(height)
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(vec![
                            255;
                            (width * height * 3) as usize
                        ]))
                        .build(),
                )
                .expect("raster"),
            );
            let [a, b, c, d, e, f] = coefficients;
            input.transform = PageTransform::try_from(
                PageTransformInput::builder()
                    .page_to_viewport(
                        AffineTransform::builder()
                            .a(a)
                            .b(b)
                            .c(c)
                            .d(d)
                            .e(e)
                            .f(f)
                            .build(),
                    )
                    .viewport_width(200.0)
                    .viewport_height(100.0)
                    .render_width(width)
                    .render_height(height)
                    .model_width(800)
                    .model_height(800)
                    .rotation(rotation)
                    .build(),
            )
            .expect("transform");
            let baseline = parser
                .parse_page(input.clone())
                .await
                .expect("valid original layout");
            assert_eq!(baseline.blocks.len(), 2);
            for mode in [TableMode::Fallback, TableMode::ExternalOnly] {
                let page = parser
                    .parse_page_with_options(
                        input.clone(),
                        ParseOptions::builder()
                            .table(TableOptions::builder().mode(mode).build())
                            .table_engine(Some(Arc::new(Engine::new(
                                Reply::Grid(1, 1),
                            ))
                                as Arc<dyn TableStructureEngine>))
                            .build(),
                    )
                    .await
                    .expect("external input must preserve page validity");
                assert_eq!(page.blocks.len(), 2);
                for block in &page.blocks {
                    let original = baseline
                        .blocks
                        .iter()
                        .find(|b| b.id == block.id)
                        .expect("original owner");
                    assert_eq!(
                        block.bbox, original.bbox,
                        "{width}x{height}, rotation {degrees}, {mode:?}"
                    );
                    assert_eq!(block.lines, original.lines);
                    assert_eq!(block.polygon, original.polygon);
                    assert_eq!(block.source_regions, original.source_regions);
                    let table = block.table.as_ref().expect("resolved table");
                    if table.source == TableStructureSource::ExternalTsr {
                        assert_eq!(
                            (table.row_count, table.column_count),
                            (1, 1)
                        );
                        assert_eq!(
                            table.cells.first().expect("cell").bbox,
                            Some(original.bbox)
                        );
                    }
                }
                assert!(
                    !page
                        .warnings
                        .iter()
                        .any(|w| w.code == "TableStructureUnavailable")
                );
            }
        }
    }
}
