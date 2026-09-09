use docparse_config::{RawConfig, ValidatedConfig};
use docparse_core::{DocParser, ResultValidator};
use docparse_layout::ModelArtifacts;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Exercises the real artifact-backed parser and exports optional native browser parity fixtures.
#[tokio::test]
#[ignore = "requires the fixed PP-DocLayoutV3 model"]
async fn native_artifacts_parse_real_pdf_bytes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    let artifacts = ModelArtifacts::from_paths(
        &root.join("models/pp-doclayout-v3/inference.onnx"),
        &root.join("models/pp-doclayout-v3/inference.yml"),
        &root.join("models/pp-doclayout-v3/model-manifest.json"),
    )
    .expect("fixed model artifacts");
    let mut raw = RawConfig::default();
    raw.runtime.page_concurrency = 1;
    raw.runtime.render_queue_capacity = 1;
    raw.runtime.blocking_task_limit = 1;
    let parser = DocParser::from_artifacts(
        ValidatedConfig::try_from(raw).expect("valid numeric configuration"),
        artifacts,
    )
    .await
    .expect("real parser initialization");
    for (name, pages) in [
        ("extraction_metadata", 1),
        ("multipage_layout", 3),
        ("embedded_layout", 2),
        ("embedded_cjk_90", 1),
        ("table_layout", 3),
    ] {
        let bytes: Arc<[u8]> = Arc::from(
            std::fs::read(
                root.join(format!("crates/core/tests/fixtures/pdf/{name}.pdf")),
            )
            .expect("PDF fixture"),
        );
        let pdf_sha256: String = Sha256::digest(bytes.as_ref())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let result = parser
            .parse_bytes(bytes)
            .await
            .expect("complete native parsing");
        ResultValidator::validate(&result)
            .expect("canonical result invariants");
        assert_eq!(result.pages.len(), pages);
        if name == "embedded_cjk_90" {
            let page = result.pages.first().expect("Chinese geometry page");
            assert_eq!(
                (page.width, page.height, page.rotation),
                (1504.0, 1144.0, 90)
            );
            assert!(
                page.iter_text_items()
                    .map(|item| item.raw_text.as_str())
                    .collect::<String>()
                    .contains("中文文档解析测试")
            );
        }
        if name == "table_layout" {
            for (page, (rows, columns, source)) in result.pages.iter().zip([
                (8, 4, docparse_core::TableStructureSource::Ruled),
                (7, 4, docparse_core::TableStructureSource::TextAlignment),
                (8, 4, docparse_core::TableStructureSource::TaggedPdf),
            ]) {
                let table = page
                    .blocks
                    .iter()
                    .find_map(|block| block.table.as_ref())
                    .expect("real model table detection and structure");
                assert_eq!(
                    (table.row_count, table.column_count, table.source),
                    (rows, columns, source)
                );
                assert!(
                    table
                        .cells
                        .iter()
                        .filter(|cell| cell.row == 0)
                        .all(|cell| cell.is_header)
                );
                assert!(table.cells.iter().any(|cell| cell.text == "32.5"));
                assert!(table.cells.iter().any(|cell| cell.text == "28.1"));
                if rows == 8 {
                    assert!(table.cells.iter().any(|cell| cell.row == 4
                        && cell.column == 1
                        && cell.text.is_empty()));
                    assert!(table.cells.iter().any(|cell| cell.row == 4
                        && cell.column == 2
                        && cell.text == "25.4"));
                    assert!(table.cells.iter().any(|cell| cell.text
                        == "System"
                        && cell.row_span == 2));
                    assert!(
                        table.cells.iter().any(|cell| cell.text
                            == "Performance measurements"
                            && cell.column_span == 3)
                    );
                }
                assert!(
                    page.blocks
                        .iter()
                        .filter(|block| block.label
                            != docparse_layout::LayoutLabel::Table)
                        .any(|block| block
                            .text
                            .contains("This sentence is outside"))
                );
            }
        }
        assert!(
            result
                .pages
                .iter()
                .flat_map(|page| page.iter_text_items())
                .any(|item| !item.raw_text.is_empty()),
            "the PDF must retain its real text"
        );
        assert!(
            result.errors.is_empty(),
            "real fixtures must not pass by silently degrading"
        );
        if let Some(directory) = std::env::var_os("DOCPARSE_WEB_REFERENCE_DIR")
        {
            let directory = PathBuf::from(directory);
            let directory = if directory.is_absolute() {
                directory
            } else {
                root.join(directory)
            };
            std::fs::create_dir_all(&directory).expect("reference directory");
            std::fs::write(
                directory.join(format!("{name}.sha256")),
                &pdf_sha256,
            )
            .expect("reference input checksum");
            std::fs::write(
                directory.join(format!("{name}.json")),
                serde_json::to_vec_pretty(&result).expect("canonical JSON"),
            )
            .expect("reference output");
        }
    }
}
