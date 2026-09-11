//! Table replay tests and support, compiled only within the parent test module.
use super::fixtures::CapturedTable;
use super::*;

/// Replays local diagnostic captures and writes inspectable structured results.
#[test]
#[ignore = "requires TSR_CAPTURE_DIR containing real model and source captures"]
fn replay_tsr_captures() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
    let directory = std::path::PathBuf::from(
        std::env::var("TSR_CAPTURE_DIR").expect("capture directory"),
    )
    .join("crops");
    let mut outcomes = Vec::new();
    for entry in std::fs::read_dir(&directory).expect("directory") {
        let path = entry.expect("entry").path();
        let Some(stem) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".source.json"))
        else {
            continue;
        };
        let (width, height) =
            image::image_dimensions(directory.join(format!("{stem}.png")))
                .expect("crop dimensions");
        let case = CapturedTable {
            source: serde_json::from_slice(
                &std::fs::read(&path).expect("source"),
            )
            .expect("source JSON"),
            prediction: serde_json::from_slice(
                &std::fs::read(directory.join(format!("{stem}.json")))
                    .expect("model response"),
            )
            .expect("model JSON"),
            size: [width, height],
        };
        let result = case.reconstruct();
        let status = match result {
            Ok(block) => {
                std::fs::write(
                    directory.join(format!("{stem}.result.json")),
                    serde_json::to_vec(&block).expect("table JSON"),
                )
                .expect("result file");
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        };
        eprintln!("{stem}: {status:?}");
        outcomes.push((stem.to_owned(), status));
    }
    assert!(!outcomes.is_empty());
    assert!(
        outcomes.iter().all(|(_, status)| status.is_ok()),
        "{outcomes:?}"
    );
}

/// Captures measured source words beside real model crops for deterministic postprocessing replay.
#[tokio::test]
#[ignore = "requires TSR_CAPTURE_DIR and TSR_CAPTURE_PDF from a real-model acceptance run"]
async fn capture_tsr_source_facts() {
    use crate::runtime::{PdfInput, PdfiumExecutor};
    let directory = std::path::PathBuf::from(
        std::env::var("TSR_CAPTURE_DIR").expect("capture directory"),
    );
    let document: crate::DocumentResult = serde_json::from_slice(
        &std::fs::read(directory.join("document-0-full.json"))
            .expect("document"),
    )
    .expect("document JSON");
    let raw = docparse_config::RawConfig::default();
    let executor = PdfiumExecutor::open(
        PdfInput::Path(std::env::var("TSR_CAPTURE_PDF").expect("PDF").into()),
        &raw.runtime,
    )
    .await
    .expect("PDFium");
    for page in document.pages {
        let tables: Vec<_> = page
            .blocks
            .into_iter()
            .filter(|b| b.label == docparse_layout::LayoutLabel::Table)
            .collect();
        if tables.is_empty() {
            continue;
        }
        let extracted = executor
            .pre_scan_page(page.page_number, None)
            .await
            .expect("source facts")
            .extracted;
        let assembler =
            TableAssembler::new(&raw.fusion, &extracted.table_evidence, &[]);
        for block in tables {
            let spans = assembler.locate(&block).expect("source spans");
            let facts = spans.iter().map(|s| serde_json::json!({"span":s.span,"baseline":s.baseline,"measured_baseline":s.measured_baseline,"mcid":s.mcid})).collect::<Vec<_>>();
            std::fs::write(directory.join("crops").join(format!("{}.source.json",block.id.as_str().replace(':',"-"))),serde_json::to_vec(&serde_json::json!({"block":block,"words":facts,"rules":extracted.table_evidence.rules.iter().map(|r|match *r {crate::table::TableRule::Horizontal {y,left,right}=>serde_json::json!(["h",y,left,right]),crate::table::TableRule::Vertical {x,top,bottom}=>serde_json::json!(["v",x,top,bottom])}).collect::<Vec<_>>()})).expect("source JSON")).expect("source fixture");
        }
    }
    executor.close().await.expect("close PDFium");
}
