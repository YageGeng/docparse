//! Optional recovery of undecodable glyphs from their vector outlines.

/// Sampling size shared with LiteParse-compatible outline databases.
pub const GLYPH_RESOLVER_FONT_SIZE: f32 = 10.0;

/// Last-resort recovery after glyph names and embedded Unicode cmaps fail.
pub trait GlyphResolver: crate::WasmCompatSend + crate::WasmCompatSync {
    /// Returns the glyph's text from `(segment_type, x, y)` paths sampled at 10pt.
    /// Empty strings, controls and invalid sentinels are rejected by extraction.
    fn resolve(&self, segments: &[(i32, f32, f32)]) -> Option<String>;
}
