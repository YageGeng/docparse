use docparse_common::timing::Timings;
use docparse_config::{RenderConfig, RuntimeConfig};
use docparse_core::pdfium_ipc::Raster;
use docparse_core::{
    LocalPdfiumProvider, PdfInput, PdfiumProvider, RenderedPage,
};
use ipc_channel::ipc::IpcSharedMemory;

/// IPC raster reconstruction must preserve geometry and reject a short shared pixel buffer.
#[tokio::test]
async fn raster_roundtrip_validates_shared_pixel_length() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pdf/extraction_metadata.pdf");
    let local = LocalPdfiumProvider;
    let limits = RuntimeConfig::default();
    let session = local
        .open(PdfInput::Path(path), &limits, Timings::default())
        .await
        .expect("open");
    let raster = session
        .render_page(1, &RenderConfig::default())
        .await
        .expect("render");
    let encoded = Raster::from(raster.clone());
    let decoded = RenderedPage::try_from(encoded).expect("valid raster");
    assert_eq!(decoded.transform, raster.transform);
    assert_eq!(decoded.image.data(), raster.image.data());
    // The builder supplies an empty image blob for this invalid raster.
    let invalid = Raster::builder()
        .page_number(1)
        .transform(raster.transform)
        .pixels(IpcSharedMemory::from_bytes(&[0]))
        .image_bytes(IpcSharedMemory::from_bytes(&[]))
        .build();
    RenderedPage::try_from(invalid)
        .expect_err("short shared raster must be rejected");
    session.close().await.expect("close");
}
