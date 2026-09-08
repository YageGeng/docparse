use docparse_layout::{Point, Polygon, Quad};

/// A tilted bound must retain its narrow footprint and reject malformed corner order.
#[test]
fn quad_preserves_tilt_and_validates_corners() {
    let points = [
        Point::new(0.0, 100.0),
        Point::new(100.0, 0.0),
        Point::new(110.0, 10.0),
        Point::new(10.0, 110.0),
    ];
    let quad = Quad::try_from(points).expect("convex tilted quad");
    let polygon = Polygon::from(quad.clone());
    assert!((polygon.area() - 2000.0).abs() < 1e-6);
    assert!(polygon.bbox().area() > polygon.area() * 5.0);
    assert!(!polygon.contains_point(Point::new(5.0, 5.0)));
    assert_eq!(
        serde_json::from_value::<Quad>(
            serde_json::to_value(quad).expect("quad serialization")
        )
        .expect("quad deserialization")
        .points(),
        &points
    );
    Quad::try_from([points[0], points[2], points[1], points[3]])
        .expect_err("crossed quad");
    Quad::try_from([points[0]; 4]).expect_err("degenerate quad");
}
