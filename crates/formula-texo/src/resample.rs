//! Pillow-compatible image resampling.
//!
//! Reproduces `Resample.c` and `Reduce.c` exactly: the same coefficient
//! precomputation, the same 22-bit fixed-point accumulation for 8-bit samples,
//! the same two-pass ordering, and the same fixed-point `reduce` (which is not
//! quite a rounded box average - see `reduce_multiplier`).
//!
//! Matching bit-for-bit matters: the tokens this crate emits were checked
//! against the original implementation, and on wide inputs a one-step
//! difference here is enough to change them.

// Adapted from best-ocr-rust at 5e00bb7ac26c3f8ce9b09f857725c1d17f61e7ae (AGPL-3.0-only).
// Fixed-point rounding is part of the trained image contract, so image::resize is not equivalent.
#![allow(
    clippy::indexing_slicing,
    clippy::cast_sign_loss,
    clippy::float_cmp,
    clippy::needless_range_loop
)]

/// Interleaved 8-bit plane.
#[derive(Clone, typed_builder::TypedBuilder)]
pub struct Plane {
    pub w: usize,
    pub h: usize,
    pub c: usize,
    pub data: Vec<u8>,
}

impl Plane {
    /// Allocates a plane after dimensions have been bounded by preprocessing.
    pub fn new(w: usize, h: usize, c: usize) -> Self {
        Plane::builder()
            .w(w)
            .h(h)
            .c(c)
            .data(vec![0; w * h * c])
            .build()
    }
}

#[derive(Clone, Copy, PartialEq)]
pub enum Filter {
    Bilinear,
    Bicubic,
}

impl Filter {
    /// Returns the filter radius used by Pillow.
    fn support(self) -> f64 {
        match self {
            Filter::Bilinear => 1.0,
            Filter::Bicubic => 2.0,
        }
    }

    /// Evaluates the selected interpolation kernel.
    fn eval(self, x: f64) -> f64 {
        let x = x.abs();
        match self {
            Filter::Bilinear => {
                if x < 1.0 {
                    1.0 - x
                } else {
                    0.0
                }
            }
            Filter::Bicubic => {
                // Catmull-Rom style cubic with a = -0.5, as in Pillow.
                const A: f64 = -0.5;
                if x < 1.0 {
                    ((A + 2.0) * x - (A + 3.0)) * x * x + 1.0
                } else if x < 2.0 {
                    (((x - 5.0) * x + 8.0) * x - 4.0) * A
                } else {
                    0.0
                }
            }
        }
    }
}

const PRECISION_BITS: i32 = 32 - 8 - 2;

struct Coeffs {
    ksize: usize,
    /// (xmin, xlen) per output position.
    bounds: Vec<(i32, i32)>,
    /// Fixed-point kernel, `ksize` entries per output position.
    kk: Vec<i32>,
}

/// Port of `precompute_coeffs` + `normalize_coeffs_8bpc`.
fn precompute_coeffs(
    in_size: usize,
    in0: f32,
    in1: f32,
    out_size: usize,
    f: Filter,
) -> Coeffs {
    let scale = (in1 - in0) as f64 / out_size as f64;
    let filterscale = if scale < 1.0 { 1.0 } else { scale };
    let support = f.support() * filterscale;
    let ksize = (support.ceil() as usize) * 2 + 1;

    let mut bounds = Vec::with_capacity(out_size);
    let mut kk = vec![0i32; out_size * ksize];
    let mut k = vec![0f64; ksize];

    for xx in 0..out_size {
        let center = in0 as f64 + (xx as f64 + 0.5) * scale;
        let ss = 1.0 / filterscale;
        let mut xmin = (center - support + 0.5) as i32;
        if xmin < 0 {
            xmin = 0;
        }
        let mut xmax = (center + support + 0.5) as i32;
        if xmax > in_size as i32 {
            xmax = in_size as i32;
        }
        xmax -= xmin;
        if xmax < 0 {
            xmax = 0;
        }

        let mut ww = 0.0f64;
        for x in 0..xmax as usize {
            let w = f.eval((x as f64 + xmin as f64 - center + 0.5) * ss);
            k[x] = w;
            ww += w;
        }
        if ww != 0.0 {
            for x in 0..xmax as usize {
                k[x] /= ww;
            }
        }
        for x in xmax as usize..ksize {
            k[x] = 0.0;
        }
        // normalize_coeffs_8bpc: round-half-away-from-zero into fixed point
        let scale_fp = (1i64 << PRECISION_BITS) as f64;
        for x in 0..ksize {
            let v = k[x] * scale_fp;
            kk[xx * ksize + x] = if k[x] < 0.0 {
                (v - 0.5) as i32
            } else {
                (v + 0.5) as i32
            };
        }
        bounds.push((xmin, xmax));
    }

    Coeffs { ksize, bounds, kk }
}

/// Rounds a fixed-point accumulator into an unsigned sample.
#[inline]
fn clip8(v: i32) -> u8 {
    (v >> PRECISION_BITS).clamp(0, 255) as u8
}

/// Applies the horizontal pass over the bounded source region.
fn resample_horizontal(
    src: &Plane,
    out_w: usize,
    offset: usize,
    out_h: usize,
    c: &Coeffs,
) -> Plane {
    let mut out = Plane::new(out_w, out_h, src.c);
    let ch = src.c;
    for yy in 0..out_h {
        let srow = (yy + offset) * src.w * ch;
        let drow = yy * out_w * ch;
        for xx in 0..out_w {
            let (xmin, xlen) = c.bounds[xx];
            let k = &c.kk[xx * c.ksize..xx * c.ksize + c.ksize];
            for b in 0..ch {
                let mut ss = 1i32 << (PRECISION_BITS - 1);
                for x in 0..xlen as usize {
                    let px =
                        src.data[srow + (xmin as usize + x) * ch + b] as i32;
                    ss = ss.wrapping_add(px.wrapping_mul(k[x]));
                }
                out.data[drow + xx * ch + b] = clip8(ss);
            }
        }
    }
    out
}

/// Applies the vertical pass with Pillow-compatible rounding.
fn resample_vertical(src: &Plane, out_h: usize, c: &Coeffs) -> Plane {
    let mut out = Plane::new(src.w, out_h, src.c);
    let ch = src.c;
    for yy in 0..out_h {
        let (ymin, ylen) = c.bounds[yy];
        let k = &c.kk[yy * c.ksize..yy * c.ksize + c.ksize];
        let drow = yy * src.w * ch;
        for xx in 0..src.w {
            for b in 0..ch {
                let mut ss = 1i32 << (PRECISION_BITS - 1);
                for y in 0..ylen as usize {
                    let px = src.data
                        [(ymin as usize + y) * src.w * ch + xx * ch + b]
                        as i32;
                    ss = ss.wrapping_add(px.wrapping_mul(k[y]));
                }
                out.data[drow + xx * ch + b] = clip8(ss);
            }
        }
    }
    out
}

/// `ImagingResample`: two-pass resample of `src` region `box` into `out_w x out_h`.
pub fn resample(
    src: &Plane,
    out_w: usize,
    out_h: usize,
    f: Filter,
    bx: [f32; 4],
) -> Plane {
    let need_h = out_w != src.w || bx[0] != 0.0 || bx[2] != src.w as f32;
    let need_v = out_h != src.h || bx[1] != 0.0 || bx[3] != src.h as f32;
    if !need_h && !need_v {
        return src.clone();
    }

    let ch_horiz = precompute_coeffs(src.w, bx[0], bx[2], out_w, f);
    let mut ch_vert = precompute_coeffs(src.h, bx[1], bx[3], out_h, f);

    let ybox_first = ch_vert.bounds[0].0 as usize;
    let last = ch_vert.bounds[out_h - 1];
    let ybox_last = (last.0 + last.1) as usize;

    let mut cur = if need_h {
        for b in ch_vert.bounds.iter_mut() {
            b.0 -= ybox_first as i32;
        }
        resample_horizontal(
            src,
            out_w,
            ybox_first,
            ybox_last - ybox_first,
            &ch_horiz,
        )
    } else {
        src.clone()
    };

    if need_v {
        cur = resample_vertical(&cur, out_h, &ch_vert);
    }
    cur
}

/// Pillow scales box sums by a truncated fixed-point reciprocal rather than
/// dividing, so results can sit one step below a true rounded average. The
/// shift is 24 and the divisor is the block's actual pixel count, which for
/// trailing partial blocks is smaller than `fx * fy`.
const REDUCE_SHIFT: u32 = 24;

/// Returns the truncated reciprocal used by Pillow box reduction.
#[inline]
fn reduce_multiplier(n: u32) -> u64 {
    (1u64 << REDUCE_SHIFT) / n as u64
}

/// `ImagingReduce` over the whole image: box sums scaled by the fixed-point
/// reciprocal above, with smaller divisors for the trailing partial blocks.
pub fn reduce(src: &Plane, fx: usize, fy: usize) -> Plane {
    if fx == 1 && fy == 1 {
        return src.clone();
    }
    let ow = src.w.div_ceil(fx);
    let oh = src.h.div_ceil(fy);
    let ch = src.c;
    let mut out = Plane::new(ow, oh, ch);

    for yy in 0..oh {
        let y0 = yy * fy;
        let ys = (fy).min(src.h - y0);
        for xx in 0..ow {
            let x0 = xx * fx;
            let xs = (fx).min(src.w - x0);
            let n = (xs * ys) as u32;
            let amend = n / 2;
            let mult = reduce_multiplier(n);
            for b in 0..ch {
                let mut acc: u32 = amend;
                for y in 0..ys {
                    let row = (y0 + y) * src.w * ch;
                    for x in 0..xs {
                        acc += src.data[row + (x0 + x) * ch + b] as u32;
                    }
                }
                out.data[(yy * ow + xx) * ch + b] =
                    ((acc as u64 * mult) >> REDUCE_SHIFT) as u8;
            }
        }
    }
    out
}

/// `Image.resize` including the `reducing_gap` pre-reduction step.
pub fn resize(
    src: &Plane,
    out_w: usize,
    out_h: usize,
    f: Filter,
    reducing_gap: Option<f64>,
) -> Plane {
    let mut bx = [0f32, 0f32, src.w as f32, src.h as f32];
    if src.w == out_w && src.h == out_h {
        return src.clone();
    }
    let mut cur = src.clone();
    if let Some(gap) = reducing_gap {
        let fx =
            (((bx[2] - bx[0]) as f64 / out_w as f64 / gap) as usize).max(1);
        let fy =
            (((bx[3] - bx[1]) as f64 / out_h as f64 / gap) as usize).max(1);
        if fx > 1 || fy > 1 {
            // `_get_safe_box` clamps back to the full image for a full-image box
            cur = reduce(&cur, fx, fy);
            bx = [0.0, 0.0, src.w as f32 / fx as f32, src.h as f32 / fy as f32];
        }
    }
    resample(&cur, out_w, out_h, f, bx)
}

/// `Image.thumbnail(size, BICUBIC, reducing_gap=2.0)`: shrink in place so both
/// sides fit within `max_side`, preserving aspect with Pillow's rounding.
pub fn thumbnail(src: &Plane, max_side: usize) -> Plane {
    let (x, y) = (max_side, max_side);
    if x >= src.w && y >= src.h {
        return src.clone();
    }
    let aspect = src.w as f64 / src.h as f64;
    let (fw, fh);
    if x as f64 / y as f64 >= aspect {
        fw = round_aspect(y as f64 * aspect, |n| {
            (aspect - n as f64 / y as f64).abs()
        });
        fh = y;
    } else {
        fw = x;
        fh = round_aspect(x as f64 / aspect, |n| {
            if n == 0 {
                0.0
            } else {
                (aspect - x as f64 / n as f64).abs()
            }
        });
    }
    if fw == src.w && fh == src.h {
        return src.clone();
    }
    resize(src, fw, fh, Filter::Bicubic, Some(2.0))
}

/// Pillow's `round_aspect`: pick floor or ceil, whichever preserves the
/// aspect ratio better; ties go to the smaller value; never below 1.
fn round_aspect<F: Fn(usize) -> f64>(number: f64, key: F) -> usize {
    let lo = number.floor().max(0.0) as usize;
    let hi = number.ceil().max(0.0) as usize;
    let best = if key(hi) < key(lo) { hi } else { lo };
    best.max(1)
}

/// `torchvision.transforms.functional.resize(img, size)` on a PIL image:
/// scale so the short side equals `size`, preserving aspect, PIL BILINEAR.
pub fn resize_short_side(src: &Plane, size: usize) -> Plane {
    let (w, h) = (src.w, src.h);
    if (w <= h && w == size) || (h <= w && h == size) {
        return src.clone();
    }
    let (ow, oh) = if w < h {
        (size, ((size as f64 * h as f64) / w as f64) as usize)
    } else {
        (((size as f64 * w as f64) / h as f64) as usize, size)
    };
    let (ow, oh) = (ow.max(1), oh.max(1));
    resize(src, ow, oh, Filter::Bilinear, None)
}
