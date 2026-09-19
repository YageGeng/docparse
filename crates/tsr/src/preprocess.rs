//! SLANet_plus BGR resize, normalization, and padding over owned PDF rasters.
use crate::TsrError;
use docparse_layout::PageImage;
use ndarray::{Array2, Array4};

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

    /// Preserves the original SLANet+ preprocessing entry point.
    fn try_from(image: &PageImage) -> Result<Self, Self::Error> {
        Self::try_from((image, EDGE))
    }
}

impl TryFrom<(&PageImage, usize)> for SlanetInput {
    type Error = TsrError;

    /// Preserves aspect ratio, converts RGB to BGR, and follows the pinned PaddleX tensor contract.
    #[allow(
        clippy::cast_sign_loss,
        reason = "coordinates and interpolated pixels are clamped to nonnegative ranges"
    )]
    fn try_from(
        (image, edge): (&PageImage, usize),
    ) -> Result<Self, Self::Error> {
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
        let ratio = edge as f64 / width.max(height) as f64;
        let resized_width = (width as f64 * ratio).round_ties_even() as usize;
        let resized_height = (height as f64 * ratio).round_ties_even() as usize;
        if resized_width == 0
            || resized_height == 0
            || resized_width > edge
            || resized_height > edge
        {
            return Err(TsrError::InvalidInput {
                reason: "table aspect ratio produces an empty model dimension"
                    .to_owned(),
            });
        }
        let columns = LinearAxis::resize(width, resized_width);
        let rows = LinearAxis::resize(height, resized_height);
        let mut tensor = Array4::<f32>::zeros((1, 3, edge, edge));
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

/// Three owned RT-DETR tensors retain the original crop scale through inference.
pub(crate) struct CellInput {
    pub image: Array4<f32>,
    pub image_shape: Array2<f32>,
    pub scale_factor: Array2<f32>,
}

impl TryFrom<&PageImage> for CellInput {
    type Error = TsrError;

    /// Uses the same cubic RGB resize as layout and the pinned detector normalization.
    #[allow(
        clippy::indexing_slicing,
        reason = "the verified RGB resizer returns exactly 640 by 640 packed pixels and NCHW indices have fixed bounds"
    )]
    fn try_from(image: &PageImage) -> Result<Self, Self::Error> {
        let pixels = image.resize_rgb_cubic(640, 640).map_err(|error| {
            TsrError::InvalidInput {
                reason: error.to_string(),
            }
        })?;
        let tensor =
            Array4::from_shape_fn((1, 3, 640, 640), |(_, channel, y, x)| {
                f32::from(pixels[(y * 640 + x) * 3 + channel]) / 255.0
            });
        Ok(Self {
            image: tensor,
            image_shape: ndarray::arr2(&[[640.0, 640.0]]),
            scale_factor: ndarray::arr2(&[[
                640.0 / image.height() as f32,
                640.0 / image.width() as f32,
            ]]),
        })
    }
}

/// Owns the appropriate tensor set until either native inference or the browser Promise settles.
pub(crate) enum ModelInput {
    Structure(SlanetInput),
    Cells(CellInput),
    Tatr(crate::tatr::TatrInput),
}

impl ModelInput {
    /// Concatenates ready crops and keeps detector image sizes/scales in the same sample order.
    pub(crate) fn batch(inputs: &[&Self]) -> Result<Self, TsrError> {
        let invalid = || TsrError::InvalidInput {
            reason: "TSR batches require 1..=32 compatible singleton crops"
                .to_owned(),
        };
        if inputs.is_empty() || inputs.len() > 32 {
            return Err(invalid());
        }
        // TATR batches need per-crop masks because their resized spatial dimensions differ.
        if inputs.iter().any(|input| matches!(input, Self::Tatr(_))) {
            let tatr = inputs
                .iter()
                .map(|input| match input {
                    Self::Tatr(input) => Ok(input),
                    _ => Err(invalid()),
                })
                .collect::<Result<Vec<_>, _>>()?;
            return crate::tatr::TatrInput::batch(&tatr).map(Self::Tatr);
        }
        let mut images = Vec::with_capacity(inputs.len());
        let mut shapes = Vec::new();
        let mut scales = Vec::new();
        for input in inputs {
            let image = match input {
                Self::Tatr(_) => return Err(invalid()),
                Self::Structure(input) => &input.0,
                Self::Cells(input) => {
                    if input.image_shape.dim() != (1, 2)
                        || input.scale_factor.dim() != (1, 2)
                    {
                        return Err(invalid());
                    }
                    shapes.push(input.image_shape.view());
                    scales.push(input.scale_factor.view());
                    &input.image
                }
            };
            if image.dim().0 != 1 {
                return Err(invalid());
            }
            images.push(image.view());
        }
        let shape_error = |error: ndarray::ShapeError| TsrError::InvalidInput {
            reason: format!("incompatible TSR batch tensors: {error}"),
        };
        let image = ndarray::concatenate(ndarray::Axis(0), &images)
            .map_err(shape_error)?;
        if shapes.is_empty() {
            Ok(Self::Structure(SlanetInput(image)))
        } else if shapes.len() == inputs.len() {
            Ok(Self::Cells(CellInput {
                image,
                image_shape: ndarray::concatenate(ndarray::Axis(0), &shapes)
                    .map_err(shape_error)?,
                scale_factor: ndarray::concatenate(ndarray::Axis(0), &scales)
                    .map_err(shape_error)?,
            }))
        } else {
            Err(invalid())
        }
    }

    /// Borrows mixed float/int tensors so TATR masks share the native and browser session path.
    pub(crate) fn values(
        &self,
    ) -> Result<
        Vec<(
            std::borrow::Cow<'static, str>,
            ort::session::SessionInputValue<'_>,
        )>,
        ort::Error,
    > {
        use ort::value::TensorRef;
        Ok(match self {
            Self::Structure(input) => {
                ort::inputs!["x" => TensorRef::from_array_view(&input.0)?]
            }
            Self::Cells(input) => ort::inputs![
                "image" => TensorRef::from_array_view(&input.image)?,
                "im_shape" => TensorRef::from_array_view(&input.image_shape)?,
                "scale_factor" => TensorRef::from_array_view(&input.scale_factor)?,
            ],
            Self::Tatr(input) => ort::inputs![
                "pixel_values" => TensorRef::from_array_view(&input.pixels)?,
                "pixel_mask" => TensorRef::from_array_view(&input.mask)?,
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_layout::{PageImageInput, PixelFormat};
    use std::sync::Arc;

    /// Batched tensors preserve crop order and keep each detector's resize metadata attached to its image.
    #[test]
    fn batches_preserve_sample_order_and_detector_scales() {
        let structures = [1.0, 2.0].map(|value| {
            ModelInput::Structure(SlanetInput(Array4::from_elem(
                (1, 3, 2, 2),
                value,
            )))
        });
        let ModelInput::Structure(batch) =
            ModelInput::batch(&[&structures[0], &structures[1]])
                .expect("structure batch")
        else {
            unreachable!("structure")
        };
        assert_eq!(batch.0.dim(), (2, 3, 2, 2));
        assert_eq!(batch.0.get((0, 0, 0, 0)), Some(&1.0));
        assert_eq!(batch.0.get((1, 0, 0, 0)), Some(&2.0));
        let cells = [1.0, 2.0].map(|value| {
            ModelInput::Cells(CellInput {
                image: Array4::from_elem((1, 3, 2, 2), value),
                image_shape: ndarray::arr2(&[[100.0 * value, 200.0 * value]]),
                scale_factor: ndarray::arr2(&[[value, value * 2.0]]),
            })
        });
        let ModelInput::Cells(batch) =
            ModelInput::batch(&[&cells[0], &cells[1]]).expect("cell batch")
        else {
            unreachable!("cells")
        };
        assert_eq!(batch.image.dim(), (2, 3, 2, 2));
        assert_eq!(
            batch.image_shape,
            ndarray::arr2(&[[100.0, 200.0], [200.0, 400.0]])
        );
        assert_eq!(
            batch.scale_factor,
            ndarray::arr2(&[[1.0, 2.0], [2.0, 4.0]])
        );
        assert!(ModelInput::batch(&[]).is_err());
        assert!(ModelInput::batch(&[&structures[0], &cells[0]]).is_err());
    }

    /// Cell detection keeps RGB channels and describes the actual stretched 640-pixel input.
    #[test]
    fn cell_tensor_uses_rgb_and_actual_scale_factors() {
        let image = PageImage::try_from(
            PageImageInput::builder()
                .width(2)
                .height(1)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from([255_u8, 0, 0, 255, 0, 0]))
                .build(),
        )
        .expect("RGB");
        let input = CellInput::try_from(&image).expect("cell tensor");
        assert_eq!(input.image.shape(), [1, 3, 640, 640]);
        assert!(
            (input.image.get((0, 0, 0, 0)).expect("red") - 1.0).abs()
                < f32::EPSILON
        );
        assert!(
            input.image.get((0, 2, 0, 0)).expect("blue").abs() < f32::EPSILON
        );
        assert_eq!(
            input.scale_factor.as_slice().expect("scale"),
            [640.0, 320.0]
        );
    }

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
