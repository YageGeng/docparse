use docparse_layout::{
    AffineTransform, PageRotation, PageTransform, PageTransformInput,
};

/// Deserializing an IPC transform must retain geometry validation instead of accepting zero dimensions.
#[test]
fn serialized_transform_preserves_mapping_and_rejects_invalid_dimensions() {
    let input = PageTransformInput::builder()
        .page_to_viewport(
            AffineTransform::builder()
                .a(1.0)
                .b(0.0)
                .c(0.0)
                .d(-1.0)
                .e(0.0)
                .f(100.0)
                .build(),
        )
        .viewport_width(200.0)
        .viewport_height(100.0)
        .render_width(400)
        .render_height(200)
        .model_width(800)
        .model_height(800)
        .rotation(PageRotation::Degrees0)
        .build();
    let transform = PageTransform::try_from(input).expect("valid transform");
    let mut json =
        serde_json::to_value(&transform).expect("serialize transform");
    assert_eq!(
        serde_json::from_value::<PageTransform>(json.clone())
            .expect("round trip"),
        transform
    );
    *json
        .get_mut("render_width")
        .expect("serialized render width") = serde_json::json!(0);
    serde_json::from_value::<PageTransform>(json)
        .expect_err("zero render width must be rejected");
}
