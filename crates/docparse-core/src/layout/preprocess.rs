//! PP-StructureV3 image preprocessing.

use image::{DynamicImage, imageops::FilterType};

use super::{LayoutBatchInput, LayoutError, LayoutInput, OriginalImageSize};

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

    let width = options.target_width as usize;
    let height = options.target_height as usize;
    let mut tensor = ndarray::Array4::<f32>::zeros((1, 3, height, width));
    // Reuse the same tensor writer as batched preprocessing so single-page and
    // multi-page inference feed identical pixels into the model.
    write_image_to_batch_tensor(&mut tensor, 0, image, options)?;

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

/// Converts multiple images into PP-StructureV3 input tensors with a shared batch dimension.
pub fn preprocess_images(
    images: &[DynamicImage],
    options: PreprocessOptions,
) -> Result<LayoutBatchInput, LayoutError> {
    if images.is_empty() {
        return Err(LayoutError::Preprocess(
            "input image batch must not be empty".to_owned(),
        ));
    }

    let width = options.target_width as usize;
    let height = options.target_height as usize;
    let mut image_tensor =
        ndarray::Array4::<f32>::zeros((images.len(), 3, height, width));
    let mut im_shape = ndarray::Array2::<f32>::zeros((images.len(), 2));
    let mut scale_factor = ndarray::Array2::<f32>::zeros((images.len(), 2));
    let mut original_sizes = Vec::with_capacity(images.len());

    for (batch_index, image) in images.iter().enumerate() {
        if image.width() == 0 || image.height() == 0 {
            return Err(LayoutError::Preprocess(
                "input image dimensions must be non-zero".to_owned(),
            ));
        }

        write_image_to_batch_tensor(
            &mut image_tensor,
            batch_index,
            image,
            options,
        )?;
        *im_shape.get_mut((batch_index, 0)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write batch image height".to_owned(),
            )
        })? = options.target_height as f32;
        *im_shape.get_mut((batch_index, 1)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write batch image width".to_owned(),
            )
        })? = options.target_width as f32;
        *scale_factor.get_mut((batch_index, 0)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write batch height scale".to_owned(),
            )
        })? = options.target_height as f32 / image.height() as f32;
        *scale_factor.get_mut((batch_index, 1)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write batch width scale".to_owned(),
            )
        })? = options.target_width as f32 / image.width() as f32;
        original_sizes.push(OriginalImageSize {
            width: image.width(),
            height: image.height(),
        });
    }

    Ok(LayoutBatchInput {
        image: image_tensor,
        im_shape,
        scale_factor,
        original_sizes,
    })
}

/// Writes one resized RGB image into a selected batch slot of an NCHW tensor.
fn write_image_to_batch_tensor(
    tensor: &mut ndarray::Array4<f32>,
    batch_index: usize,
    image: &DynamicImage,
    options: PreprocessOptions,
) -> Result<(), LayoutError> {
    let rgb = image.to_rgb8();
    let resized = image::imageops::resize(
        &rgb,
        options.target_width,
        options.target_height,
        FilterType::Triangle,
    );

    for (x, y, pixel) in resized.enumerate_pixels() {
        let x = x as usize;
        let y = y as usize;
        *tensor.get_mut((batch_index, 0, y, x)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write red channel into input tensor".to_owned(),
            )
        })? = f32::from(pixel[0]) / 255.0;
        *tensor.get_mut((batch_index, 1, y, x)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write green channel into input tensor".to_owned(),
            )
        })? = f32::from(pixel[1]) / 255.0;
        *tensor.get_mut((batch_index, 2, y, x)).ok_or_else(|| {
            LayoutError::Preprocess(
                "failed to write blue channel into input tensor".to_owned(),
            )
        })? = f32::from(pixel[2]) / 255.0;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use image::{DynamicImage, RgbImage};

    use super::{PreprocessOptions, preprocess_image, preprocess_images};

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

    #[test]
    fn preprocess_images_returns_batched_tensors_and_original_sizes() {
        let images = [
            DynamicImage::ImageRgb8(RgbImage::new(2, 4)),
            DynamicImage::ImageRgb8(RgbImage::new(4, 2)),
        ];

        let input = preprocess_images(
            &images,
            PreprocessOptions {
                target_width: 8,
                target_height: 6,
            },
        )
        .expect("batch preprocess should succeed");

        assert_eq!(input.image.shape(), &[2, 3, 6, 8]);
        assert_eq!(input.im_shape.shape(), &[2, 2]);
        assert_eq!(input.scale_factor.shape(), &[2, 2]);
        assert_eq!(input.original_sizes.first().expect("first size").width, 2);
        assert_eq!(input.original_sizes.get(1).expect("second size").height, 2);
        assert_close(
            *input.scale_factor.get((0, 0)).expect("first height scale"),
            1.5,
        );
        assert_close(
            *input.scale_factor.get((1, 1)).expect("second width scale"),
            2.0,
        );
    }
}
