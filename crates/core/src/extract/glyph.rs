//! Native glyph recovery rules, independent of text segmentation and source ownership.
//! Add exact name mappings below; geometry-dependent overlay rules belong in `compose`.

use std::collections::HashMap;

use docparse_layout::Bbox;
use pdfium::{Font, TextChar};

const NAMED_REPLACEMENTS: &[(&str, char)] =
    &[("CIRCLE", '●'), ("Circle", '○'), ("LEFTCIRCLE", '◖')];

/// A resolved scalar and its borrowed font, reused by the extraction metrics path.
pub(super) struct ResolvedGlyph<'a> {
    pub character: char,
    pub recovered: bool,
    pub font: Option<Font<'a>>,
}

/// Page-scoped rule lookup; borrowed font identities never escape the page lifetime.
#[derive(Default)]
pub(super) struct GlyphNormalizer {
    names: HashMap<(usize, u32), Option<char>>,
    pub recovered_count: usize,
}

impl GlyphNormalizer {
    /// Recovers named glyphs before dropping invalid scalars, preserving meaningful Unicode.
    pub fn resolve<'a>(
        &mut self,
        character: &'a TextChar<'_>,
    ) -> Option<ResolvedGlyph<'a>> {
        let original = char::from_u32(character.unicode())
            .filter(|value| !matches!(*value, '\0' | '\u{FFFE}' | '\u{FFFF}'));
        // Generated layout characters and real Unicode symbols are authoritative.
        // A replacement character carries no useful mapping and may be repaired.
        let eligible = !character.is_generated()
            && original
                .is_none_or(|value| value.is_ascii() || value == '\u{FFFD}')
            && (!original.is_some_and(char::is_whitespace)
                || character
                    .char_box()
                    .is_some_and(|b| b.right > b.left && b.top > b.bottom));
        let font = eligible.then(|| character.font()).flatten();
        let replacement = font.as_ref().and_then(|font| {
            let code = character.char_code();
            *self
                .names
                .entry((font.handle() as usize, code))
                .or_insert_with(|| {
                    let name = font.char_glyph_name(code)?;
                    NAMED_REPLACEMENTS.iter().find_map(|&(candidate, value)| {
                        (candidate == name).then_some(value)
                    })
                })
        });
        let value = replacement.or(original)?;
        let recovered = replacement.is_some() && original != Some(value);
        self.recovered_count += usize::from(recovered);
        Some(ResolvedGlyph {
            character: value,
            recovered,
            font,
        })
    }

    /// Combines only coincident half-circle/outline overlays; separated marks stay independent.
    pub fn compose(
        previous: (char, Bbox),
        incoming: (char, Bbox),
    ) -> Option<char> {
        let (half, circle) = match (previous.0, incoming.0) {
            ('◖', '○') => (previous.1, incoming.1),
            ('○', '◖') => (incoming.1, previous.1),
            _ => return None,
        };
        let tolerance = (circle.height() * 0.05).max(0.1);
        ((half.left - circle.left).abs() <= tolerance
            && (half.top - circle.top).abs() <= tolerance
            && (half.bottom - circle.bottom).abs() <= tolerance
            && (half.width() / circle.width() - 0.5).abs() <= 0.1)
            .then_some('◐')
    }
}

#[cfg(test)]
mod tests {
    use super::super::text::extract_page_text_items;

    /// Restores named visible glyphs and composes only coincident semicircle/outline pairs.
    #[test]
    fn symbol_glyph_names_preserve_visible_spaces_and_overlays() {
        let library = pdfium::Library::try_init().expect("PDFium");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pdf/symbol_glyph_names.pdf");
        let document = library
            .load_document(path.to_str().expect("path"), None)
            .expect("symbol fixture");
        let page = document.page(0).expect("page");
        let view = page.view_box().expect("view");
        let text = page.text().expect("text");
        let mut evidence = crate::TableEvidence::default();
        let items =
            extract_page_text_items(&page, &text, &view, 1, &mut evidence)
                .expect("native extraction");
        let visible: Vec<_> = items
            .iter()
            .map(|i| i.raw_text.trim())
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(
            visible,
            ["●", "○", "◐", "◐", "G # A B", "◖", "○", "•", "●", "○", "◖"]
        );
        let invalid = items
            .iter()
            .rfind(|i| i.raw_text.trim() == "●")
            .expect("null Unicode recovery");
        assert!(
            invalid
                .repair_actions
                .contains(&crate::RepairAction::GlyphNameRecovery)
        );
        assert!(
            invalid
                .provenance
                .as_ref()
                .expect("provenance")
                .char_codes
                .contains(&32)
        );
        let composed: Vec<_> =
            items.iter().filter(|i| i.raw_text.trim() == "◐").collect();
        assert_eq!(composed.len(), 2);
        for item in composed {
            let codes = &item
                .provenance
                .as_ref()
                .expect("native provenance")
                .char_codes;
            assert!(
                codes.contains(&71) && codes.contains(&35),
                "both original glyph codes remain inspectable"
            );
            let words = evidence.words.get(&item.id).expect("measured words");
            assert!(words.iter().any(|w| {
                item.raw_text.get(w.byte_range.clone()).map(str::trim)
                    == Some("◐")
            }));
            assert!(
                item.repair_actions
                    .contains(&crate::RepairAction::GlyphNameRecovery)
            );
        }
        assert!(
            items
                .iter()
                .find(|i| i.raw_text.trim() == "G # A B")
                .expect("ordinary text")
                .repair_actions
                .is_empty()
        );
    }
}
