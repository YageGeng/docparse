//! OpenCV-compatible image preprocessing used by PP-DocLayoutV3.
//!
//! The resize loop indexes fixed-size coefficient taps and validated image
//! buffers; keeping direct indexing here keeps the hot preprocessing path small.
#![allow(clippy::cast_sign_loss, clippy::indexing_slicing)]

use image::{DynamicImage, RgbImage};

const COEF_BITS: u32 = 11;
const COEF_SCALE: i64 = 1 << COEF_BITS;

/// Per-output-pixel bicubic taps: four source indices plus four fixed-point weights.
fn cubic_coeffs(dst: usize, src: usize) -> Vec<([usize; 4], [i64; 4])> {
    const A: f64 = -0.75;
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|d| {
            let mapped = (d as f64 + 0.5) * scale - 0.5;
            let s = mapped.floor() as isize;
            let x = mapped - s as f64;
            let c0 = ((A * (x + 1.0) - 5.0 * A) * (x + 1.0) + 8.0 * A)
                * (x + 1.0)
                - 4.0 * A;
            let c1 = ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0;
            let c2 =
                ((A + 2.0) * (1.0 - x) - (A + 3.0)) * (1.0 - x) * (1.0 - x)
                    + 1.0;
            let c3 = 1.0 - c0 - c1 - c2;
            let weights = [c0, c1, c2, c3]
                .map(|c| (c * COEF_SCALE as f64).round_ties_even() as i64);
            let last = src as isize - 1;
            let idx =
                [s - 1, s, s + 1, s + 2].map(|i| i.clamp(0, last) as usize);
            (idx, weights)
        })
        .collect()
}

/// Resizes an image like `cv2.resize(..., INTER_CUBIC)` on RGB8 channels.
pub(crate) fn resize_cubic_cv2(
    image: &DynamicImage,
    dst_w: usize,
    dst_h: usize,
) -> RgbImage {
    let src = image.to_rgb8();
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let xc = cubic_coeffs(dst_w, sw);
    let yc = cubic_coeffs(dst_h, sh);
    let raw = src.as_raw();
    let row_stride = sw * 3;

    let mut out = RgbImage::new(dst_w as u32, dst_h as u32);
    let out_raw = out.as_mut();
    for (dy, (yi, yw)) in yc.iter().enumerate() {
        for (dx, (xi, xw)) in xc.iter().enumerate() {
            let out_base = (dy * dst_w + dx) * 3;
            for ch in 0..3 {
                let mut value: i64 = 1 << (2 * COEF_BITS - 1);
                for ky in 0..4 {
                    let row = yi[ky] * row_stride;
                    let mut horizontal: i64 = 0;
                    for kx in 0..4 {
                        horizontal +=
                            raw[row + xi[kx] * 3 + ch] as i64 * xw[kx];
                    }
                    value += horizontal * yw[ky];
                }
                out_raw[out_base + ch] =
                    (value >> (2 * COEF_BITS)).clamp(0, 255) as u8;
            }
        }
    }
    out
}
