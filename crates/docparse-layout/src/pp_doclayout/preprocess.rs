//! PP-DocLayoutV3 image preprocessing.
//!
//! Preprocessor config validation guarantees three image channels and matching
//! normalization vectors, so the NCHW conversion can index directly.
#![allow(clippy::indexing_slicing)]

use anyhow::{Result, bail};
use image::{DynamicImage, GenericImageView};

use crate::pp_doclayout::config::PpDocLayoutPreprocessorConfig;

/// Normalized model input plus original image dimensions.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LayoutInput {
    pub(crate) values: Vec<f32>,
    pub(crate) channels: usize,
    pub(crate) height: usize,
    pub(crate) width: usize,
    pub(crate) original_height: u32,
    pub(crate) original_width: u32,
}

/// Converts an image into normalized NCHW PP-DocLayoutV3 input values.
pub(crate) fn preprocess_layout_image(
    image: &DynamicImage,
    config: &PpDocLayoutPreprocessorConfig,
) -> Result<LayoutInput> {
    let (original_width, original_height) = image.dimensions();
    if original_width == 0 || original_height == 0 {
        bail!("layout input image cannot be empty");
    }
    if config.image_mean.len() < 3 || config.image_std.len() < 3 {
        bail!("layout image normalization config must have three channels");
    }

    // Keep PaddleX parity: PP-DocLayoutV3 uses OpenCV INTER_CUBIC resize.
    let rgb = if config.do_resize {
        crate::ml::imageproc::resize_cubic_cv2(
            image,
            config.size.width,
            config.size.height,
        )
    } else {
        image.to_rgb8()
    };
    let width = rgb.width() as usize;
    let height = rgb.height() as usize;
    let mut values = vec![0.0; 3 * height * width];

    for y in 0..height {
        for x in 0..width {
            let pixel = rgb.get_pixel(x as u32, y as u32);
            for channel in 0..3 {
                let scaled = f32::from(pixel[channel]) / 255.0;
                values[channel * height * width + y * width + x] = (scaled
                    - config.image_mean[channel])
                    / config.image_std[channel];
            }
        }
    }

    Ok(LayoutInput {
        values,
        channels: 3,
        height,
        width,
        original_height,
        original_width,
    })
}
