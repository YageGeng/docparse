use docparse_layout::LayoutLabel;

/// Parser-derived watermarks must serialize canonically without extending the model's class IDs.
#[test]
fn watermark_is_a_semantic_label_without_a_model_index() {
    let label = LayoutLabel::from("watermark");
    assert_eq!(
        serde_json::to_value(&label).expect("label JSON"),
        "watermark"
    );
    assert_eq!(label.idx(), None);
    assert_eq!(LayoutLabel::ALL.len(), 25);
    LayoutLabel::try_from(25).expect_err("watermark must not extend model IDs");
}

/// Keeps numeric model classes, raw names, and the existing JSON schema aligned.
#[test]
fn known_labels_preserve_the_fixed_model_order() {
    let expected = [
        "abstract",
        "algorithm",
        "aside_text",
        "chart",
        "content",
        "display_formula",
        "doc_title",
        "figure_title",
        "footer",
        "footer_image",
        "footnote",
        "formula_number",
        "header",
        "header_image",
        "image",
        "inline_formula",
        "number",
        "paragraph_title",
        "reference",
        "reference_content",
        "seal",
        "table",
        "text",
        "vertical_text",
        "vision_footnote",
    ];

    assert_eq!(LayoutLabel::ALL.len(), expected.len());
    for (index, raw) in expected.into_iter().enumerate() {
        let label = LayoutLabel::try_from(index).expect("known model class");
        assert_eq!(label.to_str(), raw);
        assert_eq!(label.idx(), Some(index));
        assert_eq!(LayoutLabel::from(raw), label);
        assert_eq!(LayoutLabel::from(raw.to_owned()), label);
        assert_eq!(serde_json::to_value(&label).expect("label JSON"), raw);
        assert_eq!(
            serde_json::from_value::<LayoutLabel>(serde_json::json!(raw))
                .expect("known label JSON"),
            label
        );
    }
    assert_eq!(
        LayoutLabel::try_from(22).expect("text ID"),
        LayoutLabel::Text
    );
    assert_eq!(
        LayoutLabel::try_from(21_i64).expect("table ID"),
        LayoutLabel::Table
    );
}

/// Rejects unsupported IDs without truncation or unsigned wraparound.
#[test]
fn invalid_numeric_labels_are_rejected() {
    for index in [25_usize, usize::MAX] {
        let error = LayoutLabel::try_from(index).expect_err("invalid index");
        assert!(error.to_string().contains(&index.to_string()));
    }
    for index in [i64::MIN, -1, 25, i64::MAX] {
        let error = LayoutLabel::try_from(index).expect_err("invalid class ID");
        assert!(error.to_string().contains(&index.to_string()));
    }
    LayoutLabel::try_from(-1).expect_err("negative i32 label index");
    LayoutLabel::try_from(i32::MAX).expect_err("out-of-range i32 label index");
}

/// Retains unknown label text without assigning it a fixed-model index.
#[test]
fn unknown_labels_remain_forward_compatible() {
    let label = LayoutLabel::from("future_widget");
    assert_eq!(label.to_str(), "future_widget");
    assert_eq!(label.idx(), None);
    assert_eq!(
        serde_json::to_value(&label).expect("unknown label JSON"),
        serde_json::json!({ "unknown": "future_widget" })
    );
    let unknown = LayoutLabel::Unknown("text".to_owned());
    assert_eq!(unknown.to_str(), "text");
    assert_eq!(unknown.idx(), None);
}
