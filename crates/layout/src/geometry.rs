use geo::algorithm::Validation;
use geo::line_intersection::{LineIntersection, line_intersection};
use geo::{
    Area, BooleanOps, BoundingRect, ConvexHull, Coord, Intersects, Line,
    MultiPoint, Polygon as GeoPolygon,
};
use serde::de::Error as DeserializeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use typed_builder::TypedBuilder;

use crate::GeometryError;

const GEOMETRY_EPSILON: f64 = 1.0e-9;

/// Four ordered convex corners, retaining rotation and affine shear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "[Point; 4]", into = "[Point; 4]")]
pub struct Quad([Point; 4]);

impl Quad {
    /// Returns the corners in perimeter order without closing the ring.
    pub fn points(&self) -> &[Point; 4] {
        &self.0
    }
}

impl TryFrom<[Point; 4]> for Quad {
    type Error = GeometryError;

    /// Rejects non-finite, self-crossing, concave, or degenerate quadrilaterals.
    fn try_from(points: [Point; 4]) -> Result<Self, Self::Error> {
        Polygon::try_from(points.to_vec())?;
        let [p0, p1, p2, p3] = points;
        let mut sign = None;
        for (a, b, c) in
            [(p0, p1, p2), (p1, p2, p3), (p2, p3, p0), (p3, p0, p1)]
        {
            let cross = (b.x - a.x) * (c.y - b.y) - (b.y - a.y) * (c.x - b.x);
            if cross.abs() <= GEOMETRY_EPSILON
                || sign.is_some_and(|positive| {
                    positive != cross.is_sign_positive()
                })
            {
                return Err(GeometryError::InvalidPolygon {
                    reason:
                        "quad corners must form a strictly convex perimeter"
                            .to_owned(),
                });
            }
            sign = Some(cross.is_sign_positive());
        }
        Ok(Self(points))
    }
}

impl From<Quad> for [Point; 4] {
    /// Returns the validated public corner representation.
    fn from(quad: Quad) -> Self {
        quad.0
    }
}

impl From<Quad> for Polygon {
    /// Converts already validated corners without replacing them with an AABB.
    fn from(quad: Quad) -> Self {
        Self {
            points: quad.0.to_vec(),
        }
    }
}

/// A point in one explicitly documented two-dimensional coordinate space.
#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TypedBuilder,
)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    /// Creates a point without assigning an implicit coordinate-space meaning.
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Returns whether both coordinates are finite.
    fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// A finite axis-aligned box using left, top, right, and bottom coordinates.
#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TypedBuilder,
)]
pub struct Bbox {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

impl Bbox {
    /// Returns the positive box width.
    pub fn width(self) -> f64 {
        self.right - self.left
    }

    /// Returns the positive box height.
    pub fn height(self) -> f64 {
        self.bottom - self.top
    }

    /// Returns the box area.
    pub fn area(self) -> f64 {
        self.width() * self.height()
    }

    /// Returns whether the entire other box lies inside this box, including shared edges.
    pub fn contains_bbox(self, other: Self) -> bool {
        self.left <= other.left
            && self.top <= other.top
            && self.right >= other.right
            && self.bottom >= other.bottom
    }

    /// Returns intersection over union; touching boxes and non-finite areas have no usable overlap.
    pub fn iou(self, other: Self) -> f64 {
        let width =
            (self.right.min(other.right) - self.left.max(other.left)).max(0.0);
        let height =
            (self.bottom.min(other.bottom) - self.top.max(other.top)).max(0.0);
        let intersection = width * height;
        let union = self.area() + other.area() - intersection;
        if union.is_finite() && union > 0.0 {
            (intersection / union).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }

    /// Returns the geometric center of the box.
    pub fn center(self) -> Point {
        Point::new(
            (self.left + self.right) / 2.0,
            (self.top + self.bottom) / 2.0,
        )
    }

    /// Converts this public box into a private rectangular polygon.
    fn as_geo_polygon(self) -> GeoPolygon<f64> {
        GeoPolygon::new(
            vec![
                Coord {
                    x: self.left,
                    y: self.top,
                },
                Coord {
                    x: self.right,
                    y: self.top,
                },
                Coord {
                    x: self.right,
                    y: self.bottom,
                },
                Coord {
                    x: self.left,
                    y: self.bottom,
                },
                Coord {
                    x: self.left,
                    y: self.top,
                },
            ]
            .into(),
            Vec::new(),
        )
    }
}

impl TryFrom<[f64; 4]> for Bbox {
    type Error = GeometryError;

    /// Validates and constructs a box from `[left, top, right, bottom]`.
    fn try_from(value: [f64; 4]) -> Result<Self, Self::Error> {
        let [left, top, right, bottom] = value;
        for (field, coordinate) in [
            ("bbox.left", left),
            ("bbox.top", top),
            ("bbox.right", right),
            ("bbox.bottom", bottom),
        ] {
            if !coordinate.is_finite() {
                return Err(GeometryError::NonFinite { field });
            }
        }
        if right <= left || bottom <= top {
            return Err(GeometryError::DegenerateBbox);
        }
        Ok(Self::builder()
            .left(left)
            .top(top)
            .right(right)
            .bottom(bottom)
            .build())
    }
}

/// Six coefficients for a two-dimensional affine transform.
#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, TypedBuilder,
)]
pub struct AffineTransform {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl AffineTransform {
    /// Builds an identity affine transform.
    pub fn identity() -> Self {
        Self::builder()
            .a(1.0)
            .b(0.0)
            .c(0.0)
            .d(1.0)
            .e(0.0)
            .f(0.0)
            .build()
    }

    /// Applies the affine transform to one point.
    pub fn transform_point(self, point: Point) -> Point {
        Point::new(
            self.a * point.x + self.b * point.y + self.e,
            self.c * point.x + self.d * point.y + self.f,
        )
    }

    /// Validates all coefficients and returns the inverse transform.
    pub fn inverse(self) -> Result<Self, GeometryError> {
        for (field, coefficient) in [
            ("page_to_viewport.a", self.a),
            ("page_to_viewport.b", self.b),
            ("page_to_viewport.c", self.c),
            ("page_to_viewport.d", self.d),
            ("page_to_viewport.e", self.e),
            ("page_to_viewport.f", self.f),
        ] {
            if !coefficient.is_finite() {
                return Err(GeometryError::NonFinite { field });
            }
        }

        let determinant = self.a * self.d - self.b * self.c;
        if !determinant.is_finite() || determinant.abs() <= GEOMETRY_EPSILON {
            return Err(GeometryError::NonInvertibleTransform);
        }
        Ok(Self::builder()
            .a(self.d / determinant)
            .b(-self.b / determinant)
            .c(-self.c / determinant)
            .d(self.a / determinant)
            .e((self.b * self.f - self.d * self.e) / determinant)
            .f((self.c * self.e - self.a * self.f) / determinant)
            .build())
    }
}

/// Clockwise page rotation already reflected in viewport dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageRotation {
    Degrees0,
    Degrees90,
    Degrees180,
    Degrees270,
}

/// Unvalidated inputs needed to derive all page coordinate transforms.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct PageTransformInput {
    pub page_to_viewport: AffineTransform,
    pub viewport_width: f64,
    pub viewport_height: f64,
    pub render_width: u32,
    pub render_height: u32,
    pub model_width: u32,
    pub model_height: u32,
    pub rotation: PageRotation,
}

/// Validated page, viewport, rendered-pixel, and model-input transforms.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct PageTransform {
    page_to_viewport: AffineTransform,
    viewport_to_page: AffineTransform,
    viewport_width: f64,
    viewport_height: f64,
    render_width: u32,
    render_height: u32,
    model_width: u32,
    model_height: u32,
    rotation: PageRotation,
}

impl PageTransform {
    /// Converts a PDF page-space point to canonical viewport points.
    pub fn page_to_viewport(&self, point: Point) -> Point {
        self.page_to_viewport.transform_point(point)
    }

    /// Converts canonical viewport points back to PDF page space.
    pub fn viewport_to_page(&self, point: Point) -> Point {
        self.viewport_to_page.transform_point(point)
    }

    /// Converts canonical viewport points to rendered image pixels.
    pub fn viewport_to_rendered(&self, point: Point) -> Point {
        Point::new(
            point.x * f64::from(self.render_width) / self.viewport_width,
            point.y * f64::from(self.render_height) / self.viewport_height,
        )
    }

    /// Converts rendered image pixels to canonical viewport points.
    pub fn rendered_to_viewport(&self, point: Point) -> Point {
        Point::new(
            point.x * self.viewport_width / f64::from(self.render_width),
            point.y * self.viewport_height / f64::from(self.render_height),
        )
    }

    /// Converts rendered image pixels to non-uniform model-input coordinates.
    pub fn rendered_to_model(&self, point: Point) -> Point {
        Point::new(
            point.x * f64::from(self.model_width)
                / f64::from(self.render_width),
            point.y * f64::from(self.model_height)
                / f64::from(self.render_height),
        )
    }

    /// Converts model-input coordinates back to rendered image pixels.
    pub fn model_to_rendered(&self, point: Point) -> Point {
        Point::new(
            point.x * f64::from(self.render_width)
                / f64::from(self.model_width),
            point.y * f64::from(self.render_height)
                / f64::from(self.model_height),
        )
    }

    /// Returns the clockwise page rotation associated with this transform.
    pub fn rotation(&self) -> PageRotation {
        self.rotation
    }

    /// Returns canonical viewport dimensions in points.
    pub fn viewport_size(&self) -> (f64, f64) {
        (self.viewport_width, self.viewport_height)
    }

    /// Returns rendered image dimensions in pixels.
    pub fn render_size(&self) -> (u32, u32) {
        (self.render_width, self.render_height)
    }

    /// Returns model input dimensions in pixels.
    pub fn model_size(&self) -> (u32, u32) {
        (self.model_width, self.model_height)
    }
}

impl TryFrom<PageTransformInput> for PageTransform {
    type Error = GeometryError;

    /// Validates transform coefficients and every coordinate-space dimension.
    fn try_from(input: PageTransformInput) -> Result<Self, Self::Error> {
        for (field, dimension) in [
            ("viewport_width", input.viewport_width),
            ("viewport_height", input.viewport_height),
        ] {
            if !dimension.is_finite() {
                return Err(GeometryError::NonFinite { field });
            }
            if dimension <= 0.0 {
                return Err(GeometryError::InvalidDimension { field });
            }
        }
        for (field, dimension) in [
            ("render_width", input.render_width),
            ("render_height", input.render_height),
            ("model_width", input.model_width),
            ("model_height", input.model_height),
        ] {
            if dimension == 0 {
                return Err(GeometryError::InvalidDimension { field });
            }
        }

        let viewport_to_page = input.page_to_viewport.inverse()?;
        Ok(Self::builder()
            .page_to_viewport(input.page_to_viewport)
            .viewport_to_page(viewport_to_page)
            .viewport_width(input.viewport_width)
            .viewport_height(input.viewport_height)
            .render_width(input.render_width)
            .render_height(input.render_height)
            .model_width(input.model_width)
            .model_height(input.model_height)
            .rotation(input.rotation)
            .build())
    }
}

/// A validated simple polygon whose public representation never exposes `geo` types.
#[derive(Debug, Clone, PartialEq)]
pub struct Polygon {
    points: Vec<Point>,
}

impl Polygon {
    /// Encloses source corners while preserving a tilted footprint across split text runs.
    pub fn enclosing(
        points: impl IntoIterator<Item = Point>,
    ) -> Result<Self, GeometryError> {
        let points: Vec<_> = points.into_iter().collect();
        if points.iter().any(|point| !point.is_finite()) {
            return Err(GeometryError::InvalidPolygon {
                reason: "all coordinates must be finite".to_owned(),
            });
        }
        let hull = MultiPoint::from(
            points
                .into_iter()
                .map(|point| geo::Point::new(point.x, point.y))
                .collect::<Vec<_>>(),
        )
        .convex_hull();
        let mut corners: Vec<_> = hull
            .exterior()
            .0
            .iter()
            .map(|point| Point::new(point.x, point.y))
            .collect();
        if corners.first() == corners.last() {
            corners.pop();
        }
        Self::try_from(corners)
    }

    /// Clips a convex source footprint to a run or page without expanding its empty corners.
    pub fn clipped(&self, bbox: Bbox) -> Option<Self> {
        let intersection =
            self.as_geo_polygon().intersection(&bbox.as_geo_polygon());
        Self::enclosing(intersection.0.iter().flat_map(|polygon| {
            polygon
                .exterior()
                .0
                .iter()
                .map(|point| Point::new(point.x, point.y))
        }))
        .ok()
    }

    /// Returns the polygon vertices without the private closing coordinate.
    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// Returns the finite axis-aligned bounding box of this polygon.
    pub fn bbox(&self) -> Bbox {
        let rect = self
            .as_geo_polygon()
            .bounding_rect()
            .expect("validated polygons always have a bounding rectangle");
        Bbox::try_from([rect.min().x, rect.min().y, rect.max().x, rect.max().y])
            .expect("validated polygon bounds always have positive finite area")
    }

    /// Returns the unsigned polygon area.
    pub fn area(&self) -> f64 {
        self.as_geo_polygon().unsigned_area()
    }

    /// Returns the area shared with one axis-aligned box.
    pub fn intersection_area(&self, bbox: Bbox) -> f64 {
        self.as_geo_polygon()
            .intersection(&bbox.as_geo_polygon())
            .unsigned_area()
    }

    /// Returns true for points in the polygon interior or on its boundary.
    pub fn contains_point(&self, point: Point) -> bool {
        point.is_finite()
            && self.as_geo_polygon().intersects(&Coord {
                x: point.x,
                y: point.y,
            })
    }

    /// Measures the portion of a baseline segment lying inside or on the polygon.
    pub fn baseline_inside_length(&self, start: Point, end: Point) -> f64 {
        if !start.is_finite() || !end.is_finite() {
            return 0.0;
        }
        let delta_x = end.x - start.x;
        let delta_y = end.y - start.y;
        let length_squared = delta_x * delta_x + delta_y * delta_y;
        if length_squared <= GEOMETRY_EPSILON {
            return 0.0;
        }

        let baseline = Line::new(
            Coord {
                x: start.x,
                y: start.y,
            },
            Coord { x: end.x, y: end.y },
        );
        let polygon = self.as_geo_polygon();
        let mut parameters = vec![0.0, 1.0];
        for edge in polygon.exterior().lines() {
            match line_intersection(baseline, edge) {
                Some(LineIntersection::SinglePoint {
                    intersection, ..
                }) => {
                    parameters.push(
                        ((intersection.x - start.x) * delta_x
                            + (intersection.y - start.y) * delta_y)
                            / length_squared,
                    );
                }
                Some(LineIntersection::Collinear { intersection }) => {
                    for point in [intersection.start, intersection.end] {
                        parameters.push(
                            ((point.x - start.x) * delta_x
                                + (point.y - start.y) * delta_y)
                                / length_squared,
                        );
                    }
                }
                None => {}
            }
        }
        parameters.retain(|parameter| {
            parameter.is_finite()
                && *parameter >= -GEOMETRY_EPSILON
                && *parameter <= 1.0 + GEOMETRY_EPSILON
        });
        parameters.iter_mut().for_each(|parameter| {
            *parameter = parameter.clamp(0.0, 1.0);
        });
        parameters.sort_by(f64::total_cmp);
        parameters
            .dedup_by(|left, right| (*left - *right).abs() <= GEOMETRY_EPSILON);

        let baseline_length = length_squared.sqrt();
        parameters
            .windows(2)
            .filter_map(|window| match window {
                [start_parameter, end_parameter] => {
                    let midpoint = (start_parameter + end_parameter) / 2.0;
                    let midpoint = Coord {
                        x: start.x + midpoint * delta_x,
                        y: start.y + midpoint * delta_y,
                    };
                    polygon.intersects(&midpoint).then_some(
                        (end_parameter - start_parameter) * baseline_length,
                    )
                }
                _ => None,
            })
            .sum()
    }

    /// Converts validated public points into a closed private `geo` polygon.
    fn as_geo_polygon(&self) -> GeoPolygon<f64> {
        let mut coordinates: Vec<_> = self
            .points
            .iter()
            .map(|point| Coord {
                x: point.x,
                y: point.y,
            })
            .collect();
        if let Some(first) = coordinates.first().copied() {
            coordinates.push(first);
        }
        GeoPolygon::new(coordinates.into(), Vec::new())
    }
}

impl TryFrom<Vec<Point>> for Polygon {
    type Error = GeometryError;

    /// Validates finite vertices, topology, and positive area before construction.
    fn try_from(points: Vec<Point>) -> Result<Self, Self::Error> {
        if points.iter().any(|point| !point.is_finite()) {
            return Err(GeometryError::InvalidPolygon {
                reason: "all coordinates must be finite".to_owned(),
            });
        }
        if points.len() < 3 {
            return Err(GeometryError::InvalidPolygon {
                reason: "at least three vertices are required".to_owned(),
            });
        }

        let polygon = Self { points };
        if let Err(error) = polygon.as_geo_polygon().check_validation() {
            return Err(GeometryError::InvalidPolygon {
                reason: error.to_string(),
            });
        }
        if polygon.area() <= GEOMETRY_EPSILON {
            return Err(GeometryError::InvalidPolygon {
                reason: "area must be positive".to_owned(),
            });
        }
        Ok(polygon)
    }
}

impl Serialize for Polygon {
    /// Serializes only public vertices and omits the private closing coordinate.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.points.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Polygon {
    /// Deserializes through the same validating constructor used by callers.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let points = Vec::<Point>::deserialize(deserializer)?;
        Self::try_from(points).map_err(D::Error::custom)
    }
}
