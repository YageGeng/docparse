use std::collections::BTreeMap;
use std::io::Write;
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

/// Structured text and Markdown preserve physical indentation while leaving source text and formula ranges intact.
#[test]
fn structured_renderers_keep_geometry_based_whitespace() {
    for label in [
        LayoutLabel::Algorithm,
        LayoutLabel::Content,
        LayoutLabel::Chart,
    ] {
        let fenced =
            matches!(label, LayoutLabel::Algorithm | LayoutLabel::Chart);
        let mut document = document();
        let block = document
            .pages
            .first_mut()
            .expect("page")
            .blocks
            .first_mut()
            .expect("block");
        block.label = label;
        let template = block.lines.first().expect("line").clone();
        block.lines = [
            ("begin", 10.0, 10.0),
            ("nested", 30.0, 25.0),
            ("2:", 10.0, 40.0),
            ("return", 30.0, 40.0),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (text, left, top))| {
            let mut line = template.clone();
            line.id = LineId::new(&block.id, index as u32);
            line.inline_spans.clear();
            line.text = text.to_owned();
            line.bbox = Bbox::try_from([
                left,
                top,
                left + text.chars().count() as f64 * 5.0,
                top + 10.0,
            ])
            .expect("bbox");
            let item = line.text_items.first_mut().expect("item");
            item.raw_text = text.to_owned();
            item.bbox = line.bbox;
            line
        })
        .collect();
        let before = serde_json::to_value(&document).expect("snapshot");
        assert_eq!(
            TextRenderer::new(RenderView::Semantic, "[formula]")
                .render(&document),
            "begin\n    nested\n2:  return"
        );
        assert_eq!(
            MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                .render(&document),
            if fenced {
                "```\nbegin\n    nested\n2:  return\n```"
            } else {
                "begin  \n&nbsp;&nbsp;&nbsp;&nbsp;nested  \n2:&nbsp;&nbsp;return"
            }
        );
        assert_eq!(serde_json::to_value(&document).expect("snapshot"), before);
    }
}

/// Image Markdown keeps delivered pixels while native-only image text stays out of semantic output.
#[test]
fn image_markdown_uses_inline_and_file_delivery() {
    use docparse_core::{
        FigureDelivery, FigureImage, FigureMediaType, FigureSource,
        PageImageAsset,
    };

    let mut document = document();
    let page = document.pages.first_mut().expect("page");
    let block = page.blocks.first_mut().expect("block");
    block.label = LayoutLabel::Image;
    block.lines.first_mut().expect("line").inline_spans.clear();
    block.image = Some(
        FigureImage::builder()
            .source(FigureSource::Embedded)
            .media_type(FigureMediaType::Png)
            .width(1)
            .height(1)
            .delivery(FigureDelivery::Inline {
                data_base64: "AA==".into(),
            })
            .build(),
    );
    page.images.push(PageImageAsset {
        id: "p1:img:1".into(),
        bbox: Bbox::try_from([10.0, 30.0, 90.0, 40.0]).expect("asset bbox"),
        image: FigureImage::builder()
            .source(FigureSource::Raster)
            .media_type(FigureMediaType::Png)
            .width(1)
            .height(1)
            .delivery(FigureDelivery::File {
                path: "/tmp/figure 1.png".into(),
            })
            .build(),
    });
    docparse_core::ResultValidator::validate(&document)
        .expect("valid image result");
    let renderer = MarkdownRenderer::new(RenderView::Semantic, "[formula]");
    assert_eq!(
        renderer.render(&document),
        "![image](data:image/png;base64,AA==)\n\n![image](</tmp/figure 1.png>)"
    );
    let mut portable = Vec::new();
    renderer
        .write_with_images(&document, &mut portable, |image, path, writer| {
            assert_eq!(image.media_type.as_str(), "image/png");
            assert_eq!(path, "/tmp/figure 1.png");
            writer.write_all(b"data:image/png;base64,AA==")
        })
        .expect("portable Markdown");
    assert_eq!(
        String::from_utf8(portable).expect("UTF-8 Markdown"),
        "![image](data:image/png;base64,AA==)\n\n![image](data:image/png;base64,AA==)"
    );
    document
        .context
        .metadata
        .insert("ocr_enabled".into(), "true".into());
    assert_eq!(
        renderer.render(&document),
        "![image](data:image/png;base64,AA==)\n\nhello\n\n![image](</tmp/figure 1.png>)"
    );
    document
        .context
        .metadata
        .insert("ocr_enabled".into(), "false".into());
    assert_eq!(
        renderer.render(&document),
        "![image](data:image/png;base64,AA==)\n\n![image](</tmp/figure 1.png>)"
    );
    document
        .context
        .metadata
        .insert("ocr_enabled".into(), "true".into());
    document
        .pages
        .first_mut()
        .expect("page")
        .blocks
        .first_mut()
        .expect("block")
        .lines
        .first_mut()
        .expect("line")
        .text_items
        .first_mut()
        .expect("item")
        .source = TextSource::Ocr;
    assert_eq!(
        renderer.render(&document),
        "![image](data:image/png;base64,AA==)\n\nhello\n\n![image](</tmp/figure 1.png>)"
    );
}

/// Page chrome stays visible within separated pages, while watermarks never enter Markdown.
#[test]
fn markdown_separates_pages_without_watermarks() {
    let mut document = document();
    let first = document.pages.first_mut().expect("first page");
    let header = first.blocks.first_mut().expect("header");
    header.label = LayoutLabel::Header;
    header.text = "Running title".into();
    let line = header.lines.first_mut().expect("line");
    line.text = header.text.clone();
    line.text_items.first_mut().expect("item").raw_text = line.text.clone();
    line.inline_spans.clear();
    let mut second = first.clone();
    second.page_number = 2;
    let number = second.blocks.first_mut().expect("number");
    number.id = BlockId::fallback(2, &docparse_core::RegionPath::root(), 0);
    number.label = LayoutLabel::Number;
    number.text = "2".into();
    let line = number.lines.first_mut().expect("line");
    line.id = LineId::new(&number.id, 0);
    line.text = number.text.clone();
    let item = line.text_items.first_mut().expect("item");
    item.id = TextItemId::native(2, 0);
    item.raw_text = line.text.clone();
    document.pages.push(second);
    document.context.page_count = 2;
    document
        .relations
        .relations
        .first_mut()
        .expect("relation")
        .kind = RelationKind::RepeatedChrome;
    docparse_core::ResultValidator::validate(&document)
        .expect("valid document");
    let renderer = MarkdownRenderer::new(RenderView::Semantic, "[formula]");
    assert_eq!(renderer.render(&document), "Running title\n\n---\n\n2");

    let number = document
        .pages
        .get_mut(1)
        .expect("second page")
        .blocks
        .first_mut()
        .expect("block");
    number.label = LayoutLabel::Watermark;
    assert_eq!(renderer.render(&document), "Running title");
    assert_eq!(
        MarkdownRenderer::new(RenderView::Raw, "[formula]").render(&document),
        "<!-- page 1 -->\n\nRunning title\n\n<!-- page 2 -->"
    );
}

/// Algorithm fences contain literal Markdown syntax and grow when the source itself contains backtick fences.
#[test]
fn algorithm_fences_preserve_literal_content() {
    for (source, fence) in [
        ("return x_y < z & w", "```"),
        ("```", "````"),
        ("`````", "``````"),
    ] {
        let mut document = document();
        let block = document
            .pages
            .first_mut()
            .expect("page")
            .blocks
            .first_mut()
            .expect("block");
        block.label = LayoutLabel::Algorithm;
        let line = block.lines.first_mut().expect("line");
        line.inline_spans.clear();
        line.text = source.to_owned();
        line.text_items.first_mut().expect("item").raw_text = source.to_owned();
        let before = document.clone();
        let expected = format!("{fence}\n{source}\n{fence}");
        for view in [RenderView::Semantic, RenderView::Raw] {
            let markdown =
                MarkdownRenderer::new(view, "[formula]").render(&document);
            assert!(markdown.ends_with(&expected), "{markdown:?}");
            assert_eq!(document, before);
        }
    }
}

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

/// JSON keeps both formula representations even when evidence and diagnostics are hidden.
#[test]
fn recognized_formula_json_and_markdown_preserve_source_text() {
    let original = document();
    let page = original.pages.first().expect("page");
    let block = page.blocks.first().expect("block");
    let line = block.lines.first().expect("line");
    let formula = docparse_core::FormulaResult::builder()
        .id(docparse_core::ModelRegionId::detected(1, 0))
        .label(LayoutLabel::InlineFormula)
        .bbox(Bbox::try_from([91.0, 10.0, 99.0, 20.0]).expect("formula bbox"))
        .block_id(Some(block.id.clone()))
        .line_id(Some(line.id.clone()))
        .text_item_range(Some(TextItemRange::new(1, 1)))
        .latex(Some("x^{2}".into()))
        .markdown(Some("$x^{2}$".into()))
        .build();
    let mut recognized = original.clone();
    recognized
        .pages
        .first_mut()
        .expect("page")
        .formulas
        .push(formula);
    let visibility = OutputConfig::builder()
        .formula_placeholder("[formula]".into())
        .include_evidence(false)
        .include_diagnostics(false)
        .build();
    let json = serde_json::to_value(JsonRenderer::view_with_config(
        &recognized,
        &visibility,
    ))
    .expect("JSON view");
    assert_eq!(
        json.pointer("/pages/0/formulas/0/latex")
            .and_then(serde_json::Value::as_str),
        Some("x^{2}")
    );
    assert_eq!(
        json.pointer("/pages/0/formulas/0/markdown")
            .and_then(serde_json::Value::as_str),
        Some("$x^{2}$")
    );
    assert_eq!(
        MarkdownRenderer::new(RenderView::Semantic, "[formula]")
            .render(&recognized),
        "hello $x^{2}$"
    );
    assert_eq!(
        recognized.pages.first().expect("page").blocks,
        original.pages.first().expect("original").blocks
    );
    let mut invalid = recognized.clone();
    invalid
        .pages
        .first_mut()
        .expect("page")
        .formulas
        .first_mut()
        .expect("formula")
        .markdown = Some("$different$".into());
    docparse_core::ResultValidator::validate(&invalid)
        .expect_err("JSON formula representations must agree");
}

/// A measured formula must preserve adjacent prose, wrapped words, and list boundaries.
#[test]
fn inline_formula_preserves_prose_and_list_markers() {
    let mut document = document();
    let page = document.pages.first_mut().expect("page");
    let block = page.blocks.first_mut().expect("block");
    let line = block.lines.first_mut().expect("line");
    let item = line.text_items.first_mut().expect("item");
    item.raw_text = "learning rate of 7, next over-".into();
    line.text = item.raw_text.clone();
    block.text = line.text.clone();
    line.inline_spans.clear();
    page.formulas.push(
        docparse_core::FormulaResult::builder()
            .id(docparse_core::ModelRegionId::detected(1, 0))
            .label(LayoutLabel::InlineFormula)
            .bbox(item.bbox)
            .block_id(Some(block.id.clone()))
            .line_id(Some(line.id.clone()))
            .text_item_range(Some(TextItemRange::new(0, 1)))
            .text_spans(vec![docparse_core::TableTextSpan {
                text_item_id: item.id.clone(),
                byte_range: 17..18,
                bbox: item.bbox,
            }])
            .latex(Some("7".into()))
            .markdown(Some("$7$".into()))
            .build(),
    );
    let block = page.blocks.first_mut().expect("block");
    let mut continuation = block.lines.first().expect("line").clone();
    continuation.id = LineId::new(&block.id, 1);
    continuation.text = "lap by assigning its volume".into();
    continuation.bbox =
        Bbox::try_from([10.0, 20.0, 90.0, 30.0]).expect("line bbox");
    let item = continuation.text_items.first_mut().expect("item");
    item.id = TextItemId::native(1, 1);
    item.raw_text = continuation.text.clone();
    item.bbox = continuation.bbox;
    block.lines.push(continuation);
    let mut list_line = block.lines.last().expect("continuation").clone();
    list_line.id = LineId::new(&block.id, 2);
    list_line.text = "* another [item]".into();
    list_line.bbox =
        Bbox::try_from([10.0, 30.0, 90.0, 40.0]).expect("list bbox");
    let item = list_line.text_items.first_mut().expect("list item");
    item.id = TextItemId::native(1, 2);
    item.raw_text = list_line.text.clone();
    item.bbox = list_line.bbox;
    block.lines.push(list_line);
    block.bbox = Bbox::try_from([10.0, 10.0, 90.0, 40.0]).expect("block bbox");
    block.text =
        "learning rate of 7, next over- lap by assigning its volume * another [item]"
            .into();
    docparse_core::ResultValidator::validate(&document)
        .expect("valid parsed document");
    assert_eq!(
        MarkdownRenderer::new(RenderView::Semantic, "[formula]")
            .render(&document),
        "learning rate of $7$, next overlap by assigning its volume\n* another \\[item\\]"
    );
    assert!(
        MarkdownRenderer::new(RenderView::Raw, "[formula]")
            .render(&document)
            .contains("over-\nlap")
    );
}

/// Unselected source words between two formula slices must survive the Markdown projection.
#[test]
fn disjoint_formula_slices_preserve_intervening_source() {
    let mut document = document();
    let page = document.pages.first_mut().expect("page");
    let block = page.blocks.first_mut().expect("block");
    let line = block.lines.first_mut().expect("line");
    let item = line.text_items.first_mut().expect("item");
    item.raw_text = "a 引用 b".into();
    line.text = item.raw_text.clone();
    block.text = line.text.clone();
    line.inline_spans.clear();
    page.formulas.push(
        docparse_core::FormulaResult::builder()
            .id(docparse_core::ModelRegionId::detected(1, 0))
            .label(LayoutLabel::InlineFormula)
            .bbox(item.bbox)
            .block_id(Some(block.id.clone()))
            .line_id(Some(line.id.clone()))
            .text_item_range(Some(TextItemRange::new(0, 1)))
            .text_spans(vec![
                docparse_core::TableTextSpan {
                    text_item_id: item.id.clone(),
                    byte_range: 0..1,
                    bbox: item.bbox,
                },
                docparse_core::TableTextSpan {
                    text_item_id: item.id.clone(),
                    byte_range: 9..10,
                    bbox: item.bbox,
                },
            ])
            .latex(Some("a+b".into()))
            .markdown(Some("$a+b$".into()))
            .build(),
    );
    assert_eq!(
        MarkdownRenderer::new(RenderView::Semantic, "[formula]")
            .render(&document),
        "$a+b$ 引用"
    );
}

/// Semantic prose heals wrapped words while raw and non-prose output retain physical lines.
#[test]
fn character_cleanup_is_a_read_only_presentation() {
    let mut document = document();
    let block = document
        .pages
        .first_mut()
        .expect("page")
        .blocks
        .first_mut()
        .expect("block");
    let template = block.lines.first().expect("line").clone();
    block.lines = ["  architec-", "    ture   works", "  well-", "  Known"]
        .into_iter()
        .enumerate()
        .map(|(index, text)| {
            let mut line = template.clone();
            line.id = LineId::new(&block.id, index as u32);
            line.inline_spans.clear();
            line.text = text.to_owned();
            line.text_items.first_mut().expect("item").raw_text =
                text.to_owned();
            line
        })
        .collect();
    let before = document.clone();
    assert_eq!(
        MarkdownRenderer::new(RenderView::Semantic, "[formula]")
            .render(&document),
        "architecture works well- Known"
    );
    assert!(
        MarkdownRenderer::new(RenderView::Raw, "[formula]")
            .render(&document)
            .contains("architec-\n    ture")
    );
    assert_eq!(
        TextRenderer::new(RenderView::Raw, "[formula]").render(&document),
        "architec-\n  ture   works\nwell-\nKnown"
    );
    assert_eq!(document, before);
    document
        .pages
        .first_mut()
        .expect("page")
        .blocks
        .first_mut()
        .expect("block")
        .label = LayoutLabel::Algorithm;
    // Algorithm code fences preserve literal whitespace without prose dehyphenation.
    assert!(
        MarkdownRenderer::new(RenderView::Semantic, "[formula]")
            .render(&document)
            .contains("architec-\n    ture")
    );
}

/// Browser JSON omits absorbed layout bounds while direct serialization retains provenance.
#[test]
fn configured_json_omits_merged_layout_sources() {
    let mut document = document();
    let block = document
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .expect("block");
    block.source_regions = [1, 2]
        .into_iter()
        .map(|index| {
            docparse_core::SourceRegionEvidence::builder()
                .label(LayoutLabel::Text)
                .model_region_id(Some(docparse_core::ModelRegionId::detected(
                    1, index,
                )))
                .bbox(block.bbox)
                .geometry_source(
                    docparse_layout::GeometrySource::DerivedFromBbox,
                )
                .build()
        })
        .collect();
    block.source_region = block.source_regions.first().cloned();
    let direct = serde_json::to_value(&document).expect("direct JSON");
    let rendered =
        JsonRenderer::render_with_config(&document, &OutputConfig::default())
            .expect("configured JSON");
    let configured: serde_json::Value =
        serde_json::from_str(&rendered).expect("parse JSON");
    assert!(direct.pointer("/pages/0/blocks/0/source_regions").is_some());
    assert!(
        configured
            .pointer("/pages/0/blocks/0/source_region")
            .is_none()
    );
    assert!(
        configured
            .pointer("/pages/0/blocks/0/source_regions")
            .is_none()
    );
    let _: DocumentResult = serde_json::from_value(configured)
        .expect("browser JSON stays schema-compatible");
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
    // Outer envelopes must retain the same filtering when serializing the borrowed view directly.
    let borrowed: DocumentResult = serde_json::from_value(
        serde_json::to_value(JsonRenderer::view_with_config(
            &document, &options,
        ))
        .expect("borrowed view"),
    )
    .expect("configured schema");
    assert_eq!(borrowed, view);
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
