//! PP-StructureV3 image preprocessing.

use image::{DynamicImage, imageops::FilterType};

use super::{LayoutError, LayoutInput};

/// Preprocessing options.
#[derive(Debug, Clone, Copy)]
pub struct PreprocessOptions {
    /// Target model width.
    pub target_width: u32,
    /// Target model height.
    pub target_height: u32,
}

impl Default for PreprocessOptions {
    fn default() -> Self {
        Self {
            target_width: 800,
            target_height: 800,
        }
    }
}

/// Converts an image into PP-StructureV3 input tensors.
pub fn preprocess_image(
    image: &DynamicImage,
    options: PreprocessOptions,
) -> Result<LayoutInput, LayoutError> {
    if image.width() == 0 || image.height() == 0 {
        return Err(LayoutError::Preprocess(
            "input image dimensions must be non-zero".to_owned(),
        ));
    }

    let rgb = image.to_rgb8();
    let resized = image::imageops::resize(
        &rgb,
        options.target_width,
        options.target_height,
        FilterType::Triangle,
    );
    let width = options.target_width as usize;
    let height = options.target_height as usize;
    let mut tensor = ndarray::Array4::<f32>::zeros((1, 3, height, width));

    for (x, y, pixel) in resized.enumerate_pixels() {
        let x = x as usize;
        let y = y as usize;
        *tensor.get_mut((0, 0, y, x)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write red channel into input tensor".to_owned(),
            )
        })? = f32::from(pixel[0]) / 255.0;
        *tensor.get_mut((0, 1, y, x)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write green channel into input tensor".to_owned(),
            )
        })? = f32::from(pixel[1]) / 255.0;
        *tensor.get_mut((0, 2, y, x)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write blue channel into input tensor".to_owned(),
            )
        })? = f32::from(pixel[2]) / 255.0;
    }

    Ok(LayoutInput {
        image: tensor,
        im_shape: ndarray::arr2(&[[
            options.target_height as f32,
            options.target_width as f32,
        ]]),
        scale_factor: ndarray::arr2(&[[
            options.target_height as f32 / image.height() as f32,
            options.target_width as f32 / image.width() as f32,
        ]]),
        original_width: image.width(),
        original_height: image.height(),
    })
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, RgbImage};

    use super::{PreprocessOptions, preprocess_image};

    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() < f32::EPSILON,
            "expected {actual} to be close to {expected}"
        );
    }

    #[test]
    fn preprocess_returns_nchw_tensor_and_scale_factors() {
        let image = DynamicImage::ImageRgb8(RgbImage::new(2, 4));
        let input = preprocess_image(
            &image,
            PreprocessOptions {
                target_width: 8,
                target_height: 6,
            },
        )
        .expect("preprocess should succeed");

        assert_eq!(input.image.shape(), &[1, 3, 6, 8]);
        assert_close(
            *input
                .im_shape
                .get((0, 0))
                .expect("height shape should exist"),
            6.0,
        );
        assert_close(
            *input
                .im_shape
                .get((0, 1))
                .expect("width shape should exist"),
            8.0,
        );
        assert_close(
            *input
                .scale_factor
                .get((0, 0))
                .expect("height scale should exist"),
            1.5,
        );
        assert_close(
            *input
                .scale_factor
                .get((0, 1))
                .expect("width scale should exist"),
            4.0,
        );
    }
}
