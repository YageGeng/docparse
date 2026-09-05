// SPDX-License-Identifier: Apache-2.0
// Derived from LiteParse revision b2e76ec5b0c1cb4eb11d67296e916792f4fb5858 and modified for docparse.
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::ptr::NonNull;

#[derive(Debug, Clone, Copy, Default)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct CharBox {
    pub left: f64,
    pub right: f64,
    pub bottom: f64,
    pub top: f64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct TextRect {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// A double-precision point in PDF page space.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PointF {
    pub x: f64,
    pub y: f64,
}

/// Opaque temporary identity for a text page object owned by a live page.
#[derive(Clone, Copy)]
pub struct TextObjectIdentity<'page> {
    handle: NonNull<pdfium_sys::fpdf_pageobject_t__>,
    _page: PhantomData<&'page ()>,
}

impl TextObjectIdentity<'_> {
    /// Wraps a non-null PDFium object solely for in-page identity comparison.
    pub(crate) fn from_handle(
        handle: pdfium_sys::FPDF_PAGEOBJECT,
    ) -> Option<Self> {
        NonNull::new(handle).map(|handle| Self {
            handle,
            _page: PhantomData,
        })
    }
}

impl fmt::Debug for TextObjectIdentity<'_> {
    /// Redacts the process-local pointer value from diagnostics.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("TextObjectIdentity(..)")
    }
}

impl<'left, 'right> PartialEq<TextObjectIdentity<'right>>
    for TextObjectIdentity<'left>
{
    /// Compares temporary identities without exposing their pointer values.
    fn eq(&self, other: &TextObjectIdentity<'right>) -> bool {
        self.handle == other.handle
    }
}

impl Eq for TextObjectIdentity<'_> {}

impl Hash for TextObjectIdentity<'_> {
    /// Hashes the process-local identity for temporary in-page lookup only.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.handle.hash(state);
    }
}
