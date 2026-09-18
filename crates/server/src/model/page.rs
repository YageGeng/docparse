use serde::{Deserialize, Serialize};
use std::num::NonZeroU32;

/// One-based page parameter; omission is represented by Option<Page> at the endpoint boundary.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(transparent)]
#[schema(value_type = u32)]
pub struct Page(NonZeroU32);

impl From<Page> for u32 {
    /// Exposes a validated page number for indexing and byte-range selection.
    fn from(page: Page) -> Self {
        page.0.get()
    }
}

/// A selected document page with total page count and parsing errors.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct Pagination<T> {
    /// Total source PDF pages, including pages that failed parsing.
    pub page_count: u32,
    /// Document parsing errors remain visible when a requested page is missing.
    pub errors: Vec<docparse_core::PageError>,
    /// Null when this source page has no canonical output because parsing failed.
    pub page: Option<T>,
}
