use std::collections::BTreeMap;
use std::sync::Arc;

use docparse_layout::{
    AffineTransform, Bbox, GeometryError, GeometrySource, LayoutDetection,
    LayoutEngine, LayoutError, LayoutLabel, LayoutRequest, PageImage,
    PageImageError, PageImageInput, PageRotation, PageTransform,
    PageTransformInput, PixelFormat, Point, Polygon,
};

/// Asserts two geometry values are within the public round-trip tolerance.
fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 0.01,
        "expected {expected}, got {actual}"
    );
}

/// Builds a valid RGB image used by trait-object tests.
fn test_image() -> Arc<PageImage> {
    Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(2)
                .height(2)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::<[u8]>::from(vec![0; 12]))
                .build(),
        )
        .expect("the image dimensions and buffer must agree"),
    )
}

/// Builds an identity page-space transform for trait-object tests.
fn identity_transform() -> PageTransform {
    PageTransform::try_from(
        PageTransformInput::builder()
            .page_to_viewport(AffineTransform::identity())
            .viewport_width(2.0)
            .viewport_height(2.0)
            .render_width(2)
            .render_height(2)
            .model_width(800)
            .model_height(800)
            .rotation(PageRotation::Degrees0)
            .build(),
    )
    .expect("the identity transform must be valid")
}

struct FakeLayoutEngine;

impl LayoutEngine for FakeLayoutEngine {
    /// Returns the stable fake engine name.
    fn name(&self) -> &str {
        "fake-layout"
    }

    /// Returns the stable fake model revision.
    fn model_revision(&self) -> &str {
        "fixture-v1"
    }

    /// Returns known and future labels without changing their raw strings.
    fn detect(
        &self,
        _request: LayoutRequest,
    ) -> docparse_layout::wasm_compat::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    > {
        Box::pin(async move {
            let bbox = Bbox::try_from([0.0, 0.0, 1.0, 1.0])?;
            Ok(vec![
                LayoutDetection::builder()
                    .source_detection_index(0)
                    .raw_label("text".to_owned())
                    .class_id(22)
                    .label(LayoutLabel::Text)
                    .confidence(0.9)
                    .bbox(bbox)
                    .geometry_source(GeometrySource::DerivedFromBbox)
                    .model_order(0)
                    .metadata(BTreeMap::new())
                    .build(),
                LayoutDetection::builder()
                    .source_detection_index(1)
                    .raw_label("future_widget".to_owned())
                    .class_id(25)
                    .label(LayoutLabel::from("future_widget"))
                    .confidence(0.8)
                    .bbox(bbox)
                    .geometry_source(GeometrySource::DerivedFromBbox)
                    .model_order(1)
                    .metadata(BTreeMap::new())
                    .build(),
            ])
        })
    }
}

/// Verifies an explicit Letter-page affine transform round-trips page coordinates.
#[test]
fn letter_page_transform_round_trips() {
    let transform = PageTransform::try_from(
        PageTransformInput::builder()
            .page_to_viewport(
                AffineTransform::builder()
                    .a(1.0)
                    .b(0.0)
                    .c(0.0)
                    .d(-1.0)
                    .e(0.0)
                    .f(792.0)
                    .build(),
            )
            .viewport_width(612.0)
            .viewport_height(792.0)
            .render_width(1224)
            .render_height(1584)
            .model_width(800)
            .model_height(800)
            .rotation(PageRotation::Degrees0)
            .build(),
    )
    .expect("the Letter transform must be valid");

    let viewport = transform.page_to_viewport(Point::new(72.0, 720.0));
    let page = transform.viewport_to_page(viewport);
    let rendered = transform.viewport_to_rendered(viewport);

    assert_close(viewport.x, 72.0);
    assert_close(viewport.y, 72.0);
    assert_close(page.x, 72.0);
    assert_close(page.y, 720.0);
    assert_close(rendered.x, 144.0);
    assert_close(rendered.y, 144.0);
}

/// Verifies rotation metadata and non-uniform model scaling remain invertible.
#[test]
fn rotated_page_uses_independent_model_scales() {
    let transform = PageTransform::try_from(
        PageTransformInput::builder()
            .page_to_viewport(
                AffineTransform::builder()
                    .a(0.0)
                    .b(1.0)
                    .c(1.0)
                    .d(0.0)
                    .e(0.0)
                    .f(0.0)
                    .build(),
            )
            .viewport_width(792.0)
            .viewport_height(612.0)
            .render_width(1584)
            .render_height(1224)
            .model_width(800)
            .model_height(800)
            .rotation(PageRotation::Degrees90)
            .build(),
    )
    .expect("the rotated transform must be valid");

    let viewport = transform.page_to_viewport(Point::new(72.0, 144.0));
    let page = transform.viewport_to_page(viewport);
    let model = transform.rendered_to_model(Point::new(792.0, 612.0));
    let rendered = transform.model_to_rendered(model);

    assert_eq!(transform.rotation(), PageRotation::Degrees90);
    assert_close(viewport.x, 144.0);
    assert_close(viewport.y, 72.0);
    assert_close(page.x, 72.0);
    assert_close(page.y, 144.0);
    assert_close(model.x, 400.0);
    assert_close(model.y, 400.0);
    assert_close(rendered.x, 792.0);
    assert_close(rendered.y, 612.0);
}

/// Verifies non-finite and non-invertible transforms are rejected.
#[test]
fn invalid_page_transforms_are_rejected() {
    let invalid_affine = AffineTransform::builder()
        .a(f64::NAN)
        .b(0.0)
        .c(0.0)
        .d(1.0)
        .e(0.0)
        .f(0.0)
        .build();
    let input = PageTransformInput::builder()
        .page_to_viewport(invalid_affine)
        .viewport_width(612.0)
        .viewport_height(792.0)
        .render_width(1224)
        .render_height(1584)
        .model_width(800)
        .model_height(800)
        .rotation(PageRotation::Degrees0)
        .build();
    assert!(matches!(
        PageTransform::try_from(input),
        Err(GeometryError::NonFinite { .. })
    ));

    let singular = PageTransformInput::builder()
        .page_to_viewport(
            AffineTransform::builder()
                .a(1.0)
                .b(2.0)
                .c(2.0)
                .d(4.0)
                .e(0.0)
                .f(0.0)
                .build(),
        )
        .viewport_width(612.0)
        .viewport_height(792.0)
        .render_width(1224)
        .render_height(1584)
        .model_width(800)
        .model_height(800)
        .rotation(PageRotation::Degrees0)
        .build();
    assert!(matches!(
        PageTransform::try_from(singular),
        Err(GeometryError::NonInvertibleTransform)
    ));
}

/// Verifies polygon validation and geometry operations use inclusive boundaries.
#[test]
fn polygon_operations_preserve_valid_geometry() {
    let polygon = Polygon::try_from(vec![
        Point::new(0.0, 0.0),
        Point::new(10.0, 0.0),
        Point::new(10.0, 10.0),
        Point::new(0.0, 10.0),
    ])
    .expect("the square polygon must be valid");
    let intersection = Bbox::try_from([5.0, 5.0, 15.0, 15.0])
        .expect("the intersection box must be valid");

    assert_close(polygon.area(), 100.0);
    assert_eq!(
        polygon.bbox(),
        Bbox::try_from([0.0, 0.0, 10.0, 10.0])
            .expect("the polygon bbox must be valid")
    );
    assert_close(polygon.intersection_area(intersection), 25.0);
    assert!(polygon.contains_point(Point::new(0.0, 5.0)));
    assert_close(
        polygon.baseline_inside_length(
            Point::new(-5.0, 5.0),
            Point::new(15.0, 5.0),
        ),
        10.0,
    );
}

/// Verifies degenerate, self-intersecting, and non-finite polygons are rejected.
#[test]
fn invalid_polygons_are_rejected() {
    assert!(matches!(
        Polygon::try_from(vec![Point::new(0.0, 0.0), Point::new(1.0, 1.0),]),
        Err(GeometryError::InvalidPolygon { .. })
    ));
    assert!(matches!(
        Polygon::try_from(vec![
            Point::new(0.0, 0.0),
            Point::new(1.0, 1.0),
            Point::new(2.0, 2.0),
        ]),
        Err(GeometryError::InvalidPolygon { .. })
    ));
    assert!(matches!(
        Polygon::try_from(vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
            Point::new(10.0, 0.0),
        ]),
        Err(GeometryError::InvalidPolygon { .. })
    ));
    assert!(matches!(
        Polygon::try_from(vec![
            Point::new(0.0, 0.0),
            Point::new(f64::INFINITY, 0.0),
            Point::new(0.0, 1.0),
        ]),
        Err(GeometryError::InvalidPolygon { .. })
    ));
}

/// Verifies image construction rejects invalid lengths and arithmetic overflow.
#[test]
fn page_image_validates_rgb_buffer_length() {
    let wrong_length = PageImageInput::builder()
        .width(2)
        .height(2)
        .pixel_format(PixelFormat::Rgb8)
        .data(Arc::<[u8]>::from(vec![0; 11]))
        .build();
    assert!(matches!(
        PageImage::try_from(wrong_length),
        Err(PageImageError::BufferLength {
            expected: 12,
            actual: 11
        })
    ));

    let overflow = PageImageInput::builder()
        .width(u32::MAX)
        .height(u32::MAX)
        .pixel_format(PixelFormat::Rgb8)
        .data(Arc::<[u8]>::from(Vec::new()))
        .build();
    assert!(matches!(
        PageImage::try_from(overflow),
        Err(PageImageError::ArithmeticOverflow)
    ));
}

/// Verifies dynamic engines preserve known labels and future raw labels.
#[tokio::test]
async fn layout_engine_trait_object_preserves_unknown_labels() {
    let engine: Arc<dyn LayoutEngine> = Arc::new(FakeLayoutEngine);
    let request = LayoutRequest::builder()
        .page_number(1)
        .image(test_image())
        .transform(identity_transform())
        .build();

    let detections = engine
        .detect(request)
        .await
        .expect("detection must succeed");

    assert_eq!(engine.name(), "fake-layout");
    assert_eq!(engine.model_revision(), "fixture-v1");
    assert_eq!(detections.len(), 2);
    let (known, unknown) = match detections.as_slice() {
        [known, unknown] => (known, unknown),
        unexpected => {
            assert_eq!(unexpected.len(), 2);
            return;
        }
    };
    assert_eq!(known.label, LayoutLabel::Text);
    assert_eq!(unknown.raw_label, "future_widget");
    assert_eq!(
        unknown.label,
        LayoutLabel::Unknown("future_widget".to_owned())
    );
}
