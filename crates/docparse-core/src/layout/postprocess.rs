//! PP-StructureV3 layout postprocessing.

use super::{LayoutBlock, LayoutBox, LayoutError, LayoutLabel, LayoutPage};

/// Postprocessing options.
#[derive(Debug, Clone, Copy)]
pub struct PostprocessOptions {
    /// Minimum confidence retained in output.
    pub threshold: f32,
}

impl Default for PostprocessOptions {
    fn default() -> Self {
        Self { threshold: 0.5 }
    }
}

/// Decodes Paddle-style fetch rows into typed layout detections.
pub fn postprocess_fetch_rows(
    values: &[f32],
    columns: usize,
    image_width: u32,
    image_height: u32,
    options: PostprocessOptions,
) -> Result<LayoutPage, LayoutError> {
    if columns < 6 {
        return Err(LayoutError::Postprocess(
            "fetch rows must contain at least 6 columns".to_owned(),
        ));
    }
    if !values.len().is_multiple_of(columns) {
        return Err(LayoutError::Postprocess(
            "fetch row value count must be divisible by columns".to_owned(),
        ));
    }

    let mut blocks = Vec::new();
    for row in values.chunks_exact(columns) {
        let class_value = row.first().copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing class id".to_owned())
        })?;
        let score = row.get(1).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing score".to_owned())
        })?;
        if score < options.threshold {
            continue;
        }
        let class_id = if class_value.is_sign_negative() {
            0
        } else {
            class_value as i64
        };
        let order = if columns >= 7 {
            row.get(6).copied().map(|value| value as i64)
        } else {
            None
        };
        let x_min = row.get(2).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing x_min".to_owned())
        })?;
        let y_min = row.get(3).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing y_min".to_owned())
        })?;
        let x_max = row.get(4).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing x_max".to_owned())
        })?;
        let y_max = row.get(5).copied().ok_or_else(|| {
            LayoutError::Postprocess("fetch row is missing y_max".to_owned())
        })?;
        blocks.push(LayoutBlock {
            label: LayoutLabel::from_class_id(class_id),
            score,
            bbox: LayoutBox::from_xyxy_clamped(
                x_min,
                y_min,
                x_max,
                y_max,
                image_width,
                image_height,
            ),
            order,
        });
    }

    blocks.sort_by(|left, right| {
        left.order
            .unwrap_or(i64::MAX)
            .cmp(&right.order.unwrap_or(i64::MAX))
    });

    Ok(LayoutPage {
        width: image_width,
        height: image_height,
        blocks,
    })
}

#[cfg(test)]
mod tests {
    use super::{LayoutBox, LayoutLabel};

    use super::{PostprocessOptions, postprocess_fetch_rows};

    #[test]
    fn fetch_postprocess_filters_threshold_and_orders_results() {
        let values = vec![
            22.0, 0.95, 1.0, 2.0, 10.0, 20.0, 2.0, 8.0, 0.20, 0.0, 0.0, 5.0,
            5.0, 1.0, 14.0, 0.90, -1.0, -2.0, 30.0, 40.0, 1.0,
        ];

        let page = postprocess_fetch_rows(
            &values,
            7,
            25,
            30,
            PostprocessOptions { threshold: 0.5 },
        )
        .expect("postprocess should succeed");

        assert_eq!(page.blocks.len(), 2);
        let first = page.blocks.first().expect("first block should exist");
        let second = page.blocks.get(1).expect("second block should exist");
        assert_eq!(first.label, LayoutLabel::Image);
        assert_eq!(
            first.bbox,
            LayoutBox {
                x: 0.0,
                y: 0.0,
                width: 25.0,
                height: 30.0
            }
        );
        assert_eq!(second.label, LayoutLabel::Text);
    }
}
