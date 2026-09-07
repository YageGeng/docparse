use docparse_layout::{Bbox, GeometryError, Point};

/// Local inline and cross-line axes for text whose angle is expressed in viewport space.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TextAxes {
    rotation: f64,
    cosine: f64,
    sine: f64,
}

impl From<f64> for TextAxes {
    /// Builds an orthonormal frame from clockwise viewport degrees.
    fn from(rotation: f64) -> Self {
        let rotation = rotation.rem_euclid(360.0);
        let (sine, cosine) = rotation.to_radians().sin_cos();
        Self {
            rotation,
            cosine,
            sine,
        }
    }
}

impl TextAxes {
    /// Identifies angles outside the existing two-degree cardinal tolerance.
    pub(crate) fn is_oblique(self) -> bool {
        let remainder = self.rotation.rem_euclid(90.0);
        remainder > 2.0 && remainder < 88.0
    }

    /// Projects a viewport point onto inline x and cross-line y coordinates.
    pub(crate) fn project(self, point: Point) -> Point {
        Point::new(
            self.cosine * point.x + self.sine * point.y,
            -self.sine * point.x + self.cosine * point.y,
        )
    }

    /// Restores viewport coordinates from this orthonormal text frame.
    pub(crate) fn unproject(self, point: Point) -> Point {
        Point::new(
            self.cosine * point.x - self.sine * point.y,
            self.sine * point.x + self.cosine * point.y,
        )
    }

    /// Projects an axis-aligned box conservatively without treating it as an oriented glyph quad.
    pub(crate) fn project_bbox(
        self,
        bbox: Bbox,
    ) -> Result<Bbox, GeometryError> {
        let center = self.project(bbox.center());
        let half_width = (self.cosine.abs() * bbox.width()
            + self.sine.abs() * bbox.height())
            * 0.5;
        let half_height = (self.sine.abs() * bbox.width()
            + self.cosine.abs() * bbox.height())
            * 0.5;
        Bbox::try_from([
            center.x - half_width,
            center.y - half_height,
            center.x + half_width,
            center.y + half_height,
        ])
    }
}
