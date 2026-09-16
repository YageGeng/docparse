use docparse_config::{ConfigLoader, OcrPolicy, TableMode, ValidatedConfig};
use docparse_core::{
    DocParser, LocalPdfiumProvider, PageInput, PdfInput, PdfiumProvider,
};
use docparse_formula::{FormulaEngine, FormulaError, PpFormulaNetEngine};
use docparse_layout::{
    PageImage, timing::Timings, wasm_compat::WasmBoxedFuture,
};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

/// Records the exact production crops before forwarding them to the real recognizer.
struct Capture {
    engine: PpFormulaNetEngine,
    output: PathBuf,
    sequence: AtomicUsize,
}

impl FormulaEngine for Capture {
    /// Preserves the actual model and execution-provider identity in parser results.
    fn name(&self) -> &str {
        self.engine.name()
    }

    /// Saves pixels without changing the request, then runs the configured formula model.
    fn recognize(
        &self,
        images: Vec<Arc<PageImage>>,
        timings: Timings,
    ) -> WasmBoxedFuture<'_, Result<Vec<String>, FormulaError>> {
        Box::pin(async move {
            for image in &images {
                let index = self.sequence.fetch_add(1, Ordering::Relaxed);
                image::save_buffer(
                    self.output.join(format!("crop-{index:03}.png")),
                    image.data(),
                    image.width(),
                    image.height(),
                    image::ColorType::Rgb8,
                )
                .expect("save actual formula crop");
            }
            self.engine.recognize(images, timings).await
        })
    }
}

/// Preserves the detached overbar in the real page-four motion descriptor without duplicating it in prose.
#[tokio::test]
#[ignore = "requires FORMULA_CROP_PDF=2604.18583v1.pdf, provisioned models and FORMULA_CROP_OUTPUT"]
async fn real_motion_descriptor_preserves_overbar() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = PathBuf::from(
        std::env::var("FORMULA_CROP_OUTPUT").expect("output directory"),
    );
    std::fs::create_dir_all(&output).expect("output directory");
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
                std::env::var("FORMULA_CROP_PDF").expect("PDF"),
            )),
            config.runtime(),
            Timings::default(),
        )
        .await
        .expect("PDFium session");
    let extracted = session
        .pre_scan_page(4, None)
        .await
        .expect("extraction")
        .extracted;
    std::fs::write(
        output.join("extracted-page4.json"),
        serde_json::to_vec_pretty(&extracted).expect("JSON"),
    )
    .expect("save extraction");
    let rendered = session
        .render_page(4, config.render())
        .await
        .expect("raster");
    image::save_buffer(
        output.join("page4-pdfium.png"),
        rendered.image.data(),
        rendered.image.width(),
        rendered.image.height(),
        image::ColorType::Rgb8,
    )
    .expect("save raster");
    session.close().await.expect("close PDF");
    let engine = PpFormulaNetEngine::from_config(Arc::clone(&config))
        .await
        .expect("formula engine");
    let parser = DocParser::builder()
        .config(config)
        .formula_engine(Arc::new(Capture {
            engine,
            output: output.clone(),
            sequence: AtomicUsize::new(0),
        }))
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
        output.join("page4.json"),
        serde_json::to_vec_pretty(&page).expect("JSON"),
    )
    .expect("save page");
    let formula = page
        .formulas
        .iter()
        .find(|formula| {
            (150.0..160.0).contains(&formula.bbox.left)
                && (570.0..580.0).contains(&formula.bbox.top)
        })
        .expect("motion descriptor");
    eprintln!(
        "motion descriptor: {:?}, crop {:?}, spans {:?}",
        formula.latex, formula.crop_bbox, formula.text_spans
    );
    let latex: String = formula
        .latex
        .as_ref()
        .expect("LaTeX")
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    assert!(
        latex.contains("\\bar")
            && latex.contains("\\theta")
            && latex.contains("_{f}"),
        "missing motion descriptor overbar: {latex}"
    );
    assert!(
        formula
            .text_spans
            .iter()
            .any(|span| span.text_item_id.as_str() == "p4:t124"),
        "overbar must be bound to the formula"
    );
    let block = page
        .blocks
        .iter()
        .find(|block| Some(&block.id) == formula.block_id.as_ref())
        .expect("block");
    assert!(
        !block.markdown.as_ref().expect("Markdown").contains('¯'),
        "overbar must not remain as duplicate prose"
    );
    assert!(
        block.text.contains('¯'),
        "canonical source text must remain intact"
    );
}

/// Reproduces missing-script and neighboring-row cases through real PDFium, layout and formula inference.
#[tokio::test]
#[ignore = "requires FORMULA_CROP_PDF, provisioned models and FORMULA_CROP_OUTPUT"]
async fn real_softmax_crop_preserves_component_subscript() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = PathBuf::from(
        std::env::var("FORMULA_CROP_OUTPUT").expect("output directory"),
    );
    std::fs::create_dir_all(&output).expect("output directory");
    let mut raw = ConfigLoader::new(root.join("docparse.toml"))
        .load_raw()
        .expect("config");
    // Isolate formula inference while retaining the production raster and layout configuration.
    raw.tsr.mode = TableMode::RulesOnly;
    raw.ocr.policy = OcrPolicy::Disabled;
    raw.formula.enabled = true;
    let config =
        Arc::new(ValidatedConfig::try_from(raw).expect("validated config"));
    let session = LocalPdfiumProvider
        .open(
            PdfInput::Path(PathBuf::from(
                std::env::var("FORMULA_CROP_PDF").expect("PDF"),
            )),
            config.runtime(),
            Timings::default(),
        )
        .await
        .expect("PDFium session");
    let mut inputs = Vec::new();
    for number in [4, 13] {
        let extracted = session
            .pre_scan_page(number, None)
            .await
            .expect("extraction")
            .extracted;
        let rendered = session
            .render_page(number, config.render())
            .await
            .expect("page raster");
        image::save_buffer(
            output.join(format!("page{number}-pdfium.png")),
            rendered.image.data(),
            rendered.image.width(),
            rendered.image.height(),
            image::ColorType::Rgb8,
        )
        .expect("raster");
        inputs.push((
            number,
            PageInput::builder()
                .extracted(extracted)
                .image(rendered.image)
                .transform(rendered.transform)
                .build(),
        ));
    }
    session.close().await.expect("close PDF");
    let engine = PpFormulaNetEngine::from_config(Arc::clone(&config))
        .await
        .expect("formula model");
    let parser = DocParser::builder()
        .config(config)
        .formula_engine(Arc::new(Capture {
            engine,
            output: output.clone(),
            sequence: AtomicUsize::new(0),
        }))
        .build()
        .await
        .expect("parser");
    for (number, input) in inputs {
        let result = parser.parse_page(input).await.expect("page result");
        std::fs::write(
            output.join(format!("page{number}.json")),
            serde_json::to_vec_pretty(&result).expect("JSON"),
        )
        .expect("save result");
        let equation = result
            .formulas
            .iter()
            .find(|formula| {
                if number == 4 {
                    (315.0..325.0).contains(&formula.bbox.left)
                        && (615.0..621.0).contains(&formula.bbox.top)
                } else {
                    (224.0..230.0).contains(&formula.bbox.left)
                        && (94.0..100.0).contains(&formula.bbox.top)
                }
            })
            .expect("equation above a neighboring formula row");
        assert!(
            equation.error.is_none(),
            "formula recognition failed: {:?}",
            equation.error
        );
        assert!(
            equation.crop_bbox.unwrap_or(equation.bbox).bottom
                <= equation.bbox.bottom,
            "page {number}: crop includes the next equation row"
        );
        let anchor = result
            .blocks
            .iter()
            .flat_map(|block| &block.lines)
            .find(|line| Some(&line.id) == equation.line_id.as_ref())
            .expect("source line");
        assert!(
            equation.text_spans.iter().all(|span| anchor
                .text_items
                .iter()
                .any(|item| item.id == span.text_item_id)),
            "page {number}: equation claimed text from a neighboring row"
        );
        eprintln!(
            "page {number} equation crop {:?}, output {:?}",
            equation.crop_bbox.unwrap_or(equation.bbox),
            equation.latex
        );
        if number != 4 {
            continue;
        }
        let target = result
            .formulas
            .iter()
            .find(|formula| {
                formula.bbox.left < 300.0
                    && formula.bbox.top > 670.0
                    && formula.latex.as_deref().is_some_and(|text| {
                        text.replace(' ', "").contains("softmax")
                    })
            })
            .expect("softmax normalization formula");
        eprintln!(
            "softmax detection {:?}, recognition crop {:?}, output {:?}",
            target.bbox, target.crop_bbox, target.latex
        );
        let latex: String = target
            .latex
            .as_ref()
            .expect("recognized formula")
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(
            latex.matches("_{i}").count() >= 2,
            "both the sum index and component subscript must survive: {latex}"
        );
        assert!(
            target
                .text_spans
                .iter()
                .any(|span| span.text_item_id.as_str() == "p4:t374"),
            "recovered subscript must replace its native source instead of appearing twice"
        );
    }
}
