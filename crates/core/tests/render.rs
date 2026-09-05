use std::collections::BTreeMap;
use std::sync::Arc;

use docparse_config::OutputConfig;
use docparse_core::{
    Block, BlockId, DocumentContext, DocumentRelation, DocumentRelations,
    DocumentResult, Evidence, InlineContentStatus, InlineSpan, JsonRenderer,
    LabelSource, Line, LineId, MarkdownRenderer, NodeRef, OverlayRenderer,
    PageResult, RelationKind, RenderView, SchemaVersion, TextItem, TextItemId,
    TextItemRange, TextRenderer, TextSource, WritingDirection,
};
use docparse_layout::{
    Bbox, LayoutLabel, PageImage, PageImageInput, PixelFormat,
};

/// Builds one small canonical result with a missing inline formula.
fn document() -> DocumentResult {
    let bbox = Bbox::try_from([10.0, 10.0, 90.0, 20.0]).expect("valid bbox");
    let block_id = BlockId::fallback(1, &docparse_core::RegionPath::root(), 0);
    let item = TextItem::builder()
        .id(TextItemId::native(1, 0))
        .raw_text("hello".to_owned())
        .bbox(bbox)
        .source(TextSource::Native)
        .build();
    let span = InlineSpan::builder()
        .label(LayoutLabel::InlineFormula)
        .bbox(
            Bbox::try_from([91.0, 10.0, 99.0, 20.0])
                .expect("valid formula bbox"),
        )
        .text_item_range(TextItemRange::new(1, 1))
        .content_status(InlineContentStatus::Missing)
        .build();
    let line = Line::builder()
        .id(LineId::new(&block_id, 0))
        .text("hello".to_owned())
        .bbox(bbox)
        .direction(WritingDirection::LeftToRight)
        .inline_spans(vec![span])
        .text_items(vec![item])
        .build();
    let relation_node = NodeRef::builder()
        .page_number(1)
        .block_id(block_id.clone())
        .line_id(Some(line.id.clone()))
        .build();
    let block = Block::builder()
        .id(block_id)
        .label(LayoutLabel::Text)
        .text("hello".to_owned())
        .raw_label(Some("future<&>".to_owned()))
        .label_source(LabelSource::Fallback)
        .bbox(bbox)
        .final_order(0)
        .semantic_hints(BTreeMap::new())
        .lines(vec![line])
        .build();
    DocumentResult::builder()
        .schema_version(SchemaVersion::V2_0)
        .context(DocumentContext::builder().page_count(1).build())
        .pages(vec![
            PageResult::builder()
                .page_number(1)
                .width(100.0)
                .height(100.0)
                .rotation(0)
                .blocks(vec![block])
                .build(),
        ])
        .relations(DocumentRelations {
            relations: vec![
                DocumentRelation::builder()
                    .kind(RelationKind::HeadingHierarchy)
                    .source(relation_node.clone())
                    .target(relation_node)
                    .score(Some(0.75))
                    .evidence(vec![
                        Evidence::builder()
                            .kind("relation-evidence".to_owned())
                            .build(),
                    ])
                    .build(),
            ],
        })
        .build()
}

/// Verifies every textual renderer remains a read-only projection.
#[test]
fn renderers_preserve_canonical_document() {
    let document = document();
    let before =
        serde_json::to_vec(&document).expect("document must serialize");

    let json = JsonRenderer::render(&document).expect("JSON must render");
    let text =
        TextRenderer::new(RenderView::Raw, "[formula]").render(&document);
    let markdown =
        MarkdownRenderer::new(RenderView::Raw, "[formula]").render(&document);

    assert!(json.contains("schema_version"));
    assert_eq!(text, "hello[formula]");
    assert!(markdown.contains("hello[formula]"));
    assert_eq!(
        before,
        serde_json::to_vec(&document).expect("document must serialize")
    );
}

/// Verifies the overlay emits PNG bytes and XML-escapes model-controlled labels.
#[test]
fn overlay_encodes_background_and_escaped_svg() {
    let document = document();
    let image = PageImage::try_from(
        PageImageInput::builder()
            .width(2)
            .height(2)
            .pixel_format(PixelFormat::Rgb8)
            .data(Arc::<[u8]>::from(vec![255; 12]))
            .build(),
    )
    .expect("test image must be valid");

    let output = OverlayRenderer::render_page(
        &image,
        document.pages.first().expect("test page must exist"),
        "page&1.png",
    )
    .expect("overlay must render");

    assert!(output.png.starts_with(b"\x89PNG"));
    assert!(output.svg.contains("page&amp;1.png"));
    assert!(output.svg.contains("future&lt;&amp;&gt;"));
}

/// Verifies text renderers never infer spaces between separate source items.
#[test]
fn text_renderers_do_not_invent_cross_item_spaces() {
    let mut document = document();
    let line = document
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .and_then(|block| block.lines.first_mut())
        .expect("test line must exist");
    line.inline_spans.clear();
    let first = line.text_items.first_mut().expect("first item must exist");
    first.bbox = Bbox::try_from([10.0, 10.0, 35.0, 20.0])
        .expect("first item bbox must be valid");
    line.text_items.push(
        TextItem::builder()
            .id(TextItemId::native(1, 1))
            .raw_text("world".to_owned())
            .bbox(
                Bbox::try_from([38.0, 10.0, 63.0, 20.0])
                    .expect("second item bbox must be valid"),
            )
            .source(TextSource::Native)
            .build(),
    );

    assert_eq!(
        TextRenderer::new(RenderView::Raw, "[formula]").render(&document),
        "helloworld"
    );
    assert!(
        MarkdownRenderer::new(RenderView::Raw, "[formula]")
            .render(&document)
            .contains("helloworld")
    );
}

/// Verifies JSON visibility options clear details without mutating or breaking schema shape.
#[test]
fn json_renderer_applies_evidence_and_diagnostics_visibility() {
    let mut document = document();
    document
        .pages
        .first_mut()
        .expect("test page must exist")
        .diagnostics
        .insert("internal".to_owned(), "detail".to_owned());
    document
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .expect("test block must exist")
        .evidence
        .push(Evidence::builder().kind("assignment".to_owned()).build());
    let before = document.clone();
    let options = OutputConfig::builder()
        .formula_placeholder("[formula]".to_owned())
        .include_evidence(false)
        .include_diagnostics(false)
        .build();

    let rendered = JsonRenderer::render_with_config(&document, &options)
        .expect("configured JSON must render");
    let view: DocumentResult =
        serde_json::from_str(&rendered).expect("configured JSON must decode");

    let page = view.pages.first().expect("rendered page must exist");
    let block = page.blocks.first().expect("rendered block must exist");
    let relation = view
        .relations
        .relations
        .first()
        .expect("rendered relation must exist");
    assert!(page.diagnostics.is_empty());
    assert!(block.evidence.is_empty());
    assert!(relation.evidence.is_empty());
    assert_eq!(document, before);
}

/// Verifies writer-based JSON preserves every canonical field when visibility is enabled.
#[test]
fn json_writer_matches_complete_canonical_schema() {
    let document = document();
    let options = OutputConfig::builder()
        .formula_placeholder("[formula]".to_owned())
        .include_evidence(true)
        .include_diagnostics(true)
        .build();
    let mut streamed = Vec::new();

    JsonRenderer::write_with_config(&document, &options, &mut streamed)
        .expect("configured JSON must stream");

    let expected = JsonRenderer::render(&document)
        .expect("canonical JSON must render")
        .into_bytes();
    assert_eq!(streamed, expected);
}
