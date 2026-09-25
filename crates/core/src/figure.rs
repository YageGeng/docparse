//! Label-independent embedded-image delivery with visual-layout raster fallbacks.
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
};

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
const COMPONENT_COVERAGE: f64 = 0.5;
const AMBIGUOUS_AREA_RATIO: f64 = 1.15;
const MIN_SIDE_POINTS: f64 = 8.0;
const MAX_PAGE_COVERAGE: f64 = 0.90;
const BOUNDS_EPSILON: f64 = 0.5;

/// Internal payload format; raw pixels are encoded only after leaving PDFium's critical section.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize,
)]
pub(crate) enum EmbeddedImageFormat {
    Encoded(FigureMediaType),
    Rgba,
}

/// One embedded image carried from PDFium into page fusion.
///
/// `bytes` stay off the JSON snapshot. IPC carries every payload in one shared-memory region.
#[derive(
    Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, TypedBuilder,
)]
pub(crate) struct EmbeddedImage {
    pub(crate) bounds: Bbox,
    pub(crate) pixel_width: u32,
    pub(crate) pixel_height: u32,
    #[builder(default)]
    // Binary IPC requires a fixed field layout even when image extraction failed.
    pub(crate) format: Option<EmbeddedImageFormat>,
    #[builder(default)]
    #[serde(skip)]
    pub(crate) bytes: Option<Vec<u8>>,
}

impl TryFrom<::pdfium::EmbeddedImage> for EmbeddedImage {
    type Error = docparse_layout::GeometryError;

    /// Moves an original file or deferred pixel buffer across the PDFium boundary without encoding or cloning it.
    fn try_from(image: ::pdfium::EmbeddedImage) -> Result<Self, Self::Error> {
        let bounds = Bbox::try_from([
            f64::from(image.bounds.left),
            f64::from(image.bounds.top),
            f64::from(image.bounds.right),
            f64::from(image.bounds.bottom),
        ])?;
        let (format, bytes) = match image.data {
            Some(::pdfium::EmbeddedImageData::Encoded(encoded)) => {
                let media_type = match encoded.kind {
                    ::pdfium::EncodedImageKind::Jpeg => FigureMediaType::Jpeg,
                    ::pdfium::EncodedImageKind::Png => FigureMediaType::Png,
                    ::pdfium::EncodedImageKind::Jp2 => FigureMediaType::Jp2,
                    ::pdfium::EncodedImageKind::Jpx => FigureMediaType::Jpx,
                };
                (
                    Some(EmbeddedImageFormat::Encoded(media_type)),
                    Some(encoded.bytes),
                )
            }
            Some(::pdfium::EmbeddedImageData::Rgba(pixels)) => {
                (Some(EmbeddedImageFormat::Rgba), Some(pixels))
            }
            None => (None, None),
        };
        Ok(Self::builder()
            .bounds(bounds)
            .pixel_width(image.pixel_width)
            .pixel_height(image.pixel_height)
            .format(format)
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

    /// Borrows original files or encodes mask-aware pixels on the CPU stage, sharing the same fallback for every owner.
    fn prepare(
        &self,
        rendered: &RenderedPage,
    ) -> Result<PreparedFigure<'_>, String> {
        let Some((format, bytes)) = self
            .format
            .zip(self.bytes.as_deref())
            .filter(|(_, bytes)| !bytes.is_empty())
        else {
            return rendered.cropped_figure(self.bounds);
        };
        let (media_type, bytes) = match format {
            EmbeddedImageFormat::Encoded(media) => {
                (media, Cow::Borrowed(bytes))
            }
            EmbeddedImageFormat::Rgba => {
                let expected = usize::try_from(self.pixel_width)
                    .ok()
                    .zip(usize::try_from(self.pixel_height).ok())
                    .and_then(|(width, height)| width.checked_mul(height))
                    .and_then(|pixels| pixels.checked_mul(4));
                if expected != Some(bytes.len())
                    || self.pixel_width == 0
                    || self.pixel_height == 0
                {
                    return Err(
                        "invalid embedded RGBA dimensions or length".into()
                    );
                }
                let mut png = Vec::new();
                PngEncoder::new(&mut png)
                    .write_image(
                        bytes,
                        self.pixel_width,
                        self.pixel_height,
                        image::ExtendedColorType::Rgba8,
                    )
                    .map_err(|error| {
                        format!("failed to encode embedded PNG: {error}")
                    })?;
                (FigureMediaType::Png, Cow::Owned(png))
            }
        };
        Ok(PreparedFigure::builder()
            .media_type(media_type)
            .bytes(bytes)
            .width(self.pixel_width)
            .height(self.pixel_height)
            .source(FigureSource::Embedded)
            .build())
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

    /// Records geometric image matches independently of the model's semantic label.
    pub(crate) fn match_detections(
        &self,
        detections: &[LayoutDetection],
        page: Bbox,
    ) -> BTreeMap<u32, FigureMatch> {
        let mut composite_detections = BTreeSet::new();
        let mut composite_images = vec![false; self.images.len()];
        for detection in detections.iter().filter(|detection| {
            detection.label == docparse_layout::LayoutLabel::Image
        }) {
            let parts: Vec<_> = self.parts(detection.bbox, page).collect();
            if parts.len() > 1 {
                composite_detections.insert(detection.source_detection_index);
                for index in parts {
                    if let Some(owned) = composite_images.get_mut(index) {
                        *owned = true;
                    }
                }
            }
        }
        let mut matches = BTreeMap::new();
        for detection in detections {
            // A composite's components belong to its raster crop, not to competing layout boxes.
            if composite_detections.contains(&detection.source_detection_index)
            {
                continue;
            }
            // Algorithms, text, tables and any future label may be backed by an embedded image.
            let Some(matched) = self.container(detection.bbox, page) else {
                continue;
            };
            if composite_images
                .get(matched.image_index as usize)
                .copied()
                .unwrap_or(false)
            {
                continue;
            }
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

    /// Delivers embedded images once, returning unclaimed placements independently of layout labels.
    pub(crate) fn attach(
        &self,
        blocks: &mut [Block],
        rendered: &RenderedPage,
        assets: &FigureAssets,
        warnings: &mut Vec<PageWarning>,
    ) -> Vec<crate::PageImageAsset> {
        let mut delivered = vec![false; self.images.len()];
        let (width, height) = rendered.transform.viewport_size();
        let page = Bbox::try_from([0.0, 0.0, width, height])
            .expect("validated rendered viewport");
        // Raster fallbacks remain limited to visual regions; embedded-image matches accept every label.
        for block in blocks.iter_mut().filter(|block| {
            block.label.is_figure() || block.embedded_image_index.is_some()
        }) {
            let parts: Vec<_> =
                if block.label == docparse_layout::LayoutLabel::Image {
                    self.parts(block.bbox, page).collect()
                } else {
                    Vec::new()
                };
            let composite = parts.len() > 1;
            let index = block
                .embedded_image_index
                .take()
                .and_then(|index| usize::try_from(index).ok());
            if !composite
                && index.is_some_and(|index| {
                    delivered.get(index).copied().unwrap_or(false)
                })
            {
                continue;
            }
            let prepared = if composite {
                tracing::debug!(
                    "using page raster for composite image block {} with {} embedded parts",
                    block.id.as_str(),
                    parts.len()
                );
                rendered.cropped_figure(block.bbox)
            } else {
                match index.and_then(|index| self.images.get(index)) {
                    Some(image) => image.prepare(rendered),
                    None => rendered.cropped_figure(block.bbox),
                }
            };
            let asset = prepared
                .and_then(|image| image.deliver(block.id.as_str(), assets));
            match asset {
                Ok(image) => {
                    block.image = Some(image);
                    if composite {
                        for index in parts {
                            if let Some(slot) = delivered.get_mut(index) {
                                *slot = true;
                            }
                        }
                    } else if let Some(slot) =
                        index.and_then(|index| delivered.get_mut(index))
                    {
                        *slot = true;
                    }
                }
                Err(message) => {
                    warnings.push(missing(block.id.as_str(), &message))
                }
            }
        }
        // Matching thresholds only protect layout geometry. They must never discard actual PDF images.
        let mut remaining = Vec::new();
        for (index, image) in
            self.images.iter().enumerate().filter(|(index, _)| {
                !delivered.get(*index).copied().unwrap_or(false)
            })
        {
            let id = format!("p{}:image{index}", rendered.page_number);
            match image
                .prepare(rendered)
                .and_then(|image| image.deliver(&id, assets))
            {
                Ok(asset) => remaining.push(crate::PageImageAsset {
                    id,
                    bbox: image.bounds,
                    image: asset,
                }),
                Err(message) => warnings.push(missing(&id, &message)),
            }
        }
        remaining
    }

    /// Finds embedded images mostly inside an image layout or covering most of it.
    fn parts(
        &self,
        layout: Bbox,
        page: Bbox,
    ) -> impl Iterator<Item = usize> + '_ {
        let layout_fills_page = layout.width()
            >= page.width() * MAX_PAGE_COVERAGE
            && layout.height() >= page.height() * MAX_PAGE_COVERAGE;
        self.images
            .iter()
            .enumerate()
            .filter_map(move |(index, image)| {
                let bounds = image.bounds;
                // Full-page layers belong to a full-page image layout but not to a small figure.
                if !layout_fills_page
                    && bounds.width() > page.width() * MAX_PAGE_COVERAGE
                    && bounds.height() > page.height() * MAX_PAGE_COVERAGE
                {
                    return None;
                }
                let overlap = layout.intersection_area(bounds);
                // A component may extend beyond a detector box; majority overlap still belongs to the layout.
                (overlap / layout.area().max(f64::EPSILON) >= CONTAINMENT
                    || overlap / bounds.area().max(f64::EPSILON)
                        >= COMPONENT_COVERAGE)
                    .then_some(index)
            })
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
    media_type: FigureMediaType,
    bytes: Cow<'a, [u8]>,
    width: u32,
    height: u32,
    source: FigureSource,
}

impl PreparedFigure<'_> {
    /// Encodes this image inline or writes it into the parse's private directory.
    fn deliver(
        &self,
        id: &str,
        assets: &FigureAssets,
    ) -> Result<FigureImage, String> {
        if self.width == 0 || self.height == 0 || self.bytes.is_empty() {
            return Err("image file is empty".to_owned());
        }
        let delivery = match assets.config.delivery {
            docparse_config::FigureDelivery::Inline => FigureDelivery::Inline {
                data_base64: STANDARD.encode(self.bytes.as_ref()),
            },
            docparse_config::FigureDelivery::File => FigureDelivery::File {
                path: assets.write(id, self.media_type, self.bytes.as_ref())?,
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
fn missing(id: &str, message: &str) -> PageWarning {
    PageWarning {
        code: "VisualAssetUnavailable".to_owned(),
        stage: "figure".to_owned(),
        message: format!("figure {id} has no image: {message}"),
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
    fn cropped_figure(
        &self,
        bbox: Bbox,
    ) -> Result<PreparedFigure<'static>, String> {
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
        Ok(PreparedFigure::builder()
            .media_type(FigureMediaType::Png)
            .bytes(Cow::Owned(png))
            .width(image.width())
            .height(image.height())
            .source(FigureSource::Raster)
            .build())
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

    use super::{EmbeddedImage, EmbeddedImageFormat, FigureCatalog};
    use crate::pdfium::RenderedPage;
    use crate::{
        Block, BlockId, ExtractedPage, FigureDelivery, FigureMediaType,
        FigureSource, LabelSource,
    };

    /// Supplies deterministic viewport bounds for matching tests.
    fn page() -> Bbox {
        Bbox::try_from([0.0, 0.0, 400.0, 600.0]).expect("page")
    }

    /// An embedded image is associated geometrically even when the model calls it algorithm or text.
    #[test]
    fn image_matching_does_not_depend_on_layout_label() {
        let images = vec![image([20.0, 20.0, 90.0, 90.0], true)];
        for label in [
            LayoutLabel::Algorithm,
            LayoutLabel::Content,
            LayoutLabel::Text,
            LayoutLabel::Chart,
        ] {
            let detections =
                vec![detection(0, label.clone(), [25.0, 25.0, 85.0, 85.0])];
            assert_eq!(
                FigureCatalog::new(&images)
                    .match_detections(&detections, page())
                    .len(),
                1,
                "{label:?}"
            );
        }
    }

    /// Tiny, full-page and extra images survive independently, without duplicating an image matched twice.
    #[test]
    fn every_embedded_placement_is_delivered_once() {
        let images = vec![
            image([0.0, 0.0, 2.0, 2.0], true),
            image([0.0, 0.0, 4.0, 4.0], true),
            image([3.0, 3.0, 3.1, 3.1], true),
        ];
        let mut blocks = vec![
            figure_block(
                0,
                LayoutLabel::Algorithm,
                [0.0, 0.0, 2.0, 2.0],
                Some(0),
                None,
            ),
            figure_block(
                1,
                LayoutLabel::Text,
                [0.0, 0.0, 2.0, 2.0],
                Some(0),
                None,
            ),
        ];
        let mut warnings = Vec::new();
        let remaining = FigureCatalog::new(&images).attach(
            &mut blocks,
            &rendered_page(),
            &super::FigureAssets::new(
                FigureConfig::default(),
                "all-images-".into(),
            ),
            &mut warnings,
        );
        assert!(warnings.is_empty());
        assert_eq!(
            blocks.iter().filter(|block| block.image.is_some()).count(),
            1
        );
        assert_eq!(remaining.len(), 2);
        assert!(
            remaining
                .iter()
                .all(|asset| asset.image.source == FigureSource::Embedded)
        );
        let page = crate::PageResult::builder()
            .page_number(1)
            .width(4.0)
            .height(4.0)
            .rotation(0)
            .blocks(Vec::new())
            .images(remaining)
            .build();
        crate::ResultValidator::validate_page(&page)
            .expect("image content validates");
        let document = crate::DocumentResult::builder()
            .schema_version(crate::SchemaVersion::V2_0)
            .context(crate::DocumentContext::builder().page_count(1).build())
            .pages(vec![page])
            .build();
        let config = docparse_config::OutputConfig::builder()
            .formula_placeholder("[formula]".into())
            .include_evidence(false)
            .include_diagnostics(false)
            .build();
        let json = crate::JsonRenderer::render_with_config(&document, &config)
            .expect("JSON");
        let restored: crate::DocumentResult =
            serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(restored.pages.first().expect("page").images.len(), 2);
    }

    /// Deferred RGBA keeps alpha during CPU-side PNG encoding and rejects malformed IPC pixel lengths.
    #[test]
    fn deferred_pixels_encode_without_losing_transparency() {
        let pixels = vec![255, 0, 0, 0, 0, 255, 0, 128];
        let mut embedded = EmbeddedImage::builder()
            .bounds(Bbox::try_from([0.0, 0.0, 2.0, 1.0]).expect("bounds"))
            .pixel_width(2)
            .pixel_height(1)
            .format(Some(EmbeddedImageFormat::Rgba))
            .bytes(Some(pixels.clone()))
            .build();
        let rendered = rendered_page();
        let prepared = embedded.prepare(&rendered).expect("prepare");
        assert_eq!(prepared.media_type, FigureMediaType::Png);
        assert_eq!(prepared.source, FigureSource::Embedded);
        assert_eq!(
            image::load_from_memory(prepared.bytes.as_ref())
                .expect("PNG")
                .to_rgba8()
                .into_raw(),
            pixels
        );
        drop(prepared);
        embedded.bytes.as_mut().expect("pixels").pop();
        assert!(embedded.prepare(&rendered).is_err());
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
            .format(
                file.then_some(EmbeddedImageFormat::Encoded(
                    FigureMediaType::Jpeg,
                )),
            )
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
        // Semantic text labels no longer suppress a geometric embedded-image match.
        assert_eq!(matches.get(&2).expect("text image").image_index, 0);
        assert!(!matches.contains_key(&3));
    }

    /// A composite image region uses one raster crop and does not publish its pieces separately.
    #[test]
    fn composite_image_layout_uses_raster_and_consumes_parts() {
        let images = vec![
            image([10.0, 10.0, 130.0, 130.0], true),
            image([100.0, 30.0, 130.0, 60.0], true),
        ];
        assert!(
            FigureCatalog::new(&images)
                .match_detections(
                    &[
                        detection(
                            0,
                            LayoutLabel::Image,
                            [20.0, 20.0, 120.0, 120.0]
                        ),
                        detection(
                            1,
                            LayoutLabel::Text,
                            [20.0, 20.0, 120.0, 120.0]
                        ),
                    ],
                    page(),
                )
                .is_empty()
        );

        let images = vec![
            image([0.0, 0.0, 2.0, 4.0], true),
            image([2.0, 0.0, 4.0, 4.0], true),
        ];
        let mut blocks = vec![figure_block(
            0,
            LayoutLabel::Image,
            [0.0, 0.0, 4.0, 4.0],
            Some(0),
            None,
        )];
        let mut warnings = Vec::new();
        let remaining = FigureCatalog::new(&images).attach(
            &mut blocks,
            &rendered_page(),
            &super::FigureAssets::new(
                FigureConfig::default(),
                "composite-".into(),
            ),
            &mut warnings,
        );
        assert!(warnings.is_empty());
        assert_eq!(
            blocks
                .first()
                .and_then(|block| block.image.as_ref())
                .map(|image| image.source),
            Some(FigureSource::Raster)
        );
        assert!(remaining.is_empty());

        // A page-sized image layout may itself be composed of multiple page-sized PDF layers.
        let images = vec![
            image([0.0, 0.0, 4.0, 4.0], true),
            image([0.0, 0.0, 4.0, 4.0], true),
        ];
        let mut blocks = vec![figure_block(
            1,
            LayoutLabel::Image,
            [0.0, 0.0, 4.0, 4.0],
            None,
            None,
        )];
        let remaining = FigureCatalog::new(&images).attach(
            &mut blocks,
            &rendered_page(),
            &super::FigureAssets::new(
                FigureConfig::default(),
                "page-layers-".into(),
            ),
            &mut warnings,
        );
        assert_eq!(
            blocks
                .first()
                .and_then(|block| block.image.as_ref())
                .map(|image| image.source),
            Some(FigureSource::Raster)
        );
        assert!(remaining.is_empty());
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
                .format(Some(EmbeddedImageFormat::Encoded(
                    FigureMediaType::Jpeg,
                )))
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
                .format(Some(EmbeddedImageFormat::Encoded(
                    FigureMediaType::Png,
                )))
                .bytes(Some(png.clone()))
                .build(),
        ];
        let second_images = vec![
            EmbeddedImage::builder()
                .bounds(Bbox::try_from([0.0, 0.0, 2.0, 2.0]).expect("bounds"))
                .pixel_width(2)
                .pixel_height(2)
                .format(Some(EmbeddedImageFormat::Encoded(
                    FigureMediaType::Jpeg,
                )))
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
