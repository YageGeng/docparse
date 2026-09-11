//! Page-scoped glyph recovery, retaining DocParse's geometry-dependent circle rules.
// SPDX-License-Identifier: Apache-2.0
// Recovery order is adapted from LiteParse 4d4a51c246ff56d382166930942898f3ff563eba.

use std::cell::OnceCell;
use std::collections::{HashMap, HashSet};

use docparse_layout::Bbox;
use pdfium::{Font, FontType, TextChar, TextPage};
use typed_builder::TypedBuilder;

use super::{font_cmap::reverse_cmap, glyph_names::resolve_glyph_name};
use crate::text_rules::{
    ligature_expansion, normalize_punctuation, suspicious_codepoint,
};
use crate::{GlyphResolver, RepairAction};

/// One source glyph; all text transformations and their evidence are finalized before segmentation.
#[derive(TypedBuilder)]
pub(super) struct ResolvedGlyph<'a> {
    pub character: char,
    #[builder(default)]
    pub tail: String,
    #[builder(default)]
    pub repair_actions: Vec<RepairAction>,
    #[builder(default)]
    pub font: Option<Font<'a>>,
}

impl ResolvedGlyph<'_> {
    /// Normalizes native and recovered text once, retaining one source glyph and every applied repair.
    pub(super) fn normalize(mut self) -> Self {
        match self.character {
            '\u{0001}' => self.character = ' ',
            '\u{0002}' => {
                self.character = '-';
                self.repair_actions.push(RepairAction::EncodedHyphen);
            }
            _ => {}
        }
        if let Some(expansion) = ligature_expansion(self.character) {
            let mut chars = expansion.chars();
            if let Some(first) = chars.next() {
                self.character = first;
                self.tail.insert_str(0, chars.as_str());
            }
        }
        // Any multi-character decoding expands one source glyph, regardless of the recovery provider.
        if !self.tail.is_empty() {
            self.repair_actions.push(RepairAction::LigatureExpansion);
        }
        let normalized = normalize_punctuation(self.character);
        let punctuation_changed = normalized != self.character
            || self.tail.chars().any(|c| normalize_punctuation(c) != c);
        self.character = normalized;
        if punctuation_changed {
            self.repair_actions
                .push(RepairAction::PunctuationNormalization);
        }
        if self.tail.chars().any(|c| {
            ligature_expansion(c).is_some() || normalize_punctuation(c) != c
        }) {
            let mut tail = String::with_capacity(self.tail.len());
            for c in self.tail.chars() {
                match ligature_expansion(c) {
                    Some(expansion) => tail.push_str(expansion),
                    None => tail.push(normalize_punctuation(c)),
                }
            }
            self.tail = tail;
        }
        if self.character.is_control() && !matches!(self.character, '\n' | '\r')
        {
            self.repair_actions.push(RepairAction::RemovedControl);
        }
        self
    }
}

/// Raw font lookup results, each initialized only when that recovery stage is needed.
#[derive(Default)]
pub(super) struct GlyphCandidates {
    name: OnceCell<Option<String>>,
    cmap: OnceCell<Option<String>>,
    outline: OnceCell<Option<String>>,
}

/// Font-level trust and raw lookup caches; occurrence-specific decisions never enter cached values.
#[derive(TypedBuilder)]
pub(super) struct FontGlyphInfo {
    untrusted: bool,
    encoding_lies: bool,
    #[builder(default)]
    candidates: HashMap<u32, GlyphCandidates>,
    #[builder(default)]
    reverse_cmap: OnceCell<Option<HashMap<u32, u32>>>,
}

impl From<&Font<'_>> for FontGlyphInfo {
    /// Detects custom mappings and embedded subset fonts whose declared encoding is unreliable.
    fn from(font: &Font<'_>) -> Self {
        let encoding_lies = font.is_embedded()
            && font.base_name().is_some_and(|name| {
                name.starts_with("TT")
                    || name.contains("+TT")
                    || (font.font_type() == FontType::Type1
                        && name.as_bytes().get(6) == Some(&b'_'))
            });
        let standard = matches!(
            font.encoding().as_deref(),
            Some(
                "WinAnsiEncoding"
                    | "MacRomanEncoding"
                    | "MacExpertEncoding"
                    | "StandardEncoding"
            )
        );
        Self::builder()
            .untrusted(encoding_lies || (!font.has_to_unicode() && !standard))
            .encoding_lies(encoding_lies)
            .build()
    }
}

impl FontGlyphInfo {
    /// Chooses a raw candidate for this occurrence while caching only font/code-dependent lookups.
    fn decode(
        &mut self,
        character: &TextChar<'_>,
        font: &Font<'_>,
        resolver: Option<&dyn GlyphResolver>,
    ) -> Option<(&str, RepairAction)> {
        let unicode = character.unicode();
        let original = char::from_u32(unicode);
        let recover = self.untrusted
            || suspicious_codepoint(unicode)
            || character.has_unicode_map_error();
        let symbol_eligible = original.is_none_or(|c| {
            c.is_ascii() || matches!(c, '\u{FFFD}'..='\u{FFFF}')
        }) && (!original
            .is_some_and(char::is_whitespace)
            || character
                .char_box()
                .is_some_and(|b| b.right > b.left && b.top > b.bottom));
        if !recover && !symbol_eligible {
            return None;
        }
        let code = character.char_code();
        let candidates = self.candidates.entry(code).or_default();
        let valid = |text: &str| {
            !text.is_empty()
                && text
                    .chars()
                    .all(|c| !c.is_control() && !suspicious_codepoint(c as u32))
        };
        if !self.encoding_lies {
            let named = candidates.name.get_or_init(|| {
                font.char_glyph_name(code)
                    .as_deref()
                    .and_then(resolve_glyph_name)
            });
            if let Some(text) = named.as_deref().filter(|text| valid(text)) {
                let mut chars = text.chars();
                let symbol = chars.next().is_some_and(|c| {
                    !c.is_ascii() && !c.is_alphanumeric() && !c.is_whitespace()
                }) && chars.next().is_none();
                // The current Unicode and painted-space test must be reevaluated even after a cache hit.
                if recover || (symbol_eligible && symbol) {
                    return Some((text, RepairAction::GlyphNameRecovery));
                }
            }
        }
        if !recover {
            return None;
        }
        let mapped = candidates.cmap.get_or_init(|| {
            let glyph = font.char_glyph_index(code)?;
            let map = self
                .reverse_cmap
                .get_or_init(|| {
                    font.font_data().as_deref().and_then(reverse_cmap)
                })
                .as_ref()?;
            char::from_u32(*map.get(&glyph)?).map(|c| c.to_string())
        });
        if let Some(text) = mapped.as_deref().filter(|text| valid(text))
            && text
                .chars()
                .next()
                .is_some_and(|c| c as u32 != code || c as u32 == unicode)
        {
            // Identity-map rejection depends on this occurrence, not on whichever occurrence filled the cache.
            return Some((text, RepairAction::FontCmapRecovery));
        }
        let resolver = resolver?;
        candidates
            .outline
            .get_or_init(|| {
                let segments = font.glyph_path_segments(
                    code,
                    crate::GLYPH_RESOLVER_FONT_SIZE,
                )?;
                resolver.resolve(&segments)
            })
            .as_deref()
            .filter(|text| valid(text))
            .map(|text| (text, RepairAction::GlyphOutlineRecovery))
    }
}

/// Page-scoped font caches and a borrowed optional outline resolver.
#[derive(TypedBuilder)]
pub(super) struct GlyphNormalizer<'r> {
    #[builder(default)]
    fonts: HashMap<usize, FontGlyphInfo>,
    #[builder(default)]
    garbage_fonts: HashSet<usize>,
    #[builder(default)]
    resolver: Option<&'r dyn GlyphResolver>,
    #[builder(default)]
    pub recovered_count: usize,
}

impl<'r> GlyphNormalizer<'r> {
    /// Flags fonts with at least twenty sampled glyphs and ten percent suspicious mappings.
    pub fn new(
        text: &TextPage<'_, '_>,
        resolver: Option<&'r dyn GlyphResolver>,
    ) -> Self {
        // Ordinary pages avoid a second full pass of text-object/font FFI lookups.
        if !text.chars().any(|c| suspicious_codepoint(c.unicode())) {
            return Self::builder().resolver(resolver).build();
        }
        let mut counts: HashMap<usize, (u32, u32)> = HashMap::new();
        for character in text.chars() {
            let value = character.unicode();
            if character.is_generated()
                || matches!(value, 0x09 | 0x0A | 0x0D | 0x20)
            {
                continue;
            }
            if let Some(font) = character.font() {
                let (total, suspicious) =
                    counts.entry(font.handle() as usize).or_default();
                *total += 1;
                *suspicious += u32::from(suspicious_codepoint(value));
            }
        }
        let garbage_fonts = counts
            .into_iter()
            .filter(|(_, (total, suspicious))| {
                *total >= 20 && u64::from(*suspicious) * 10 >= u64::from(*total)
            })
            .map(|(key, _)| key)
            .collect();
        Self::builder()
            .garbage_fonts(garbage_fonts)
            .resolver(resolver)
            .build()
    }

    /// Resolves a glyph before dropping invalid fallback scalars or segmenting its decoded text.
    pub fn resolve<'a>(
        &mut self,
        character: &'a TextChar<'_>,
        page: &TextPage<'_, '_>,
        index: i32,
    ) -> Option<ResolvedGlyph<'a>> {
        let original = char::from_u32(character.unicode())
            .filter(|c| !matches!(c, '\0' | '\u{FFFE}' | '\u{FFFF}'));
        if character.is_generated() {
            return Some(
                ResolvedGlyph::builder()
                    .character(original?)
                    .build()
                    .normalize(),
            );
        }
        let font = character.font();
        let decoded = font.as_ref().and_then(|font| {
            let key = font.handle() as usize;
            let info = self.fonts.entry(key).or_insert_with(|| {
                let mut info = FontGlyphInfo::from(font);
                info.untrusted |= self.garbage_fonts.contains(&key);
                info
            });
            info.decode(character, font, self.resolver)
        });
        // Confirmation is an occurrence decision too; a cached lookup is not itself proof of a repair.
        let decoded = decoded.filter(|(text, _)| {
            character.has_unicode_map_error()
                || !original.is_some_and(|c| {
                    let mut chars = text.chars();
                    chars.next() == Some(c) && chars.next().is_none()
                })
        });
        let mut glyph = if let Some((text, action)) = decoded {
            let mut chars = text.chars();
            ResolvedGlyph::builder()
                .character(chars.next()?)
                .tail(chars.as_str().to_owned())
                .repair_actions(vec![action])
                .build()
                .normalize()
        } else {
            ResolvedGlyph::builder()
                .character(original?)
                .build()
                .normalize()
        };
        // Compare normalized punctuation during recognition, but require multiple actual source records.
        if !glyph.tail.is_empty()
            && Self::already_expanded(
                page,
                index,
                &format!("{}{}", glyph.character, glyph.tail),
            )
        {
            glyph = ResolvedGlyph::builder()
                .character(original?)
                .build()
                .normalize();
        }
        self.recovered_count +=
            usize::from(glyph.repair_actions.iter().any(|action| {
                matches!(
                    action,
                    RepairAction::GlyphNameRecovery
                        | RepairAction::FontCmapRecovery
                        | RepairAction::GlyphOutlineRecovery
                )
            }));
        glyph.font = font;
        Some(glyph)
    }

    /// Confirms a complete decoded sequence around this index with one source code and text object.
    fn already_expanded(
        page: &TextPage<'_, '_>,
        index: i32,
        decoded: &str,
    ) -> bool {
        let Ok(count) = i32::try_from(decoded.chars().count()) else {
            return false;
        };
        if count < 2 {
            return false;
        }
        let Some(current) = page.char_at(index) else {
            return false;
        };
        let Some(object) = current.text_object_identity() else {
            return false;
        };
        let code = current.char_code();
        let mut around = String::new();
        let mut position = 0;
        // A window of at most twice the expansion length keeps recognition linear in the decoded text.
        for offset in index.saturating_sub(count - 1).max(0)
            ..index.saturating_add(count).min(page.char_count())
        {
            let character = page.char_at_unchecked(offset);
            if offset == index {
                position = around.len();
            }
            let value = if !character.is_generated()
                && character.char_code() == code
                && character.text_object_identity() == Some(object)
            {
                char::from_u32(character.unicode())
                    .map(normalize_punctuation)
                    .unwrap_or('\0')
            } else {
                '\0'
            };
            around.push(value);
        }
        around.match_indices(decoded).any(|(start, matched)| {
            start <= position && position < start + matched.len()
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
    // Share PDF serialization while keeping every test helper inside the test-only module.
    mod pdf {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/common/pdf.rs"));
    }
    use super::super::text::extract_page_text_items;

    /// Creates a real Type3 PDF with explicit names and either missing or deliberately wrong mappings.
    fn named_pdf(names: &[&str], unicode: Option<&[&str]>) -> Vec<u8> {
        let names_dictionary = names
            .iter()
            .map(|name| format!("/{name} 6 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        let differences = names
            .iter()
            .map(|name| format!("/{name}"))
            .collect::<Vec<_>>()
            .join(" ");
        let widths = vec!["600"; names.len()].join(" ");
        let hex = (1..=names.len())
            .map(|code| format!("{code:02X}"))
            .collect::<String>();
        let content = format!("BT /F1 12 Tf 20 60 Td <{hex}> Tj ET");
        let glyph = "600 0 0 0 600 600 d1 30 30 500 500 re f";
        let mapping = unicode.map(|values| {
            let pairs = values.iter().enumerate().map(|(i, value)| format!("<{:02X}> <{value}>", i + 1)).collect::<Vec<_>>().join("\n");
            format!("/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def /CMapName /Test def /CMapType 2 def 1 begincodespacerange <00> <FF> endcodespacerange {} beginbfchar {pairs} endbfchar endcmap CMapName currentdict /CMap defineresource pop end end", values.len())
        });
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 100] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
            format!("<< /Type /Font /Subtype /Type3 /Name /Recovery /FontBBox [0 0 600 600] /FontMatrix [0.001 0 0 0.001 0 0] /CharProcs << {names_dictionary} >> /Encoding << /Type /Encoding /Differences [1 {differences}] >> /FirstChar 1 /LastChar {} /Widths [{widths}] /Resources << >> {} >>", names.len(), if mapping.is_some() { "/ToUnicode 7 0 R" } else { "" }),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            format!("<< /Length {} >>\nstream\n{glyph}\nendstream", glyph.len()),
        ];
        if let Some(mapping) = mapping {
            objects.push(format!(
                "<< /Length {} >>\nstream\n{mapping}\nendstream",
                mapping.len()
            ));
        }
        pdf::document(&objects)
    }

    /// Checks deterministic recovery, PDFium ligature deduplication, font-wide distrust and outline fallback on real PDFs.
    #[test]
    fn named_font_recovery_preserves_source_ranges() {
        struct Outline;
        impl crate::GlyphResolver for Outline {
            /// Supplies a recognizable fallback only when a real glyph outline is present.
            fn resolve(&self, segments: &[(i32, f32, f32)]) -> Option<String> {
                (!segments.is_empty()).then(|| "✓".to_owned())
            }
        }
        let library = pdfium::Library::try_init().expect("PDFium");
        let cases = [
            (
                vec!["Aacute", "uni03A9", "f_f_i", "u1F600", "unknown"],
                Some(vec!["0000"; 5]),
                "ÁΩffi😀",
            ),
            (vec!["fi", "n", "d"], None, "find"),
            // A bad CMap can truncate every ligature to a plausible first letter; this is not PDFium expansion.
            (
                vec!["fi"; 20],
                Some(
                    (0..20)
                        .map(|index| if index < 2 { "0000" } else { "0066" })
                        .collect(),
                ),
                "fifififififififififififififififififififi",
            ),
            (vec!["bullet"], Some(vec!["0041"]), "•"), // General symbol recovery also covers names beyond circles.
            (vec!["Aacute"], Some(vec!["03B1"]), "α"), // Meaningful Unicode must win over a different glyph name.
            (
                vec!["Aacute"; 20],
                Some(
                    (0..20)
                        .map(|index| if index < 2 { "0000" } else { "0041" })
                        .collect(),
                ),
                "ÁÁÁÁÁÁÁÁÁÁÁÁÁÁÁÁÁÁÁÁ",
            ),
        ];
        for (names, unicode, expected) in cases {
            let bytes = named_pdf(&names, unicode.as_deref());
            let document =
                library.load_document_from_bytes(&bytes, None).expect("PDF");
            let page = document.page(0).expect("page");
            let text = page.text().expect("text");
            let mut evidence = crate::TableEvidence::default();
            let items = extract_page_text_items(
                &page,
                &text,
                &page.view_box().expect("view"),
                1,
                &mut evidence,
                Some(&Outline),
            )
            .expect("extraction");
            assert_eq!(
                items
                    .iter()
                    .map(|item| item.raw_text.as_str())
                    .collect::<String>()
                    .trim(),
                expected
            );
            for item in &items {
                let words = evidence.words.get(&item.id).expect("source words");
                assert!(words.iter().all(|word| {
                    item.raw_text.get(word.byte_range.clone()).is_some()
                }));
                assert_eq!(
                    item.provenance.as_ref().expect("source").unicode_mapping,
                    crate::UnicodeMappingStatus::Complete
                );
            }
        }
    }

    /// Recovered ligatures record expansion while PDFium's existing expansion keeps its original source records.
    #[test]
    fn normalization_records_expansion_for_every_source() {
        let library = pdfium::Library::try_init().expect("PDFium");
        for mapping in ["0000", "FB01"] {
            let bytes = named_pdf(&["uniFB01"], Some(&[mapping]));
            let document =
                library.load_document_from_bytes(&bytes, None).expect("PDF");
            let page = document.page(0).expect("page");
            let text = page.text().expect("text");
            let mut evidence = crate::TableEvidence::default();
            let items = extract_page_text_items(
                &page,
                &text,
                &page.view_box().expect("view"),
                1,
                &mut evidence,
                None,
            )
            .expect("extraction");
            let item = items.first().expect("ligature");
            assert_eq!(item.raw_text, "fi");
            let codes = &item.provenance.as_ref().expect("source").char_codes;
            if mapping == "0000" {
                assert!(
                    item.repair_actions
                        .contains(&crate::RepairAction::LigatureExpansion)
                );
                assert_eq!(codes, &[1]);
            } else {
                // PDFium itself expands a valid FB01 mapping to two records before DocParse sees it.
                assert!(item.repair_actions.is_empty());
                assert_eq!(codes, &[1, 1]);
            }
            assert_eq!(
                evidence
                    .words
                    .get(&item.id)
                    .expect("words")
                    .first()
                    .expect("word")
                    .byte_range,
                0..2
            );
        }
    }

    /// Every source uses one normalization policy, including ligatures after the first decoded character.
    #[test]
    fn normalization_is_independent_of_recovery_source() {
        use crate::RepairAction;
        for repair in [
            None,
            Some(RepairAction::GlyphNameRecovery),
            Some(RepairAction::FontCmapRecovery),
            Some(RepairAction::GlyphOutlineRecovery),
        ] {
            for (character, tail, expected) in
                [('ﬁ', "", "fi"), ('f', "i", "fi"), ('x', "ﬃ—", "xffi-")]
            {
                let glyph = super::ResolvedGlyph::builder()
                    .character(character)
                    .tail(tail.to_owned())
                    .repair_actions(repair.clone().into_iter().collect())
                    .build()
                    .normalize();
                assert_eq!(
                    format!("{}{}", glyph.character, glyph.tail),
                    expected
                );
                assert_eq!(
                    glyph
                        .repair_actions
                        .iter()
                        .filter(|action| **action
                            == RepairAction::LigatureExpansion)
                        .count(),
                    1
                );
                if let Some(action) = &repair {
                    assert!(glyph.repair_actions.contains(action));
                }
                assert_eq!(
                    glyph
                        .repair_actions
                        .contains(&RepairAction::PunctuationNormalization),
                    expected.ends_with('-')
                );
            }
        }
    }

    /// A valid and a suspicious Unicode record sharing one source code cannot poison each other's cache decisions.
    #[test]
    fn font_cache_does_not_cache_occurrence_policy() {
        let library = pdfium::Library::try_init().expect("PDFium");
        let bytes = named_pdf(&["fi"], Some(&["0066FFFD"]));
        let document =
            library.load_document_from_bytes(&bytes, None).expect("PDF");
        let page = document.page(0).expect("page");
        let text = page.text().expect("text");
        let records: Vec<_> =
            text.chars().filter(|c| !c.is_generated()).collect();
        assert_eq!(
            records.iter().map(|c| c.unicode()).collect::<Vec<_>>(),
            [0x66, 0xFFFD]
        );
        let font = records.first().expect("record").font().expect("font");
        for order in [[0, 1], [1, 0]] {
            let mut info = super::FontGlyphInfo::from(&font);
            for index in order {
                let character = records.get(index).expect("record");
                let decoded = info.decode(character, &font, None);
                if index == 0 {
                    assert_eq!(
                        decoded, None,
                        "the valid mapping must remain authoritative"
                    );
                } else {
                    assert_eq!(
                        decoded,
                        Some(("ﬁ", crate::RepairAction::GlyphNameRecovery)),
                        "a cached miss must not disable later recovery"
                    );
                }
            }
        }
    }

    /// A real embedded outline remains usable when both font names and cmap are known to be untrustworthy.
    #[test]
    fn outline_fallback_uses_real_font_paths() {
        struct Outline;
        impl crate::GlyphResolver for Outline {
            /// Recognizes the supplied nonempty vector path without depending on PDFium handles.
            fn resolve(&self, segments: &[(i32, f32, f32)]) -> Option<String> {
                (!segments.is_empty()).then(|| "✓".to_owned())
            }
        }
        let library = pdfium::Library::try_init().expect("PDFium");
        let bytes =
            include_bytes!("../../tests/fixtures/pdf/embedded_layout.pdf");
        let document =
            library.load_document_from_bytes(bytes, None).expect("PDF");
        let page = document.page(0).expect("page");
        let text = page.text().expect("text");
        let character = (0..text.char_count())
            .filter_map(|index| text.char_at(index))
            .find(|character| {
                !character.is_generated()
                    && character.font().is_some_and(|font| {
                        font.glyph_path_segments(
                            character.char_code(),
                            crate::GLYPH_RESOLVER_FONT_SIZE,
                        )
                        .is_some_and(|segments| !segments.is_empty())
                    })
            })
            .expect("embedded outline");
        let font = character.font().expect("font");
        let mut info = super::FontGlyphInfo::builder()
            .untrusted(true)
            .encoding_lies(true)
            .reverse_cmap(std::cell::OnceCell::from(None))
            .build();
        assert_eq!(
            info.decode(&character, &font, Some(&Outline)),
            Some(("✓", crate::RepairAction::GlyphOutlineRecovery))
        );
    }

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
        let items = extract_page_text_items(
            &page,
            &text,
            &view,
            1,
            &mut evidence,
            None,
        )
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
