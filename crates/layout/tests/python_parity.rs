use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use docparse_config::{ConfigLoader, ValidatedConfig};
use docparse_layout::{
    AffineTransform, Bbox, LayoutEngine, LayoutRequest, PageImage,
    PageImageInput, PageRotation, PageTransform, PageTransformInput,
    PixelFormat, PpDocLayoutV3Engine,
};
use serde::Deserialize;
use typed_builder::TypedBuilder;

#[derive(Debug, Deserialize, TypedBuilder)]
struct OracleDetection {
    source_detection_index: u32,
    class_id: i64,
    label: String,
    score: f64,
    bbox: [f64; 4],
    order_seq: i64,
}

#[derive(Debug, Deserialize)]
struct OracleCollection {
    schema_version: u32,
    samples: Vec<OracleSample>,
}

#[derive(Debug, Deserialize, TypedBuilder)]
struct OracleSample {
    input: OracleInput,
    raw_outputs: RawOutputs,
    threshold: f64,
    detections: Vec<OracleDetection>,
}

#[derive(Debug, Deserialize, TypedBuilder)]
struct OracleInput {
    basename: String,
    width: u32,
    height: u32,
}

#[derive(Debug, Deserialize, TypedBuilder)]
struct ArrayContract {
    name: String,
    dtype: String,
    shape: Vec<usize>,
}

#[derive(Debug, Deserialize, TypedBuilder)]
struct BboxNumberContract {
    name: String,
    dtype: String,
    shape: Vec<usize>,
    values: Vec<i32>,
}

#[derive(Debug, Deserialize, TypedBuilder)]
struct RawOutputs {
    bbox: ArrayContract,
    bbox_num: BboxNumberContract,
    masks: ArrayContract,
}

/// Resolves a repository path from the layout crate directory.
fn repository_path(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

/// Builds the real engine from the repository's validated default configuration.
async fn engine() -> PpDocLayoutV3Engine {
    let raw = ConfigLoader::new(repository_path("docparse.toml"))
        .load_raw()
        .expect("the repository configuration must load");
    let config = ValidatedConfig::try_from(raw)
        .expect("the repository configuration must validate");
    PpDocLayoutV3Engine::from_config(Arc::new(config))
        .await
        .expect("the fixed model must initialize")
}

/// Verifies Rust inference and postprocessing match every fixed Python sample.
#[tokio::test]
#[ignore = "requires fixed PP-DocLayoutV3 model"]
async fn fixed_images_match_python_detections() {
    let oracle: OracleCollection = serde_json::from_slice(
        &fs::read(repository_path(
            "crates/layout/tests/fixtures/model/python_outputs.json",
        ))
        .expect("the Python oracle must be readable"),
    )
    .expect("the Python oracle must deserialize");
    assert_eq!(oracle.schema_version, 1);
    assert_eq!(oracle.samples.len(), 5);
    let engine = engine().await;
    for (sample_index, sample) in oracle.samples.into_iter().enumerate() {
        assert!((sample.threshold - 0.5).abs() <= f64::EPSILON);
        assert_eq!(sample.raw_outputs.bbox.name, "fetch_name_0");
        assert_eq!(sample.raw_outputs.bbox.dtype, "float32");
        assert_eq!(sample.raw_outputs.bbox.shape, vec![300, 7]);
        assert_eq!(sample.raw_outputs.bbox_num.name, "fetch_name_1");
        assert_eq!(sample.raw_outputs.bbox_num.dtype, "int32");
        assert_eq!(sample.raw_outputs.bbox_num.shape, vec![1]);
        assert_eq!(sample.raw_outputs.bbox_num.values, vec![300]);
        assert_eq!(sample.raw_outputs.masks.name, "fetch_name_2");
        assert_eq!(sample.raw_outputs.masks.dtype, "int32");
        assert_eq!(sample.raw_outputs.masks.shape, vec![300, 200, 200]);
        let image = image::open(repository_path(&format!(
            "crates/layout/tests/fixtures/model/{}",
            sample.input.basename
        )))
        .expect("the input fixture must decode")
        .into_rgb8();
        assert_eq!(image.width(), sample.input.width);
        assert_eq!(image.height(), sample.input.height);
        let width = image.width();
        let height = image.height();
        let page_image = Arc::new(
            PageImage::try_from(
                PageImageInput::builder()
                    .width(width)
                    .height(height)
                    .pixel_format(PixelFormat::Rgb8)
                    .data(Arc::<[u8]>::from(image.into_raw()))
                    .build(),
            )
            .expect("the fixture RGB buffer must be valid"),
        );
        let transform = PageTransform::try_from(
            PageTransformInput::builder()
                .page_to_viewport(AffineTransform::identity())
                .viewport_width(f64::from(width))
                .viewport_height(f64::from(height))
                .render_width(width)
                .render_height(height)
                .model_width(800)
                .model_height(800)
                .rotation(PageRotation::Degrees0)
                .build(),
        )
        .expect("the fixture transform must be valid");
        let request = LayoutRequest::builder()
            .page_number(
                u32::try_from(sample_index + 1)
                    .expect("sample page number must fit u32"),
            )
            .image(Arc::clone(&page_image))
            .transform(transform)
            .build();

        let detections = engine
            .detect(request)
            .await
            .expect("real layout inference must succeed");

        assert_eq!(detections.len(), sample.detections.len());
        for (actual, expected) in detections.iter().zip(&sample.detections) {
            assert_eq!(
                actual.source_detection_index,
                expected.source_detection_index
            );
            assert_eq!(actual.class_id, expected.class_id);
            assert_eq!(actual.raw_label, expected.label);
            assert!((actual.confidence - expected.score).abs() <= 1.0e-6);
            assert_eq!(
                actual.bbox,
                Bbox::try_from(expected.bbox)
                    .expect("oracle bboxes must be valid")
            );
            assert_eq!(actual.model_order, expected.order_seq);
        }
    }
}
