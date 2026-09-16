//! PP-FormulaNet foreground cropping, normalization, and batching over owned RGB crops.
use crate::{FormulaError, artifacts::ModelKind};
use docparse_layout::PageImage;
use ndarray::Array4;
use std::sync::Arc;

/// Pinned formula preprocessing, adapted from Apache-2.0 OAR-OCR formula_preprocess.rs.
pub(crate) struct FormulaInput(pub(crate) Array4<f32>);

impl TryFrom<(Vec<Arc<PageImage>>, ModelKind)> for FormulaInput {
    type Error = FormulaError;

    /// Crops normalized dark foreground, centers it on black, and produces one grayscale channel.
    #[allow(clippy::cast_sign_loss)] // Pixel differences are nonnegative; resized dimensions are clamped to 1..768.
    fn try_from(
        (images, kind): (Vec<Arc<PageImage>>, ModelKind),
    ) -> Result<Self, Self::Error> {
        // The pinned Small/Medium and Large graphs have different fixed spatial input dimensions.
        let edge: u32 = match kind {
            ModelKind::PlusS | ModelKind::PlusM => 384,
            ModelKind::PlusL => 768,
        };
        let batch = images.len();
        let mut values =
            Vec::with_capacity(batch * edge as usize * edge as usize);
        for page in images {
            let image = image::RgbImage::from_raw(
                page.width(),
                page.height(),
                page.data().to_vec(),
            )
            .ok_or_else(|| FormulaError::Invalid("invalid RGB crop".into()))?;
            let gray = image::DynamicImage::ImageRgb8(image.clone()).to_luma8();
            let minimum = gray.as_raw().iter().copied().min().unwrap_or(0);
            let maximum = gray.as_raw().iter().copied().max().unwrap_or(0);
            let (mut left, mut top, mut right, mut bottom) =
                (image.width(), image.height(), 0, 0);
            if maximum > minimum {
                for (x, y, pixel) in gray.enumerate_pixels() {
                    let normalized = ((f32::from(pixel.0[0])
                        - f32::from(minimum))
                        / f32::from(maximum - minimum)
                        * 255.0) as u8;
                    if normalized < 200 {
                        left = left.min(x);
                        top = top.min(y);
                        right = right.max(x);
                        bottom = bottom.max(y);
                    }
                }
            }
            let cropped = if left < right && top < bottom {
                image::imageops::crop_imm(
                    &image,
                    left,
                    top,
                    right - left + 1,
                    bottom - top + 1,
                )
                .to_image()
            } else {
                image
            };
            let scale =
                edge as f32 / cropped.width().max(cropped.height()) as f32;
            let width = (cropped.width() as f32 * scale)
                .floor()
                .clamp(1.0, edge as f32) as u32;
            let height = (cropped.height() as f32 * scale)
                .floor()
                .clamp(1.0, edge as f32) as u32;
            let resized = image::imageops::resize(
                &cropped,
                width,
                height,
                image::imageops::FilterType::Triangle,
            );
            let mut padded = image::RgbImage::new(edge, edge);
            image::imageops::overlay(
                &mut padded,
                &resized,
                i64::from((edge - width) / 2),
                i64::from((edge - height) / 2),
            );
            for pixel in padded.pixels() {
                let [r, g, b] = pixel.0.map(|value| {
                    (f32::from(value) * (1.0 / 255.0) - 0.7931) / 0.1738
                });
                values.push(0.114 * b + 0.587 * g + 0.299 * r);
            }
        }
        Array4::from_shape_vec((batch, 1, edge as usize, edge as usize), values)
            .map(Self)
            .map_err(|error| FormulaError::Invalid(error.to_string()))
    }
}
