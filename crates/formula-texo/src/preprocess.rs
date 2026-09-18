//! UniMERNet/Texo image preprocessing: white-margin crop, aspect-preserving
//! resize into a 384x384 black-padded canvas, greyscale, normalise.

// Adapted from best-ocr-rust (AGPL-3.0-only); see NOTICE.md.
#![allow(
    clippy::indexing_slicing,
    clippy::float_cmp,
    clippy::needless_range_loop
)]

use crate::resample::{Plane, resize_short_side, thumbnail};
use docparse_formula::FormulaError;
use docparse_layout::PageImage;
use ndarray::Array4;
use std::sync::Arc;

pub const IMAGE_SIZE: usize = 384;

/// Exact float32 constants used by the reference `albumentations.Normalize`
/// (mean 0.7931, std 0.1738, max_pixel_value 255). These are the shortest
/// decimals that round-trip to the reference bit patterns; `constants_are_exact`
/// below pins them so a stray edit cannot silently shift every pixel.
const MEAN: f32 = 202.2405; // 0x434a3d91
const INV_STD: f32 = 0.022563687; // 0x3cb8d77b, i.e. 1 / 44.319

/// PIL `convert("L")`: ITU-R 601-2 luma in 16-bit fixed point.
#[inline]
fn pil_luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 19595 + g as u32 * 38470 + b as u32 * 7471 + 0x8000) >> 16)
        as u8
}

/// OpenCV `COLOR_RGB2GRAY`, which is what `albumentations.ToGray` calls.
#[inline]
fn cv_luma(r: u8, g: u8, b: u8) -> u8 {
    ((r as u32 * 4899 + g as u32 * 9617 + b as u32 * 1868 + 8192) >> 14) as u8
}

/// Trim near-white margins. Mirrors `crop_margin` in the reference processor:
/// contrast-stretch the luma plane, threshold at 200, take the bounding box.
fn crop_margin(p: &Plane) -> Plane {
    let n = p.w * p.h;
    let mut luma = vec![0u8; n];
    for i in 0..n {
        luma[i] = pil_luma(p.data[i * 3], p.data[i * 3 + 1], p.data[i * 3 + 2]);
    }
    let min_val = *luma.iter().min().unwrap_or(&0) as f64;
    let max_val = *luma.iter().max().unwrap_or(&0) as f64;
    if max_val == min_val {
        return p.clone();
    }
    let span = max_val - min_val;

    let (mut x0, mut y0, mut x1, mut y1) =
        (usize::MAX, usize::MAX, 0usize, 0usize);
    for y in 0..p.h {
        for x in 0..p.w {
            let v = (luma[y * p.w + x] as f64 - min_val) / span * 255.0;
            if v < 200.0 {
                if x < x0 {
                    x0 = x;
                }
                if y < y0 {
                    y0 = y;
                }
                if x > x1 {
                    x1 = x;
                }
                if y > y1 {
                    y1 = y;
                }
            }
        }
    }
    if x0 == usize::MAX {
        return p.clone(); // nothing below threshold: leave the image alone
    }

    let (cw, chh) = (x1 - x0 + 1, y1 - y0 + 1);
    let mut out = Plane::new(cw, chh, 3);
    for y in 0..chh {
        let s = ((y0 + y) * p.w + x0) * 3;
        let d = y * cw * 3;
        out.data[d..d + cw * 3].copy_from_slice(&p.data[s..s + cw * 3]);
    }
    out
}

/// Centre the image on a `size x size` black canvas (`ImageOps.expand`).
fn pad_center(p: &Plane, size: usize) -> Plane {
    let mut out = Plane::new(size, size, 3);
    let pw = (size - p.w) / 2;
    let ph = (size - p.h) / 2;
    for y in 0..p.h {
        let s = y * p.w * 3;
        let d = ((ph + y) * size + pw) * 3;
        out.data[d..d + p.w * 3].copy_from_slice(&p.data[s..s + p.w * 3]);
    }
    out
}

/// Full pipeline: RGB crop -> CHW float tensor of shape (3, 384, 384).
pub(crate) fn preprocess(img: &PageImage) -> Result<Vec<f32>, FormulaError> {
    let w = img.width() as usize;
    let h = img.height() as usize;
    if w == 0
        || h == 0
        || w.checked_mul(h)
            .is_none_or(|n| n > 16_777_216 || img.data().len() != n * 3)
    {
        return Err(FormulaError::Invalid(
            "Texo crop must contain 1..16777216 pixels with an exact RGB buffer".into(),
        ));
    }
    let rgb = Plane::builder()
        .w(w)
        .h(h)
        .c(3)
        .data(img.data().to_vec())
        .build();
    let cropped = crop_margin(&rgb);
    // Bound the intermediate short-side enlargement before allocating its canvas.
    let long = cropped
        .w
        .max(cropped.h)
        .checked_mul(IMAGE_SIZE)
        .and_then(|n| n.checked_div(cropped.w.min(cropped.h)));
    if long.is_none_or(|n| n > 16_777_216 / IMAGE_SIZE) {
        return Err(FormulaError::Invalid(
            "Texo crop aspect ratio exceeds the preprocessing allocation limit"
                .into(),
        ));
    }
    let scaled = resize_short_side(&cropped, IMAGE_SIZE);
    let thumbed = thumbnail(&scaled, IMAGE_SIZE);
    let canvas = pad_center(&thumbed, IMAGE_SIZE);

    // ToGray -> 3 identical channels -> Normalize -> CHW.
    // All three channels are equal, so compute the plane once and share it.
    let n = IMAGE_SIZE * IMAGE_SIZE;
    let mut chan = vec![0f32; n];
    for i in 0..n {
        let g = cv_luma(
            canvas.data[i * 3],
            canvas.data[i * 3 + 1],
            canvas.data[i * 3 + 2],
        );
        chan[i] = (g as f32 - MEAN) * INV_STD;
    }
    let mut out = Vec::with_capacity(n * 3);
    for _ in 0..3 {
        out.extend_from_slice(&chan);
    }
    Ok(out)
}

/// Model-ready NCHW tensors preserve crop order and partial batches.
pub(crate) struct FormulaInput(pub(crate) Array4<f32>);

impl TryFrom<Vec<Arc<PageImage>>> for FormulaInput {
    type Error = FormulaError;
    /// Runs the reference transform independently and assembles one real image batch.
    fn try_from(images: Vec<Arc<PageImage>>) -> Result<Self, Self::Error> {
        let batch = images.len();
        if !(1..=32).contains(&batch) {
            return Err(FormulaError::Invalid(
                "Texo batch size must be 1..32".into(),
            ));
        }
        let mut values =
            Vec::with_capacity(batch * 3 * IMAGE_SIZE * IMAGE_SIZE);
        for image in images {
            values.extend(preprocess(&image)?);
        }
        Array4::from_shape_vec((batch, 3, IMAGE_SIZE, IMAGE_SIZE), values)
            .map(Self)
            .map_err(|error| FormulaError::Invalid(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pillow-compatible preprocessing must reproduce independent Python tensors exactly.
    #[test]
    fn matches_python_preprocessing() {
        use sha2::{Digest, Sha256};
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/reference.json"
        ))
        .expect("reference");
        for case in reference["cases"].as_array().expect("cases") {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures")
                .join(case["image"].as_str().expect("image"));
            let rgb = image::open(path).expect("fixture").to_rgb8();
            let page = PageImage::try_from(
                docparse_layout::PageImageInput::builder()
                    .width(rgb.width())
                    .height(rgb.height())
                    .pixel_format(docparse_layout::PixelFormat::Rgb8)
                    .data(Arc::from(rgb.into_raw()))
                    .build(),
            )
            .expect("image");
            let pixels = preprocess(&page).expect("preprocessing");
            let mut hasher = Sha256::new();
            for pixel in pixels {
                hasher.update(pixel.to_le_bytes());
            }
            let hash: String = hasher
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert_eq!(hash, case["preprocess_sha256"].as_str().expect("hash"));
        }
    }

    /// Core's shape type permits empty images, which Texo must reject before resizing.
    #[test]
    fn rejects_empty_and_extreme_crops() {
        for (width, height) in [(0, 0), (50_000, 1)] {
            let image = PageImage::try_from(
                docparse_layout::PageImageInput::builder()
                    .width(width)
                    .height(height)
                    .pixel_format(docparse_layout::PixelFormat::Rgb8)
                    .data(Arc::from(vec![
                        255;
                        width as usize * height as usize * 3
                    ]))
                    .build(),
            )
            .expect("shape");
            preprocess(&image).expect_err("invalid crop");
        }
        // PageImage's public builder can bypass TryFrom; validate again at the model boundary.
        let image = PageImage::builder()
            .width(2)
            .height(2)
            .pixel_format(docparse_layout::PixelFormat::Rgb8)
            .data(Arc::from([]))
            .build();
        preprocess(&image).expect_err("truncated RGB buffer");
    }

    /// Normalization constants must retain the upstream f32 rounding.
    #[test]
    fn constants_are_exact() {
        assert_eq!(MEAN.to_bits(), 0x434a_3d91);
        assert_eq!(INV_STD.to_bits(), 0x3cb8_d77b);
    }

    /// Cropping and normalization use distinct upstream luma kernels.
    #[test]
    fn luma_kernels_match_their_sources() {
        // PIL convert("L") on pure primaries
        assert_eq!(pil_luma(255, 0, 0), 76);
        assert_eq!(pil_luma(0, 255, 0), 150);
        assert_eq!(pil_luma(0, 0, 255), 29);
        assert_eq!(pil_luma(255, 255, 255), 255);
        // OpenCV COLOR_RGB2GRAY on the same
        assert_eq!(cv_luma(255, 0, 0), 76);
        assert_eq!(cv_luma(0, 255, 0), 150);
        assert_eq!(cv_luma(0, 0, 255), 29);
        assert_eq!(cv_luma(255, 255, 255), 255);
        // The two kernels agree on most inputs but not all, so each has to be
        // used where the reference pipeline uses it: PIL's for crop_margin,
        // OpenCV's for ToGray.
        assert_eq!((pil_luma(1, 8, 136), cv_luma(1, 8, 136)), (20, 21));
        assert_eq!((pil_luma(2, 152, 120), cv_luma(2, 152, 120)), (104, 103));
    }
}
