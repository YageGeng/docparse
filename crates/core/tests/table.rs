use std::collections::BTreeMap;
use std::sync::Arc;

use docparse_config::{RawConfig, ValidatedConfig};
use docparse_core::{
    Baseline, DocParser, ExtractedPage, PageInput, TextItem, TextItemId,
    TextSource, TextStyle,
};
use docparse_layout::{
    AffineTransform, Bbox, GeometrySource, LayoutDetection, LayoutEngine,
    LayoutError, LayoutLabel, LayoutRequest, PageImage, PageImageInput,
    PageRotation, PageTransform, PageTransformInput, PixelFormat, Point,
};

/// Supplies only the already-known table region; the real Rust pipeline reconstructs its cells.
struct TableLayout;

impl LayoutEngine for TableLayout {
    /// Identifies the deterministic region provider used by this native integration test.
    fn name(&self) -> &str {
        "table-integration"
    }

    /// Returns the fixed test revision without loading a model.
    fn model_revision(&self) -> &str {
        "table-integration-v1"
    }

    /// Marks the input region as a table without predicting any rows or columns.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> docparse_layout::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    > {
        Box::pin(async {
            Ok(vec![
                LayoutDetection::builder()
                    .source_detection_index(0)
                    .raw_label("table".to_owned())
                    .class_id(21)
                    .label(LayoutLabel::Table)
                    .confidence(0.99)
                    .bbox(
                        Bbox::try_from([10.0, 10.0, 290.0, 130.0])
                            .expect("table bbox"),
                    )
                    .geometry_source(GeometrySource::DerivedFromBbox)
                    .model_order(0)
                    .metadata(BTreeMap::new())
                    .build(),
            ])
        })
    }
}

impl TableLayout {
    /// Builds one native source item with an explicit baseline and consistent test font metrics.
    fn item(index: u32, text: &str, x: f64, y: f64, bold: bool) -> TextItem {
        let right = x + text.chars().count() as f64 * 5.0;
        TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text(text.to_owned())
            .bbox(Bbox::try_from([x, y, right, y + 10.0]).expect("item bbox"))
            .baseline(Some(Baseline {
                start: Point::new(x, y + 8.0),
                end: Point::new(right, y + 8.0),
            }))
            .source(TextSource::Native)
            .style(Some(
                TextStyle::builder()
                    .font_size(Some(10.0))
                    .bold(bold)
                    .build(),
            ))
            .build()
    }

    /// Runs the public parser with native/OCR-like source facts and the real table composition pipeline.
    async fn parse(
        items: Vec<TextItem>,
        evidence: docparse_core::TableEvidence,
    ) -> docparse_core::PageResult {
        let parser = DocParser::builder()
            .config(Arc::new(
                ValidatedConfig::try_from(RawConfig::default())
                    .expect("config"),
            ))
            .layout_engine(Arc::new(TableLayout))
            .build()
            .await
            .expect("parser");
        let image = Arc::new(
            PageImage::try_from(
                PageImageInput::builder()
                    .width(2)
                    .height(2)
                    .pixel_format(PixelFormat::Rgb8)
                    .data(Arc::<[u8]>::from(vec![255; 12]))
                    .build(),
            )
            .expect("image"),
        );
        let transform = PageTransform::try_from(
            PageTransformInput::builder()
                .page_to_viewport(AffineTransform::identity())
                .viewport_width(300.0)
                .viewport_height(150.0)
                .render_width(2)
                .render_height(2)
                .model_width(800)
                .model_height(800)
                .rotation(PageRotation::Degrees0)
                .build(),
        )
        .expect("transform");
        parser
            .parse_page(
                PageInput::builder()
                    .extracted(
                        ExtractedPage::builder()
                            .page_number(1)
                            .width(300.0)
                            .height(150.0)
                            .rotation(0)
                            .text_items(items)
                            .table_evidence(evidence)
                            .build(),
                    )
                    .image(image)
                    .transform(transform)
                    .build(),
            )
            .await
            .expect("page")
    }

    /// Supplies a three-column grid with independent row separators.
    fn rules() -> docparse_core::TableEvidence {
        let mut evidence = docparse_core::TableEvidence::default();
        evidence.rules.extend([10.0, 48.0, 78.0, 110.0].map(|y| {
            docparse_core::TableRule::Horizontal {
                y,
                left: 10.0,
                right: 290.0,
            }
        }));
        evidence.rules.extend([10.0, 100.0, 195.0, 290.0].map(|x| {
            docparse_core::TableRule::Vertical {
                x,
                top: 10.0,
                bottom: 110.0,
            }
        }));
        evidence
    }
}

/// Borderless cells must follow row/column geometry while source facts remain unique.
#[tokio::test]
async fn borderless_table_reconstructs_cells_without_flattening_columns() {
    let values = [
        ["Metric", "Control", "Treatment"],
        ["Count", "10", "20"],
        ["Score", "5.2", "6.4"],
    ];
    let mut items = Vec::new();
    for (row, columns) in values.iter().enumerate() {
        for (column, text) in columns.iter().enumerate() {
            items.push(TableLayout::item(
                (row * 3 + column) as u32,
                text,
                20.0 + column as f64 * 95.0,
                30.0 + row as f64 * 28.0,
                row == 0,
            ));
        }
    }
    let source_items = items.clone();
    let page = TableLayout::parse(items, Default::default()).await;
    let block = page.blocks.first().expect("table block");
    let table = block.table.as_ref().expect("structured table");
    assert_eq!((table.row_count, table.column_count), (3, 3));
    assert_eq!(
        block.text,
        "Metric\tControl\tTreatment\nCount\t10\t20\nScore\t5.2\t6.4"
    );
    assert_eq!(
        table.to_markdown(),
        "| Metric | Control | Treatment |\n| --- | --- | --- |\n| Count | 10 | 20 |\n| Score | 5.2 | 6.4 |"
    );
    let mut actual: Vec<_> = block
        .lines
        .iter()
        .flat_map(|line| &line.text_items)
        .map(|item| (item.id.clone(), item.raw_text.clone()))
        .collect();
    actual.sort();
    let mut expected: Vec<_> = source_items
        .into_iter()
        .map(|item| (item.id, item.raw_text))
        .collect();
    expected.sort();
    assert_eq!(
        actual, expected,
        "cell views must not replace or duplicate raw facts"
    );
}

/// A single long native run can supply several cells, including UTF-8 text and a blank middle cell.
#[tokio::test]
async fn measured_words_split_one_source_run_without_duplicate_ownership() {
    let mut evidence = TableLayout::rules();
    let mut items = Vec::new();
    for (row, cells) in [
        ["Name", "Qty", "Price"],
        ["Alpha", "10", "20"],
        ["甲", "", "30"],
    ]
    .iter()
    .enumerate()
    {
        let y = 30.0 + row as f64 * 28.0;
        let raw = cells.concat();
        let mut item = TableLayout::item(row as u32, &raw, 20.0, y, row == 0);
        item.bbox =
            Bbox::try_from([20.0, y, 270.0, y + 10.0]).expect("source run");
        let mut offset = 0;
        let mut words = Vec::new();
        for (column, text) in cells.iter().enumerate() {
            let start = offset;
            offset += text.len();
            if text.is_empty() {
                continue;
            }
            let x = 20.0 + column as f64 * 95.0;
            words.push(
                docparse_core::TableWord::builder()
                    .byte_range(start..offset)
                    .bbox(
                        Bbox::try_from([
                            x,
                            y,
                            x + text.chars().count() as f64 * 5.0,
                            y + 10.0,
                        ])
                        .expect("word bbox"),
                    )
                    .build(),
            );
        }
        evidence.words.insert(item.id.clone(), words);
        items.push(item);
    }
    let page = TableLayout::parse(items, evidence).await;
    let block = page.blocks.first().expect("block");
    let table = block.table.as_ref().expect("measured word table");
    assert_eq!(block.text, "Name\tQty\tPrice\nAlpha\t10\t20\n甲\t\t30");
    assert_eq!(
        block
            .lines
            .iter()
            .map(|line| line.text_items.len())
            .sum::<usize>(),
        3
    );
    assert!(
        table.cells.iter().any(|cell| cell.row == 2
            && cell.column == 1
            && cell.text.is_empty())
    );
    let mut broken = page.clone();
    let cell = broken
        .blocks
        .first_mut()
        .and_then(|block| block.table.as_mut())
        .and_then(|table| {
            table
                .cells
                .iter_mut()
                .find(|cell| cell.row == 2 && cell.column == 0)
        })
        .expect("Chinese cell");
    cell.lines
        .first_mut()
        .and_then(|line| line.spans.first_mut())
        .expect("source span")
        .byte_range
        .end = 1;
    let error = docparse_core::ResultValidator::validate_page(&broken)
        .expect_err("partial UTF-8 character must be rejected");
    assert!(error.to_string().contains("UTF-8"));
}

/// Missing internal rules recover both rowspan and colspan, including multi-row header semantics.
#[tokio::test]
async fn ruled_merged_cells_keep_header_rows_and_html_spans() {
    use docparse_core::TableRule::{Horizontal, Vertical};
    let mut evidence = TableLayout::rules();
    evidence.rules.retain(|rule| {
        !matches!(rule, Horizontal { y, .. } if (*y - 48.0).abs() < 0.001)
            && !matches!(rule, Vertical { x, .. } if (*x - 195.0).abs() < 0.001)
    });
    evidence.rules.push(Horizontal {
        y: 48.0,
        left: 100.0,
        right: 290.0,
    });
    evidence.rules.push(Vertical {
        x: 195.0,
        top: 48.0,
        bottom: 110.0,
    });
    let mut heading = TableLayout::item(1, "Results", 165.0, 30.0, true);
    heading.bbox =
        Bbox::try_from([165.0, 30.0, 230.0, 40.0]).expect("spanning header");
    let items = vec![
        TableLayout::item(0, "Group", 20.0, 30.0, true),
        heading,
        TableLayout::item(2, "Control", 115.0, 58.0, true),
        TableLayout::item(3, "Treatment", 210.0, 58.0, true),
        TableLayout::item(4, "Score", 20.0, 86.0, false),
        TableLayout::item(5, "10", 115.0, 86.0, false),
        TableLayout::item(6, "20", 210.0, 86.0, false),
    ];
    let page = TableLayout::parse(items, evidence).await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("ruled table");
    assert_eq!(
        (table.row_count, table.column_count, table.cells.len()),
        (3, 3, 7)
    );
    assert!(
        table
            .cells
            .iter()
            .any(|cell| cell.text == "Group" && cell.row_span == 2)
    );
    assert!(
        table
            .cells
            .iter()
            .any(|cell| cell.text == "Results" && cell.column_span == 2)
    );
    assert!(
        table
            .cells
            .iter()
            .any(|cell| cell.text == "Control" && cell.is_header)
    );
    let html = table.to_markdown();
    assert!(html.contains("rowspan=\"2\"") && html.contains("colspan=\"2\""));
    assert_eq!(html.matches("Results").count(), 1);
}

/// Numeric data must never be silently promoted to the mandatory Markdown header row.
#[tokio::test]
async fn numeric_first_row_uses_html_without_inventing_headers() {
    let mut items = Vec::new();
    for row in 0..3 {
        for column in 0..3 {
            items.push(TableLayout::item(
                row * 3 + column,
                &format!("{}", row * 3 + column + 1),
                20.0 + f64::from(column) * 95.0,
                30.0 + f64::from(row) * 28.0,
                true,
            ));
        }
    }
    let page = TableLayout::parse(items, TableLayout::rules()).await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("numeric table");
    assert!(table.cells.iter().all(|cell| !cell.is_header));
    assert!(table.to_markdown().starts_with("<table>"));
}

/// An ambiguous one-column region keeps physical lines instead of fabricating cell boundaries.
#[tokio::test]
async fn ambiguous_table_retains_original_lines_with_a_diagnostic() {
    let items = vec![
        TableLayout::item(0, "First paragraph", 20.0, 30.0, false),
        TableLayout::item(1, "Second paragraph", 20.0, 58.0, false),
    ];
    let page = TableLayout::parse(items, Default::default()).await;
    let block = page.blocks.first().expect("block");
    assert!(block.table.is_none());
    assert_eq!(block.text, "First paragraph\nSecond paragraph");
    assert!(
        page.warnings
            .iter()
            .any(|warning| warning.code == "TableStructureUnavailable")
    );
}

/// A glyph's tight box can leave a large gap around punctuation without representing a source space.
#[tokio::test]
async fn decimal_fragments_keep_source_punctuation_without_inserted_spaces() {
    let page = TableLayout::parse(
        vec![
            TableLayout::item(0, "Metric", 20.0, 30.0, true),
            TableLayout::item(1, "Value", 115.0, 30.0, true),
            TableLayout::item(2, "Count", 210.0, 30.0, true),
            TableLayout::item(3, "Latency", 20.0, 58.0, false),
            TableLayout::item(4, "32", 115.0, 58.0, false),
            TableLayout::item(5, ".", 128.0, 58.0, false),
            TableLayout::item(6, "5", 136.0, 58.0, false),
            TableLayout::item(7, "10", 210.0, 58.0, false),
            TableLayout::item(8, "Throughput", 20.0, 86.0, false),
            TableLayout::item(9, "100", 115.0, 86.0, false),
            TableLayout::item(10, "20", 210.0, 86.0, false),
        ],
        TableLayout::rules(),
    )
    .await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("table");
    assert_eq!(
        table
            .cells
            .iter()
            .find(|cell| cell.row == 1 && cell.column == 1)
            .map(|cell| cell.text.as_str()),
        Some("32.5")
    );
}

/// PDFium may omit an empty tagged TD from its page-filtered tree; later cells must retain their columns.
#[tokio::test]
async fn tagged_sparse_rows_recover_empty_columns_from_complete_row_geometry() {
    use docparse_core::{
        PdfProvenance, TableEvidence, TaggedTable, TaggedTableCell,
        UnicodeMappingStatus,
    };
    let mut items = Vec::new();
    let mut cells = Vec::new();
    for (index, (row, logical_column, tree_column, text)) in [
        (0, 0, 0, "Metric"),
        (0, 1, 1, "Control"),
        (0, 2, 2, "Treatment"),
        (1, 0, 0, "Alpha"),
        (1, 2, 1, "20"),
        (2, 0, 0, "Beta"),
        (2, 1, 1, "30"),
        (2, 2, 2, "40"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut item = TableLayout::item(
            index as u32,
            text,
            20.0 + f64::from(logical_column) * 95.0,
            30.0 + row as f64 * 28.0,
            row == 0,
        );
        item.provenance = Some(
            PdfProvenance::builder()
                .mcid(Some(index as i32))
                .unicode_mapping(UnicodeMappingStatus::Complete)
                .build(),
        );
        items.push(item);
        cells.push(
            TaggedTableCell::builder()
                .row(row)
                .column(tree_column)
                .row_span(1)
                .column_span(1)
                .is_header(row == 0)
                .mcids(std::collections::BTreeSet::from([index as i32]))
                .build(),
        );
    }
    let evidence = TableEvidence {
        tagged_tables: vec![TaggedTable {
            row_count: 3,
            column_count: 3,
            cells,
        }],
        ..Default::default()
    };
    let page = TableLayout::parse(items, evidence).await;
    let block = page.blocks.first().expect("block");
    assert_eq!(
        block.table.as_ref().map(|table| table.source),
        Some(docparse_core::TableStructureSource::TaggedPdf)
    );
    assert_eq!(
        block.text,
        "Metric\tControl\tTreatment\nAlpha\t\t20\nBeta\t30\t40"
    );
}

/// Sparse horizontal rules and centered group labels must not collapse distinct body columns.
#[tokio::test]
async fn sparse_rules_recover_group_headers_without_merging_data_columns() {
    let mut items = vec![
        TableLayout::item(0, "Key", 20.0, 20.0, false),
        TableLayout::item(1, "Arch", 65.0, 20.0, false),
        TableLayout::item(2, "Python", 125.0, 20.0, false),
        TableLayout::item(3, "Go", 215.0, 20.0, false),
    ];
    for (index, (x, text)) in [
        (110.0, "Std"),
        (150.0, "Sync"),
        (190.0, "Std"),
        (230.0, "Sync"),
    ]
    .into_iter()
    .enumerate()
    {
        items.push(TableLayout::item(4 + index as u32, text, x, 35.0, false));
    }
    for (row, y) in [65.0, 95.0].into_iter().enumerate() {
        for (column, (x, text)) in [
            (20.0, "A"),
            (65.0, "B"),
            (110.0, "10"),
            (150.0, "20"),
            (190.0, "30"),
            (230.0, "40"),
        ]
        .into_iter()
        .enumerate()
        {
            items.push(TableLayout::item(
                8 + (row * 6 + column) as u32,
                text,
                x,
                y,
                false,
            ));
        }
    }
    let evidence = docparse_core::TableEvidence {
        rules: [10.0, 55.0, 120.0]
            .map(|y| docparse_core::TableRule::Horizontal {
                y,
                left: 10.0,
                right: 290.0,
            })
            .to_vec(),
        ..Default::default()
    };
    let page = TableLayout::parse(items, evidence).await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("sparse-rule table");
    assert_eq!((table.row_count, table.column_count), (4, 6));
    assert!(
        table
            .cells
            .iter()
            .any(|cell| cell.text == "Python" && cell.column_span == 2)
    );
    assert!(
        table
            .cells
            .iter()
            .any(|cell| cell.text == "Go" && cell.column_span == 2)
    );
    assert!(
        table
            .cells
            .iter()
            .any(|cell| cell.text == "Key" && cell.row_span == 2)
    );
    assert_eq!(
        table
            .cells
            .iter()
            .find(|cell| cell.row == 2 && cell.column == 3)
            .map(|cell| cell.text.as_str()),
        Some("20")
    );
}

/// Configured JSON output must retain structural cells even when optional evidence is hidden.
#[tokio::test]
async fn configured_json_preserves_tables_and_roundtrips_their_text_contract() {
    let mut items = Vec::new();
    for (row, values) in [
        ["Name", "Value", "Count"],
        ["A", "10", "20"],
        ["B", "30", "40"],
    ]
    .iter()
    .enumerate()
    {
        for (column, value) in values.iter().enumerate() {
            items.push(TableLayout::item(
                (row * 3 + column) as u32,
                value,
                20.0 + column as f64 * 95.0,
                30.0 + row as f64 * 28.0,
                row == 0,
            ));
        }
    }
    let page = TableLayout::parse(items, TableLayout::rules()).await;
    let document = docparse_core::DocumentResult::builder()
        .schema_version(docparse_core::SchemaVersion::V2_0)
        .context(
            docparse_core::DocumentContext::builder()
                .page_count(1)
                .build(),
        )
        .pages(vec![page])
        .build();
    for evidence in [false, true] {
        let config = docparse_config::OutputConfig {
            include_evidence: evidence,
            include_diagnostics: true,
            ..Default::default()
        };
        let output =
            docparse_core::JsonRenderer::render_with_config(&document, &config)
                .expect("configured JSON");
        let restored: docparse_core::DocumentResult =
            serde_json::from_str(&output).expect("canonical roundtrip");
        docparse_core::ResultValidator::validate(&restored)
            .expect("table text still matches its structure");
        assert_eq!(
            restored
                .pages
                .first()
                .and_then(|page| page.blocks.first())
                .and_then(|block| block.table.as_ref()),
            document
                .pages
                .first()
                .and_then(|page| page.blocks.first())
                .and_then(|block| block.table.as_ref())
        );
    }
}

/// A centered label between sparse horizontal rules owns its complete row group exactly once.
#[tokio::test]
async fn sparse_rule_band_recovers_a_centered_rowspan_label() {
    let mut items = vec![
        TableLayout::item(0, "Model", 20.0, 20.0, true),
        TableLayout::item(1, "A", 115.0, 20.0, true),
        TableLayout::item(2, "B", 210.0, 20.0, true),
        TableLayout::item(3, "Model A", 20.0, 70.0, false),
    ];
    for (index, (y, a, b)) in
        [(50.0, "10", "20"), (70.0, "30", "40"), (90.0, "50", "60")]
            .into_iter()
            .enumerate()
    {
        items.push(TableLayout::item(4 + index as u32 * 2, a, 115.0, y, false));
        items.push(TableLayout::item(5 + index as u32 * 2, b, 210.0, y, false));
    }
    let evidence = docparse_core::TableEvidence {
        rules: [10.0, 45.0, 115.0]
            .map(|y| docparse_core::TableRule::Horizontal {
                y,
                left: 10.0,
                right: 290.0,
            })
            .to_vec(),
        ..Default::default()
    };
    let page = TableLayout::parse(items, evidence).await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("grouped table");
    assert!(table.cells.iter().any(|cell| cell.text == "Model A"
        && cell.row == 1
        && cell.row_span == 3));
    assert_eq!(
        table.to_text(),
        "Model\tA\tB\nModel A\t10\t20\n\t30\t40\n\t50\t60"
    );
}

/// A sparse row in a previously empty column cannot be a continuation of the preceding cell.
#[tokio::test]
async fn sparse_text_in_another_column_stays_a_separate_row() {
    let items = vec![
        TableLayout::item(0, "Name", 20.0, 30.0, true),
        TableLayout::item(1, "Value", 115.0, 30.0, true),
        TableLayout::item(2, "Note", 210.0, 30.0, true),
        TableLayout::item(3, "Alpha", 20.0, 58.0, false),
        TableLayout::item(4, "10", 115.0, 58.0, false),
        TableLayout::item(5, "annotation", 210.0, 70.0, false),
        TableLayout::item(6, "Beta", 20.0, 98.0, false),
        TableLayout::item(7, "20", 115.0, 98.0, false),
        TableLayout::item(8, "yes", 210.0, 98.0, false),
    ];
    let page = TableLayout::parse(items, Default::default()).await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("sparse table");
    assert_eq!(table.row_count, 4);
    assert_eq!(
        table
            .cells
            .iter()
            .find(|cell| cell.row == 2 && cell.column == 2)
            .map(|cell| cell.text.as_str()),
        Some("annotation")
    );
}

/// Cell contents must remain literal text in both supported markup projections.
#[tokio::test]
async fn table_markup_escapes_source_html_and_pipe_syntax() {
    let mut items = Vec::new();
    for (row, values) in [
        ["Name", "Value", "Count"],
        ["A", "<b>x</b>|&", "10"],
        ["B", "20", "30"],
    ]
    .iter()
    .enumerate()
    {
        for (column, text) in values.iter().enumerate() {
            items.push(TableLayout::item(
                (row * 3 + column) as u32,
                text,
                20.0 + column as f64 * 95.0,
                30.0 + row as f64 * 28.0,
                row == 0,
            ));
        }
    }
    let page = TableLayout::parse(items, TableLayout::rules()).await;
    let mut table = page
        .blocks
        .first()
        .and_then(|block| block.table.clone())
        .expect("table");
    assert!(
        table
            .to_markdown()
            .contains("&lt;b&gt;x&lt;/b&gt;&#124;&amp;")
    );
    for cell in &mut table.cells {
        cell.is_header = false;
    }
    let html = table.to_markdown();
    assert!(html.contains("&lt;b&gt;x&lt;/b&gt;|&amp;"));
    assert!(!html.contains("<b>"));
}

/// Small heading overhangs across inferred gutters do not establish a multi-column cell.
#[tokio::test]
async fn wide_heading_ink_does_not_steal_neighboring_columns() {
    let mut heading = TableLayout::item(0, "Precomputed", 70.0, 20.0, true);
    heading.bbox =
        Bbox::try_from([70.0, 20.0, 170.0, 30.0]).expect("wide heading");
    let items = vec![
        heading,
        TableLayout::item(1, "End", 210.0, 20.0, true),
        TableLayout::item(2, "A", 20.0, 58.0, false),
        TableLayout::item(3, "10", 115.0, 58.0, false),
        TableLayout::item(4, "20", 210.0, 58.0, false),
        TableLayout::item(5, "B", 20.0, 86.0, false),
        TableLayout::item(6, "30", 115.0, 86.0, false),
        TableLayout::item(7, "40", 210.0, 86.0, false),
    ];
    let page = TableLayout::parse(items, Default::default()).await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("wide heading must not invalidate an otherwise clear grid");
    assert_eq!(table.column_count, 3);
    assert!(table.cells.iter().any(|cell| cell.text == "Precomputed"
        && cell.column == 1
        && cell.column_span == 1));
}

/// Existing schema-2 documents without cell structure keep their legacy table summary contract.
#[tokio::test]
async fn legacy_table_summaries_remain_valid_without_structured_cells() {
    let mut items = Vec::new();
    for (row, values) in [
        ["Name", "Value", "Count"],
        ["A", "10", "20"],
        ["B", "30", "40"],
    ]
    .iter()
    .enumerate()
    {
        for (column, text) in values.iter().enumerate() {
            items.push(TableLayout::item(
                (row * 3 + column) as u32,
                text,
                20.0 + column as f64 * 95.0,
                30.0 + row as f64 * 28.0,
                row == 0,
            ));
        }
    }
    let mut page = TableLayout::parse(items, TableLayout::rules()).await;
    let block = page.blocks.first_mut().expect("table");
    block.table = None;
    block.text = "Name Value Count A 10 20 B 30 40".to_owned();
    docparse_core::ResultValidator::validate_page(&page)
        .expect("legacy table projection");
}

/// A public result must not claim a source word belongs to unrelated cell geometry.
#[tokio::test]
async fn table_validation_rejects_unrelated_cell_bounds() {
    let mut items = Vec::new();
    for (row, values) in [
        ["Name", "Value", "Count"],
        ["A", "10", "20"],
        ["B", "30", "40"],
    ]
    .iter()
    .enumerate()
    {
        for (column, text) in values.iter().enumerate() {
            items.push(TableLayout::item(
                (row * 3 + column) as u32,
                text,
                20.0 + column as f64 * 95.0,
                30.0 + row as f64 * 28.0,
                row == 0,
            ));
        }
    }
    let mut page = TableLayout::parse(items, TableLayout::rules()).await;
    let cell = page
        .blocks
        .first_mut()
        .and_then(|block| block.table.as_mut())
        .and_then(|table| {
            table
                .cells
                .iter_mut()
                .find(|cell| cell.row == 1 && cell.column == 1)
        })
        .expect("cell");
    cell.bbox = Some(
        Bbox::try_from([110.0, 58.0, 111.0, 59.0]).expect("unrelated cell box"),
    );
    docparse_core::ResultValidator::validate_page(&page)
        .expect_err("cell must overlap the words it claims");
}

/// Wide subheaders constrain gutters more tightly than the short right-aligned body values below them.
#[tokio::test]
async fn subheader_widths_refine_gutters_from_right_aligned_data() {
    let mut items = vec![
        TableLayout::item(0, "Key", 20.0, 20.0, false),
        TableLayout::item(1, "Values", 170.0, 20.0, false),
        TableLayout::item(2, "First", 110.0, 35.0, false),
        TableLayout::item(3, "Second", 172.0, 35.0, false),
    ];
    for (row, (y, a, b)) in [(65.0, "1234567", "89"), (95.0, "7654321", "98")]
        .into_iter()
        .enumerate()
    {
        items.push(TableLayout::item(4 + row as u32 * 3, "A", 20.0, y, false));
        items.push(TableLayout::item(5 + row as u32 * 3, a, 115.0, y, false));
        items.push(TableLayout::item(6 + row as u32 * 3, b, 210.0, y, false));
    }
    let evidence = docparse_core::TableEvidence {
        rules: [10.0, 55.0, 120.0]
            .map(|y| docparse_core::TableRule::Horizontal {
                y,
                left: 10.0,
                right: 290.0,
            })
            .to_vec(),
        ..Default::default()
    };
    let page = TableLayout::parse(items, evidence).await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("subheaders must refine their body gutters");
    assert_eq!((table.row_count, table.column_count), (4, 3));
    assert_eq!(
        table
            .cells
            .iter()
            .find(|cell| cell.row == 1 && cell.column == 2)
            .map(|cell| cell.text.as_str()),
        Some("Second")
    );
}

/// Raw PDF font weight is valid heading evidence even when the convenience bold flag was not populated.
#[tokio::test]
async fn font_weight_recovers_headers_without_rewriting_raw_style() {
    let mut items = Vec::new();
    for (row, values) in [
        ["Name", "Value", "Count"],
        ["A", "10", "20"],
        ["B", "30", "40"],
    ]
    .iter()
    .enumerate()
    {
        for (column, text) in values.iter().enumerate() {
            let mut item = TableLayout::item(
                (row * 3 + column) as u32,
                text,
                20.0 + column as f64 * 95.0,
                30.0 + row as f64 * 28.0,
                false,
            );
            item.style = Some(
                TextStyle::builder()
                    .font_size(Some(10.0))
                    .weight(Some(if row == 0 { 800 } else { 400 }))
                    .build(),
            );
            items.push(item);
        }
    }
    let page = TableLayout::parse(items, Default::default()).await;
    let block = page.blocks.first().expect("block");
    let table = block.table.as_ref().expect("table");
    assert!(
        table
            .cells
            .iter()
            .filter(|cell| cell.row == 0)
            .all(|cell| cell.is_header)
    );
    assert!(table.to_markdown().starts_with("| Name | Value | Count |"));
    assert!(
        block
            .lines
            .iter()
            .flat_map(|line| &line.text_items)
            .all(|item| item.style.as_ref().is_some_and(|style| !style.bold))
    );
}

/// A few drawn separators must not hide repeated numeric subcolumns and rows inside a coarse cell.
#[tokio::test]
async fn partial_rule_grid_refines_repeated_numeric_subcolumns() {
    let mut items = vec![
        TableLayout::item(0, "Key", 20.0, 20.0, true),
        TableLayout::item(1, "Arch", 65.0, 20.0, true),
        TableLayout::item(2, "Python", 125.0, 20.0, true),
        TableLayout::item(3, "Go", 215.0, 20.0, true),
    ];
    for (index, (x, text)) in [
        (110.0, "Std"),
        (150.0, "Sync"),
        (190.0, "Std"),
        (230.0, "Sync"),
    ]
    .into_iter()
    .enumerate()
    {
        items.push(TableLayout::item(4 + index as u32, text, x, 35.0, true));
    }
    for (row, y) in [65.0, 95.0].into_iter().enumerate() {
        for (column, (x, text)) in [
            (20.0, "A"),
            (65.0, "B"),
            (110.0, "10"),
            (150.0, "20"),
            (190.0, "30"),
            (230.0, "40"),
        ]
        .into_iter()
        .enumerate()
        {
            items.push(TableLayout::item(
                8 + (row * 6 + column) as u32,
                text,
                x,
                y,
                false,
            ));
        }
    }
    let mut evidence = docparse_core::TableEvidence {
        rules: [10.0, 55.0, 120.0]
            .map(|y| docparse_core::TableRule::Horizontal {
                y,
                left: 10.0,
                right: 290.0,
            })
            .to_vec(),
        ..Default::default()
    };
    evidence.rules.extend([45.0, 90.0].map(|x| {
        docparse_core::TableRule::Vertical {
            x,
            top: 10.0,
            bottom: 120.0,
        }
    }));
    let page = TableLayout::parse(items, evidence).await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("refined partial grid");
    assert_eq!((table.row_count, table.column_count), (4, 6));
    assert_eq!(
        table
            .cells
            .iter()
            .find(|cell| cell.row == 3 && cell.column == 4)
            .map(|cell| cell.text.as_str()),
        Some("30")
    );
}

/// Even-sized ruled groups may place their label on either central baseline rather than between them.
#[tokio::test]
async fn even_row_groups_preserve_their_single_label_as_a_span() {
    let mut items = vec![
        TableLayout::item(0, "Model", 20.0, 20.0, true),
        TableLayout::item(1, "A", 115.0, 20.0, true),
        TableLayout::item(2, "B", 210.0, 20.0, true),
        TableLayout::item(3, "Family", 20.0, 65.0, false),
    ];
    for (index, y) in [50.0, 65.0, 80.0, 95.0].into_iter().enumerate() {
        items.push(TableLayout::item(
            4 + index as u32 * 2,
            "10",
            115.0,
            y,
            false,
        ));
        items.push(TableLayout::item(
            5 + index as u32 * 2,
            "20",
            210.0,
            y,
            false,
        ));
    }
    let evidence = docparse_core::TableEvidence {
        rules: [10.0, 45.0, 120.0]
            .map(|y| docparse_core::TableRule::Horizontal {
                y,
                left: 10.0,
                right: 290.0,
            })
            .to_vec(),
        ..Default::default()
    };
    let page = TableLayout::parse(items, evidence).await;
    let table = page
        .blocks
        .first()
        .and_then(|block| block.table.as_ref())
        .expect("grouped table");
    assert!(table.cells.iter().any(|cell| cell.text == "Family"
        && cell.row == 1
        && cell.row_span == 4));
}
