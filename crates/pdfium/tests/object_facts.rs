use docparse_pdfium::Library;

/// Builds a small PDF with the caller's marked text stream.
fn pdf(content: &str) -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream",content.len()),
    ];
    encoded(&objects)
}

/// Encodes a complete object list with correct cross-reference offsets.
fn encoded(objects: &[String]) -> Vec<u8> {
    let size = objects.len() + 1;
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(
            format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes(),
        );
    }
    let xref = bytes.len();
    bytes.extend_from_slice(
        format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes(),
    );
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
        )
        .as_bytes(),
    );
    bytes
}

/// Only an explicit watermark mark is authoritative; generic artifacts are not watermarks.
#[test]
fn pdfium_marks_and_rotated_bounds_are_read_without_heuristics() {
    let library = Library::init();
    for (mark, expected) in [
        ("/Watermark BMC", true),
        ("/Artifact << /Subtype (Watermark) >> BDC", true),
        ("/Artifact BMC", false),
        ("/Span BMC", false),
    ] {
        let bytes = pdf(&format!(
            "{mark} BT /F1 24 Tf .707107 .707107 -.707107 .707107 100 200 Tm (TEST) Tj ET EMC"
        ));
        let document = library
            .load_document_from_bytes(&bytes, None)
            .expect("PDF must open");
        let page = document.page(0).expect("page must open");
        let facts = page.text_object_facts();
        let fact = facts.first().expect("one text object");
        assert_eq!(fact.watermark, expected, "{mark}");
        let quad = fact.quad.expect("PDFium rotated bounds");
        assert!((quad[0].y - quad[1].y).abs() > 10.0);
    }
}

/// Reused form instances retain distinct identities, inherited marks, and composed coordinates.
#[test]
fn nested_form_marks_and_transforms_are_instance_local() {
    let text =
        "BT /F1 24 Tf .707107 .707107 -.707107 .707107 10 20 Tm (TEST) Tj ET";
    let content = "q /Watermark BMC 1 0 0 1 40 50 cm /Fm Do EMC Q q 2 0 0 2 200 300 cm /Fm Do Q";
    let bytes=encoded(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << /XObject << /Fm 6 0 R >> >> /Contents 5 0 R >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream",content.len()),
        format!("<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] /Matrix [1 0 0 1 7 11] /Resources << /Font << /F1 4 0 R >> >> /Length {} >>\nstream\n{text}\nendstream",text.len()),
    ]);
    let library = Library::init();
    let document = library
        .load_document_from_bytes(&bytes, None)
        .expect("form PDF");
    let page = document.page(0).expect("page");
    let facts = page.text_object_facts();
    assert_eq!(facts.len(), 2);
    let first = facts.first().expect("first form instance");
    let second = facts.get(1).expect("second form instance");
    assert!(first.watermark);
    assert!(!second.watermark);
    assert_ne!(first.identity, second.identity);
    for (left, right) in first
        .quad
        .expect("first quad")
        .into_iter()
        .zip(second.quad.expect("second quad"))
    {
        assert!((right.x - ((left.x - 40.0) * 2.0 + 200.0)).abs() < 0.001);
        assert!((right.y - ((left.y - 50.0) * 2.0 + 300.0)).abs() < 0.001);
    }
}

/// PDFium exposes an explicit Watermark annotation independently of its optional appearance text.
#[test]
fn watermark_annotation_subtype_is_read_from_pdfium() {
    let bytes=encoded(&[
        "<< /Type /Catalog /Pages 2 0 R >>".into(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << >> /Contents 5 0 R /Annots [6 0 R] >>".into(),
        "null".into(),
        "<< /Length 0 >>\nstream\n\nendstream".into(),
        "<< /Type /Annot /Subtype /Watermark /Rect [20 30 120 130] /Contents (Annotation note) >>".into(),
    ]);
    let library = Library::init();
    let document = library
        .load_document_from_bytes(&bytes, None)
        .expect("annotation PDF");
    let page = document.page(0).expect("page");
    let annotations = page.annotations(&page.view_box().expect("page box"));
    let annotation = annotations.first().expect("watermark annotation");
    assert_eq!(annotation.subtype, "watermark");
    assert_eq!(annotation.contents.as_deref(), Some("Annotation note"));
    let rect = annotation.rect.expect("annotation rectangle");
    assert!((rect.top - 670.0).abs() < 0.001);
}
