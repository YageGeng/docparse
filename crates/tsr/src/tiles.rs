//! Bounded image-only retries for tables exceeding the model's decoder window.
use docparse_layout::{PageImage, PageImageInput};
use std::sync::Arc;

/// One owned vertical segment retains its original crop coordinates.
pub(crate) struct TableSlice {
    pub image: Arc<PageImage>,
    pub offset: u32,
    depth: u8,
}
impl TableSlice {
    /// Starts an unmodified model crop at its local origin.
    pub fn new(image: Arc<PageImage>) -> Self {
        Self {
            image,
            offset: 0,
            depth: 0,
        }
    }

    /// Splits only on a visible full-width separator or blank scanline, never through text ink.
    pub fn split(&self) -> Option<[Self; 2]> {
        let (width, height) = (self.image.width(), self.image.height());
        if self.depth >= 3 || height < 80 {
            return None;
        }
        let stride = width as usize * 3;
        let rows: Vec<_> = self
            .image
            .data()
            .chunks_exact(stride)
            .map(|row| {
                row.as_chunks::<3>()
                    .0
                    .iter()
                    .filter(|rgb| rgb.iter().all(|&v| v < 200))
                    .count() as f64
                    / f64::from(width)
            })
            .collect();
        let range = height / 5..height * 4 / 5;
        let separator = range
            .clone()
            .filter(|&y| rows.get(y as usize).is_some_and(|&ink| ink >= 0.75))
            .min_by_key(|&y| y.abs_diff(height / 2));
        let cut = separator.or_else(|| {
            range
                .filter(|&y| {
                    rows.get(y as usize).is_some_and(|&ink| ink <= 0.001)
                })
                .min_by_key(|&y| y.abs_diff(height / 2))
        })?;
        let parts = [(0, cut), (cut, height)].map(|(start, end)| {
            let pixels = self
                .image
                .data()
                .get(start as usize * stride..end as usize * stride)?;
            let image = PageImage::try_from(
                PageImageInput::builder()
                    .width(width)
                    .height(end - start)
                    .pixel_format(self.image.pixel_format())
                    .data(Arc::from(pixels))
                    .build(),
            )
            .ok()?;
            Some(Self {
                image: Arc::new(image),
                offset: self.offset + start,
                depth: self.depth + 1,
            })
        });
        let [Some(top), Some(bottom)] = parts else {
            return None;
        };
        Some([top, bottom])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docparse_layout::PixelFormat;

    /// Splitting preserves every pixel and original coordinate while bounding the retry tree.
    #[test]
    fn image_segments_preserve_pixels_and_stop_at_the_depth_limit() {
        let mut pixels = vec![255_u8; 32 * 800 * 3];
        pixels
            .get_mut(400 * 32 * 3..401 * 32 * 3)
            .expect("separator row")
            .fill(0);
        let image = Arc::new(
            PageImage::try_from(
                PageImageInput::builder()
                    .width(32)
                    .height(800)
                    .pixel_format(PixelFormat::Rgb8)
                    .data(Arc::from(pixels.clone()))
                    .build(),
            )
            .expect("image"),
        );
        let [top, bottom] = TableSlice::new(Arc::clone(&image))
            .split()
            .expect("visible separator");
        assert_eq!((top.offset, bottom.offset), (0, 400));
        assert_eq!(top.image.height() + bottom.image.height(), 800);
        let joined =
            [top.image.data().as_ref(), bottom.image.data().as_ref()].concat();
        assert_eq!(joined, pixels);
        let exhausted = TableSlice {
            image,
            offset: 123,
            depth: 3,
        };
        assert!(exhausted.split().is_none());
    }
}
