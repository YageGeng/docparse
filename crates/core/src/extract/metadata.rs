use std::collections::BTreeMap;

use docparse_layout::Bbox;
use typed_builder::TypedBuilder;

use crate::ExtractedPage;

/// Lightweight page-level facts used to freeze document context before rendering.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub struct PageProbe {
    pub page_number: u32,
    pub width: f64,
    pub height: f64,
    pub rotation: i32,
    #[builder(default)]
    pub content_bounds: Option<Bbox>,
    #[builder(default)]
    pub font_size_histogram: BTreeMap<i32, u32>,
    #[builder(default)]
    pub top_fingerprints: Vec<String>,
    #[builder(default)]
    pub bottom_fingerprints: Vec<String>,
    #[builder(default)]
    pub page_number_candidates: Vec<String>,
    #[builder(default)]
    pub title_font_sizes: Vec<f64>,
}

impl From<&ExtractedPage> for PageProbe {
    /// Derives deterministic lightweight statistics without retaining page handles or pixels.
    fn from(page: &ExtractedPage) -> Self {
        let mut font_size_histogram = BTreeMap::new();
        let mut top_fingerprints = Vec::new();
        let mut bottom_fingerprints = Vec::new();
        let mut page_number_candidates = Vec::new();
        let mut title_font_sizes = Vec::new();
        for item in &page.text_items {
            if item.watermark.is_some() {
                continue;
            }
            let text = item.raw_text.trim().to_lowercase();
            if text.is_empty() {
                continue;
            }
            // Punctuation-only facts are common PDF drawing artifacts and cannot
            // identify meaningful repeated chrome across pages.
            if text.chars().any(char::is_alphanumeric) {
                if item.bbox.top <= page.height * 0.15 {
                    top_fingerprints.push(text.clone());
                }
                if item.bbox.bottom >= page.height * 0.85 {
                    bottom_fingerprints.push(text.clone());
                }
            }
            if text.bytes().all(|byte| byte.is_ascii_digit()) {
                page_number_candidates.push(text);
            }
            if let Some(font_size) = item
                .style
                .as_ref()
                .and_then(|style| style.font_size)
                .filter(|font_size| font_size.is_finite() && *font_size > 0.0)
            {
                let bucket = (font_size * 10.0).round() as i32;
                *font_size_histogram.entry(bucket).or_insert(0) += 1;
                if font_size >= 12.0 {
                    title_font_sizes.push(font_size);
                }
            }
        }
        top_fingerprints.sort();
        top_fingerprints.dedup();
        bottom_fingerprints.sort();
        bottom_fingerprints.dedup();
        page_number_candidates.sort();
        page_number_candidates.dedup();
        title_font_sizes.sort_by(f64::total_cmp);
        title_font_sizes
            .dedup_by(|left, right| (*left - *right).abs() <= f64::EPSILON);
        Self::builder()
            .page_number(page.page_number)
            .width(page.width)
            .height(page.height)
            .rotation(page.rotation)
            .content_bounds(page.content_bounds)
            .font_size_histogram(font_size_histogram)
            .top_fingerprints(top_fingerprints)
            .bottom_fingerprints(bottom_fingerprints)
            .page_number_candidates(page_number_candidates)
            .title_font_sizes(title_font_sizes)
            .build()
    }
}

#[cfg(test)]
mod tests {
    use docparse_layout::Bbox;

    use super::PageProbe;
    use crate::extract::text::{SegmentBuilder, TextCharFact};
    use crate::{ExtractedPage, TextItem, UnicodeMappingStatus};

    /// Creates one richly annotated character fact.
    fn fact(character: char, x: f64, font_size: f64) -> TextCharFact {
        let bbox = Bbox::try_from([x, 10.0, x + 5.0, 20.0])
            .expect("the metadata test bbox must be valid");
        TextCharFact::builder()
            .character(character)
            .bbox(bbox)
            .loose_bbox(bbox)
            .font_name(Some("FixtureFont".to_owned()))
            .font_size(font_size)
            .font_height(Some(font_size * 1.1))
            .font_ascent(Some(font_size * 0.8))
            .font_descent(Some(-font_size * 0.2))
            .font_weight(Some(700))
            .font_flags(Some(32))
            .text_matrix(Some([1.0, 0.0, 0.0, 1.0, x, 10.0]))
            .fill_color(Some([10, 20, 30, 255]))
            .stroke_color(Some([40, 50, 60, 255]))
            .char_code(u32::from(character))
            .mcid(Some(7))
            .text_object_index(Some(3))
            .link(Some("https://example.com".to_owned()))
            .strike(true)
            .build()
    }

    /// Asserts two optional floating-point metadata values within a stable tolerance.
    fn assert_optional_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("the metadata value must exist");
        assert!(
            (actual - expected).abs() <= 1.0e-9,
            "expected {expected}, got {actual}"
        );
    }

    /// Verifies item metadata is aggregated without leaking temporary identities.
    #[test]
    fn metadata_aggregation_preserves_stable_facts() {
        let mut builder = SegmentBuilder::new(1);
        builder.push(fact('A', 0.0, 10.0)).expect("fact must push");
        builder.push(fact('B', 5.0, 14.0)).expect("fact must push");
        let draft = builder.finish().expect("segment must finish").remove(0);
        let item = TextItem::try_from(draft).expect("draft must convert");
        let style = item.style.expect("style metadata must exist");
        let provenance = item.provenance.expect("PDF provenance must exist");

        assert_eq!(style.font_name.as_deref(), Some("FixtureFont"));
        assert_optional_close(style.font_size, 12.0);
        assert_optional_close(style.font_height, 13.2);
        assert_optional_close(style.font_ascent, 9.6);
        assert_optional_close(style.font_descent, -2.4);
        assert_eq!(style.weight, Some(700));
        assert_eq!(style.flags, Some(32));
        assert_eq!(style.fill_color, Some([10, 20, 30, 255]));
        assert_eq!(style.stroke_color, Some([40, 50, 60, 255]));
        assert_eq!(provenance.char_codes, vec![65, 66]);
        assert_eq!(provenance.mcid, Some(7));
        assert_eq!(provenance.text_object_index, Some(3));
        assert_eq!(provenance.unicode_mapping, UnicodeMappingStatus::Complete);
        assert_eq!(provenance.link.as_deref(), Some("https://example.com"));
        assert!(provenance.strike);
    }

    /// Verifies PageProbe contains only lightweight deterministic document signals.
    #[test]
    fn extracted_page_converts_to_lightweight_probe() {
        let mut builder = SegmentBuilder::new(2);
        builder.push(fact('A', 0.0, 10.0)).expect("fact must push");
        let item = TextItem::try_from(
            builder.finish().expect("segment must finish").remove(0),
        )
        .expect("draft must convert");
        let punctuation = TextItem::builder()
            .id(crate::TextItemId::native(2, 1))
            .raw_text("-".to_owned())
            .bbox(
                Bbox::try_from([0.0, 700.0, 5.0, 710.0])
                    .expect("punctuation bbox must be valid"),
            )
            .source(crate::TextSource::Native)
            .build();
        let page = ExtractedPage::builder()
            .page_number(2)
            .width(612.0)
            .height(792.0)
            .rotation(0)
            .text_items(vec![item, punctuation])
            .build();

        let probe = PageProbe::from(&page);

        assert_eq!(probe.page_number, 2);
        assert_eq!(probe.font_size_histogram.get(&100), Some(&1));
        assert_eq!(probe.top_fingerprints, vec!["a"]);
        assert!(probe.bottom_fingerprints.is_empty());
    }
}
