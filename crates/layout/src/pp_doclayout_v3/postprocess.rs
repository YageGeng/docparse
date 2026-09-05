use std::collections::BTreeMap;

use ndarray::ArrayView2;

use crate::{
    Bbox, GeometrySource, LayoutDetection, LayoutLabel, PageTransform, Point,
    PostprocessError,
};

use super::LABELS;

/// Converts one page's exported bbox rows into stable neutral detections.
pub(crate) fn postprocess_page(
    rows: ArrayView2<'_, f32>,
    bbox_count: i32,
    threshold: f64,
    transform: &PageTransform,
) -> Result<Vec<LayoutDetection>, PostprocessError> {
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(PostprocessError::InvalidThreshold);
    }
    if rows.ncols() != 7 {
        return Err(PostprocessError::InvalidColumns {
            actual: rows.ncols(),
        });
    }
    let Ok(bbox_count_usize) = usize::try_from(bbox_count) else {
        return Err(PostprocessError::InvalidCount {
            count: bbox_count,
            rows: rows.nrows(),
        });
    };
    if bbox_count_usize > rows.nrows() {
        return Err(PostprocessError::InvalidCount {
            count: bbox_count,
            rows: rows.nrows(),
        });
    }

    let (render_width, render_height) = transform.render_size();
    let render_width = render_width as f32;
    let render_height = render_height as f32;
    let mut detections = Vec::new();
    for (source_index, row) in
        rows.outer_iter().take(bbox_count_usize).enumerate()
    {
        let Some([class_value, score, xmin, ymin, xmax, ymax, order_value]) =
            row.as_slice()
        else {
            return Err(PostprocessError::InvalidRow {
                index: source_index,
            });
        };
        if !class_value.is_finite()
            || class_value.fract().abs() > f32::EPSILON
            || *class_value < 0.0
            || !score.is_finite()
            || f64::from(*score) <= threshold
            || !order_value.is_finite()
            || order_value.fract().abs() > f32::EPSILON
        {
            continue;
        }
        let class_id = *class_value as i64;
        let Ok(class_index) = usize::try_from(class_id) else {
            continue;
        };
        let Some(raw_label) = LABELS.get(class_index).copied() else {
            continue;
        };
        if [xmin, ymin, xmax, ymax]
            .into_iter()
            .any(|coordinate| !coordinate.is_finite())
        {
            continue;
        }

        let left = xmin.round_ties_even().clamp(0.0, render_width);
        let top = ymin.round_ties_even().clamp(0.0, render_height);
        let right = xmax.round_ties_even().clamp(0.0, render_width);
        let bottom = ymax.round_ties_even().clamp(0.0, render_height);
        if right <= left || bottom <= top {
            continue;
        }
        let top_left = transform
            .rendered_to_viewport(Point::new(f64::from(left), f64::from(top)));
        let bottom_right = transform.rendered_to_viewport(Point::new(
            f64::from(right),
            f64::from(bottom),
        ));
        let bbox = Bbox::try_from([
            top_left.x.min(bottom_right.x),
            top_left.y.min(bottom_right.y),
            top_left.x.max(bottom_right.x),
            top_left.y.max(bottom_right.y),
        ])?;
        let source_detection_index =
            u32::try_from(source_index).map_err(|_source| {
                PostprocessError::SourceIndexOverflow {
                    index: source_index,
                }
            })?;
        detections.push(
            LayoutDetection::builder()
                .source_detection_index(source_detection_index)
                .raw_label(raw_label.to_owned())
                .class_id(class_id)
                .label(LayoutLabel::from(raw_label))
                .confidence(f64::from(*score))
                .bbox(bbox)
                .geometry_source(GeometrySource::DerivedFromBbox)
                .model_order(*order_value as i64)
                .metadata(BTreeMap::new())
                .build(),
        );
    }
    detections.sort_by_key(|detection| {
        (detection.model_order, detection.source_detection_index)
    });
    Ok(detections)
}

#[cfg(test)]
mod tests {
    use ndarray::array;

    use crate::{
        AffineTransform, Bbox, GeometrySource, LayoutLabel, PageRotation,
        PageTransform, PageTransformInput,
    };

    use super::postprocess_page;

    /// Builds a transform where two rendered pixels equal one viewport point.
    fn transform() -> PageTransform {
        PageTransform::try_from(
            PageTransformInput::builder()
                .page_to_viewport(AffineTransform::identity())
                .viewport_width(50.0)
                .viewport_height(100.0)
                .render_width(100)
                .render_height(200)
                .model_width(800)
                .model_height(800)
                .rotation(PageRotation::Degrees0)
                .build(),
        )
        .expect("the test transform must be valid")
    }

    /// Verifies threshold, ties-to-even, clipping, and source indices.
    #[test]
    fn filtering_and_rounding_match_lossless_profile() {
        let rows = array![
            [22.0, 0.5, 1.0, 1.0, 9.0, 9.0, 1.0],
            [22.0, 0.9, 2.5, 3.5, 102.5, 199.4, 2.0],
            [22.0, 0.8, 8.0, 8.0, 8.1, 8.2, 3.0],
            [22.0, f32::NAN, 0.0, 0.0, 10.0, 10.0, 4.0],
        ];

        let detections = postprocess_page(rows.view(), 4, 0.5, &transform())
            .expect("valid rows must postprocess");

        assert_eq!(detections.len(), 1);
        let detection = detections
            .first()
            .expect("one detection must survive filtering");
        assert_eq!(detection.source_detection_index, 1);
        assert_eq!(detection.raw_label, "text");
        assert_eq!(detection.label, LayoutLabel::Text);
        assert_eq!(detection.model_order, 2);
        assert_eq!(detection.geometry_source, GeometrySource::DerivedFromBbox);
        assert_eq!(detection.polygon, None);
        assert_eq!(
            detection.bbox,
            Bbox::try_from([1.0, 2.0, 50.0, 99.5])
                .expect("the expected bbox must be valid")
        );
    }

    /// Verifies exported order and raw source index form the stable sort key.
    #[test]
    fn detections_sort_by_order_then_source_index() {
        let rows = array![
            [21.0, 0.9, 0.0, 0.0, 20.0, 20.0, 7.0],
            [22.0, 0.9, 20.0, 0.0, 40.0, 20.0, 3.0],
            [17.0, 0.9, 40.0, 0.0, 60.0, 20.0, 3.0],
        ];

        let detections = postprocess_page(rows.view(), 3, 0.5, &transform())
            .expect("valid rows must postprocess");
        let keys: Vec<_> = detections
            .iter()
            .map(|detection| {
                (detection.model_order, detection.source_detection_index)
            })
            .collect();

        assert_eq!(keys, vec![(3, 1), (3, 2), (7, 0)]);
    }

    /// Verifies malformed output shapes and counts fail before row access.
    #[test]
    fn malformed_bbox_output_is_rejected() {
        let wrong_columns = array![[22.0, 0.9, 0.0, 0.0, 1.0, 1.0]];
        assert!(matches!(
            postprocess_page(wrong_columns.view(), 1, 0.5, &transform()),
            Err(crate::PostprocessError::InvalidColumns { .. })
        ));

        let rows = array![[22.0, 0.9, 0.0, 0.0, 1.0, 1.0, 0.0]];
        assert!(matches!(
            postprocess_page(rows.view(), 2, 0.5, &transform()),
            Err(crate::PostprocessError::InvalidCount { .. })
        ));
    }
}
