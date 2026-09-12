//! DB probability-map decoding without an OCR or OpenCV runtime dependency.
// Geometry follows the DB rectangle/score/unclip pipeline used by PaddleOCR and OAR.
use crate::OcrError;
use docparse_config::OcrConfig;
use docparse_layout::{Point, Quad};
use geo::{
    Area, Buffer, MinimumRotatedRect, MultiPoint, Point as GeoPoint,
    Polygon as GeoPolygon,
};
use ndarray::Array2;

/// Owned detector probabilities, bounded before contour extraction.
pub(crate) struct DetectionMap(pub Array2<f32>);

impl DetectionMap {
    /// Extracts connected outer contours, scores and expands their rectangles, and restores image coordinates.
    pub fn decode(
        &self,
        width: u32,
        height: u32,
        config: &OcrConfig,
    ) -> Result<Vec<Quad>, OcrError> {
        let (rows, columns) = self.0.dim();
        if width == 0
            || height == 0
            || rows == 0
            || columns == 0
            || rows > 4096
            || columns > 4096
        {
            return Err(OcrError::InvalidData(
                "invalid DB probability-map dimensions".into(),
            ));
        }
        let mut mask = Vec::with_capacity(self.0.len());
        for &value in &self.0 {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(OcrError::InvalidData(
                    "invalid detector probability".into(),
                ));
            }
            mask.push(u8::from(f64::from(value) > config.detection_threshold));
        }
        let mut boxes = Vec::new();
        let mut queue = Vec::new();
        let mut points = Vec::new();
        let mut candidates = 0;
        for start in 0..mask.len() {
            if mask.get(start) != Some(&1) {
                continue;
            }
            if candidates >= config.max_candidates {
                tracing::warn!(
                    "OCR detection reached its {} candidate limit",
                    config.max_candidates
                );
                break;
            }
            candidates += 1;
            queue.clear();
            points.clear();
            queue.push(start);
            if let Some(value) = mask.get_mut(start) {
                *value = 0;
            }
            let mut position = 0;
            while let Some(&index) = queue.get(position) {
                position += 1;
                let (x, y) = (index % columns, index / columns);
                let mut boundary = false;
                for dy in -1_isize..=1 {
                    for dx in -1_isize..=1 {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let neighbor = x
                            .checked_add_signed(dx)
                            .zip(y.checked_add_signed(dy))
                            .filter(|&(x, y)| x < columns && y < rows);
                        if let Some((nx, ny)) = neighbor {
                            if dx == 0 || dy == 0 {
                                boundary |=
                                    self.0.get((ny, nx)).is_none_or(|value| {
                                        f64::from(*value)
                                            <= config.detection_threshold
                                    });
                            }
                            if let Some(value) = mask.get_mut(ny * columns + nx)
                                && *value == 1
                            {
                                *value = 0;
                                queue.push(ny * columns + nx);
                            }
                        } else {
                            boundary = true;
                        }
                    }
                }
                // Interior pixels need no hull point; holes do not create duplicate text candidates.
                if boundary {
                    points.push(GeoPoint::new(x as f64, y as f64));
                }
            }
            if points.len() < 3 {
                continue;
            }
            let Some(rectangle) = MultiPoint::from(std::mem::take(&mut points))
                .minimum_rotated_rect()
            else {
                continue;
            };
            let Some(quad) = ordered_quad(&rectangle) else {
                continue;
            };
            let [a, b, c, d] = *quad.points();
            let edge = |a: Point, b: Point| (a.x - b.x).hypot(a.y - b.y);
            let (box_width, box_height) = (edge(a, b), edge(b, c));
            if box_width.min(box_height) < 3.0
                || self.score(&quad) < config.box_threshold
            {
                continue;
            }
            let perimeter = edge(a, b) + edge(b, c) + edge(c, d) + edge(d, a);
            let distance =
                rectangle.unsigned_area() * config.unclip_ratio / perimeter;
            let expanded = rectangle.buffer(distance);
            let Some(expanded) = expanded
                .0
                .first()
                .filter(|_| expanded.0.len() == 1)
                .and_then(MinimumRotatedRect::minimum_rotated_rect)
            else {
                continue;
            };
            let Some(quad) = ordered_quad(&expanded) else {
                continue;
            };
            let [a, b, c, _] = *quad.points();
            if edge(a, b).min(edge(b, c)) < 5.0 {
                continue;
            }
            let corners = quad.points().map(|point| {
                Point::new(
                    (point.x * f64::from(width) / columns as f64)
                        .round()
                        .clamp(0.0, f64::from(width)),
                    (point.y * f64::from(height) / rows as f64)
                        .round()
                        .clamp(0.0, f64::from(height)),
                )
            });
            if let Ok(quad) = Quad::try_from(corners) {
                boxes.push(quad);
            }
        }
        // Stable top-left order keeps recognition, source IDs and confidence ties deterministic.
        boxes.sort_by(|left, right| {
            let [a, _, _, _] = *left.points();
            let [b, _, _, _] = *right.points();
            a.y.total_cmp(&b.y).then_with(|| a.x.total_cmp(&b.x))
        });
        Ok(boxes)
    }

    /// Averages the original probability map inside a convex minimum-area rectangle.
    #[expect(
        clippy::cast_sign_loss,
        reason = "rectangle coordinates are clamped to nonnegative map indices"
    )]
    fn score(&self, quad: &Quad) -> f64 {
        let [a, b, c, d] = *quad.points();
        let points = [a, b, c, d];
        let left = points
            .iter()
            .map(|p| p.x.floor())
            .fold(f64::INFINITY, f64::min)
            .max(0.0) as usize;
        let top = points
            .iter()
            .map(|p| p.y.floor())
            .fold(f64::INFINITY, f64::min)
            .max(0.0) as usize;
        let right = points
            .iter()
            .map(|p| p.x.ceil())
            .fold(0.0, f64::max)
            .min((self.0.ncols() - 1) as f64) as usize;
        let bottom = points
            .iter()
            .map(|p| p.y.ceil())
            .fold(0.0, f64::max)
            .min((self.0.nrows() - 1) as f64) as usize;
        let (mut total, mut count) = (0.0, 0_u32);
        for y in top..=bottom {
            for x in left..=right {
                let mut sign = None;
                let inside = [(a, b), (b, c), (c, d), (d, a)].into_iter().all(
                    |(a, b)| {
                        let cross = (b.x - a.x) * (y as f64 - a.y)
                            - (b.y - a.y) * (x as f64 - a.x);
                        if cross.abs() < 1e-7 {
                            return true;
                        }
                        let positive = cross > 0.0;
                        let matches =
                            sign.is_none_or(|previous| previous == positive);
                        sign = Some(positive);
                        matches
                    },
                );
                if inside && let Some(value) = self.0.get((y, x)) {
                    total += f64::from(*value);
                    count += 1;
                }
            }
        }
        if count == 0 {
            0.0
        } else {
            total / f64::from(count)
        }
    }
}

/// Orders a geometry-library rectangle as top-left, top-right, bottom-right, bottom-left.
fn ordered_quad(polygon: &GeoPolygon<f64>) -> Option<Quad> {
    let mut points: [Point; 4] = polygon
        .exterior()
        .0
        .iter()
        .take(4)
        .map(|p| Point::new(p.x, p.y))
        .collect::<Vec<_>>()
        .try_into()
        .ok()?;
    points
        .sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
    let [l0, l1, r0, r1] = points;
    let (tl, bl) = if l0.y <= l1.y { (l0, l1) } else { (l1, l0) };
    let (tr, br) = if r0.y <= r1.y { (r0, r1) } else { (r1, r0) };
    Quad::try_from([tl, tr, br, bl]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_layout::Polygon;

    /// Two independent text regions survive unclip and rescaling while weak components are rejected.
    #[test]
    fn db_recovers_scored_quads_in_source_coordinates() {
        let mut map = Array2::zeros((64, 128));
        for y in 10..20 {
            for x in 10..50 {
                *map.get_mut((y, x)).expect("pixel") = 0.9;
            }
        }
        for y in 35..45 {
            for x in 60..110 {
                *map.get_mut((y, x)).expect("pixel") = 0.8;
            }
        }
        for y in 50..58 {
            for x in 10..20 {
                *map.get_mut((y, x)).expect("pixel") = 0.3;
            }
        }
        let boxes = DetectionMap(map)
            .decode(256, 128, &OcrConfig::default())
            .expect("DB boxes");
        assert_eq!(boxes.len(), 2);
        let first =
            Polygon::from(boxes.first().expect("first box").clone()).bbox();
        assert!(
            first.left < 20.0
                && first.right > 98.0
                && first.top < 20.0
                && first.bottom > 38.0
        );
        DetectionMap(Array2::from_elem((2, 2), f32::NAN))
            .decode(10, 10, &OcrConfig::default())
            .expect_err("nonfinite detector probabilities must fail");
    }
}
