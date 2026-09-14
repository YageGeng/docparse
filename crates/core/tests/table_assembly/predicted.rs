//! Table predicted tests and support, compiled only within the parent test module.
use super::fixtures::CapturedTable;
use super::*;

/// Real independent detections fix split gate headers while preserving every original PDF text reference.
#[test]
fn independent_cells_recover_real_gate_table_headers() {
    let decoder = flate2::read::GzDecoder::new(
        include_bytes!("../fixtures/tsr/gate-table-independent-cells.json.gz")
            .as_slice(),
    );
    let case: CapturedTable =
        serde_json::from_reader(decoder).expect("real captured table");
    let result = case.reconstruct().expect("detected table");
    let table = result.table.as_ref().expect("structure");
    table.validate(&result).expect("source conservation");
    assert_eq!((table.row_count, table.column_count), (17, 9));
    let headers: Vec<_> = table
        .cells
        .iter()
        .filter(|cell| cell.row == 0)
        .map(|cell| cell.text.as_str())
        .collect();
    assert_eq!(
        headers,
        [
            "Setting",
            "Gate Pos.",
            "Gate Act.",
            "Rank Pres.",
            "Train Sco.",
            "10 step",
            "100 step",
            "1000 step",
            "1 epoch"
        ]
    );
}

/// Every captured survey table must recover without losing source facts or accepting known row/column errors.
#[test]
#[allow(
    clippy::indexing_slicing,
    reason = "semantic expectations address the fixed recorded corpus"
)]
fn captured_survey_tsr_tables_recover_complete_source() {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(
        include_bytes!("../fixtures/tsr/survey-model-captures.json.gz")
            .as_slice(),
    )
    .read_to_string(&mut json)
    .expect("captured corpus");
    let cases: Vec<CapturedTable> =
        serde_json::from_str(&json).expect("model cases");
    assert_eq!(cases.len(), 24);
    for case in cases {
        let original: Block =
            serde_json::from_value(case.source["block"].clone())
                .expect("original");
        let result = case
            .reconstruct()
            .map_err(|e| format!("{}: {e}", original.id.as_str()))
            .expect("captured table structure");
        assert_eq!(result.bbox, original.bbox);
        assert_eq!(result.lines, original.lines);
        let table = result.table.as_ref().expect("structured table");
        assert_eq!(table.source, crate::TableStructureSource::ExternalTsr);
        table
            .validate(&result)
            .expect("complete unique source references");
        if let Some(shape) = case.source.get("expected_shape") {
            assert_eq!(
                serde_json::json!([table.row_count, table.column_count]),
                *shape,
                "{}",
                original.id.as_str()
            );
        }
        match case.prediction["page"].as_u64().expect("page") {
            8 => {
                let t5 = table
                    .cells
                    .iter()
                    .find(|c| c.text == "T5 [82]")
                    .expect("T5 row");
                let mt5 = table
                    .cells
                    .iter()
                    .find(|c| c.text == "mT5 [83]")
                    .expect("mT5 row");
                assert_eq!(
                    (t5.column, mt5.column, mt5.row),
                    (1, 1, t5.row + 1)
                );
                assert!(table.cells.iter().any(|c| c.row == t5.row
                    && c.column == 2
                    && c.text == "Oct-2019"));
                assert!(table.cells.iter().any(|c| c.row == mt5.row
                    && c.column == 2
                    && c.text == "Oct-2020"));
                assert!(
                    table
                        .cells
                        .iter()
                        .any(|c| c.text == "Adaptation" && c.column_span == 2)
                );
            }
            33 => {
                assert!(
                    table.cells.iter().any(|c| c.row == 2
                        && c.column == 8
                        && c.text == "36.6")
                );
                assert!(
                    table
                        .cells
                        .iter()
                        .any(|c| c.row == 2 && c.column == 9 && c.text == "1")
                );
                assert!(table.cells.iter().any(|c| c.text
                    == "A800 LoRA Tuning"
                    && c.column_span == 3));
            }
            47 => {
                for (text, row, span) in [
                    ("Contextual Information", 7, 4),
                    ("Demonstration", 11, 9),
                    ("Other Designs", 20, 8),
                ] {
                    assert!(table.cells.iter().any(|c| c.text == text
                        && c.row == row
                        && c.row_span == span));
                }
            }
            57 => {
                assert!(table.cells.iter().any(|c| c.text == "Basic"
                    && c.row == 1
                    && c.row_span == 9
                    && c.column_span == 1));
                assert!(table.cells.iter().any(|c| c.row == 2
                    && c.column == 3
                    && c.text.contains("WMT")));
                assert!(!table.cells.iter().any(|c| c.row == 1
                    && c.column == 3
                    && c.text.contains("WMT")));
            }
            68 => {
                assert!(
                    table
                        .cells
                        .iter()
                        .any(|c| c.text == "KU" && c.row_span == 6)
                );
                assert!(
                    table
                        .cells
                        .iter()
                        .any(|c| c.text == "CR" && c.row_span == 4)
                );
                assert!(
                    table.cells.iter().any(|c| c.row == 1
                        && c.column == 4
                        && c.text == "20.66")
                );
                assert!(
                    table.cells.iter().any(|c| c.row == 2
                        && c.column == 4
                        && c.text == "21.12")
                );
            }
            82 => {
                for row in 1..=10 {
                    assert!(table.cells.iter().any(|c| c.row == row
                        && c.column == 1
                        && !c.text.is_empty()));
                }
            }
            _ => {}
        }
    }
}

/// A dense model grid must preserve a ruled section's full-width heading and shared values.
#[test]
fn captured_terminal_universe_shared_settings_recover() {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(
        include_bytes!(
            "../fixtures/tsr/terminal-universe-configurations.json.gz"
        )
        .as_slice(),
    )
    .read_to_string(&mut json)
    .expect("capture");
    let case: CapturedTable =
        serde_json::from_str(&json).expect("capture schema");
    let block = case.reconstruct().expect("configuration table");
    let table = block.table.as_ref().expect("structured");
    table.validate(&block).expect("complete source ownership");
    assert_eq!((table.row_count, table.column_count), (16, 7));
    let section = table
        .cells
        .iter()
        .find(|c| c.text == "Shared across all reproductions")
        .expect("section heading");
    assert_eq!(
        (section.row, section.column, section.column_span),
        (11, 0, 7)
    );
    for (row, value) in [(12, "4 h"), (13, "10 h"), (14, "6"), (15, "4")] {
        let cell = table
            .cells
            .iter()
            .find(|c| c.row == row && c.column == 1)
            .expect("shared value");
        assert_eq!((cell.column_span, cell.text.as_str()), (6, value));
    }
    assert!(
        table
            .cells
            .iter()
            .all(|cell| cell.is_header == matches!(cell.row, 0 | 11))
    );
    // Sparse values without enclosing rules, or across a real divider, do not justify a colspan.
    let mut unsupported: CapturedTable =
        serde_json::from_str(&json).expect("capture schema");
    *unsupported.source.get_mut("rules").expect("rules") =
        serde_json::json!([]);
    unsupported
        .reconstruct()
        .expect_err("unruled shared values are ambiguous");
    let mut divided: CapturedTable =
        serde_json::from_str(&json).expect("capture schema");
    divided
        .source
        .get_mut("rules")
        .and_then(serde_json::Value::as_array_mut)
        .expect("rules")
        .push(serde_json::json!(["v", 200.0, 605.8, 656.5]));
    divided
        .reconstruct()
        .expect_err("full divider prevents a shared section");
    let mut partial: CapturedTable =
        serde_json::from_str(&json).expect("capture schema");
    partial
        .source
        .get_mut("rules")
        .and_then(serde_json::Value::as_array_mut)
        .expect("rules")
        .push(serde_json::json!(["v", 200.0, 615.0, 635.0]));
    partial
        .reconstruct()
        .expect_err("partial divider prevents a shared section");
}

/// A geometrically covering model box must not hide two repeated numeric source columns.
#[test]
fn covering_model_cells_still_recover_missing_numeric_columns() {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(
        include_bytes!("../fixtures/tsr/coarse-numeric-columns.json.gz")
            .as_slice(),
    )
    .read_to_string(&mut json)
    .expect("coarse model capture");
    let case: CapturedTable =
        serde_json::from_str(&json).expect("capture schema");
    let block = case.reconstruct().expect("recovered columns");
    let table = block.table.as_ref().expect("table");
    table.validate(&block).expect("source ownership");
    assert_eq!((table.row_count, table.column_count), (5, 5));
    for (column, text) in [(2, "12"), (3, "17")] {
        assert!(table.cells.iter().any(|cell| cell.row == 1
            && cell.column == column
            && cell.text == text));
    }
}
