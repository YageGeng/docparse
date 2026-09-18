//! Raster-aware inline formula crops reuse the rendered page without another PDF rendering pass.
use super::RenderedPage;
use docparse_formula::FormulaError;
use docparse_layout::{Bbox, PageImage, PageImageInput, Point};
use std::sync::Arc;

/// A viewport formula box paired with the already rendered page raster.
pub(super) struct FormulaCrop<'a> {
    pub(super) bbox: Bbox,
    pub(super) rendered: &'a RenderedPage,
    /// Optional viewport search limits; display formulas retain the original cropping policy.
    pub(super) expansion: Option<Bbox>,
}

impl TryFrom<&mut FormulaCrop<'_>> for PageImage {
    type Error = FormulaError;

    /// Uses the same viewport-to-pixel transform as layout, including rotation and crop-box offsets.
    #[allow(clippy::cast_sign_loss)] // Coordinates are clamped to the validated raster before conversion.
    fn try_from(crop: &mut FormulaCrop<'_>) -> Result<Self, Self::Error> {
        let image = &crop.rendered.image;
        let transform = &crop.rendered.transform;
        if transform.render_size() != (image.width(), image.height()) {
            return Err(FormulaError::Invalid(
                "formula image/transform mismatch".into(),
            ));
        }
        let start = transform
            .viewport_to_rendered(Point::new(crop.bbox.left, crop.bbox.top));
        let end = transform.viewport_to_rendered(Point::new(
            crop.bbox.right,
            crop.bbox.bottom,
        ));
        let mut left = start.x.floor().clamp(0.0, image.width() as f64) as u32;
        let mut top = start.y.floor().clamp(0.0, image.height() as f64) as u32;
        let mut right = end.x.ceil().clamp(0.0, image.width() as f64) as u32;
        let mut bottom = end.y.ceil().clamp(0.0, image.height() as f64) as u32;
        if left >= right || top >= bottom {
            return Err(FormulaError::Invalid(
                "formula crop is outside the page".into(),
            ));
        }
        if let Some(limit) = crop.expansion {
            // Search locally: crossing a full formula height without a separator is ambiguous, not permission to take another row.
            let margin = (bottom - top).max(2);
            let min_x = left.saturating_sub(margin);
            let min_y = top.saturating_sub(margin);
            let max_x = right.saturating_add(margin).min(image.width());
            let max_y = bottom.saturating_add(margin).min(image.height());
            let pixel = |x: u32, y: u32| {
                let offset =
                    (y as usize * image.width() as usize + x as usize) * 3;
                image
                    .data()
                    .get(offset..offset + 3)
                    .and_then(|rgb| <&[u8; 3]>::try_from(rgb).ok())
            };
            // Sample the outer perimeter rather than the tightly cropped ink; quantization tolerates scan and antialias noise.
            let perimeter = (min_x..max_x)
                .flat_map(|x| [(x, min_y), (x, max_y - 1)])
                .chain(
                    (min_y..max_y).flat_map(|y| [(min_x, y), (max_x - 1, y)]),
                );
            // Four bits per RGB channel address a fixed palette, avoiding tree lookups and node allocations per crop.
            let mut colors = [0_usize; 4096];
            let mut background = [255_u8; 3];
            let mut largest = 0;
            for (x, y) in perimeter {
                let rgb = pixel(x, y).ok_or_else(|| {
                    FormulaError::Invalid(
                        "formula background sample exceeds raster".into(),
                    )
                })?;
                let [red, green, blue] = *rgb;
                let bin = (usize::from(red >> 4) << 8)
                    | (usize::from(green >> 4) << 4)
                    | usize::from(blue >> 4);
                let count = colors
                    .get_mut(bin)
                    .expect("12-bit RGB bin fits the palette");
                *count += 1;
                if *count > largest {
                    largest = *count;
                    background = *rgb;
                }
            }
            // Every pixel must resemble the local background; a thin dark stroke must never pass a percentage-based blank test.
            let blank = |x, y| {
                pixel(x, y).is_some_and(|rgb| {
                    rgb.iter().zip(background).all(|(channel, reference)| {
                        channel.abs_diff(reference) <= 24
                    })
                })
            };
            // Use the wider perimeter for background estimation, then prevent the search from entering neighboring rows.
            let limit_start = transform
                .viewport_to_rendered(Point::new(limit.left, limit.top));
            let limit_end = transform
                .viewport_to_rendered(Point::new(limit.right, limit.bottom));
            let min_x = min_x.max(
                (limit_start.x.ceil().clamp(0.0, f64::from(image.width()))
                    as u32)
                    .min(left),
            );
            let min_y = min_y.max(
                (limit_start.y.ceil().clamp(0.0, f64::from(image.height()))
                    as u32)
                    .min(top),
            );
            let max_x = max_x.min(
                (limit_end.x.floor().clamp(0.0, f64::from(image.width()))
                    as u32)
                    .max(right),
            );
            let max_y = max_y.min(
                (limit_end.y.floor().clamp(0.0, f64::from(image.height()))
                    as u32)
                    .max(bottom),
            );
            // Probe each side against the seed's fixed span and stop at its first background line.
            // Another side's expansion must never reopen a resolved edge or pull in distant corner ink.
            let horizontal = left..right;
            let vertical = top..bottom;
            top = (min_y..=top)
                .rev()
                .find(|&y| horizontal.clone().all(|x| blank(x, y)))
                .unwrap_or(top);
            bottom = (bottom - 1..max_y)
                .find(|&y| horizontal.clone().all(|x| blank(x, y)))
                .map_or(bottom, |y| y + 1);
            left = (min_x..=left)
                .rev()
                .find(|&x| vertical.clone().all(|y| blank(x, y)))
                .unwrap_or(left);
            right = (right - 1..max_x)
                .find(|&x| vertical.clone().all(|y| blank(x, y)))
                .map_or(right, |x| x + 1);
            // Recheck only the original probe spans for diagnostics, not corners exposed by other sides.
            if tracing::enabled!(tracing::Level::DEBUG) {
                let clear = [
                    horizontal.clone().all(|x| blank(x, top)),
                    horizontal.clone().all(|x| blank(x, bottom - 1)),
                    vertical.clone().all(|y| blank(left, y)),
                    vertical.clone().all(|y| blank(right - 1, y)),
                ];
                if clear.iter().any(|edge| !edge) {
                    tracing::debug!(
                        "kept {} unresolved inline crop edges inside the local search window for {:?}",
                        clear.iter().filter(|edge| !**edge).count(),
                        crop.bbox
                    );
                }
            }
            let start = transform.rendered_to_viewport(Point::new(
                f64::from(left),
                f64::from(top),
            ));
            let end = transform.rendered_to_viewport(Point::new(
                f64::from(right),
                f64::from(bottom),
            ));
            // Pixel rounding must not shrink the existing detector/glyph coverage in viewport coordinates.
            crop.bbox = Bbox::try_from([
                crop.bbox.left.min(start.x),
                crop.bbox.top.min(start.y),
                crop.bbox.right.max(end.x),
                crop.bbox.bottom.max(end.y),
            ])
            .map_err(|error| FormulaError::Invalid(error.to_string()))?;
        }
        let row_bytes = (right - left) as usize * 3;
        let mut pixels =
            Vec::with_capacity(row_bytes * (bottom - top) as usize);
        for y in top..bottom {
            let offset =
                (y as usize * image.width() as usize + left as usize) * 3;
            pixels.extend_from_slice(
                image.data().get(offset..offset + row_bytes).ok_or_else(
                    || {
                        FormulaError::Invalid(
                            "formula crop exceeds raster".into(),
                        )
                    },
                )?,
            );
        }
        PageImage::try_from(
            PageImageInput::builder()
                .width(right - left)
                .height(bottom - top)
                .pixel_format(image.pixel_format())
                .data(Arc::from(pixels))
                .build(),
        )
        .map_err(|error| FormulaError::Invalid(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_layout::{
        AffineTransform, PageRotation, PageTransform, PageTransformInput,
        PixelFormat,
    };

    /// Page edges, blank crops and continuous ink must remain bounded without inventing a distant separator.
    #[test]
    fn background_expansion_handles_edges_and_missing_separators() {
        for (seed, ink, expected) in [
            (
                [0.0, 0.0, 10.0, 10.0],
                vec![[0, 0, 12, 12]],
                [0.0, 0.0, 13.0, 13.0],
            ),
            (
                [20.0, 20.0, 30.0, 30.0],
                vec![[0, 20, 50, 30], [20, 0, 30, 50]],
                [20.0, 20.0, 30.0, 30.0],
            ),
            ([20.0, 20.0, 30.0, 30.0], vec![], [20.0, 20.0, 30.0, 30.0]),
            ([20.3, 20.4, 29.6, 29.7], vec![], [20.0, 20.0, 30.0, 30.0]),
            (
                [20.0, 20.0, 30.0, 30.0],
                vec![[20, 20, 30, 30], [17, 19, 20, 25], [15, 17, 17, 19]],
                // Corner ink must not move an edge past its first background line.
                [16.0, 19.0, 31.0, 31.0],
            ),
            (
                [20.0, 20.0, 30.0, 30.0],
                vec![[20, 20, 30, 30], [30, 25, 33, 31], [33, 31, 35, 33]],
                // Mirror the corner case to cover the bottom and right edges too.
                [19.0, 19.0, 34.0, 31.0],
            ),
        ] {
            let mut raster = image::RgbImage::from_pixel(
                50,
                50,
                image::Rgb([255, 255, 255]),
            );
            for [left, top, right, bottom] in ink {
                for y in top..bottom {
                    for x in left..right {
                        raster.put_pixel(x, y, image::Rgb([0, 0, 0]));
                    }
                }
            }
            let rendered = RenderedPage {
                page_number: 1,
                image: Arc::new(
                    PageImage::try_from(
                        PageImageInput::builder()
                            .width(50)
                            .height(50)
                            .pixel_format(PixelFormat::Rgb8)
                            .data(Arc::from(raster.into_raw()))
                            .build(),
                    )
                    .expect("image"),
                ),
                transform: PageTransform::try_from(
                    PageTransformInput::builder()
                        .page_to_viewport(AffineTransform::identity())
                        .viewport_width(50.0)
                        .viewport_height(50.0)
                        .render_width(50)
                        .render_height(50)
                        .model_width(50)
                        .model_height(50)
                        .rotation(PageRotation::Degrees0)
                        .build(),
                )
                .expect("transform"),
            };
            let original = Bbox::try_from(seed).expect("seed");
            let mut crop = FormulaCrop {
                bbox: original,
                rendered: &rendered,
                expansion: Some(
                    Bbox::try_from([0.0, 0.0, 50.0, 50.0])
                        .expect("page bounds"),
                ),
            };
            let image = PageImage::try_from(&mut crop).expect("crop");
            assert_eq!(crop.bbox, Bbox::try_from(expected).expect("expected"));
            assert!(crop.bbox.contains_bbox(original));
            assert!(
                (f64::from(image.width()) - crop.bbox.width()).abs()
                    < f64::EPSILON
            );
            assert!(
                (f64::from(image.height()) - crop.bbox.height()).abs()
                    < f64::EPSILON
            );
        }
    }
}
