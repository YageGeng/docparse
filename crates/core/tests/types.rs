use std::collections::BTreeMap;

use docparse_core::{
    Block, BlockId, DocumentContext, DocumentRelations, DocumentResult,
    InlineContentStatus, InlineSpan, LabelSource, Line, LineId, ModelRegionId,
    NodeRef, PageResult, RepairAction, ResultValidator, SchemaVersion,
    SourceRegionEvidence, TextItem, TextItemId, TextItemRange, TextSource,
    ValidationError, WritingDirection,
};
use docparse_layout::{Bbox, GeometrySource, LayoutLabel};

/// Constructs a valid one-page aggregate with nested lines, text, and an inline span.
fn document_fixture() -> DocumentResult {
    let model_region_id = ModelRegionId::detected(1, 4);
    let block_id = BlockId::model(1, 4, 0);
    let first_item = TextItem::builder()
        .id(TextItemId::native(1, 0))
        .raw_text("Hello ".to_owned())
        .bbox(Bbox::try_from([10.0, 10.0, 40.0, 20.0]).expect("valid bbox"))
        .source(TextSource::Native)
        .build();
    let second_item = TextItem::builder()
        .id(TextItemId::native(1, 1))
        .raw_text("x²".to_owned())
        .bbox(Bbox::try_from([40.0, 10.0, 52.0, 20.0]).expect("valid bbox"))
        .source(TextSource::Native)
        .final_order(1)
        .repair_actions(vec![RepairAction::MergedFragment])
        .build();
    let third_item = TextItem::builder()
        .id(TextItemId::native(1, 2))
        .raw_text("World".to_owned())
        .bbox(Bbox::try_from([10.0, 30.0, 42.0, 40.0]).expect("valid bbox"))
        .source(TextSource::Native)
        .build();
    let first_line_id = LineId::new(&block_id, 0);
    let second_line_id = LineId::new(&block_id, 1);
    let first_line = Line::builder()
        .id(first_line_id)
        .text("Hello x²".to_owned())
        .bbox(Bbox::try_from([10.0, 10.0, 52.0, 20.0]).expect("valid bbox"))
        .direction(WritingDirection::LeftToRight)
        .inline_spans(vec![
            InlineSpan::builder()
                .label(LayoutLabel::InlineFormula)
                .bbox(
                    Bbox::try_from([40.0, 10.0, 52.0, 20.0])
                        .expect("valid bbox"),
                )
                .text_item_range(TextItemRange::new(1, 2))
                .extracted_text(Some("x²".to_owned()))
                .content_status(InlineContentStatus::Complete)
                .build(),
        ])
        .text_items(vec![first_item, second_item])
        .build();
    let second_line = Line::builder()
        .id(second_line_id)
        .text("World".to_owned())
        .bbox(Bbox::try_from([10.0, 30.0, 42.0, 40.0]).expect("valid bbox"))
        .direction(WritingDirection::LeftToRight)
        .text_items(vec![third_item])
        .build();
    let source_region = SourceRegionEvidence::builder()
        .model_region_id(Some(model_region_id.clone()))
        .bbox(Bbox::try_from([8.0, 8.0, 55.0, 42.0]).expect("valid bbox"))
        .geometry_source(GeometrySource::DerivedFromBbox)
        .confidence(Some(0.95))
        .model_order(Some(3))
        .build();
    let block = Block::builder()
        .id(block_id)
        .label(LayoutLabel::Text)
        .text("Hello x² World".to_owned())
        .raw_label(Some("text".to_owned()))
        .label_source(LabelSource::Model)
        .confidence(Some(0.95))
        .bbox(Bbox::try_from([10.0, 10.0, 52.0, 40.0]).expect("valid bbox"))
        .source_region(Some(source_region))
        .model_region_id(Some(model_region_id))
        .model_order(Some(3))
        .final_order(0)
        .semantic_hints(BTreeMap::from([(
            "flow".to_owned(),
            "body".to_owned(),
        )]))
        .lines(vec![first_line, second_line])
        .build();
    let page = PageResult::builder()
        .page_number(1)
        .width(612.0)
        .height(792.0)
        .rotation(0)
        .blocks(vec![block])
        .build();
    let context = DocumentContext::builder()
        .page_count(1)
        .body_font_size(Some(10.0))
        .model_revision(Some("fixture-v2".to_owned()))
        .build();
    DocumentResult::builder()
        .schema_version(SchemaVersion::V2_0)
        .context(context)
        .pages(vec![page])
        .relations(DocumentRelations::default())
        .build()
}

/// Verifies schema 2 exposes one canonical text field without a normalized alias.
#[test]
fn schema_v2_exposes_only_raw_text() {
    let value = serde_json::to_value(document_fixture())
        .expect("document must serialize");
    let item = value
        .pointer("/pages/0/blocks/0/lines/0/text_items/0")
        .and_then(serde_json::Value::as_object)
        .expect("first text item must serialize as an object");

    assert_eq!(
        value.get("schema_version"),
        Some(&serde_json::Value::String("2.0".to_owned()))
    );
    assert_eq!(
        item.get("raw_text").and_then(serde_json::Value::as_str),
        Some("Hello ")
    );
    assert!(!item.contains_key("normalized_text"));
}

/// Verifies every serialized block exposes its single-space line summary.
#[test]
fn block_serializes_single_space_text_summary() {
    let value = serde_json::to_value(document_fixture())
        .expect("document must serialize");

    assert_eq!(
        value
            .pointer("/pages/0/blocks/0/text")
            .and_then(serde_json::Value::as_str),
        Some("Hello x² World")
    );
}

/// Verifies all nested fact and evidence fields survive JSON round-trip.
#[test]
fn nested_schema_round_trips_without_reordering() {
    let document = document_fixture();

    let json =
        serde_json::to_vec_pretty(&document).expect("document must serialize");
    let decoded: DocumentResult =
        serde_json::from_slice(&json).expect("document must deserialize");

    assert_eq!(decoded, document);
    ResultValidator::validate(&decoded)
        .expect("round-tripped document must validate");
}

/// Verifies known-major future-minor payloads can add ignorable fields.
#[test]
fn future_minor_with_unknown_optional_fields_is_accepted() {
    let mut value = serde_json::to_value(document_fixture())
        .expect("document must serialize");
    let root = value
        .as_object_mut()
        .expect("document JSON must be an object");
    root.insert(
        "schema_version".to_owned(),
        serde_json::Value::String("2.9".to_owned()),
    );
    root.insert("future_optional".to_owned(), serde_json::Value::Bool(true));

    let decoded: DocumentResult = serde_json::from_value(value)
        .expect("known major must accept future optional fields");

    assert_eq!(decoded.schema_version, SchemaVersion::new(2, 9));
}

/// Verifies an unknown schema major is rejected during deserialization.
#[test]
fn unknown_schema_major_is_rejected() {
    let mut value = serde_json::to_value(document_fixture())
        .expect("document must serialize");
    value
        .as_object_mut()
        .expect("document JSON must be an object")
        .insert(
            "schema_version".to_owned(),
            serde_json::Value::String("1.0".to_owned()),
        );

    let error = serde_json::from_value::<DocumentResult>(value)
        .expect_err("unknown major must fail");

    assert!(error.to_string().contains("unsupported schema major 1"));
}

/// Verifies stable IDs reject empty deserialized strings.
#[test]
fn empty_stable_id_is_rejected() {
    let error = serde_json::from_str::<TextItemId>("\"\"")
        .expect_err("empty IDs must fail");

    assert!(error.to_string().contains("empty"));
}

/// Verifies duplicate ownership and cross-page references report exact node paths.
#[test]
fn validator_rejects_duplicate_and_cross_page_text_ids() {
    let mut duplicate = document_fixture();
    let first_line = duplicate
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .and_then(|block| block.lines.first_mut())
        .expect("fixture first line must exist");
    let duplicate_item = first_line
        .text_items
        .first()
        .expect("fixture first item must exist")
        .clone();
    first_line.text_items.push(duplicate_item);

    let error = ResultValidator::validate(&duplicate)
        .expect_err("duplicate ownership must fail");
    assert!(matches!(
        error,
        ValidationError::InvalidNode { path, .. }
            if path == "pages[0].blocks[0].lines[0].text_items[2]"
    ));

    let mut cross_page = document_fixture();
    cross_page
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .and_then(|block| block.lines.first_mut())
        .and_then(|line| line.text_items.first_mut())
        .expect("fixture first item must exist")
        .id = TextItemId::native(2, 0);
    let error = ResultValidator::validate(&cross_page)
        .expect_err("cross-page ownership must fail");
    assert!(matches!(
        error,
        ValidationError::InvalidNode { path, .. }
            if path == "pages[0].blocks[0].lines[0].text_items[0]"
    ));
}

/// Verifies non-finite geometry and incorrect final order are rejected.
#[test]
fn validator_rejects_invalid_geometry_and_order() {
    let mut invalid_geometry = document_fixture();
    invalid_geometry
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .expect("fixture block must exist")
        .bbox = Bbox::builder()
        .left(f64::NAN)
        .top(0.0)
        .right(1.0)
        .bottom(1.0)
        .build();
    assert!(ResultValidator::validate(&invalid_geometry).is_err());

    let mut invalid_order = document_fixture();
    invalid_order
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .expect("fixture block must exist")
        .final_order = 3;
    let error = ResultValidator::validate(&invalid_order)
        .expect_err("non-contiguous final order must fail");
    assert!(matches!(
        error,
        ValidationError::InvalidNode { path, .. }
            if path == "pages[0].blocks[0]"
    ));
}

/// Verifies one model region cannot be represented by multiple final Blocks.
#[test]
fn validator_rejects_duplicate_model_region_blocks() {
    let mut document = document_fixture();
    let page = document.pages.first_mut().expect("fixture page must exist");
    let mut duplicate = page
        .blocks
        .first()
        .expect("fixture model block must exist")
        .clone();
    duplicate.id = BlockId::model(1, 4, 1);
    duplicate.text.clear();
    duplicate.lines.clear();
    duplicate.final_order = 1;
    page.blocks.push(duplicate);

    let error = ResultValidator::validate(&document)
        .expect_err("duplicate model region ownership must fail");

    assert!(matches!(
        error,
        ValidationError::InvalidNode { path, .. }
            if path == "pages[0].blocks[1].model_region_id"
    ));
}

/// Verifies text-item final order must match its physical line position.
#[test]
fn validator_rejects_incorrect_text_item_final_order() {
    let mut document = document_fixture();
    document
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .and_then(|block| block.lines.first_mut())
        .and_then(|line| line.text_items.get_mut(1))
        .expect("fixture second item must exist")
        .final_order = 9;

    let error = ResultValidator::validate(&document)
        .expect_err("incorrect text-item final order must fail");

    assert!(matches!(
        error,
        ValidationError::InvalidNode { path, .. }
            if path == "pages[0].blocks[0].lines[0].text_items[1]"
    ));
}

/// Verifies line text cannot diverge from its ordered raw text facts.
#[test]
fn validator_rejects_line_text_not_derived_from_raw_items() {
    let mut document = document_fixture();
    document
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .and_then(|block| block.lines.first_mut())
        .expect("fixture first line must exist")
        .text = "inferred replacement".to_owned();

    let error = ResultValidator::validate(&document)
        .expect_err("line text must remain a raw-text projection");

    assert!(matches!(
        error,
        ValidationError::InvalidNode { path, .. }
            if path == "pages[0].blocks[0].lines[0].text"
    ));
}

/// Verifies Block text cannot diverge from its ordered Line summaries.
#[test]
fn validator_rejects_block_text_not_derived_from_lines() {
    let mut document = document_fixture();
    document
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .expect("fixture first block must exist")
        .text = "wrong summary".to_owned();

    let error = ResultValidator::validate(&document)
        .expect_err("Block text must remain a Line projection");

    assert!(matches!(
        error,
        ValidationError::InvalidNode { path, .. }
            if path == "pages[0].blocks[0].text"
    ));
}

/// Verifies validator comparison follows the physical-line algorithm summary contract.
#[test]
fn validator_accepts_algorithm_summary_with_newlines() {
    let mut document = document_fixture();
    let block = document
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .expect("fixture first block must exist");
    block.label = LayoutLabel::Algorithm;
    let second_line = block
        .lines
        .get_mut(1)
        .expect("fixture second line must exist");
    second_line
        .text_items
        .first_mut()
        .expect("fixture second line must own one item")
        .raw_text = "  World".to_owned();
    second_line.text = "  World".to_owned();
    block.text = "Hello x²\n  World".to_owned();

    ResultValidator::validate(&document)
        .expect("algorithm summary must preserve physical line boundaries");
}

/// Verifies validator comparison preserves an encoded prose hyphen boundary.
#[test]
fn validator_accepts_encoded_hyphenated_prose_summary() {
    let mut document = document_fixture();
    let block = document
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .expect("fixture first block must exist");
    let first_line = block
        .lines
        .first_mut()
        .expect("fixture first line must exist");
    first_line.inline_spans.clear();
    first_line
        .text_items
        .first_mut()
        .expect("fixture first line must own its first item")
        .raw_text = "pro".to_owned();
    let hyphen_item = first_line
        .text_items
        .get_mut(1)
        .expect("fixture first line must own its second item");
    hyphen_item.raw_text = "-".to_owned();
    hyphen_item.repair_actions = vec![RepairAction::EncodedHyphen];
    first_line.text = "pro-".to_owned();
    let second_line = block
        .lines
        .get_mut(1)
        .expect("fixture second line must exist");
    second_line
        .text_items
        .first_mut()
        .expect("fixture second line must own one item")
        .raw_text = "grams".to_owned();
    second_line.text = "grams".to_owned();
    block.text = "pro-grams".to_owned();

    ResultValidator::validate(&document)
        .expect("prose summary must preserve an encoded line-end hyphen");
}

/// Verifies validator comparison retains a lexical hyphen before a capitalized line.
#[test]
fn validator_accepts_capitalized_hyphenated_prose_summary() {
    let mut document = document_fixture();
    let block = document
        .pages
        .first_mut()
        .and_then(|page| page.blocks.first_mut())
        .expect("fixture first block must exist");
    let first_line = block
        .lines
        .first_mut()
        .expect("fixture first line must exist");
    first_line.inline_spans.clear();
    first_line
        .text_items
        .first_mut()
        .expect("fixture first line must own its first item")
        .raw_text = "Fine".to_owned();
    let hyphen_item = first_line
        .text_items
        .get_mut(1)
        .expect("fixture first line must own its second item");
    hyphen_item.raw_text = "-".to_owned();
    hyphen_item.repair_actions = vec![RepairAction::EncodedHyphen];
    first_line.text = "Fine-".to_owned();
    let second_line = block
        .lines
        .get_mut(1)
        .expect("fixture second line must exist");
    second_line
        .text_items
        .first_mut()
        .expect("fixture second line must own one item")
        .raw_text = "Tuning".to_owned();
    second_line.text = "Tuning".to_owned();
    block.text = "Fine-Tuning".to_owned();

    ResultValidator::validate(&document)
        .expect("capitalized continuation must preserve the lexical hyphen");
}

/// Verifies repeated construction and serialization remain byte deterministic.
#[test]
fn schema_serialization_is_deterministic() {
    let expected = serde_json::to_vec(&document_fixture())
        .expect("document must serialize");

    for _ in 0..100 {
        let actual = serde_json::to_vec(&document_fixture())
            .expect("document must serialize");
        assert_eq!(actual, expected);
    }
}

/// Verifies relation references use stable IDs without owning nested nodes.
#[test]
fn node_reference_is_a_stable_non_owning_value() {
    let block_id = BlockId::model(1, 4, 0);
    let reference = NodeRef::builder()
        .page_number(1)
        .block_id(block_id.clone())
        .line_id(Some(LineId::new(&block_id, 0)))
        .build();

    let json =
        serde_json::to_string(&reference).expect("reference must serialize");

    assert!(json.contains(block_id.as_str()));
    assert!(!json.contains("text_items"));
}
