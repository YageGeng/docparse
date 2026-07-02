//! WASM PDF rendering bindings.

use std::io::Cursor;

use docparse_core::pdf::{PdfRenderOptions, render_pdf_bytes};
use image::ImageFormat;
use wasm_bindgen::prelude::wasm_bindgen;

/// One rendered PDF page returned to JavaScript.
#[wasm_bindgen]
pub struct WasmPdfPage {
    page_index: usize,
    width: u32,
    height: u32,
    png_data: Vec<u8>,
}

#[wasm_bindgen]
impl WasmPdfPage {
    /// Zero-based page index.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn page_index(&self) -> usize {
        self.page_index
    }

    /// Rendered page width in pixels.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Rendered page height in pixels.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Rendered page encoded as PNG bytes.
    #[wasm_bindgen(getter)]
    #[must_use]
    pub fn png_data(&self) -> js_sys::Uint8Array {
        js_sys::Uint8Array::from(self.png_data.as_slice())
    }
}

/// Renders PDF bytes into PNG pages.
#[wasm_bindgen]
pub fn render_pdf_to_png_pages(
    pdf_bytes: &[u8],
    max_pages: usize,
    dpi: f32,
) -> Result<js_sys::Array, wasm_bindgen::JsValue> {
    if max_pages == 0 {
        return Err(wasm_bindgen::JsValue::from_str(
            "max_pages must be greater than 0",
        ));
    }
    if dpi <= 0.0 {
        return Err(wasm_bindgen::JsValue::from_str(
            "dpi must be greater than 0",
        ));
    }

    let pages =
        render_pdf_bytes(pdf_bytes, PdfRenderOptions { max_pages, dpi })
            .map_err(|error| {
                wasm_bindgen::JsValue::from_str(&error.to_string())
            })?;
    let output = js_sys::Array::new();

    for page in pages {
        let mut png_data = Vec::new();
        page.image
            .write_to(&mut Cursor::new(&mut png_data), ImageFormat::Png)
            .map_err(|error| {
                wasm_bindgen::JsValue::from_str(&format!(
                    "failed to encode rendered page as PNG: {error}"
                ))
            })?;

        output.push(
            &WasmPdfPage {
                page_index: page.page_index,
                width: page.image.width(),
                height: page.image.height(),
                png_data,
            }
            .into(),
        );
    }

    Ok(output)
}
