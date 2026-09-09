use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use typed_builder::TypedBuilder;

use crate::{
    Bbox, LayoutLabelIndexError, PageImageError, PageTransform, Polygon,
};

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

/// Fixed model labels, parser-derived semantics, and forward-compatible unknown labels.
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
    /// A parser-derived overlay, outside the fixed model class vocabulary.
    Watermark,
    Unknown(String),
}

impl LayoutLabel {
    /// Known labels in the exact numeric class order exported by PP-DocLayoutV3.
    pub const ALL: [Self; 25] = [
        Self::Abstract,
        Self::Algorithm,
        Self::AsideText,
        Self::Chart,
        Self::Content,
        Self::DisplayFormula,
        Self::DocTitle,
        Self::FigureTitle,
        Self::Footer,
        Self::FooterImage,
        Self::Footnote,
        Self::FormulaNumber,
        Self::Header,
        Self::HeaderImage,
        Self::Image,
        Self::InlineFormula,
        Self::Number,
        Self::ParagraphTitle,
        Self::Reference,
        Self::ReferenceContent,
        Self::Seal,
        Self::Table,
        Self::Text,
        Self::VerticalText,
        Self::VisionFootnote,
    ];

    /// Returns the canonical model name or the original unknown label without allocation.
    pub fn to_str(&self) -> &str {
        match self {
            Self::Abstract => "abstract",
            Self::Algorithm => "algorithm",
            Self::AsideText => "aside_text",
            Self::Chart => "chart",
            Self::Content => "content",
            Self::DisplayFormula => "display_formula",
            Self::DocTitle => "doc_title",
            Self::FigureTitle => "figure_title",
            Self::Footer => "footer",
            Self::FooterImage => "footer_image",
            Self::Footnote => "footnote",
            Self::FormulaNumber => "formula_number",
            Self::Header => "header",
            Self::HeaderImage => "header_image",
            Self::Image => "image",
            Self::InlineFormula => "inline_formula",
            Self::Number => "number",
            Self::ParagraphTitle => "paragraph_title",
            Self::Reference => "reference",
            Self::ReferenceContent => "reference_content",
            Self::Seal => "seal",
            Self::Table => "table",
            Self::Text => "text",
            Self::VerticalText => "vertical_text",
            Self::VisionFootnote => "vision_footnote",
            Self::Watermark => "watermark",
            Self::Unknown(value) => value,
        }
    }

    /// Returns the fixed-model class index; derived and unknown labels have no model index.
    pub fn idx(&self) -> Option<usize> {
        Self::ALL.iter().position(|label| label == self)
    }
}

impl TryFrom<usize> for LayoutLabel {
    type Error = LayoutLabelIndexError;

    /// Resolves a checked array index without accepting unsupported classes.
    fn try_from(index: usize) -> Result<Self, Self::Error> {
        Self::ALL
            .get(index)
            .cloned()
            .ok_or_else(|| LayoutLabelIndexError {
                index: index.to_string(),
            })
    }
}

impl TryFrom<i64> for LayoutLabel {
    type Error = LayoutLabelIndexError;

    /// Rejects negative or out-of-range model class IDs before indexing.
    fn try_from(index: i64) -> Result<Self, Self::Error> {
        let index = usize::try_from(index).map_err(|_source| {
            LayoutLabelIndexError {
                index: index.to_string(),
            }
        })?;
        Self::try_from(index)
    }
}

impl TryFrom<i32> for LayoutLabel {
    type Error = LayoutLabelIndexError;

    /// Accepts ordinary integer literals through the same checked class-ID conversion.
    fn try_from(index: i32) -> Result<Self, Self::Error> {
        Self::try_from(i64::from(index))
    }
}

impl From<&str> for LayoutLabel {
    /// Maps known raw labels while preserving any future model label verbatim.
    fn from(value: &str) -> Self {
        if value == "watermark" {
            return Self::Watermark;
        }
        Self::ALL
            .into_iter()
            .find(|label| label.to_str() == value)
            .unwrap_or_else(|| Self::Unknown(value.to_owned()))
    }
}

impl From<String> for LayoutLabel {
    /// Maps an owned raw label without losing unknown text.
    fn from(value: String) -> Self {
        if value == "watermark" {
            return Self::Watermark;
        }
        Self::ALL
            .into_iter()
            .find(|label| label.to_str() == value)
            .unwrap_or(Self::Unknown(value))
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
    /// Optional observations do not become part of detection metadata.
    #[builder(default)]
    pub timings: crate::timing::Timings,
}
