use ndarray::{Array2, Array4};

use crate::{PageImage, PageTransform, PreprocessError};

const CHANNELS: usize = 3;
const COEFFICIENT_BITS: u32 = 11;
const COEFFICIENT_SCALE: f32 = (1_u32 << COEFFICIENT_BITS) as f32;
const OUTPUT_SHIFT: u32 = COEFFICIENT_BITS * 2;

/// Tensor values supplied to the fixed three-input ONNX graph.
#[derive(Debug)]
pub(crate) struct ModelInputs {
    pub(crate) image: Array4<f32>,
    pub(crate) image_size: Array2<f32>,
    pub(crate) scale_factor: Array2<f32>,
}

#[derive(Debug, Clone, Copy)]
struct InterpolationWeights {
    indices: [usize; 4],
    coefficients: [i32; 4],
}

/// Runs OpenCV-compatible RGB8 INTER_CUBIC resize and NCHW normalization.
pub(crate) fn preprocess(
    image: &PageImage,
    transform: &PageTransform,
) -> Result<ModelInputs, PreprocessError> {
    let (render_width, render_height) = transform.render_size();
    if (image.width(), image.height()) != (render_width, render_height) {
        return Err(PreprocessError::ImageTransformMismatch {
            image_width: image.width(),
            image_height: image.height(),
            render_width,
            render_height,
        });
    }
    let (model_width, model_height) = transform.model_size();
    let resized = resize_inter_cubic(
        image.data().as_ref(),
        image.width(),
        image.height(),
        model_width,
        model_height,
    )?;

    let model_width_usize = usize::try_from(model_width)
        .map_err(|_source| PreprocessError::ArithmeticOverflow)?;
    let model_height_usize = usize::try_from(model_height)
        .map_err(|_source| PreprocessError::ArithmeticOverflow)?;
    let tensor_length = model_width_usize
        .checked_mul(model_height_usize)
        .and_then(|pixels| pixels.checked_mul(CHANNELS))
        .ok_or(PreprocessError::ArithmeticOverflow)?;
    let mut nchw = Vec::with_capacity(tensor_length);
    let scale = 1.0_f32 / 255.0_f32;
    for channel in 0..CHANNELS {
        for y in 0..model_height_usize {
            for x in 0..model_width_usize {
                let index = y
                    .checked_mul(model_width_usize)
                    .and_then(|row| row.checked_add(x))
                    .and_then(|pixel| pixel.checked_mul(CHANNELS))
                    .and_then(|pixel| pixel.checked_add(channel))
                    .ok_or(PreprocessError::ArithmeticOverflow)?;
                let value =
                    resized.get(index).ok_or(PreprocessError::PixelIndex {
                        index,
                        length: resized.len(),
                    })?;
                nchw.push(f32::from(*value) * scale);
            }
        }
    }

    let image = Array4::from_shape_vec(
        (1, CHANNELS, model_height_usize, model_width_usize),
        nchw,
    )
    .map_err(|source| PreprocessError::TensorShape { source })?;
    let image_size = Array2::from_shape_vec(
        (1, 2),
        vec![model_height as f32, model_width as f32],
    )
    .map_err(|source| PreprocessError::TensorShape { source })?;
    let scale_factor = Array2::from_shape_vec(
        (1, 2),
        vec![
            model_height as f32 / render_height as f32,
            model_width as f32 / render_width as f32,
        ],
    )
    .map_err(|source| PreprocessError::TensorShape { source })?;
    Ok(ModelInputs {
        image,
        image_size,
        scale_factor,
    })
}

/// Resizes interleaved RGB8 pixels with OpenCV's separable fixed-point cubic path.
fn resize_inter_cubic(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    destination_width: u32,
    destination_height: u32,
) -> Result<Vec<u8>, PreprocessError> {
    let source_width = usize::try_from(source_width)
        .map_err(|_source| PreprocessError::ArithmeticOverflow)?;
    let source_height = usize::try_from(source_height)
        .map_err(|_source| PreprocessError::ArithmeticOverflow)?;
    let destination_width = usize::try_from(destination_width)
        .map_err(|_source| PreprocessError::ArithmeticOverflow)?;
    let destination_height = usize::try_from(destination_height)
        .map_err(|_source| PreprocessError::ArithmeticOverflow)?;
    if source_width == 0
        || source_height == 0
        || destination_width == 0
        || destination_height == 0
    {
        return Err(PreprocessError::ArithmeticOverflow);
    }
    let source_row_length = source_width
        .checked_mul(CHANNELS)
        .ok_or(PreprocessError::ArithmeticOverflow)?;
    let source_length = source_row_length
        .checked_mul(source_height)
        .ok_or(PreprocessError::ArithmeticOverflow)?;
    let source =
        source
            .get(..source_length)
            .ok_or(PreprocessError::PixelIndex {
                index: source_length.saturating_sub(1),
                length: source.len(),
            })?;
    let output_length = destination_width
        .checked_mul(destination_height)
        .and_then(|pixels| pixels.checked_mul(CHANNELS))
        .ok_or(PreprocessError::ArithmeticOverflow)?;
    let x_weights = axis_weights(source_width, destination_width);
    let y_weights = axis_weights(source_height, destination_height);
    let intermediate_row_length = destination_width
        .checked_mul(CHANNELS)
        .ok_or(PreprocessError::ArithmeticOverflow)?;
    let intermediate_length = intermediate_row_length
        .checked_mul(source_height)
        .ok_or(PreprocessError::ArithmeticOverflow)?;
    let mut horizontal_values = Vec::with_capacity(intermediate_length);

    // The horizontal pass computes each four-tap source combination once per source row.
    // Cubic coefficients stay within a small multiple of 2^11, so the i32 accumulator is safe.
    for source_row in source.chunks_exact(source_row_length).take(source_height)
    {
        for horizontal in &x_weights {
            let [source_x_0, source_x_1, source_x_2, source_x_3] =
                horizontal.indices;
            let [horizontal_0, horizontal_1, horizontal_2, horizontal_3] =
                horizontal.coefficients;
            for channel in 0..CHANNELS {
                let indices = [
                    source_x_0 * CHANNELS + channel,
                    source_x_1 * CHANNELS + channel,
                    source_x_2 * CHANNELS + channel,
                    source_x_3 * CHANNELS + channel,
                ];
                let samples = indices.map(|index| {
                    source_row.get(index).copied().ok_or(
                        PreprocessError::PixelIndex {
                            index,
                            length: source_row.len(),
                        },
                    )
                });
                let [sample_0, sample_1, sample_2, sample_3] = samples;
                horizontal_values.push(
                    i32::from(sample_0?) * horizontal_0
                        + i32::from(sample_1?) * horizontal_1
                        + i32::from(sample_2?) * horizontal_2
                        + i32::from(sample_3?) * horizontal_3,
                );
            }
        }
    }
    if horizontal_values.len() != intermediate_length {
        return Err(PreprocessError::PixelIndex {
            index: horizontal_values.len(),
            length: intermediate_length,
        });
    }

    let mut output = Vec::with_capacity(output_length);

    // The vertical pass retains the combined 22-bit fixed-point scale and rounds only once,
    // preserving byte-for-byte parity with the previous OpenCV-compatible implementation.
    for vertical in &y_weights {
        let [vertical_0, vertical_1, vertical_2, vertical_3] =
            vertical.coefficients;
        let row_offsets = vertical.indices.map(|source_y| {
            source_y
                .checked_mul(intermediate_row_length)
                .ok_or(PreprocessError::ArithmeticOverflow)
        });
        let [row_0, row_1, row_2, row_3] = row_offsets;
        let row_0 = row_0?;
        let row_1 = row_1?;
        let row_2 = row_2?;
        let row_3 = row_3?;
        for destination_x in 0..destination_width {
            let pixel_offset = destination_x
                .checked_mul(CHANNELS)
                .ok_or(PreprocessError::ArithmeticOverflow)?;
            for channel in 0..CHANNELS {
                let offset = pixel_offset
                    .checked_add(channel)
                    .ok_or(PreprocessError::ArithmeticOverflow)?;
                let horizontal_0 = horizontal_values
                    .get(row_0 + offset)
                    .copied()
                    .ok_or(PreprocessError::PixelIndex {
                        index: row_0 + offset,
                        length: horizontal_values.len(),
                    })?;
                let horizontal_1 = horizontal_values
                    .get(row_1 + offset)
                    .copied()
                    .ok_or(PreprocessError::PixelIndex {
                        index: row_1 + offset,
                        length: horizontal_values.len(),
                    })?;
                let horizontal_2 = horizontal_values
                    .get(row_2 + offset)
                    .copied()
                    .ok_or(PreprocessError::PixelIndex {
                        index: row_2 + offset,
                        length: horizontal_values.len(),
                    })?;
                let horizontal_3 = horizontal_values
                    .get(row_3 + offset)
                    .copied()
                    .ok_or(PreprocessError::PixelIndex {
                        index: row_3 + offset,
                        length: horizontal_values.len(),
                    })?;
                let value = i64::from(horizontal_0) * i64::from(vertical_0)
                    + i64::from(horizontal_1) * i64::from(vertical_1)
                    + i64::from(horizontal_2) * i64::from(vertical_2)
                    + i64::from(horizontal_3) * i64::from(vertical_3);
                // OpenCV's integer saturation path rounds exact halves to an even output.
                let value =
                    round_shift_ties_even(value, OUTPUT_SHIFT).clamp(0, 255);
                output.push(u8::try_from(value).map_err(|source| {
                    PreprocessError::PixelConversion { value, source }
                })?);
            }
        }
    }
    Ok(output)
}

/// Divides by a power of two using OpenCV-compatible ties-to-even rounding.
fn round_shift_ties_even(value: i64, shift: u32) -> i64 {
    let divisor = 1_i64 << shift;
    let quotient = value.div_euclid(divisor);
    let remainder = value.rem_euclid(divisor);
    match (remainder * 2).cmp(&divisor) {
        std::cmp::Ordering::Less => quotient,
        std::cmp::Ordering::Greater => quotient + 1,
        std::cmp::Ordering::Equal if quotient % 2 != 0 => quotient + 1,
        std::cmp::Ordering::Equal => quotient,
    }
}

/// Precomputes OpenCV half-pixel source indices and 11-bit cubic coefficients.
fn axis_weights(
    source_length: usize,
    destination_length: usize,
) -> Vec<InterpolationWeights> {
    let scale = source_length as f64 / destination_length as f64;
    (0..destination_length)
        .map(|destination| {
            let coordinate = ((destination as f64 + 0.5) * scale - 0.5) as f32;
            let base = coordinate.floor() as isize;
            let fraction = coordinate - base as f32;
            InterpolationWeights {
                indices: [
                    clamp_index(base - 1, source_length),
                    clamp_index(base, source_length),
                    clamp_index(base + 1, source_length),
                    clamp_index(base + 2, source_length),
                ],
                coefficients: cubic_coefficients(fraction),
            }
        })
        .collect()
}

/// Clamps one signed cubic support coordinate to replicated image borders.
fn clamp_index(index: isize, length: usize) -> usize {
    if index <= 0 {
        return 0;
    }
    match usize::try_from(index) {
        Ok(index) => index.min(length.saturating_sub(1)),
        Err(_source) => length.saturating_sub(1),
    }
}

/// Reproduces OpenCV's `interpolateCubic` and fixed-point coefficient conversion.
fn cubic_coefficients(fraction: f32) -> [i32; 4] {
    let a = -0.75_f32;
    let plus_one = fraction + 1.0;
    let one_minus = 1.0 - fraction;
    let coefficient_0 =
        ((a * plus_one - 5.0 * a) * plus_one + 8.0 * a) * plus_one - 4.0 * a;
    let coefficient_1 =
        ((a + 2.0) * fraction - (a + 3.0)) * fraction * fraction + 1.0;
    let coefficient_2 =
        ((a + 2.0) * one_minus - (a + 3.0)) * one_minus * one_minus + 1.0;
    let coefficient_3 = 1.0 - coefficient_0 - coefficient_1 - coefficient_2;
    [
        (coefficient_0 * COEFFICIENT_SCALE).round_ties_even() as i32,
        (coefficient_1 * COEFFICIENT_SCALE).round_ties_even() as i32,
        (coefficient_2 * COEFFICIENT_SCALE).round_ties_even() as i32,
        (coefficient_3 * COEFFICIENT_SCALE).round_ties_even() as i32,
    ]
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;

    use serde::Deserialize;
    use sha2::{Digest, Sha256};
    use typed_builder::TypedBuilder;

    use crate::{
        AffineTransform, PageImage, PageImageInput, PageRotation,
        PageTransform, PageTransformInput, PixelFormat,
    };

    use super::{OUTPUT_SHIFT, preprocess, round_shift_ties_even};

    #[derive(Debug, Deserialize, TypedBuilder)]
    struct TensorContract {
        dtype: String,
        shape: Vec<usize>,
        min: f32,
        max: f32,
        sha256: String,
    }

    #[derive(Debug, Deserialize)]
    struct Oracle {
        tensor: TensorContract,
        image_size: [f32; 2],
        scale_factor: [f32; 2],
    }

    #[derive(Debug, Deserialize, TypedBuilder)]
    struct OracleInput {
        basename: String,
        width: u32,
        height: u32,
        color_mode: String,
        sha256: String,
        decoded_rgb_sha256: String,
    }

    #[derive(Debug, Deserialize, TypedBuilder)]
    struct OracleSample {
        input: OracleInput,
        tensor: TensorContract,
        image_size: [f32; 2],
        scale_factor: [f32; 2],
    }

    #[derive(Debug, Deserialize)]
    struct OracleCollection {
        schema_version: u32,
        samples: Vec<OracleSample>,
    }

    /// Resolves a fixture path from the layout crate root.
    fn fixture_path(name: &str) -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/model")
            .join(name)
    }

    /// Computes the mandated SHA-256 over C-contiguous little-endian f32 bytes.
    fn tensor_sha256(values: impl Iterator<Item = f32>) -> String {
        let mut digest = Sha256::new();
        for value in values {
            digest.update(value.to_le_bytes());
        }
        lowercase_hex(digest.finalize().as_ref())
    }

    /// Computes lowercase SHA-256 for one complete fixture byte slice.
    fn bytes_sha256(values: &[u8]) -> String {
        lowercase_hex(Sha256::digest(values).as_ref())
    }

    /// Encodes fixture digest bytes for compatibility with `sha2` 0.11.
    fn lowercase_hex(bytes: &[u8]) -> String {
        let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
        for byte in bytes {
            for nibble in [byte >> 4, byte & 0x0f] {
                let digit = match nibble {
                    0..=9 => b'0' + nibble,
                    _ => b'a' + (nibble - 10),
                };
                encoded.push(char::from(digit));
            }
        }
        encoded
    }

    /// Asserts two preprocessing scalars are nearly equal.
    fn assert_close(actual: f32, expected: f32) {
        assert!(
            (actual - expected).abs() <= f32::EPSILON,
            "expected {expected}, got {actual}"
        );
    }

    /// Verifies Rust preprocessing is byte-identical to the fixed PaddleX oracle.
    #[test]
    fn tensor_matches_python_oracle() {
        let image = image::open(fixture_path("input.png"))
            .expect("the PNG fixture must decode")
            .into_rgb8();
        let page_image = PageImage::try_from(
            PageImageInput::builder()
                .width(image.width())
                .height(image.height())
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::<[u8]>::from(image.into_raw()))
                .build(),
        )
        .expect("the decoded RGB image must be valid");
        let transform = PageTransform::try_from(
            PageTransformInput::builder()
                .page_to_viewport(AffineTransform::identity())
                .viewport_width(1024.0)
                .viewport_height(640.0)
                .render_width(1024)
                .render_height(640)
                .model_width(800)
                .model_height(800)
                .rotation(PageRotation::Degrees0)
                .build(),
        )
        .expect("the fixture transform must be valid");
        let oracle: Oracle = serde_json::from_slice(
            &fs::read(fixture_path("python_output.json"))
                .expect("the Python oracle must be readable"),
        )
        .expect("the Python oracle must deserialize");

        let inputs = preprocess(&page_image, &transform)
            .expect("the fixed RGB fixture must preprocess");

        assert_eq!(oracle.tensor.dtype, "float32");
        assert_eq!(inputs.image.shape(), oracle.tensor.shape);
        assert_close(
            inputs.image.iter().copied().fold(f32::INFINITY, f32::min),
            oracle.tensor.min,
        );
        assert_close(
            inputs
                .image
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max),
            oracle.tensor.max,
        );
        assert_eq!(
            tensor_sha256(inputs.image.iter().copied()),
            oracle.tensor.sha256
        );
        assert_eq!(
            inputs.image_size.as_slice(),
            Some(oracle.image_size.as_slice())
        );
        assert_eq!(
            inputs.scale_factor.as_slice(),
            Some(oracle.scale_factor.as_slice())
        );
    }

    /// Verifies every synthetic shape matches official PaddleX preprocessing bytes.
    #[test]
    fn multi_sample_tensors_match_python_oracle() {
        let oracle: OracleCollection = serde_json::from_slice(
            &fs::read(fixture_path("python_outputs.json"))
                .expect("the Python collection must be readable"),
        )
        .expect("the Python collection must deserialize");
        assert_eq!(oracle.schema_version, 1);
        assert_eq!(oracle.samples.len(), 5);
        for sample in oracle.samples {
            let bytes = fs::read(fixture_path(&sample.input.basename))
                .expect("fixture bytes must be readable");
            assert_eq!(bytes_sha256(&bytes), sample.input.sha256);
            let image = image::load_from_memory(&bytes)
                .expect("fixture PNG must decode")
                .into_rgb8();
            assert_eq!(image.width(), sample.input.width);
            assert_eq!(image.height(), sample.input.height);
            assert_eq!(sample.input.color_mode, "RGB");
            assert_eq!(
                bytes_sha256(image.as_raw()),
                sample.input.decoded_rgb_sha256,
                "decoded RGB mismatch for {}",
                sample.input.basename
            );
            let width = image.width();
            let height = image.height();
            let page_image = PageImage::try_from(
                PageImageInput::builder()
                    .width(width)
                    .height(height)
                    .pixel_format(PixelFormat::Rgb8)
                    .data(Arc::<[u8]>::from(image.into_raw()))
                    .build(),
            )
            .expect("fixture RGB image must be valid");
            let transform = PageTransform::try_from(
                PageTransformInput::builder()
                    .page_to_viewport(AffineTransform::identity())
                    .viewport_width(f64::from(width))
                    .viewport_height(f64::from(height))
                    .render_width(width)
                    .render_height(height)
                    .model_width(800)
                    .model_height(800)
                    .rotation(PageRotation::Degrees0)
                    .build(),
            )
            .expect("fixture transform must be valid");

            let inputs = preprocess(&page_image, &transform)
                .expect("fixture must preprocess");

            assert_eq!(sample.tensor.dtype, "float32");
            assert_eq!(inputs.image.shape(), sample.tensor.shape);
            assert_close(
                inputs.image.iter().copied().fold(f32::INFINITY, f32::min),
                sample.tensor.min,
            );
            assert_close(
                inputs
                    .image
                    .iter()
                    .copied()
                    .fold(f32::NEG_INFINITY, f32::max),
                sample.tensor.max,
            );
            assert_eq!(
                tensor_sha256(inputs.image.iter().copied()),
                sample.tensor.sha256,
                "tensor mismatch for {}",
                sample.input.basename
            );
            assert_eq!(
                inputs.image_size.as_slice(),
                Some(sample.image_size.as_slice())
            );
            assert_eq!(
                inputs.scale_factor.as_slice(),
                Some(sample.scale_factor.as_slice())
            );
        }
    }

    /// Verifies exact positive and negative halves select the even integer.
    #[test]
    fn fixed_point_output_rounds_ties_to_even() {
        let unit = 1_i64 << OUTPUT_SHIFT;

        assert_eq!(round_shift_ties_even(unit / 2, OUTPUT_SHIFT), 0);
        assert_eq!(round_shift_ties_even(unit + unit / 2, OUTPUT_SHIFT), 2);
        assert_eq!(round_shift_ties_even(-(unit / 2), OUTPUT_SHIFT), 0);
        assert_eq!(round_shift_ties_even(-(unit + unit / 2), OUTPUT_SHIFT), -2);
    }
}
