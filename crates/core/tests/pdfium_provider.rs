use std::{path::PathBuf, sync::Arc};

use docparse_config::{RenderConfig, RuntimeConfig};
use docparse_core::{LocalPdfiumProvider, PdfInput, PdfiumProvider};
use docparse_layout::timing::Timings;

/// The public provider must preserve owned text and raster data across its document session.
#[tokio::test]
async fn local_provider_preserves_document_lifecycle() {
    let bytes = Arc::<[u8]>::from(
        std::fs::read(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/pdf/extraction_metadata.pdf"),
        )
        .expect("fixture"),
    );
    let provider = LocalPdfiumProvider;
    let limits = RuntimeConfig::default();
    let session = provider
        .open(PdfInput::Bytes(bytes), &limits, Timings::default())
        .await
        .expect("open");
    assert_eq!(session.page_count(), 1);
    let page = session.pre_scan_page(1, None).await.expect("scan");
    assert!(!page.extracted.text_items.is_empty());
    let raster = session
        .render_page(1, &RenderConfig::default())
        .await
        .expect("render");
    assert_eq!(raster.page_number, 1);
    assert!(!raster.image.data().is_empty());
    session.close().await.expect("close");
    assert_eq!(page.extracted.page_number, 1);
}
