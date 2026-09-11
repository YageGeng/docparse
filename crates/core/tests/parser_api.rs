use std::path::{Path, PathBuf};
use std::sync::Arc;

use docparse_config::{RawConfig, ValidatedConfig};
use docparse_core::{DocParseError, DocParser, ExtractedPage, PageInput};
use docparse_layout::{
    AffineTransform, LayoutDetection, LayoutEngine, LayoutError, LayoutRequest,
    PageImage, PageImageInput, PageRotation, PageTransform, PageTransformInput,
    PixelFormat,
};

/// Offline layout engine proving dependency injection bypasses default artifacts.
struct EmptyLayoutEngine;

impl LayoutEngine for EmptyLayoutEngine {
    /// Returns one stable fake engine name.
    fn name(&self) -> &str {
        "parser-api-fake"
    }

    /// Returns one stable fake revision.
    fn model_revision(&self) -> &str {
        "parser-api-revision"
    }

    /// Returns no regions so the parser exercises full XY-cut fallback.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> docparse_layout::wasm_compat::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    > {
        Box::pin(async move { Ok(Vec::new()) })
    }
}

/// Resolves the deterministic core PDF fixture.
fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pdf/extraction_metadata.pdf")
}

/// Builds valid configuration pointing at intentionally absent model files.
fn config() -> ValidatedConfig {
    let mut raw = RawConfig::default();
    raw.tsr.mode = docparse_config::TableMode::RulesOnly;
    raw.layout.model_path = PathBuf::from("/tmp/parser-api-missing-model.onnx");
    raw.layout.model_config_path =
        PathBuf::from("/tmp/parser-api-missing-model.yml");
    raw.layout.model_manifest_path =
        PathBuf::from("/tmp/parser-api-missing-model.json");
    ValidatedConfig::try_from(raw).expect("test config must validate")
}

/// Verifies an injected engine avoids all default model artifact validation.
#[tokio::test]
async fn injected_layout_engine_parses_path_and_bytes() {
    let parser = DocParser::builder()
        .config(Arc::new(config()))
        .layout_engine(Arc::new(EmptyLayoutEngine))
        .build()
        .await
        .expect("injected parser must build without model files");

    let from_path = parser
        .parse_path(fixture_path())
        .await
        .expect("path parse must succeed");
    let bytes = Arc::<[u8]>::from(
        std::fs::read(fixture_path()).expect("fixture bytes must read"),
    );
    let from_bytes = parser
        .parse_bytes(bytes)
        .await
        .expect("byte parse must succeed");

    assert_eq!(from_path, from_bytes);
    assert_eq!(
        from_path.context.model_revision.as_deref(),
        Some("parser-api-revision")
    );
}

/// Parser injection must reach the PDFium worker and retain repaired geometry through canonical validation.
#[tokio::test]
async fn injected_glyph_resolver_reaches_document_extraction() {
    struct Outline;
    impl docparse_core::GlyphResolver for Outline {
        /// Returns a known symbol for the fixture's otherwise undecodable real vector outlines.
        fn resolve(&self, segments: &[(i32, f32, f32)]) -> Option<String> {
            (!segments.is_empty()).then(|| "✓".to_owned())
        }
    }
    let parser = DocParser::builder()
        .config(Arc::new(config()))
        .layout_engine(Arc::new(EmptyLayoutEngine))
        .glyph_resolver(Arc::new(Outline))
        .build()
        .await
        .expect("parser");
    let bytes: Arc<[u8]> =
        Arc::from(include_bytes!("fixtures/pdf/glyph_recovery.pdf").as_slice());
    let result = parser.parse_bytes(bytes).await.expect("recovered document");
    let recovered: Vec<_> = result
        .pages
        .iter()
        .flat_map(|page| &page.blocks)
        .flat_map(|block| &block.lines)
        .flat_map(|line| &line.text_items)
        .filter(|item| {
            item.repair_actions
                .contains(&docparse_core::RepairAction::GlyphOutlineRecovery)
        })
        .collect();
    assert!(
        !recovered.is_empty(),
        "the injected resolver must reach real extraction"
    );
    assert!(
        recovered
            .iter()
            .all(|item| item.raw_text.contains('✓') && item.bbox.width() > 0.0)
    );
}

/// Verifies the default engine validates missing artifacts during construction.
#[tokio::test]
async fn default_engine_rejects_missing_model() {
    let error = DocParser::from_config(config())
        .await
        .expect_err("default parser must reject absent artifacts");

    assert!(matches!(error, DocParseError::Layout(_)));
}

/// Verifies the synchronous wrapper refuses to nest inside a Tokio runtime.
#[tokio::test]
async fn blocking_wrapper_rejects_async_runtime() {
    let parser = DocParser::builder()
        .config(Arc::new(config()))
        .layout_engine(Arc::new(EmptyLayoutEngine))
        .build()
        .await
        .expect("injected parser must build");

    let error = parser
        .parse_path_blocking(fixture_path())
        .expect_err("nested blocking parse must fail");

    assert!(matches!(error, DocParseError::BlockingInsideRuntime));
}

/// Verifies standalone parsing preserves a positive source page number greater than one.
#[tokio::test]
async fn parse_page_preserves_original_page_number() {
    let parser = DocParser::builder()
        .config(Arc::new(config()))
        .layout_engine(Arc::new(EmptyLayoutEngine))
        .build()
        .await
        .expect("injected parser must build");
    let image = Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(2)
                .height(2)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::<[u8]>::from(vec![255; 12]))
                .build(),
        )
        .expect("standalone image must be valid"),
    );
    let transform = PageTransform::try_from(
        PageTransformInput::builder()
            .page_to_viewport(AffineTransform::identity())
            .viewport_width(100.0)
            .viewport_height(200.0)
            .render_width(2)
            .render_height(2)
            .model_width(800)
            .model_height(800)
            .rotation(PageRotation::Degrees0)
            .build(),
    )
    .expect("standalone transform must be valid");
    let input = PageInput::builder()
        .extracted(
            ExtractedPage::builder()
                .page_number(7)
                .width(100.0)
                .height(200.0)
                .rotation(0)
                .text_items(Vec::new())
                .build(),
        )
        .image(image)
        .transform(transform)
        .build();

    let page = parser
        .parse_page(input)
        .await
        .expect("positive standalone page numbers must parse");

    assert_eq!(page.page_number, 7);
}

/// Explicit artifact construction must reject missing TSR bytes before loading any model or path.
#[tokio::test]
async fn enabled_tsr_requires_explicit_artifacts_in_the_byte_constructor() {
    let config =
        ValidatedConfig::try_from(RawConfig::default()).expect("config");
    let artifacts = docparse_layout::ModelArtifacts {
        model: Arc::from([]),
        config: Arc::from([]),
        manifest: Arc::from([]),
    };
    let error = DocParser::from_artifacts(config, artifacts)
        .await
        .expect_err("incomplete artifact set");
    assert!(matches!(error, DocParseError::MissingTsrArtifacts));
}
