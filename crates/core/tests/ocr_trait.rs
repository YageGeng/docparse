use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use docparse_core::{OcrEngine, OcrRequest, OcrResult, OcrTextItem};
use docparse_layout::{
    AffineTransform, Bbox, PageImage, PageImageInput, PageRotation,
    PageTransform, PageTransformInput, PixelFormat,
};

/// Deterministic fake proving external OCR engines can be injected as trait objects.
struct FakeOcr {
    calls: AtomicUsize,
}

impl OcrEngine for FakeOcr {
    /// Returns a stable engine name for result diagnostics.
    fn name(&self) -> &str {
        "fake-ocr"
    }

    /// Returns two deterministic facts without assigning DocParse identities.
    fn recognize(
        &self,
        request: OcrRequest,
    ) -> docparse_layout::wasm_compat::WasmBoxedFuture<
        '_,
        Result<OcrResult, docparse_core::OcrError>,
    > {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!(request.page_number, 1);
            Ok(OcrResult::builder()
                .items(vec![
                    OcrTextItem::builder()
                        .text("first".to_owned())
                        .bbox(
                            Bbox::try_from([10.0, 10.0, 40.0, 20.0])
                                .expect("valid bbox"),
                        )
                        .confidence(0.9)
                        .build(),
                    OcrTextItem::builder()
                        .text("second".to_owned())
                        .bbox(
                            Bbox::try_from([10.0, 30.0, 50.0, 40.0])
                                .expect("valid bbox"),
                        )
                        .confidence(0.8)
                        .build(),
                ])
                .metadata(BTreeMap::from([(
                    "engine".to_owned(),
                    "fake-ocr".to_owned(),
                )]))
                .build())
        })
    }
}

/// Builds one tiny RGB page image shared by layout and OCR requests.
fn page_image() -> Arc<PageImage> {
    Arc::new(
        PageImage::try_from(
            PageImageInput::builder()
                .width(2)
                .height(2)
                .pixel_format(PixelFormat::Rgb8)
                .data(Arc::<[u8]>::from(vec![255; 12]))
                .build(),
        )
        .expect("test image must be valid"),
    )
}

/// Builds one identity page transform for OCR request contract checks.
fn page_transform() -> PageTransform {
    PageTransform::try_from(
        PageTransformInput::builder()
            .page_to_viewport(AffineTransform::identity())
            .viewport_width(100.0)
            .viewport_height(100.0)
            .render_width(2)
            .render_height(2)
            .model_width(800)
            .model_height(800)
            .rotation(PageRotation::Degrees0)
            .build(),
    )
    .expect("test transform must be valid")
}

/// Verifies the asynchronous OCR boundary is object-safe and preserves metadata.
#[tokio::test]
async fn ocr_engine_is_injectable_as_arc_trait_object() {
    let engine: Arc<dyn OcrEngine> = Arc::new(FakeOcr {
        calls: AtomicUsize::new(0),
    });
    let request = OcrRequest::builder()
        .page_number(1)
        .image(page_image())
        .transform(page_transform())
        .dpi(144)
        .missing_regions(vec![
            Bbox::try_from([0.0, 0.0, 100.0, 100.0])
                .expect("valid missing region"),
        ])
        .native_text_coverage(0.0)
        .build();

    let result = engine
        .recognize(request)
        .await
        .expect("fake OCR must succeed");

    assert_eq!(engine.name(), "fake-ocr");
    assert_eq!(result.items.len(), 2);
    assert_eq!(
        result
            .items
            .first()
            .expect("first OCR item must exist")
            .text,
        "first"
    );
    assert_eq!(
        result.metadata.get("engine").map(String::as_str),
        Some("fake-ocr")
    );
}
