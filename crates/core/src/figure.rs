//! Embedded-image matching and figure assets for visual layout blocks.
use std::collections::BTreeMap;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use docparse_layout::{Bbox, LayoutDetection};
use image::ImageEncoder;
use image::codecs::png::PngEncoder;
use typed_builder::TypedBuilder;

use crate::pdfium::RenderedPage;
use crate::{
    Block, FigureAssets, FigureDelivery, FigureImage, FigureMediaType,
    FigureSource, PageWarning,
};

const CONTAINMENT: f64 = 0.85;
const AMBIGUOUS_AREA_RATIO: f64 = 1.15;
const MIN_SIDE_POINTS: f64 = 8.0;
const MAX_PAGE_COVERAGE: f64 = 0.90;
const BOUNDS_EPSILON: f64 = 0.5;

/// One embedded image carried from PDFium into page fusion.
///
/// `bytes` stay off the JSON snapshot. IPC sends them in a shared-memory side channel.
#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, TypedBuilder,
)]
pub(crate) struct EmbeddedImage {
    pub(crate) bounds: Bbox,
    pub(crate) pixel_width: u32,
    pub(crate) pixel_height: u32,
    #[builder(default)]
    // Binary IPC requires a fixed field layout even when no original file is available.
    pub(crate) media_type: Option<FigureMediaType>,
    #[builder(default)]
    #[serde(skip)]
    pub(crate) bytes: Option<Vec<u8>>,
}

impl TryFrom<::pdfium::EmbeddedImage> for EmbeddedImage {
    type Error = docparse_layout::GeometryError;

    /// Converts one PDFium image, keeping file bytes only for a complete image file.
    fn try_from(image: ::pdfium::EmbeddedImage) -> Result<Self, Self::Error> {
        let bounds = Bbox::try_from([
            f64::from(image.bounds.left),
            f64::from(image.bounds.top),
            f64::from(image.bounds.right),
            f64::from(image.bounds.bottom),
        ])?;
        let (media_type, bytes) = match image.encoded {
            Some(encoded)
                if image.pixel_width > 0 && image.pixel_height > 0 =>
            {
                let media_type = match encoded.kind {
                    ::pdfium::EncodedImageKind::Jpeg => FigureMediaType::Jpeg,
                    ::pdfium::EncodedImageKind::Png => FigureMediaType::Png,
                    ::pdfium::EncodedImageKind::Jp2 => FigureMediaType::Jp2,
                    ::pdfium::EncodedImageKind::Jpx => FigureMediaType::Jpx,
                };
                (Some(media_type), Some(encoded.bytes))
            }
            _ => (None, None),
        };
        Ok(Self::builder()
            .bounds(bounds)
            .pixel_width(image.pixel_width)
            .pixel_height(image.pixel_height)
            .media_type(media_type)
            .bytes(bytes)
            .build())
    }
}

impl EmbeddedImage {
    /// Returns the placed rectangle when this image may replace a layout box.
    fn placement(&self, page: Bbox) -> Option<Bbox> {
        let bounds = Bbox::try_from([
            self.bounds.left.max(page.left),
            self.bounds.top.max(page.top),
            self.bounds.right.min(page.right),
            self.bounds.bottom.min(page.bottom),
        ])
        .ok()?;
        if bounds.width() < MIN_SIDE_POINTS || bounds.height() < MIN_SIDE_POINTS
        {
            return None;
        }
        if bounds.width() > page.width() * MAX_PAGE_COVERAGE
            && bounds.height() > page.height() * MAX_PAGE_COVERAGE
        {
            return None;
        }
        Some(bounds)
    }

    /// Returns the original file when PDFium kept a complete image.
    fn original(&self) -> Option<(FigureMediaType, &[u8], u32, u32)> {
        let media_type = self.media_type?;
        let bytes = self.bytes.as_deref()?;
        (self.pixel_width > 0 && self.pixel_height > 0 && !bytes.is_empty())
            .then_some((media_type, bytes, self.pixel_width, self.pixel_height))
    }
}

/// A unique embedded-image match. The layout box is unchanged until after merging.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FigureMatch {
    pub(crate) image_index: u32,
    pub(crate) bounds: Bbox,
}

/// Embedded images for one page, borrowed for matching and asset delivery.
pub(crate) struct FigureCatalog<'a> {
    images: &'a [EmbeddedImage],
}

impl<'a> FigureCatalog<'a> {
    /// Borrows the page's embedded images.
    pub(crate) fn new(images: &'a [EmbeddedImage]) -> Self {
        Self { images }
    }

    /// Records a unique image for each figure detection without moving its box.
    pub(crate) fn match_detections(
        &self,
        detections: &[LayoutDetection],
        page: Bbox,
    ) -> BTreeMap<u32, FigureMatch> {
        let mut matches = BTreeMap::new();
        for detection in detections {
            if !detection.label.is_figure() {
                continue;
            }
            let Some(matched) = self.container(detection.bbox, page) else {
                continue;
            };
            matches.insert(detection.source_detection_index, matched);
        }
        matches
    }

    /// Expands figure boxes only when the image rectangle does not nest with another block.
    pub(crate) fn place(blocks: &mut [Block]) {
        let current: Vec<Bbox> =
            blocks.iter().map(|block| block.bbox).collect();
        for (index, block) in blocks.iter_mut().enumerate() {
            let Some(bounds) = block.figure_bounds.take() else {
                continue;
            };
            let Some(candidate) = union_bbox(bounds, block.bbox) else {
                continue;
            };
            if !boxes_differ(block.bbox, candidate) {
                continue;
            }
            let nests = current.iter().enumerate().any(|(other, bbox)| {
                other != index
                    && (candidate.contains_bbox(*bbox)
                        || bbox.contains_bbox(candidate))
            });
            if nests {
                continue;
            }
            let model = block.bbox;
            block.bbox = candidate;
            block.evidence.push(
                crate::Evidence::builder()
                    .kind("embedded_image_bounds".to_owned())
                    .details(BTreeMap::from([(
                        "model_bbox".to_owned(),
                        format!(
                            "{},{},{},{}",
                            model.left, model.top, model.right, model.bottom
                        ),
                    )]))
                    .build(),
            );
        }
    }

    /// Attaches one asset to every figure block. A missing file falls back to a PNG crop.
    pub(crate) fn attach(
        &self,
        blocks: &mut [Block],
        rendered: &RenderedPage,
        assets: &FigureAssets,
        warnings: &mut Vec<PageWarning>,
    ) {
        // Text-only pages never create a directory or perform any filesystem work.
        for block in blocks.iter_mut().filter(|block| block.label.is_figure()) {
            let embedded = block.embedded_image_index.and_then(|index| {
                usize::try_from(index)
                    .ok()
                    .and_then(|index| self.images.get(index))
                    .and_then(EmbeddedImage::original)
            });
            let asset = if let Some((media, bytes, width, height)) = embedded {
                PreparedFigure::builder()
                    .block(block)
                    .media_type(media)
                    .bytes(bytes)
                    .width(width)
                    .height(height)
                    .source(FigureSource::Embedded)
                    .build()
                    .deliver(assets)
            } else {
                match rendered.crop_png(block.bbox) {
                    Ok((bytes, width, height)) => PreparedFigure::builder()
                        .block(block)
                        .media_type(FigureMediaType::Png)
                        .bytes(&bytes)
                        .width(width)
                        .height(height)
                        .source(FigureSource::Raster)
                        .build()
                        .deliver(assets),
                    Err(message) => Err(message),
                }
            };
            match asset {
                Ok(image) => block.image = Some(image),
                Err(message) => warnings.push(missing(block, &message)),
            }
            block.embedded_image_index = None;
        }
    }

    /// Picks the smallest image that contains nearly all of the layout box.
    fn container(&self, layout: Bbox, page: Bbox) -> Option<FigureMatch> {
        let area = layout.area();
        if !area.is_finite() || area <= 0.0 {
            return None;
        }
        let containers =
            self.images.iter().enumerate().filter_map(|(index, image)| {
                let bounds = image.placement(page)?;
                let image_index = u32::try_from(index).ok()?;
                let coverage = layout.intersection_area(bounds) / area;
                (coverage >= CONTAINMENT).then_some((
                    image_index,
                    bounds,
                    bounds.area(),
                ))
            });
        // Only the smallest two areas affect ambiguity. Ascending source indices
        // preserve the former stable tie-break without allocating or sorting.
        let mut smallest: Option<(u32, Bbox, f64)> = None;
        let mut second: Option<f64> = None;
        for candidate in containers {
            if smallest
                .as_ref()
                .is_none_or(|best| candidate.2.total_cmp(&best.2).is_lt())
            {
                second = smallest.map(|best| best.2);
                smallest = Some(candidate);
            } else if second
                .is_none_or(|area| candidate.2.total_cmp(&area).is_lt())
            {
                second = Some(candidate.2);
            }
        }
        let smallest = smallest?;
        if second.is_some_and(|area| area <= smallest.2 * AMBIGUOUS_AREA_RATIO)
        {
            return None;
        }
        Some(FigureMatch {
            image_index: smallest.0,
            bounds: smallest.1,
        })
    }
}

/// One figure image ready to inline or write.
#[derive(TypedBuilder)]
struct PreparedFigure<'a> {
    block: &'a Block,
    media_type: FigureMediaType,
    bytes: &'a [u8],
    width: u32,
    height: u32,
    source: FigureSource,
}

impl PreparedFigure<'_> {
    /// Encodes this image inline or writes it into the parse's private directory.
    fn deliver(&self, assets: &FigureAssets) -> Result<FigureImage, String> {
        if self.width == 0 || self.height == 0 || self.bytes.is_empty() {
            return Err("image file is empty".to_owned());
        }
        let delivery = match assets.config.delivery {
            docparse_config::FigureDelivery::Inline => FigureDelivery::Inline {
                data_base64: STANDARD.encode(self.bytes),
            },
            docparse_config::FigureDelivery::File => FigureDelivery::File {
                path: assets.write(
                    self.block.id.as_str(),
                    self.media_type,
                    self.bytes,
                )?,
            },
        };
        Ok(FigureImage::builder()
            .source(self.source)
            .media_type(self.media_type)
            .width(self.width)
            .height(self.height)
            .delivery(delivery)
            .build())
    }
}

/// Builds one block-scoped warning without discarding other page results.
fn missing(block: &Block, message: &str) -> PageWarning {
    PageWarning {
        code: "VisualAssetUnavailable".to_owned(),
        stage: "figure".to_owned(),
        message: format!(
            "figure {} has no image: {message}",
            block.id.as_str()
        ),
    }
}

/// Ignores sub-point placement noise when deciding whether to expand a block.
fn boxes_differ(left: Bbox, right: Bbox) -> bool {
    (left.left - right.left).abs() > BOUNDS_EPSILON
        || (left.top - right.top).abs() > BOUNDS_EPSILON
        || (left.right - right.right).abs() > BOUNDS_EPSILON
        || (left.bottom - right.bottom).abs() > BOUNDS_EPSILON
}

/// Builds the validated conservative extent of model and embedded-image boxes.
fn union_bbox(left: Bbox, right: Bbox) -> Option<Bbox> {
    Bbox::try_from([
        left.left.min(right.left),
        left.top.min(right.top),
        left.right.max(right.right),
        left.bottom.max(right.bottom),
    ])
    .ok()
}

impl RenderedPage {
    /// Encodes shared raster cropping as PNG without duplicating formula pixel extraction.
    fn crop_png(&self, bbox: Bbox) -> Result<(Vec<u8>, u32, u32), String> {
        let image = self.crop_pixels(self.crop_bounds(bbox)?)?;
        let mut png = Vec::new();
        PngEncoder::new(&mut png)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|error| format!("failed to encode figure PNG: {error}"))?;
        Ok((png, image.width(), image.height()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use docparse_config::{FigureConfig, FigureDelivery as ConfiguredDelivery};
    use docparse_layout::{
        AffineTransform, Bbox, GeometrySource, LayoutDetection, LayoutLabel,
        PageImage, PageImageInput, PageRotation, PageTransform,
        PageTransformInput, PixelFormat,
    };

    use super::{EmbeddedImage, FigureCatalog};
    use crate::pdfium::RenderedPage;
    use crate::{
        Block, BlockId, ExtractedPage, FigureDelivery, FigureMediaType,
        FigureSource, LabelSource,
    };

    /// Supplies deterministic viewport bounds for matching tests.
    fn page() -> Bbox {
        Bbox::try_from([0.0, 0.0, 400.0, 600.0]).expect("page")
    }

    /// Builds one model detection with a stable source identity.
    fn detection(
        index: u32,
        label: LayoutLabel,
        bbox: [f64; 4],
    ) -> LayoutDetection {
        LayoutDetection::builder()
            .source_detection_index(index)
            .raw_label(label.to_str().to_owned())
            .class_id(
                i64::try_from(label.idx().expect("fixed label"))
                    .expect("class"),
            )
            .label(label)
            .confidence(0.9)
            .bbox(Bbox::try_from(bbox).expect("bbox"))
            .geometry_source(GeometrySource::DerivedFromBbox)
            .model_order(i64::from(index))
            .metadata(BTreeMap::new())
            .build()
    }

    /// Builds an embedded-image fixture with optional encoded bytes.
    fn image(bounds: [f64; 4], file: bool) -> EmbeddedImage {
        EmbeddedImage::builder()
            .bounds(Bbox::try_from(bounds).expect("image"))
            .pixel_width(20)
            .pixel_height(10)
            .media_type(file.then_some(FigureMediaType::Jpeg))
            .bytes(file.then(|| vec![0xff, 0xd8, 0xff, 0xd9]))
            .build()
    }

    /// Builds a visual block with optional image-matching evidence.
    fn figure_block(
        index: u32,
        label: LayoutLabel,
        bbox: [f64; 4],
        embedded_image_index: Option<u32>,
        figure_bounds: Option<[f64; 4]>,
    ) -> Block {
        Block::builder()
            .id(BlockId::model(1, index, 0))
            .label(label)
            .text(String::new())
            .label_source(LabelSource::Model)
            .bbox(Bbox::try_from(bbox).expect("bbox"))
            .final_order(index)
            .lines(Vec::new())
            .embedded_image_index(embedded_image_index)
            .figure_bounds(
                figure_bounds
                    .map(|bounds| Bbox::try_from(bounds).expect("bounds")),
            )
            .build()
    }

    /// Shared cropping clips page edges and preserves exact row bytes.
    #[test]
    fn crop_rounds_outward_clips_edges_and_preserves_pixels() {
        let rendered = rendered_page();
        let bounds = rendered
            .crop_bounds(Bbox::try_from([-1.0, 1.2, 2.1, 5.0]).expect("bbox"))
            .expect("bounds");
        assert_eq!(bounds, [0, 1, 3, 4]);
        let crop = rendered.crop_pixels(bounds).expect("crop");
        assert_eq!((crop.width(), crop.height()), (3, 3));
        let expected: Vec<_> = [12..21, 24..33, 36..45]
            .into_iter()
            .flat_map(|range| {
                rendered
                    .image
                    .data()
                    .get(range)
                    .expect("fixture row")
                    .iter()
                    .copied()
            })
            .collect();
        assert_eq!(crop.as_raw(), &expected);
        rendered
            .crop_pixels([0, 0, 5, 4])
            .expect_err("out-of-bounds crop");
        rendered.crop_pixels([2, 2, 2, 3]).expect_err("empty crop");
        rendered
            .crop_bounds(Bbox::try_from([5.0, 5.0, 6.0, 6.0]).expect("bbox"))
            .expect_err("outside page");
    }

    /// A unique image is recorded, but the detection box stays put until after merging.
    #[test]
    fn matching_records_the_image_without_moving_the_detection() {
        let detections = vec![
            detection(0, LayoutLabel::Image, [20.0, 30.0, 70.0, 80.0]),
            detection(1, LayoutLabel::Seal, [110.0, 40.0, 140.0, 70.0]),
            detection(2, LayoutLabel::Text, [20.0, 30.0, 70.0, 80.0]),
            detection(3, LayoutLabel::Chart, [200.0, 200.0, 260.0, 260.0]),
        ];
        let images = vec![
            image([10.0, 20.0, 90.0, 100.0], true),
            image([100.0, 30.0, 150.0, 80.0], false),
        ];
        let matches =
            FigureCatalog::new(&images).match_detections(&detections, page());
        assert_eq!(
            detections.first().expect("image").bbox,
            Bbox::try_from([20.0, 30.0, 70.0, 80.0]).expect("model")
        );
        assert_eq!(
            matches.get(&0).expect("image match").bounds,
            images.first().expect("embedded").bounds
        );
        assert_eq!(matches.get(&1).expect("seal match").image_index, 1);
        assert!(!matches.contains_key(&2));
        assert!(!matches.contains_key(&3));
    }

    /// Expansion that would contain another block is refused; an isolated figure still expands.
    #[test]
    fn placement_skips_boxes_that_would_nest() {
        let mut blocks = vec![
            figure_block(
                0,
                LayoutLabel::Image,
                [20.0, 30.0, 70.0, 80.0],
                Some(0),
                Some([10.0, 20.0, 90.0, 100.0]),
            ),
            figure_block(
                1,
                LayoutLabel::Text,
                [30.0, 40.0, 40.0, 50.0],
                None,
                None,
            ),
            figure_block(
                2,
                LayoutLabel::Seal,
                [110.0, 40.0, 140.0, 70.0],
                Some(1),
                Some([100.0, 30.0, 150.0, 80.0]),
            ),
        ];
        FigureCatalog::place(&mut blocks);
        assert_eq!(
            blocks.first().expect("image").bbox,
            Bbox::try_from([20.0, 30.0, 70.0, 80.0]).expect("unchanged")
        );
        assert!(blocks.first().expect("image").figure_bounds.is_none());
        assert_eq!(
            blocks.get(2).expect("seal").bbox,
            Bbox::try_from([100.0, 30.0, 150.0, 80.0]).expect("expanded")
        );
        assert!(
            blocks
                .get(2)
                .expect("seal")
                .evidence
                .iter()
                .any(|item| { item.kind == "embedded_image_bounds" })
        );
    }

    /// Candidate order and equal-area ties must not change figure selection.
    #[test]
    fn smallest_container_and_ambiguity_do_not_depend_on_input_order() {
        let layout = Bbox::try_from([40.0, 40.0, 60.0, 60.0]).expect("layout");
        for order in [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ] {
            let boxes = [
                [0.0, 0.0, 100.0, 100.0],
                [30.0, 30.0, 70.0, 70.0],
                [20.0, 20.0, 80.0, 80.0],
            ];
            let images: Vec<_> = order
                .into_iter()
                .map(|index| {
                    image(*boxes.get(index).expect("permutation index"), true)
                })
                .collect();
            let matched = FigureCatalog::new(&images)
                .container(layout, page())
                .expect("unique smallest");
            assert_eq!(
                matched.bounds,
                Bbox::try_from(boxes[1]).expect("smallest")
            );
            let mut ambiguous = images;
            ambiguous.push(image(boxes[1], true));
            assert!(
                FigureCatalog::new(&ambiguous)
                    .container(layout, page())
                    .is_none()
            );
        }
    }

    /// Two similarly sized containers, a full-page image, and a corner overlap do nothing.
    #[test]
    fn ambiguous_page_sized_and_partial_images_are_left_alone() {
        let original = [40.0, 40.0, 80.0, 80.0];
        let detections = vec![
            detection(0, LayoutLabel::Chart, original),
            detection(1, LayoutLabel::Image, original),
            detection(2, LayoutLabel::HeaderImage, [10.0, 10.0, 30.0, 25.0]),
        ];
        let images = vec![
            image([30.0, 30.0, 90.0, 90.0], true),
            image([32.0, 32.0, 92.0, 92.0], true),
            image([0.0, 0.0, 390.0, 590.0], true),
            image([70.0, 70.0, 120.0, 120.0], true),
        ];
        let matches =
            FigureCatalog::new(&images).match_detections(&detections, page());
        assert!(matches.is_empty());
    }

    /// Inline delivery keeps the original bytes; a vector figure falls back to the page crop.
    #[test]
    fn inline_delivery_prefers_the_embedded_file_and_crops_otherwise() {
        let rendered = rendered_page();
        let mut blocks = vec![
            figure_block(
                0,
                LayoutLabel::Image,
                [0.0, 0.0, 4.0, 2.0],
                Some(0),
                None,
            ),
            figure_block(
                1,
                LayoutLabel::Chart,
                [1.0, 1.0, 3.0, 3.0],
                None,
                None,
            ),
        ];
        let jpeg = vec![0xff, 0xd8, 0xff, 0x00, 0xff, 0xd9];
        let images = vec![
            EmbeddedImage::builder()
                .bounds(Bbox::try_from([0.0, 0.0, 4.0, 2.0]).expect("bounds"))
                .pixel_width(8)
                .pixel_height(4)
                .media_type(Some(FigureMediaType::Jpeg))
                .bytes(Some(jpeg.clone()))
                .build(),
        ];
        let mut warnings = Vec::new();
        FigureCatalog::new(&images).attach(
            &mut blocks,
            &rendered,
            &super::FigureAssets::new(
                FigureConfig::default(),
                "figures-".into(),
            ),
            &mut warnings,
        );
        assert!(warnings.is_empty());
        let embedded = blocks
            .first()
            .and_then(|block| block.image.as_ref())
            .expect("embedded");
        assert_eq!(embedded.source, FigureSource::Embedded);
        assert_eq!((embedded.width, embedded.height), (8, 4));
        assert!(matches!(embedded.delivery, FigureDelivery::Inline { .. }));
        if let FigureDelivery::Inline { data_base64 } = &embedded.delivery {
            assert_eq!(
                base64::Engine::decode(
                    &base64::engine::general_purpose::STANDARD,
                    data_base64
                )
                .expect("base64"),
                jpeg
            );
        }
        let raster = blocks
            .get(1)
            .and_then(|block| block.image.as_ref())
            .expect("raster");
        assert_eq!(raster.source, FigureSource::Raster);
        assert_eq!(raster.media_type, FigureMediaType::Png);
    }

    /// Empty parses stay off disk and cancellation cannot outrun the last active writer.
    #[test]
    fn file_assets_are_lazy_and_live_until_the_last_writer_drops() {
        let root = tempfile::tempdir().expect("root");
        let config = FigureConfig::builder()
            .delivery(ConfiguredDelivery::File)
            .directory(Some(root.path().join("assets")))
            .build();
        let assets = Arc::new(super::FigureAssets::new(
            config.clone(),
            "attempt-".into(),
        ));
        FigureCatalog::new(&[]).attach(
            &mut [],
            &rendered_page(),
            &assets,
            &mut Vec::new(),
        );
        assert!(
            !root.path().join("assets").exists(),
            "empty pages must not create directories"
        );
        let writer = Arc::clone(&assets);
        let first = assets
            .write("p1", FigureMediaType::Png, b"first")
            .expect("image");
        let second = assets
            .write("p2", FigureMediaType::Png, b"second")
            .expect("image");
        let directory = std::path::Path::new(&first)
            .parent()
            .expect("parent")
            .to_path_buf();
        assert_eq!(
            std::path::Path::new(&second).parent(),
            Some(directory.as_path())
        );
        drop(assets);
        assert!(
            directory.exists(),
            "a cancelled waiter must not delete an active writer's files"
        );
        drop(writer);
        assert!(
            !directory.exists(),
            "the final uncommitted owner cleans the attempt"
        );
        for committed in [true, false] {
            let assets =
                super::FigureAssets::new(config.clone(), "retained-".into());
            let path = assets
                .write("p1", FigureMediaType::Png, b"retained")
                .expect("image");
            // Publication can become committed after its first acknowledgement was uncertain.
            assets.keep(false);
            assets.keep(committed);
            drop(assets);
            assert!(std::path::Path::new(&path).exists());
            assert_eq!(
                std::path::Path::new(&path)
                    .parent()
                    .expect("parent")
                    .join(".pending")
                    .exists(),
                !committed
            );
        }
    }

    /// Concurrent deliveries get distinct directories and keep their own bytes.
    #[test]
    fn file_delivery_uses_a_private_directory() {
        let directory = tempfile::tempdir().expect("directory");
        let rendered = rendered_page();
        let png = b"\x89PNG\r\n\x1a\npayload".to_vec();
        let jpeg = vec![0xff, 0xd8, 0xff, 0xd9];
        let config = FigureConfig::builder()
            .delivery(ConfiguredDelivery::File)
            .directory(Some(directory.path().to_path_buf()))
            .build();
        let mut first = vec![figure_block(
            3,
            LayoutLabel::Seal,
            [0.0, 0.0, 2.0, 2.0],
            Some(0),
            None,
        )];
        let mut second = vec![figure_block(
            3,
            LayoutLabel::Seal,
            [0.0, 0.0, 2.0, 2.0],
            Some(0),
            None,
        )];
        let first_images = vec![
            EmbeddedImage::builder()
                .bounds(Bbox::try_from([0.0, 0.0, 2.0, 2.0]).expect("bounds"))
                .pixel_width(3)
                .pixel_height(5)
                .media_type(Some(FigureMediaType::Png))
                .bytes(Some(png.clone()))
                .build(),
        ];
        let second_images = vec![
            EmbeddedImage::builder()
                .bounds(Bbox::try_from([0.0, 0.0, 2.0, 2.0]).expect("bounds"))
                .pixel_width(2)
                .pixel_height(2)
                .media_type(Some(FigureMediaType::Jpeg))
                .bytes(Some(jpeg.clone()))
                .build(),
        ];
        let first_assets =
            super::FigureAssets::new(config.clone(), "first-".into());
        let second_assets =
            super::FigureAssets::new(config.clone(), "second-".into());
        let mut warnings = Vec::new();
        FigureCatalog::new(&first_images).attach(
            &mut first,
            &rendered,
            &first_assets,
            &mut warnings,
        );
        FigureCatalog::new(&second_images).attach(
            &mut second,
            &rendered,
            &second_assets,
            &mut warnings,
        );
        assert!(warnings.is_empty());
        let left = first
            .first()
            .and_then(|block| block.image.as_ref())
            .expect("first");
        let right = second
            .first()
            .and_then(|block| block.image.as_ref())
            .expect("second");
        assert!(matches!(left.delivery, FigureDelivery::File { .. }));
        assert!(matches!(right.delivery, FigureDelivery::File { .. }));
        if let (
            FigureDelivery::File { path: left },
            FigureDelivery::File { path: right },
        ) = (&left.delivery, &right.delivery)
        {
            assert_ne!(
                std::path::Path::new(left).parent(),
                std::path::Path::new(right).parent()
            );
            assert_eq!(std::fs::read(left).expect("png"), png);
            assert_eq!(std::fs::read(right).expect("jpeg"), jpeg);
        }
    }

    /// Scan serialization never retains image metadata or payloads after render-time attachment.
    #[test]
    fn image_files_stay_outside_scan_json() {
        let mut extracted = ExtractedPage::builder()
            .page_number(1)
            .width(100.0)
            .height(100.0)
            .rotation(0)
            .text_items(Vec::new())
            .build();
        extracted.embedded_images = vec![image([1.0, 2.0, 3.0, 4.0], true)];
        let json = serde_json::to_string(&extracted).expect("json");
        assert!(!json.contains("embedded_images"));
    }

    /// Supplies a small raster shared by image delivery and crop regression tests.
    fn rendered_page() -> RenderedPage {
        let mut pixels = vec![255_u8; 4 * 4 * 3];
        for y in 1..3 {
            for x in 1..3 {
                let offset = (y * 4 + x) * 3;
                if let Some(pixel) = pixels.get_mut(offset..offset + 3) {
                    pixel.fill(0);
                }
            }
        }
        RenderedPage::builder()
            .page_number(1)
            .image(Arc::new(
                PageImage::try_from(
                    PageImageInput::builder()
                        .width(4)
                        .height(4)
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(pixels))
                        .build(),
                )
                .expect("raster"),
            ))
            .transform(
                PageTransform::try_from(
                    PageTransformInput::builder()
                        .page_to_viewport(AffineTransform::identity())
                        .viewport_width(4.0)
                        .viewport_height(4.0)
                        .render_width(4)
                        .render_height(4)
                        .model_width(4)
                        .model_height(4)
                        .rotation(PageRotation::Degrees0)
                        .build(),
                )
                .expect("transform"),
            )
            .build()
    }
}
