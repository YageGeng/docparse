use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::{Bbox, PageImageError, PageTransform, Polygon};

/// Pixel layouts supported by layout and optional OCR engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PixelFormat {
    Rgb8,
}

impl PixelFormat {
    /// Returns the number of interleaved channels for this pixel format.
    fn channels(self) -> usize {
        match self {
            Self::Rgb8 => 3,
        }
    }
}

/// Unvalidated image shape and shared pixel storage.
#[derive(Debug, Clone, TypedBuilder)]
pub struct PageImageInput {
    pub width: u32,
    pub height: u32,
    pub pixel_format: PixelFormat,
    pub data: Arc<[u8]>,
}

/// A validated page image shared without copying between inference engines.
#[derive(Debug, Clone, TypedBuilder)]
pub struct PageImage {
    width: u32,
    height: u32,
    pixel_format: PixelFormat,
    data: Arc<[u8]>,
}

impl PageImage {
    /// Returns image width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Returns image height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Returns the validated interleaved pixel format.
    pub fn pixel_format(&self) -> PixelFormat {
        self.pixel_format
    }

    /// Returns shared immutable pixel bytes.
    pub fn data(&self) -> &Arc<[u8]> {
        &self.data
    }
}

impl TryFrom<PageImageInput> for PageImage {
    type Error = PageImageError;

    /// Validates checked dimensions and exact pixel buffer length.
    fn try_from(input: PageImageInput) -> Result<Self, Self::Error> {
        let width = usize::try_from(input.width).map_err(|source| {
            PageImageError::DimensionConversion {
                field: "width",
                source,
            }
        })?;
        let height = usize::try_from(input.height).map_err(|source| {
            PageImageError::DimensionConversion {
                field: "height",
                source,
            }
        })?;
        let expected = width
            .checked_mul(height)
            .and_then(|pixels| {
                pixels.checked_mul(input.pixel_format.channels())
            })
            .ok_or(PageImageError::ArithmeticOverflow)?;
        if input.data.len() != expected {
            return Err(PageImageError::BufferLength {
                expected,
                actual: input.data.len(),
            });
        }
        Ok(Self::builder()
            .width(input.width)
            .height(input.height)
            .pixel_format(input.pixel_format)
            .data(input.data)
            .build())
    }
}

/// Normalized PP-DocLayoutV3 labels plus forward-compatible unknown labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutLabel {
    Abstract,
    Algorithm,
    AsideText,
    Chart,
    Content,
    DisplayFormula,
    DocTitle,
    FigureTitle,
    Footer,
    FooterImage,
    Footnote,
    FormulaNumber,
    Header,
    HeaderImage,
    Image,
    InlineFormula,
    Number,
    ParagraphTitle,
    Reference,
    ReferenceContent,
    Seal,
    Table,
    Text,
    VerticalText,
    VisionFootnote,
    Unknown(String),
}

impl From<&str> for LayoutLabel {
    /// Maps known raw labels while preserving any future model label verbatim.
    fn from(value: &str) -> Self {
        match value {
            "abstract" => Self::Abstract,
            "algorithm" => Self::Algorithm,
            "aside_text" => Self::AsideText,
            "chart" => Self::Chart,
            "content" => Self::Content,
            "display_formula" => Self::DisplayFormula,
            "doc_title" => Self::DocTitle,
            "figure_title" => Self::FigureTitle,
            "footer" => Self::Footer,
            "footer_image" => Self::FooterImage,
            "footnote" => Self::Footnote,
            "formula_number" => Self::FormulaNumber,
            "header" => Self::Header,
            "header_image" => Self::HeaderImage,
            "image" => Self::Image,
            "inline_formula" => Self::InlineFormula,
            "number" => Self::Number,
            "paragraph_title" => Self::ParagraphTitle,
            "reference" => Self::Reference,
            "reference_content" => Self::ReferenceContent,
            "seal" => Self::Seal,
            "table" => Self::Table,
            "text" => Self::Text,
            "vertical_text" => Self::VerticalText,
            "vision_footnote" => Self::VisionFootnote,
            unknown => Self::Unknown(unknown.to_owned()),
        }
    }
}

impl From<String> for LayoutLabel {
    /// Maps an owned raw label without losing unknown text.
    fn from(value: String) -> Self {
        Self::from(value.as_str())
    }
}

/// Records whether polygon geometry is factual or derived from a bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeometrySource {
    ModelPolygon,
    DerivedFromBbox,
}

/// Stable, deterministic metadata reported by a concrete inference engine.
pub type EngineMetadata = BTreeMap<String, String>;

/// One layout model detection before page-local ownership fusion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct LayoutDetection {
    pub source_detection_index: u32,
    pub raw_label: String,
    pub class_id: i64,
    pub label: LayoutLabel,
    pub confidence: f64,
    pub bbox: Bbox,
    #[builder(default)]
    pub polygon: Option<Polygon>,
    pub geometry_source: GeometrySource,
    pub model_order: i64,
    pub metadata: EngineMetadata,
}

/// Immutable input passed to a page layout engine.
#[derive(Debug, Clone, TypedBuilder)]
pub struct LayoutRequest {
    pub page_number: u32,
    pub image: Arc<PageImage>,
    pub transform: PageTransform,
}
