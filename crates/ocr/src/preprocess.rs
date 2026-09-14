//! BGR tensors and perspective crops shared by native and browser OCR.
use crate::OcrError;
use docparse_layout::{PageImage, Point, Quad};
use image::{
    ImageBuffer, Rgb, RgbImage,
    imageops::{self, FilterType},
};
use ndarray::Array4;

const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const STD: [f32; 3] = [0.229, 0.224, 0.225];

/// Owned model input remains live until the runtime has finished reading it.
pub(crate) struct ImageTensor(pub Array4<f32>);

impl ImageTensor {
    /// Resizes to bounded multiples of 32 and applies detection's ImageNet normalization in BGR order.
    #[expect(
        clippy::cast_sign_loss,
        reason = "validated image dimensions are rounded to positive multiples of 32"
    )]
    pub fn detection(
        image: &PageImage,
        max_side: u32,
    ) -> Result<Self, OcrError> {
        if image.width() == 0
            || image.height() == 0
            || !(32..=4096).contains(&max_side)
        {
            return Err(OcrError::InvalidData(
                "invalid detector image dimensions".into(),
            ));
        }
        let scale = (f64::from(max_side)
            / f64::from(image.width().max(image.height())))
        .min(1.0);
        let aligned = |size: u32| {
            ((f64::from(size) * scale / 32.0).round_ties_even().max(1.0) * 32.0)
                as u32
        };
        let (width, height) = (aligned(image.width()), aligned(image.height()));
        let view = ImageBuffer::<Rgb<u8>, _>::from_raw(
            image.width(),
            image.height(),
            image.data().as_ref(),
        )
        .ok_or_else(|| OcrError::InvalidData("invalid RGB buffer".into()))?;
        let resized =
            imageops::resize(&view, width, height, FilterType::Triangle);
        Self::normalized(&[resized], width, MEAN, STD, false)
    }

    /// Keeps the single-line resize contract while assembling one contiguous batch tensor.
    pub fn recognition(
        images: &[&RgbImage],
        max_width: u32,
    ) -> Result<Self, OcrError> {
        let mut resized = Vec::with_capacity(images.len());
        let mut tensor_width = 320;
        for image in images {
            let width = Self::recognition_width(
                image.width(),
                image.height(),
                max_width,
            )?;
            tensor_width = tensor_width.max(width);
            resized.push(imageops::resize(
                *image,
                width,
                48,
                FilterType::Triangle,
            ));
        }
        Self::normalized(&resized, tensor_width, [0.5; 3], [0.5; 3], false)
    }

    /// Shares exact integer width calculation between crop scheduling and tensor construction.
    #[expect(
        clippy::cast_sign_loss,
        reason = "width is clamped to validated positive bounds"
    )]
    fn recognition_width(
        width: u32,
        height: u32,
        max_width: u32,
    ) -> Result<u32, OcrError> {
        if width == 0 || height == 0 || !(320..=4096).contains(&max_width) {
            return Err(OcrError::InvalidData(
                "invalid recognizer image dimensions".into(),
            ));
        }
        Ok((48.0 * f64::from(width) / f64::from(height))
            .ceil()
            .clamp(1.0, f64::from(max_width)) as u32)
    }

    /// Batches the orientation classifier's fixed 160x80 RGB inputs.
    pub fn orientation(images: &[&RgbImage]) -> Result<Self, OcrError> {
        let mut resized = Vec::with_capacity(images.len());
        for image in images {
            if image.width() == 0 || image.height() == 0 {
                return Err(OcrError::InvalidData(
                    "empty orientation crop".into(),
                ));
            }
            resized.push(imageops::resize(
                *image,
                160,
                80,
                FilterType::Triangle,
            ));
        }
        Self::normalized(&resized, 160, MEAN, STD, true)
    }

    /// Writes every image directly into its final NCHW slice without intermediate float tensors.
    fn normalized(
        images: &[RgbImage],
        width: u32,
        means: [f32; 3],
        deviations: [f32; 3],
        rgb: bool,
    ) -> Result<Self, OcrError> {
        let height = images
            .first()
            .ok_or_else(|| OcrError::InvalidData("empty tensor batch".into()))?
            .height();
        if images.len() > 32
            || width == 0
            || height == 0
            || images
                .iter()
                .any(|image| image.height() != height || image.width() > width)
        {
            return Err(OcrError::InvalidData(
                "invalid tensor batch dimensions".into(),
            ));
        }
        let plane = width as usize * height as usize;
        let mut data = vec![0.0; images.len() * 3 * plane];
        let channels = if rgb { [0, 1, 2] } else { [2, 1, 0] };
        for (tensor, image) in data.chunks_exact_mut(3 * plane).zip(images) {
            for (output, ((channel, mean), deviation)) in tensor
                .chunks_exact_mut(plane)
                .zip(channels.into_iter().zip(means).zip(deviations))
            {
                for (row, source) in
                    output.chunks_exact_mut(width as usize).zip(
                        image.as_raw().chunks_exact(image.width() as usize * 3),
                    )
                {
                    for (pixel, source) in
                        row.iter_mut().zip(source.as_chunks::<3>().0)
                    {
                        *pixel = (f32::from(*source.get(channel).ok_or_else(
                            || {
                                OcrError::InvalidData(
                                    "truncated RGB pixel".into(),
                                )
                            },
                        )?) / 255.0
                            - mean)
                            / deviation;
                    }
                }
            }
        }
        Array4::from_shape_vec(
            (images.len(), 3, height as usize, width as usize),
            data,
        )
        .map(Self)
        .map_err(|error| OcrError::InvalidData(error.to_string()))
    }
}

/// A rectified crop and its corresponding reading-order corners in the original image.
pub(crate) struct TextCrop {
    pub image: RgbImage,
    pub quad: Quad,
}

impl TextCrop {
    /// Validates rectified crop sizes before scheduling or allocating their pixel buffers.
    #[expect(clippy::cast_sign_loss, reason = "quad distances are nonnegative")]
    fn dimensions(quad: &Quad) -> Result<(u32, u32), OcrError> {
        let [a, b, c, d] = *quad.points();
        let distance = |left: Point, right: Point| {
            (left.x - right.x).hypot(left.y - right.y)
        };
        let width = distance(a, b).max(distance(c, d)).round() as u32;
        let height = distance(a, d).max(distance(b, c)).round() as u32;
        if width == 0
            || height == 0
            || width > 8192
            || height > 8192
            || u64::from(width) * u64::from(height) > 16_777_216
        {
            return Err(OcrError::InvalidData(
                "empty or excessive text crop".into(),
            ));
        }
        Ok((width, height))
    }

    /// Groups equal-width tensors without creating every crop or adding padding that changes recognition.
    pub fn tensor_width(quad: &Quad, max_width: u32) -> Result<u32, OcrError> {
        let (mut width, mut height) = Self::dimensions(quad)?;
        if f64::from(height) / f64::from(width) >= 1.5 {
            std::mem::swap(&mut width, &mut height);
        }
        Ok(ImageTensor::recognition_width(width, height, max_width)?.max(320))
    }

    /// Corrects upside-down text while keeping the reading axis attached to the source quadrilateral.
    pub fn rotate_half_turn(&mut self) -> Result<(), OcrError> {
        let [a, b, c, d] = *self.quad.points();
        self.quad = Quad::try_from([c, d, a, b])?;
        self.image = imageops::rotate180(&self.image);
        Ok(())
    }
}

impl TryFrom<(&PageImage, &Quad)> for TextCrop {
    type Error = OcrError;

    /// Samples the inverse homography with cubic interpolation and rotates tall text into its reading frame.
    #[expect(
        clippy::cast_sign_loss,
        reason = "distances are nonnegative and sample indices and channels are clamped to image bounds"
    )]
    fn try_from(
        (source, quad): (&PageImage, &Quad),
    ) -> Result<Self, Self::Error> {
        let [a, b, c, d] = *quad.points();
        // Scheduling and rasterization share the same dimensions, including tall-line rotation.
        let (width, height) = Self::dimensions(quad)?;
        if source.width() == 0 || source.height() == 0 {
            return Err(OcrError::InvalidData(
                "empty or excessive text crop".into(),
            ));
        }
        let points = [a, b, c, d];
        let left = points
            .iter()
            .map(|p| p.x.floor())
            .fold(f64::INFINITY, f64::min)
            .max(0.0);
        let top = points
            .iter()
            .map(|p| p.y.floor())
            .fold(f64::INFINITY, f64::min)
            .max(0.0);
        let right = points
            .iter()
            .map(|p| p.x.floor())
            .fold(0.0, f64::max)
            .min(f64::from(source.width()));
        let bottom = points
            .iter()
            .map(|p| p.y.floor())
            .fold(0.0, f64::max)
            .min(f64::from(source.height()));
        if left >= right || top >= bottom {
            return Err(OcrError::InvalidData(
                "text crop lies outside image".into(),
            ));
        }
        let view = ImageBuffer::<Rgb<u8>, _>::from_raw(
            source.width(),
            source.height(),
            source.data().as_ref(),
        )
        .ok_or_else(|| OcrError::InvalidData("invalid RGB buffer".into()))?;
        // Integer rectangles need no interpolation and are common in printed documents.
        let mut image = if (a.x - d.x).abs() < 1e-8
            && (b.x - c.x).abs() < 1e-8
            && (a.y - b.y).abs() < 1e-8
            && (c.y - d.y).abs() < 1e-8
            && points
                .iter()
                .all(|p| p.x.fract().abs() < 1e-8 && p.y.fract().abs() < 1e-8)
            && (right - left - f64::from(width)).abs() < 1e-8
            && (bottom - top - f64::from(height)).abs() < 1e-8
        {
            // Copy only the crop rows; image's SubImage::to_image requires an unnecessarily static container.
            let mut pixels =
                Vec::with_capacity(width as usize * height as usize * 3);
            for row in source
                .data()
                .chunks_exact(source.width() as usize * 3)
                .skip(top as usize)
                .take(height as usize)
            {
                pixels.extend_from_slice(
                    row.get(
                        left as usize * 3..(left as usize + width as usize) * 3,
                    )
                    .ok_or_else(|| {
                        OcrError::InvalidData("crop row outside image".into())
                    })?,
                );
            }
            RgbImage::from_raw(width, height, pixels).ok_or_else(|| {
                OcrError::InvalidData("truncated crop image".into())
            })?
        } else {
            let transform = Perspective::try_from(quad)?;
            let mut output = RgbImage::new(width, height);
            for (x, y, pixel) in output.enumerate_pixels_mut() {
                let point = transform.project(
                    f64::from(x) / f64::from(width),
                    f64::from(y) / f64::from(height),
                );
                let ix = point.x.floor();
                let iy = point.y.floor();
                let mut channels = [0.0; 3];
                for dy in -1..=2 {
                    for dx in -1..=2 {
                        let sx = ix + f64::from(dx);
                        let sy = iy + f64::from(dy);
                        let weight = cubic_weight(point.x - sx)
                            * cubic_weight(point.y - sy);
                        let value = view.get_pixel(
                            sx.clamp(left, right - 1.0) as u32,
                            sy.clamp(top, bottom - 1.0) as u32,
                        );
                        for (channel, value) in channels.iter_mut().zip(value.0)
                        {
                            *channel += weight * f64::from(value);
                        }
                    }
                }
                *pixel =
                    Rgb(channels
                        .map(|value| value.round().clamp(0.0, 255.0) as u8));
            }
            output
        };
        let mut corners = *quad.points();
        if f64::from(height) / f64::from(width) >= 1.5 {
            image = imageops::rotate270(&image);
            corners = [b, c, d, a];
        }
        Ok(Self {
            image,
            quad: Quad::try_from(corners)?,
        })
    }
}

/// Inverse homography from the unit crop rectangle to the source image.
struct Perspective([f64; 8]);

impl TryFrom<&Quad> for Perspective {
    type Error = OcrError;

    /// Solves a convex quadrilateral analytically, including its affine special case.
    fn try_from(quad: &Quad) -> Result<Self, Self::Error> {
        let [a, b, c, d] = *quad.points();
        let dx1 = b.x - c.x;
        let dx2 = d.x - c.x;
        let dx3 = a.x - b.x + c.x - d.x;
        let dy1 = b.y - c.y;
        let dy2 = d.y - c.y;
        let dy3 = a.y - b.y + c.y - d.y;
        let (g, h) = if dx3.abs() < 1e-10 && dy3.abs() < 1e-10 {
            (0.0, 0.0)
        } else {
            let determinant = dx1 * dy2 - dx2 * dy1;
            if determinant.abs() < 1e-10 {
                return Err(OcrError::InvalidData(
                    "degenerate perspective transform".into(),
                ));
            }
            (
                (dx3 * dy2 - dx2 * dy3) / determinant,
                (dx1 * dy3 - dx3 * dy1) / determinant,
            )
        };
        Ok(Self([
            b.x - a.x + g * b.x,
            d.x - a.x + h * d.x,
            a.x,
            b.y - a.y + g * b.y,
            d.y - a.y + h * d.y,
            a.y,
            g,
            h,
        ]))
    }
}

impl Perspective {
    /// Maps one normalized crop coordinate through the validated projective transform.
    fn project(&self, x: f64, y: f64) -> Point {
        let [a, b, c, d, e, f, g, h] = self.0;
        let scale = g * x + h * y + 1.0;
        Point::new((a * x + b * y + c) / scale, (d * x + e * y + f) / scale)
    }
}

/// OpenCV's cubic kernel retains thin strokes during perspective resampling.
fn cubic_weight(distance: f64) -> f64 {
    let x = distance.abs();
    if x <= 1.0 {
        (1.25 * x - 2.25) * x * x + 1.0
    } else if x < 2.0 {
        ((-0.75 * x + 3.75) * x - 6.0) * x + 3.0
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_layout::{PageImageInput, PixelFormat, Point};
    use std::sync::Arc;

    /// Mixed colors and crop sizes must retain each single-line tensor exactly inside a batch.
    #[test]
    fn batch_members_preserve_single_line_pixels() {
        let first = RgbImage::from_pixel(100, 20, image::Rgb([255, 17, 0]));
        let second = RgbImage::from_pixel(80, 30, image::Rgb([0, 51, 255]));
        let batch =
            ImageTensor::recognition(&[&first, &second], 3200).expect("batch");
        assert_eq!(batch.0.shape(), &[2, 3, 48, 320]);
        let orientation =
            ImageTensor::orientation(&[&first, &second]).expect("orientation");
        for (index, image) in [&first, &second].into_iter().enumerate() {
            let single =
                ImageTensor::recognition(&[image], 3200).expect("single");
            assert_eq!(
                batch.0.index_axis(ndarray::Axis(0), index),
                single.0.index_axis(ndarray::Axis(0), 0)
            );
            let single =
                ImageTensor::orientation(&[image]).expect("single orientation");
            assert_eq!(
                orientation.0.index_axis(ndarray::Axis(0), index),
                single.0.index_axis(ndarray::Axis(0), 0)
            );
        }
        assert!(matches!(
            ImageTensor::recognition(&[], 3200),
            Err(OcrError::InvalidData(_))
        ));
        assert!(matches!(
            ImageTensor::orientation(&[]),
            Err(OcrError::InvalidData(_))
        ));
    }

    /// Metadata grouping must match actual tensor widths after rounding, clipping and vertical rotation.
    #[test]
    fn planned_width_matches_rectified_tensor() {
        let source = PageImage::try_from(
            PageImageInput::builder()
                .width(700)
                .height(700)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(vec![255; 700 * 700 * 3]))
                .build(),
        )
        .expect("page");
        for (width, height) in [(601.3, 20.4), (20.4, 601.3), (30.2, 40.1)] {
            let quad = Quad::try_from([
                Point::new(1.0, 1.0),
                Point::new(width, 1.0),
                Point::new(width, height),
                Point::new(1.0, height),
            ])
            .expect("quad");
            let crop = TextCrop::try_from((&source, &quad)).expect("crop");
            for max_width in [320, 3200] {
                let tensor =
                    ImageTensor::recognition(&[&crop.image], max_width)
                        .expect("tensor");
                assert_eq!(
                    TextCrop::tensor_width(&quad, max_width).expect("planned")
                        as usize,
                    tensor.0.dim().3
                );
            }
        }
    }

    /// Red input verifies BGR channel order, normalization and zero padding independently of inference.
    #[test]
    fn tensor_contract_preserves_bgr_and_normalized_padding() {
        let image = RgbImage::from_pixel(16, 16, image::Rgb([255, 0, 0]));
        let page = PageImage::try_from(
            PageImageInput::builder()
                .width(16)
                .height(16)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(image.as_raw().clone()))
                .build(),
        )
        .expect("page");
        let detection =
            ImageTensor::detection(&page, 960).expect("detector input");
        assert_eq!(detection.0.shape(), &[1, 3, 32, 32]);
        assert!(
            (detection.0.get((0, 0, 0, 0)).expect("B") + 0.485 / 0.229).abs()
                < 1e-5
        );
        assert!(
            (detection.0.get((0, 2, 0, 0)).expect("R") - (1.0 - 0.406) / 0.225)
                .abs()
                < 1e-5
        );
        let recognition = ImageTensor::recognition(&[&image], 3200)
            .expect("recognizer input");
        assert_eq!(recognition.0.shape(), &[1, 3, 48, 320]);
        assert!(
            (recognition.0.get((0, 0, 0, 0)).expect("B") + 1.0).abs() < 1e-6
        );
        assert!(
            recognition.0.get((0, 2, 0, 48)).expect("padding").abs() < 1e-6
        );
    }

    /// The crop's corners preserve the sampled source frame when a vertical line is rotated upright.
    #[test]
    fn perspective_crop_retains_reading_quad() {
        let pixels = vec![255; 20 * 40 * 3];
        let page = PageImage::try_from(
            PageImageInput::builder()
                .width(20)
                .height(40)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::from(pixels))
                .build(),
        )
        .expect("page");
        let quad = Quad::try_from([
            Point::new(2.0, 2.0),
            Point::new(12.0, 2.0),
            Point::new(12.0, 32.0),
            Point::new(2.0, 32.0),
        ])
        .expect("quad");
        let crop = TextCrop::try_from((&page, &quad)).expect("crop");
        assert_eq!(crop.image.dimensions(), (30, 10));
        assert_eq!(crop.quad.points().first(), Some(&Point::new(12.0, 2.0)));
    }
}
