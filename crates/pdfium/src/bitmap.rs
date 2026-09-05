// SPDX-License-Identifier: Apache-2.0
// Derived from LiteParse revision b2e76ec5b0c1cb4eb11d67296e916792f4fb5858 and modified for docparse.
use std::marker::PhantomData;
use std::ptr::NonNull;

use crate::error::PdfiumError;
use crate::ffi;
use crate::library::Library;

/// A BGRA pixel buffer owned by PDFium.
///
/// The `'lib` lifetime ties the bitmap to a held [`Library`] lock, so it
/// cannot be created (or destroyed via `Drop`) outside the PDFium critical
/// section.
pub struct Bitmap<'lib> {
    handle: pdfium_sys::FPDF_BITMAP,
    _lib: PhantomData<&'lib Library>,
}

impl<'lib> Bitmap<'lib> {
    /// Wrap an existing FPDF_BITMAP handle (takes ownership, will destroy on drop).
    ///
    /// # Safety
    /// The handle must be a valid, non-null bitmap that the caller owns,
    /// and the caller must hold a [`Library`] for at least `'lib`.
    pub unsafe fn from_handle(handle: pdfium_sys::FPDF_BITMAP) -> Self {
        Bitmap {
            handle,
            _lib: PhantomData,
        }
    }

    /// Create a new BGRA bitmap with the given dimensions.
    ///
    /// # Safety
    /// The caller must hold a [`Library`] for at least `'lib` (PDFium FFI is
    /// not thread-safe). `'lib` is not constrained by an argument, so callers
    /// must ensure it cannot outlive the held lock — usually by inferring it
    /// from the call site (e.g. returning a `Bitmap<'lib>` from a method on
    /// `Page<'_, 'lib>`, whose existence already proves the lock is held).
    pub unsafe fn new(width: i32, height: i32) -> Result<Self, PdfiumError> {
        let handle = unsafe {
            ffi!(FPDFBitmap_CreateEx(
                width,
                height,
                pdfium_sys::FPDFBitmap_BGRA as i32,
                std::ptr::null_mut(),
                0, // stride=0 lets pdfium choose
            ))
        };
        if handle.is_null() {
            return Err(PdfiumError::OperationFailed);
        }
        Ok(Bitmap {
            handle,
            _lib: PhantomData,
        })
    }

    pub fn handle(&self) -> pdfium_sys::FPDF_BITMAP {
        self.handle
    }

    pub fn width(&self) -> i32 {
        unsafe { ffi!(FPDFBitmap_GetWidth(self.handle)) }
    }

    pub fn height(&self) -> i32 {
        unsafe { ffi!(FPDFBitmap_GetHeight(self.handle)) }
    }

    pub fn stride(&self) -> i32 {
        unsafe { ffi!(FPDFBitmap_GetStride(self.handle)) }
    }

    /// Fill a rectangle with an ARGB color (0xAARRGGBB).
    pub fn fill_rect(
        &self,
        left: i32,
        top: i32,
        width: i32,
        height: i32,
        color: u64,
    ) -> Result<(), PdfiumError> {
        let color = checked_argb(color).ok_or(PdfiumError::InvalidArgument)?;
        // SAFETY: `self.handle` remains owned by this live bitmap and `color` is validated to
        // the 32-bit ARGB domain accepted by PDFium on every supported platform.
        unsafe {
            ffi!(FPDFBitmap_FillRect(
                self.handle,
                left,
                top,
                width,
                height,
                color,
            ));
        }
        Ok(())
    }

    /// Get the raw pixel buffer as a byte slice.
    /// Format is BGRA, row-major, with `stride()` bytes per row.
    pub fn buffer(&self) -> Result<&[u8], PdfiumError> {
        let (_, _, _, len) = self.layout()?;
        let ptr = unsafe { ffi!(FPDFBitmap_GetBuffer(self.handle)) };
        let ptr = NonNull::new(ptr.cast::<u8>())
            .ok_or(PdfiumError::OperationFailed)?;
        // SAFETY: validated PDFium layout metadata bounds this live bitmap allocation.
        Ok(unsafe { std::slice::from_raw_parts(ptr.as_ptr(), len) })
    }

    /// Convert the BGRA buffer to RGBA in a new Vec.
    pub fn to_rgba(&self) -> Result<Vec<u8>, PdfiumError> {
        let (width, height, stride, _) = self.layout()?;
        let src = self.buffer()?;
        let capacity = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or(PdfiumError::OperationFailed)?;
        let mut rgba = Vec::with_capacity(capacity);

        for y in 0..height {
            let start =
                y.checked_mul(stride).ok_or(PdfiumError::OperationFailed)?;
            let end = start
                .checked_add(
                    width.checked_mul(4).ok_or(PdfiumError::OperationFailed)?,
                )
                .ok_or(PdfiumError::OperationFailed)?;
            let row =
                src.get(start..end).ok_or(PdfiumError::OperationFailed)?;
            // Fixed array chunks preserve exact BGRA pixels and satisfy current Clippy guidance.
            for pixel in row.as_chunks::<4>().0 {
                // BGRA -> RGBA
                rgba.push(pixel[2]); // R
                rgba.push(pixel[1]); // G
                rgba.push(pixel[0]); // B
                rgba.push(pixel[3]); // A
            }
        }

        Ok(rgba)
    }

    /// Convert the BGRA buffer to tightly-packed RGB in a new Vec, dropping the
    /// alpha channel (pages render onto opaque white, so alpha is constant 255).
    pub fn to_rgb(&self) -> Result<Vec<u8>, PdfiumError> {
        let (width, height, stride, _) = self.layout()?;
        let src = self.buffer()?;
        let capacity = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(3))
            .ok_or(PdfiumError::OperationFailed)?;
        let mut rgb = Vec::with_capacity(capacity);

        for y in 0..height {
            let start =
                y.checked_mul(stride).ok_or(PdfiumError::OperationFailed)?;
            let end = start
                .checked_add(
                    width.checked_mul(4).ok_or(PdfiumError::OperationFailed)?,
                )
                .ok_or(PdfiumError::OperationFailed)?;
            let row =
                src.get(start..end).ok_or(PdfiumError::OperationFailed)?;
            // Fixed array chunks preserve exact BGRA pixels and satisfy current Clippy guidance.
            for pixel in row.as_chunks::<4>().0 {
                // BGRA -> RGB (drop A)
                rgb.push(pixel[2]); // R
                rgb.push(pixel[1]); // G
                rgb.push(pixel[0]); // B
            }
        }

        Ok(rgb)
    }

    /// Convert the BGRA buffer to tightly-packed 8-bit grayscale (1 byte/px)
    /// using Rec. 601 luma weights.
    pub fn to_luma(&self) -> Result<Vec<u8>, PdfiumError> {
        let (width, height, stride, _) = self.layout()?;
        let src = self.buffer()?;
        let capacity = width
            .checked_mul(height)
            .ok_or(PdfiumError::OperationFailed)?;
        let mut luma = Vec::with_capacity(capacity);

        for y in 0..height {
            let start =
                y.checked_mul(stride).ok_or(PdfiumError::OperationFailed)?;
            let end = start
                .checked_add(
                    width.checked_mul(4).ok_or(PdfiumError::OperationFailed)?,
                )
                .ok_or(PdfiumError::OperationFailed)?;
            let row =
                src.get(start..end).ok_or(PdfiumError::OperationFailed)?;
            // Fixed array chunks preserve exact BGRA pixels and satisfy current Clippy guidance.
            for pixel in row.as_chunks::<4>().0 {
                let (b, g, r) =
                    (pixel[0] as u32, pixel[1] as u32, pixel[2] as u32);
                luma.push(((77 * r + 150 * g + 29 * b) >> 8) as u8);
            }
        }

        Ok(luma)
    }

    /// Validates all PDFium-reported dimensions before pixel memory access.
    fn layout(&self) -> Result<(usize, usize, usize, usize), PdfiumError> {
        checked_bitmap_layout(self.width(), self.height(), self.stride())
            .ok_or(PdfiumError::OperationFailed)
    }
}

/// Validates positive dimensions, row capacity, and total bitmap byte length.
fn checked_bitmap_layout(
    width: i32,
    height: i32,
    stride: i32,
) -> Option<(usize, usize, usize, usize)> {
    let width = usize::try_from(width).ok().filter(|width| *width > 0)?;
    let height = usize::try_from(height).ok().filter(|height| *height > 0)?;
    let stride = usize::try_from(stride).ok()?;
    let row_bytes = width.checked_mul(4)?;
    if stride < row_bytes {
        return None;
    }
    let len = stride.checked_mul(height)?;
    // Rust slices cannot span more than `isize::MAX` bytes, including on 32-bit targets.
    if len > isize::MAX as usize {
        return None;
    }
    Some((width, height, stride, len))
}

/// Converts public ARGB input into PDFium's platform-width color type without panicking.
fn checked_argb(color: u64) -> Option<pdfium_sys::FPDF_DWORD> {
    let color = u32::try_from(color).ok()?;
    Some(pdfium_sys::FPDF_DWORD::from(color))
}

impl Drop for Bitmap<'_> {
    fn drop(&mut self) {
        unsafe { ffi!(FPDFBitmap_Destroy(self.handle)) };
    }
}

#[cfg(test)]
mod tests {
    use super::{checked_argb, checked_bitmap_layout};

    /// Verifies bitmap dimensions and stride are checked before slice construction.
    #[test]
    fn bitmap_layout_rejects_invalid_stride_and_dimensions() {
        assert_eq!(checked_bitmap_layout(10, 2, 40), Some((10, 2, 40, 80)));
        assert_eq!(checked_bitmap_layout(10, 2, 39), None);
        assert_eq!(checked_bitmap_layout(-1, 2, 40), None);
        assert_eq!(checked_bitmap_layout(10, 0, 40), None);
    }

    /// Verifies the safe color boundary rejects values outside 32-bit ARGB.
    #[test]
    fn argb_conversion_rejects_values_above_u32() {
        assert!(checked_argb(u64::from(u32::MAX)).is_some());
        assert!(checked_argb(u64::from(u32::MAX) + 1).is_none());
    }
}
