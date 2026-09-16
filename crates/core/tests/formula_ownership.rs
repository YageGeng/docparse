use docparse_config::{ConfigLoader, OcrPolicy, TableMode, ValidatedConfig};
use docparse_core::{
    DocParser, LocalPdfiumProvider, PageInput, PdfInput, PdfiumProvider,
};
use docparse_layout::{LayoutLabel, timing::Timings};
use std::{path::PathBuf, sync::Arc};

/// Replays real page-five detection and recognition to keep the caption formula out of the adjacent column.
#[tokio::test]
#[ignore = "requires FORMULA_OWNERSHIP_PDF=2604.18583v1.pdf, provisioned models and FORMULA_OWNERSHIP_OUTPUT"]
async fn real_caption_formula_keeps_its_layout_owner() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut raw = ConfigLoader::new(root.join("docparse.toml"))
        .load_raw()
        .expect("config");
    raw.tsr.mode = TableMode::RulesOnly;
    raw.ocr.policy = OcrPolicy::Disabled;
    raw.formula.enabled = true;
    let config = Arc::new(ValidatedConfig::try_from(raw).expect("config"));
    let session = LocalPdfiumProvider
        .open(
            PdfInput::Path(PathBuf::from(
                std::env::var("FORMULA_OWNERSHIP_PDF").expect("PDF"),
            )),
            config.runtime(),
            Timings::default(),
        )
        .await
        .expect("PDFium session");
    let extracted = session
        .pre_scan_page(5, None)
        .await
        .expect("extraction")
        .extracted;
    let rendered = session
        .render_page(5, config.render())
        .await
        .expect("raster");
    session.close().await.expect("close PDF");
    let parser = DocParser::builder()
        .config(config)
        .build()
        .await
        .expect("parser");
    let page = parser
        .parse_page(
            PageInput::builder()
                .extracted(extracted)
                .image(rendered.image)
                .transform(rendered.transform)
                .build(),
        )
        .await
        .expect("page");
    std::fs::write(
        std::env::var("FORMULA_OWNERSHIP_OUTPUT").expect("output path"),
        serde_json::to_vec_pretty(&page).expect("JSON"),
    )
    .expect("save page");
    let formula = page
        .formulas
        .iter()
        .find(|formula| {
            (307.0..315.0).contains(&formula.bbox.left)
                && (276.0..284.0).contains(&formula.bbox.top)
        })
        .expect("caption formula");
    let owner = page
        .blocks
        .iter()
        .find(|block| Some(&block.id) == formula.block_id.as_ref())
        .expect("formula owner");
    assert_eq!(owner.label, LayoutLabel::FigureTitle);
    assert!(owner.text.starts_with("Fig. 3:"));
    assert!(
        !formula.text_spans.is_empty(),
        "the real caption glyphs must anchor the recognized formula"
    );
    assert!(
        formula.error.is_none(),
        "recognition failure: {:?}",
        formula.error
    );
    let latex = formula.latex.as_deref().expect("LaTeX");
    assert!(
        owner
            .markdown
            .as_deref()
            .expect("caption Markdown")
            .contains(latex)
    );
    let paragraph = page
        .blocks
        .iter()
        .find(|block| block.text.starts_with("Computational Bottleneck"))
        .expect("left paragraph");
    assert!(
        !paragraph
            .markdown
            .as_deref()
            .expect("paragraph Markdown")
            .contains(latex)
    );
    assert!(
        paragraph
            .lines
            .iter()
            .flat_map(|line| &line.inline_spans)
            .all(|span| span.bbox != formula.bbox)
    );
    eprintln!(
        "caption formula {} belongs to {} with LaTeX {}",
        formula.id.as_str(),
        owner.id.as_str(),
        latex
    );
}
