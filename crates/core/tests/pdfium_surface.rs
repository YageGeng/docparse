use std::fs;
use std::path::{Path, PathBuf};

use pdfium::Library;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use typed_builder::TypedBuilder;

#[derive(Debug, Deserialize, TypedBuilder)]
struct ExpectedFixture {
    sha256: String,
    page_width: f32,
    page_height: f32,
    rotation: i32,
    title_font_weight: i32,
    required_text: Vec<String>,
    uri: String,
    mcid: i32,
}

/// Resolves a fixture path from the core crate root.
fn fixture_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pdf")
        .join(name)
}

/// Loads the hand-reviewed deterministic fixture expectations.
fn expectations() -> ExpectedFixture {
    serde_json::from_slice(
        &fs::read(fixture_path("extraction_metadata.expected.json"))
            .expect("the expected fixture JSON must be readable"),
    )
    .expect("the expected fixture JSON must deserialize")
}

/// Computes the fixture SHA-256 independently of PDFium.
fn fixture_sha256() -> String {
    let bytes = fs::read(fixture_path("extraction_metadata.pdf"))
        .expect("the PDF fixture must be readable");
    lowercase_hex(Sha256::digest(bytes).as_ref())
}

/// Encodes digest bytes without relying on removed `LowerHex` implementations.
fn lowercase_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            let digit = match nibble {
                0..=9 => b'0' + nibble,
                _ => b'a' + (nibble - 10),
            };
            encoded.push(char::from(digit));
        }
    }
    encoded
}

/// Verifies page facts, text content, links, and deterministic fixture bytes.
#[test]
fn synthetic_pdf_exposes_expected_page_surface() {
    let expected = expectations();
    assert_eq!(fixture_sha256(), expected.sha256);
    let library = Library::init();
    let document = library
        .load_document(
            fixture_path("extraction_metadata.pdf")
                .to_str()
                .expect("fixture path must be UTF-8"),
            None,
        )
        .expect("the PDF fixture must open");
    let page = document.page(0).expect("the fixture page must open");
    let view_box = page.view_box().expect("the fixture must have a view box");
    let text_page = page.text().expect("fixture text must load");
    let text = text_page.get_text(0, text_page.char_count());

    assert!((page.width() - expected.page_width).abs() < f32::EPSILON);
    assert!((page.height() - expected.page_height).abs() < f32::EPSILON);
    assert_eq!(page.rotation(), expected.rotation);
    for required in expected.required_text {
        assert!(text.contains(&required), "missing fixture text: {required}");
    }
    let links = page.links(&view_box);
    assert!(links.iter().any(|link| link.uri == expected.uri));
}

/// Verifies character metadata and safe temporary object identities remain available.
#[test]
fn text_char_exposes_rich_metadata_without_raw_handle_mapping() {
    let expected = expectations();
    let library = Library::init();
    let document = library
        .load_document(
            fixture_path("extraction_metadata.pdf")
                .to_str()
                .expect("fixture path must be UTF-8"),
            None,
        )
        .expect("the PDF fixture must open");
    let page = document.page(0).expect("the fixture page must open");
    let text_page = page.text().expect("fixture text must load");
    let object_identities = page.text_object_identities();

    let first = text_page
        .chars()
        .find(|character| character.unicode() == u32::from('D'))
        .expect("the title must contain D");
    assert_eq!(first.char_code(), u32::from('D'));
    assert!(first.font_size() > 0.0);
    assert_eq!(first.font_weight(), expected.title_font_weight);
    assert!(first.font_info().is_some());
    assert!(first.char_box().is_some());
    assert!(first.origin().is_some_and(|origin| {
        origin.x.is_finite() && origin.y.is_finite()
    }));
    assert!(first.matrix().is_some());
    assert!(first.fill_color().is_some());
    let identity = first
        .text_object_identity()
        .expect("the title character must have a text object");
    assert!(object_identities.contains(&identity));
    let font = first.font().expect("the title character must have a font");
    assert!(font.base_name().is_some());
    assert!(font.ascent(first.font_size() as f32).is_some());
    assert!(font.descent(first.font_size() as f32).is_some());

    let marked = text_page
        .chars()
        .find(|character| character.marked_content_id() == Some(expected.mcid))
        .expect("the fixture must contain MCID text");
    assert_eq!(marked.marked_content_id(), Some(expected.mcid));
    assert!(marked.text_render_mode().is_some());
    assert!(marked.stroke_color().is_some());

    let rotated = text_page
        .chars()
        .find(|character| character.angle().abs() > 1.0)
        .expect("the fixture must contain rotated text");
    assert!(rotated.matrix().is_some());

    assert!(text_page.chars().any(|character| character.is_generated()));
}
