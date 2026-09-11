//! SLANet_plus BGR resize, normalization, and padding over owned PDF rasters.
use crate::TsrError;
use docparse_layout::PageImage;
use ndarray::Array4;

const EDGE: usize = 488;
const SCALE: i32 = 2048;

/// An OpenCV-style fixed-point pair for one destination coordinate.
struct LinearAxis {
    indices: [usize; 2],
    weights: [i32; 2],
}

impl LinearAxis {
    /// Uses half-pixel centers and border replication without antialiasing the downsampled image.
    #[allow(
        clippy::cast_sign_loss,
        reason = "source coordinates are clamped to nonnegative pixel indices"
    )]
    fn resize(source: usize, destination: usize) -> Vec<Self> {
        (0..destination)
            .map(|position| {
                let coordinate = (((position as f64 + 0.5) * source as f64
                    / destination as f64
                    - 0.5) as f32)
                    .clamp(0.0, source.saturating_sub(1) as f32);
                let first = coordinate.floor() as usize;
                let weight = ((coordinate - first as f32) * SCALE as f32)
                    .round_ties_even() as i32;
                Self {
                    indices: [first, (first + 1).min(source - 1)],
                    weights: [SCALE - weight, weight],
                }
            })
            .collect()
    }
}

/// One bounded NCHW tensor; normalized padding is zero, not normalized black pixels.
pub(crate) struct SlanetInput(pub Array4<f32>);

impl TryFrom<&PageImage> for SlanetInput {
    type Error = TsrError;

    /// Preserves aspect ratio, converts RGB to BGR, and follows the pinned PaddleX tensor contract.
    #[allow(
        clippy::cast_sign_loss,
        reason = "coordinates and interpolated pixels are clamped to nonnegative ranges"
    )]
    fn try_from(image: &PageImage) -> Result<Self, Self::Error> {
        let width = image.width() as usize;
        let height = image.height() as usize;
        if width == 0
            || height == 0
            || width.checked_mul(height).and_then(|n| n.checked_mul(3))
                != Some(image.data().len())
        {
            return Err(TsrError::InvalidInput {
                reason: "TSR needs a nonempty packed RGB image".to_owned(),
            });
        }
        let ratio = EDGE as f64 / width.max(height) as f64;
        let resized_width = (width as f64 * ratio).round_ties_even() as usize;
        let resized_height = (height as f64 * ratio).round_ties_even() as usize;
        if resized_width == 0
            || resized_height == 0
            || resized_width > EDGE
            || resized_height > EDGE
        {
            return Err(TsrError::InvalidInput {
                reason: "table aspect ratio produces an empty model dimension"
                    .to_owned(),
            });
        }
        let columns = LinearAxis::resize(width, resized_width);
        let rows = LinearAxis::resize(height, resized_height);
        let mut tensor = Array4::<f32>::zeros((1, 3, EDGE, EDGE));
        let coefficients =
            [(0.485_f64, 0.229_f64), (0.456, 0.224), (0.406, 0.225)];
        for (channel, (mean, std)) in coefficients.into_iter().enumerate() {
            let alpha = (1.0 / 255.0 / std) as f32;
            let beta = (-mean / std) as f32;
            for (y, vertical) in rows.iter().enumerate() {
                for (x, horizontal) in columns.iter().enumerate() {
                    let mut total = 0_i32;
                    for (&row, &wy) in
                        vertical.indices.iter().zip(&vertical.weights)
                    {
                        let mut value = 0_i32;
                        for (&column, &wx) in
                            horizontal.indices.iter().zip(&horizontal.weights)
                        {
                            let offset =
                                (row * width + column) * 3 + (2 - channel);
                            let sample =
                                image.data().get(offset).ok_or_else(|| {
                                    TsrError::InvalidInput {
                                        reason:
                                            "RGB pixel index exceeds its image"
                                                .to_owned(),
                                    }
                                })?;
                            value += i32::from(*sample) * wx;
                        }
                        total += value * wy;
                    }
                    let pixel =
                        ((total + (1 << 21)) >> 22).clamp(0, 255) as f32;
                    let target = tensor
                        .get_mut((0, channel, y, x))
                        .ok_or_else(|| TsrError::InvalidInput {
                            reason: "TSR tensor index exceeds its shape"
                                .to_owned(),
                        })?;
                    *target = pixel * alpha + beta;
                }
            }
        }
        Ok(Self(tensor))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_layout::{PageImageInput, PixelFormat};
    use std::sync::Arc;

    /// RGB channel order and post-normalization padding must match Paddle's BGR model input.
    #[test]
    fn bgr_and_zero_padding_follow_model_contract() {
        let image = PageImage::try_from(
            PageImageInput::builder()
                .width(2)
                .height(1)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from([255_u8, 0, 0, 0, 255, 0]))
                .build(),
        )
        .expect("RGB");
        let SlanetInput(tensor) =
            SlanetInput::try_from(&image).expect("preprocess");
        assert_eq!(tensor.shape(), [1, 3, 488, 488]);
        for (channel, expected) in
            [(0, -2.117_904_f32), (1, -2.0357141), (2, 2.6399999)]
        {
            assert!(
                (tensor.get((0, channel, 0, 0)).expect("pixel") - expected)
                    .abs()
                    < 1e-5
            );
            assert_eq!(tensor.get((0, channel, 244, 0)), Some(&0.0));
        }
    }

    /// Bilinear quantization stays within one RGB level of an independently generated OpenCV tensor.
    #[test]
    fn tensor_matches_opencv_reference_with_byte_rounding_tolerance() {
        use std::io::Read;
        let raster = image::load_from_memory(include_bytes!(
            "../tests/fixtures/table.png"
        ))
        .expect("fixture")
        .into_rgb8();
        let input = PageImage::try_from(
            PageImageInput::builder()
                .width(raster.width())
                .height(raster.height())
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(raster.into_raw()))
                .build(),
        )
        .expect("RGB");
        let SlanetInput(tensor) =
            SlanetInput::try_from(&input).expect("tensor");
        let mut reference = Vec::new();
        flate2::read::GzDecoder::new(
            include_bytes!("../tests/fixtures/table-input.f32.gz").as_slice(),
        )
        .read_to_end(&mut reference)
        .expect("oracle");
        assert_eq!(reference.len(), tensor.len() * 4);
        let mut maximum = 0.0_f32;
        for (actual, bytes) in tensor.iter().zip(reference.as_chunks::<4>().0) {
            let expected = f32::from_le_bytes(*bytes);
            maximum = maximum.max((actual - expected).abs());
        }
        assert!(
            maximum <= 1.0 / (255.0 * 0.224) + 1e-5,
            "maximum normalized difference {maximum}"
        );
    }
}
