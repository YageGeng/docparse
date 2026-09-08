use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

use docparse_config::{RawConfig, ValidatedConfig};
use docparse_core::{
    DocParser, LabelSource, ParseObserver, ParseProgress, ResultValidator,
};
use docparse_layout::{
    Bbox, GeometrySource, LayoutDetection, LayoutEngine, LayoutError,
    LayoutLabel, LayoutRequest,
};

/// Deterministic fake regions vary by page while never reading model artifacts.
struct MultipageLayoutEngine;

impl LayoutEngine for MultipageLayoutEngine {
    /// Returns one stable fake name for document context.
    fn name(&self) -> &str {
        "multipage-layout-fake"
    }

    /// Returns one stable fake revision for deterministic context.
    fn model_revision(&self) -> &str {
        "multipage-layout-v1"
    }

    /// Covers both columns on page one, one section on page two, and none on page three.
    fn detect(
        &self,
        request: LayoutRequest,
    ) -> docparse_layout::wasm_compat::WasmBoxedFuture<
        '_,
        Result<Vec<LayoutDetection>, LayoutError>,
    > {
        Box::pin(async move {
            let regions = match request.page_number {
                1 => vec![
                    region(
                        0,
                        LayoutLabel::DocTitle,
                        6,
                        [45.0, 45.0, 570.0, 95.0],
                        0,
                    ),
                    region(
                        1,
                        LayoutLabel::Text,
                        22,
                        [45.0, 110.0, 570.0, 190.0],
                        1,
                    ),
                ],
                2 => vec![region(
                    0,
                    LayoutLabel::Text,
                    22,
                    [45.0, 110.0, 285.0, 190.0],
                    0,
                )],
                _ => Vec::new(),
            };
            Ok(regions)
        })
    }
}

/// Builds one bbox-only fake detection in canonical viewport points.
fn region(
    source_detection_index: u32,
    label: LayoutLabel,
    class_id: i64,
    bounds: [f64; 4],
    model_order: i64,
) -> LayoutDetection {
    let raw_label = serde_json::to_value(&label)
        .expect("label must serialize")
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    LayoutDetection::builder()
        .source_detection_index(source_detection_index)
        .raw_label(raw_label)
        .class_id(class_id)
        .label(label)
        .confidence(0.95)
        .bbox(Bbox::try_from(bounds).expect("fake bbox must be valid"))
        .polygon(None)
        .geometry_source(GeometrySource::DerivedFromBbox)
        .model_order(model_order)
        .metadata(BTreeMap::new())
        .build()
}

/// Resolves the tracked three-page synthetic fixture.
fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pdf/multipage_layout.pdf")
}

/// Builds validated fake-engine configuration at one page concurrency.
fn config(page_concurrency: usize) -> Arc<ValidatedConfig> {
    let mut raw = RawConfig::default();
    raw.layout.model_path = PathBuf::from("/tmp/multipage-missing.onnx");
    raw.layout.model_config_path = PathBuf::from("/tmp/multipage-missing.yml");
    raw.layout.model_manifest_path =
        PathBuf::from("/tmp/multipage-missing.json");
    raw.runtime.page_concurrency = page_concurrency;
    raw.runtime.render_queue_capacity = page_concurrency.min(2);
    raw.runtime.blocking_task_limit = page_concurrency;
    Arc::new(ValidatedConfig::try_from(raw).expect("fake config must validate"))
}

/// Builds one parser with a fresh fake engine and no default model access.
async fn parser(page_concurrency: usize) -> DocParser {
    DocParser::builder()
        .config(config(page_concurrency))
        .layout_engine(Arc::new(MultipageLayoutEngine))
        .build()
        .await
        .expect("fake parser must build")
}

/// Verifies three-page path/bytes and serial/concurrent outputs are canonical-identical.
#[tokio::test]
async fn multipage_pipeline_is_offline_and_concurrency_deterministic() {
    let serial = parser(1)
        .await
        .parse_path(fixture_path())
        .await
        .expect("serial path parse must succeed");
    let concurrent_parser = parser(3).await;
    let concurrent = concurrent_parser
        .parse_path(fixture_path())
        .await
        .expect("concurrent path parse must succeed");
    let bytes = Arc::<[u8]>::from(
        std::fs::read(fixture_path()).expect("fixture bytes must read"),
    );
    let from_bytes = concurrent_parser
        .parse_bytes(bytes)
        .await
        .expect("concurrent byte parse must succeed");

    assert_eq!(serial, concurrent);
    assert_eq!(concurrent, from_bytes);
    assert_eq!(serial.pages.len(), 3);
    assert_eq!(serial.context.page_count, 3);
    assert!(serial.pages.iter().any(|page| {
        page.blocks
            .iter()
            .any(|block| block.label_source == LabelSource::Model)
    }));
    assert!(serial.pages.iter().any(|page| {
        page.blocks
            .iter()
            .any(|block| block.label_source == LabelSource::Fallback)
    }));
    ResultValidator::validate(&serial).expect("multipage result must validate");
}

/// Records actual pipeline boundaries and validates borrowed PDFium image buffers.
#[derive(Default)]
struct RecordingObserver {
    progress: Mutex<Vec<ParseProgress>>,
    images: Mutex<Vec<u32>>,
}

impl ParseObserver for RecordingObserver {
    /// Retains ordered progress events without changing parser behavior.
    fn on_progress(&self, progress: ParseProgress) {
        self.progress.lock().expect("progress lock").push(progress);
    }

    /// Checks the image contract before the pipeline releases its page buffer.
    fn on_page_image(
        &self,
        page_number: u32,
        image: &docparse_layout::PageImage,
    ) {
        assert!(image.width() > 0 && image.height() > 0);
        assert_eq!(
            image.data().len(),
            image.width() as usize * image.height() as usize * 3
        );
        self.images.lock().expect("images lock").push(page_number);
    }
}

/// Observed parsing reports real completion counts and preserves the ordinary result.
#[tokio::test]
async fn progress_and_page_images_preserve_parse_results() {
    let parser = parser(3).await;
    let bytes = Arc::<[u8]>::from(
        std::fs::read(fixture_path()).expect("fixture bytes"),
    );
    let expected = parser
        .parse_bytes(Arc::clone(&bytes))
        .await
        .expect("plain parse");
    let observer = RecordingObserver::default();
    let actual = parser
        .parse_bytes_with_observer(bytes, &observer)
        .await
        .expect("observed parse");
    assert_eq!(actual, expected);
    assert_eq!(*observer.images.lock().expect("images lock"), [1, 2, 3]);
    let events = observer.progress.lock().expect("progress lock").clone();
    assert_eq!(events.first(), Some(&ParseProgress::Opening));
    assert_eq!(events.last(), Some(&ParseProgress::Complete { total: 3 }));
    for completed in 0..=3 {
        assert!(events.contains(&ParseProgress::Scanning {
            completed,
            total: 3
        }));
        assert!(events.contains(&ParseProgress::Analyzing {
            completed,
            total: 3
        }));
    }
    let failed = RecordingObserver::default();
    parser
        .parse_bytes_with_observer(Arc::from([1_u8, 2, 3]), &failed)
        .await
        .expect_err("invalid PDF must fail before completion");
    assert_eq!(
        *failed.progress.lock().expect("progress lock"),
        [ParseProgress::Opening]
    );
}
