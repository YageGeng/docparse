//! Canonical Serde types also define API schemas, avoiding a second copy of the document contract.
use std::collections::BTreeMap;
use std::fmt;

use docparse_layout::{Bbox, GeometrySource, LayoutLabel, Point, Polygon};
use serde::de::Error as DeserializeError;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use typed_builder::TypedBuilder;

use crate::label_policy::LabelPolicy;

/// Canonical schema version encoded as `major.minor` in JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaVersion {
    major: u16,
    minor: u16,
}

impl SchemaVersion {
    pub const V2_0: Self = Self::new(2, 0);

    /// Creates one in-memory schema version.
    pub const fn new(major: u16, minor: u16) -> Self {
        Self { major, minor }
    }

    /// Returns the schema major version.
    pub const fn major(self) -> u16 {
        self.major
    }

    /// Returns the schema minor version.
    pub const fn minor(self) -> u16 {
        self.minor
    }
}

impl fmt::Display for SchemaVersion {
    /// Formats the canonical dotted schema representation.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

impl Serialize for SchemaVersion {
    /// Serializes a schema version as one dotted string.
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SchemaVersion {
    /// Parses supported-major versions while allowing future 2.x minor versions.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let Some((major, minor)) = value.split_once('.') else {
            return Err(D::Error::custom(
                "schema version must use major.minor",
            ));
        };
        let major = major.parse::<u16>().map_err(|error| {
            D::Error::custom(format!("invalid schema major: {error}"))
        })?;
        let minor = minor.parse::<u16>().map_err(|error| {
            D::Error::custom(format!("invalid schema minor: {error}"))
        })?;
        if major != 2 {
            return Err(D::Error::custom(format!(
                "unsupported schema major {major}"
            )));
        }
        Ok(Self::new(major, minor))
    }
}

macro_rules! stable_id {
    ($(#[$metadata:meta])* $name:ident) => {
        $(#[$metadata])*
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, utoipa::ToSchema)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Returns the canonical stable identifier string.
            pub fn as_str(&self) -> &str {
                &self.0
            }

            /// Extracts the one-based page number encoded in the stable ID.
            pub fn page_number(&self) -> Option<u32> {
                self.0
                    .strip_prefix('p')
                    .and_then(|value| value.split(':').next())
                    .and_then(|value| value.parse().ok())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            /// Rejects empty stable identifiers while preserving their exact text.
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                if value.trim().is_empty() {
                    return Err(D::Error::custom("stable ID cannot be empty"));
                }
                Ok(Self(value))
            }
        }
    };
}

stable_id!(/// Stable identity for one native or OCR text fact.
    TextItemId);
stable_id!(/// Stable identity for one model detection row.
    ModelRegionId);
stable_id!(/// Stable identity for one residual XY-cut region.
    FallbackRegionId);
stable_id!(/// Stable identity for one final block.
    BlockId);
stable_id!(/// Stable identity for one final line.
    LineId);

impl TextItemId {
    /// Builds a native text ID from page and extraction indices.
    pub fn native(page_number: u32, extraction_index: u32) -> Self {
        Self(format!("p{page_number}:t{extraction_index}"))
    }

    /// Builds an OCR text ID from page and source result indices.
    pub fn ocr(page_number: u32, source_result_index: u32) -> Self {
        Self(format!("p{page_number}:o{source_result_index}"))
    }

    /// Retains an OCR result's identity when native overlap divides it into multiple fragments.
    pub(crate) fn ocr_fragment(&self, index: usize) -> Self {
        if index == 0 {
            self.clone()
        } else {
            Self(format!("{}:s{index}", self.0))
        }
    }
}

impl ModelRegionId {
    /// Builds a model region ID from the unfiltered source row index.
    pub fn detected(page_number: u32, source_detection_index: u32) -> Self {
        Self(format!("p{page_number}:m{source_detection_index}"))
    }
}

/// Stable recursive XY-cut path independent of temporary vector positions.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
#[serde(transparent)]
pub struct RegionPath(String);

impl RegionPath {
    /// Creates the root XY-cut path.
    pub fn root() -> Self {
        Self("r".to_owned())
    }

    /// Creates a stable horizontal child path.
    pub fn horizontal_child(&self, ordinal: u32) -> Self {
        Self(format!("{}.h{ordinal}", self.0))
    }

    /// Creates a stable vertical child path.
    pub fn vertical_child(&self, ordinal: u32) -> Self {
        Self(format!("{}.v{ordinal}", self.0))
    }

    /// Returns the canonical path string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FallbackRegionId {
    /// Builds a fallback region ID from its stable XY-cut path.
    pub fn from_path(page_number: u32, path: &RegionPath) -> Self {
        Self(format!("p{page_number}:f{}", path.as_str()))
    }
}

impl BlockId {
    /// Identifies a detached watermark without consuming a model or fallback region identity.
    pub(crate) fn watermark(
        page_number: u32,
        ordinal: u32,
        annotation: bool,
    ) -> Self {
        Self(format!(
            "p{page_number}:b:w{}:{ordinal}",
            if annotation { "a" } else { "t" }
        ))
    }

    /// Builds a model-backed block ID from source and split indices.
    pub fn model(
        page_number: u32,
        source_detection_index: u32,
        split_ordinal: u32,
    ) -> Self {
        Self(format!(
            "p{page_number}:b:m{source_detection_index}:s{split_ordinal}"
        ))
    }

    /// Builds a fallback block ID from path and split indices.
    pub fn fallback(
        page_number: u32,
        path: &RegionPath,
        split_ordinal: u32,
    ) -> Self {
        Self(format!(
            "p{page_number}:b:f{}:s{split_ordinal}",
            path.as_str()
        ))
    }
}

impl LineId {
    /// Builds a line ID as a child of one stable block ID.
    pub fn new(block_id: &BlockId, line_ordinal: u32) -> Self {
        Self(format!("{}:l{line_ordinal}", block_id.as_str()))
    }
}

/// Origin of one final block's primary semantic label.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub enum LabelSource {
    Model,
    Pdf,
    Heuristic,
    Fallback,
}

/// Origin of one text fact.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub enum TextSource {
    Native,
    Ocr,
}

/// Positive watermark evidence; absence leaves an ordinary text fact eligible for fusion.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WatermarkSource {
    PdfMarkedContent,
    TextPattern,
}

/// Final inline ordering direction for a line.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub enum WritingDirection {
    LeftToRight,
    RightToLeft,
    Vertical,
}

/// Explicit text repair evidence retained beside the original text fact.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub enum RepairAction {
    RemovedControl,
    EncodedHyphen,
    MergedFragment,
    /// A named symbol repaired an incorrect Unicode mapping; source glyph codes remain in provenance.
    GlyphNameRecovery,
    /// The embedded font's Unicode cmap recovered an otherwise untrusted glyph.
    FontCmapRecovery,
    /// A caller-supplied outline resolver recovered an otherwise untrusted glyph.
    GlyphOutlineRecovery,
    /// Geometrically coincident source glyphs were combined into one visible symbol.
    GlyphComposition,
    /// One source glyph expanded into its ordinary character sequence.
    LigatureExpansion,
    /// Typographic punctuation was folded to the extraction policy's ASCII equivalent.
    PunctuationNormalization,
    /// A gap between separate OCR words supplied an explicit canonical word boundary.
    OcrSpacing,
    /// Reliable native text replaced the matching portion of an OCR result.
    OcrNativeOverlap,
}

/// Completeness of text located beneath one inline formula region.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub enum InlineContentStatus {
    Complete,
    Partial,
    Missing,
}

/// Half-open text-item ordinal range inside one final line.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub struct TextItemRange {
    pub start: usize,
    pub end: usize,
}

impl TextItemRange {
    /// Creates a half-open text-item range validated by `ResultValidator`.
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// A baseline segment in canonical viewport coordinates.
#[derive(
    Debug, Clone, Copy, PartialEq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub struct Baseline {
    pub start: Point,
    pub end: Point,
}

/// Generic deterministic evidence attached to a result decision.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct Evidence {
    pub kind: String,
    #[builder(default)]
    pub score: Option<f64>,
    #[builder(default)]
    pub details: BTreeMap<String, String>,
}

/// Original model or fallback region geometry retained beside final content bounds.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct SourceRegionEvidence {
    /// Original semantic label, including alternate labels retained during a merge.
    #[builder(default, setter(strip_option))]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<LayoutLabel>,
    #[builder(default)]
    pub model_region_id: Option<ModelRegionId>,
    #[builder(default)]
    pub fallback_region_id: Option<FallbackRegionId>,
    pub bbox: Bbox,
    #[builder(default)]
    pub polygon: Option<Polygon>,
    pub geometry_source: GeometrySource,
    #[builder(default)]
    pub confidence: Option<f64>,
    #[builder(default)]
    pub model_order: Option<i64>,
}

/// Font-descriptor flag bits PDFium reports alongside an embedded face.
pub(crate) const FLAG_FIXED_PITCH: u32 = 1 << 0;
pub(crate) const FLAG_ITALIC: u32 = 1 << 6;
pub(crate) const FLAG_FORCE_BOLD: u32 = 1 << 18;

/// Weights at or above this are heavy. PDFium reports `-1` when it has no answer;
/// values above the OS/2 maximum can appear when the face carries no usable weight
/// class and are excluded from this range.
const BOLD_WEIGHT: u16 = 600;
const MAX_WEIGHT: u16 = 1000;

/// Reports whether the recorded weight and descriptor flags mark a face bold.
///
/// This is the single reading of that evidence: the extractor uses it to populate
/// [`TextStyle::bold`], and [`TextStyle::is_bold`] applies the same fallback for
/// results written before the flag existed.
pub(crate) fn font_evidence_is_bold(
    weight: Option<u16>,
    flags: Option<u32>,
) -> bool {
    weight.is_some_and(|value| (BOLD_WEIGHT..=MAX_WEIGHT).contains(&value))
        || flags.is_some_and(|value| value & FLAG_FORCE_BOLD != 0)
}

/// Rich font and paint facts aggregated over one text item.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct TextStyle {
    #[builder(default)]
    pub font_name: Option<String>,
    #[builder(default)]
    pub font_size: Option<f64>,
    #[builder(default)]
    pub font_height: Option<f64>,
    #[builder(default)]
    pub font_ascent: Option<f64>,
    #[builder(default)]
    pub font_descent: Option<f64>,
    #[builder(default)]
    pub font_size_estimated: bool,
    #[builder(default)]
    pub weight: Option<u16>,
    #[builder(default)]
    pub flags: Option<u32>,
    #[builder(default)]
    pub bold: bool,
    #[builder(default)]
    pub italic: bool,
    #[builder(default)]
    pub monospace: bool,
    #[builder(default)]
    pub fill_color: Option<[u8; 4]>,
    #[builder(default)]
    pub stroke_color: Option<[u8; 4]>,
    #[builder(default)]
    pub text_matrix: Option<[f64; 6]>,
}

impl TextStyle {
    /// Reports whether this style is bold, from the classifier's flag or the raw evidence.
    ///
    /// The evidence fallback keeps results that were produced before the classifier
    /// wrote `bold` deciding the same way as freshly extracted text.
    pub(crate) fn is_bold(&self) -> bool {
        self.bold || font_evidence_is_bold(self.weight, self.flags)
    }
}

/// Whether PDFium could map source character codes to Unicode.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub enum UnicodeMappingStatus {
    Complete,
    Partial,
    Missing,
}

/// Stable PDF provenance that excludes temporary handles and pointer values.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct PdfProvenance {
    #[builder(default)]
    pub char_codes: Vec<u32>,
    #[builder(default)]
    pub mcid: Option<i32>,
    #[builder(default)]
    pub text_object_index: Option<u32>,
    pub unicode_mapping: UnicodeMappingStatus,
    #[builder(default)]
    pub generated_space: bool,
    #[builder(default)]
    pub link: Option<String>,
    #[builder(default)]
    pub strike: bool,
}

/// One continuous native or OCR text fact.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct TextItem {
    pub id: TextItemId,
    pub raw_text: String,
    #[builder(default)]
    pub raw_bbox: Option<Bbox>,
    pub bbox: Bbox,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub polygon: Option<Polygon>,
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watermark: Option<WatermarkSource>,
    #[builder(default)]
    pub baseline: Option<Baseline>,
    #[builder(default)]
    pub rotation: f64,
    pub source: TextSource,
    #[builder(default)]
    pub confidence: Option<f64>,
    #[builder(default)]
    pub extraction_order: u32,
    #[builder(default)]
    pub final_order: u32,
    #[builder(default)]
    pub style: Option<TextStyle>,
    #[builder(default)]
    pub provenance: Option<PdfProvenance>,
    #[builder(default)]
    pub repair_actions: Vec<RepairAction>,
}

impl TextItem {
    /// Requests visual recovery only for strong mapping failures, preserving unusual valid prose and code.
    pub(crate) fn needs_ocr(&self) -> bool {
        if self.source != TextSource::Native || self.watermark.is_some() {
            return false;
        }
        let mut total = 0;
        let mut invalid = 0;
        for character in self.raw_text.chars().filter(|c| !c.is_whitespace()) {
            total += 1;
            if character == '\u{fffd}'
                || character.is_control()
                || matches!(character as u32, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd)
            {
                invalid += 1;
            }
        }
        // Successful glyph recovery can repair a PDF whose original ToUnicode map was missing.
        invalid > 0 && invalid * 4 >= total
            || total == 0
                && self.provenance.as_ref().is_some_and(|p| {
                    p.unicode_mapping == UnicodeMappingStatus::Missing
                })
    }
}

/// A non-owning inline formula annotation attached to one line.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct InlineSpan {
    pub label: LayoutLabel,
    #[builder(default)]
    pub confidence: Option<f64>,
    pub bbox: Bbox,
    #[builder(default)]
    pub polygon: Option<Polygon>,
    pub text_item_range: TextItemRange,
    #[builder(default)]
    pub extracted_text: Option<String>,
    pub content_status: InlineContentStatus,
}

/// One final line that exclusively owns its text items.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct Line {
    pub id: LineId,
    pub text: String,
    pub bbox: Bbox,
    #[builder(default)]
    pub baseline: Option<Baseline>,
    #[builder(default)]
    pub rotation: f64,
    pub direction: WritingDirection,
    #[builder(default)]
    pub model_region_coverage: Option<f64>,
    #[builder(default)]
    pub inline_spans: Vec<InlineSpan>,
    pub text_items: Vec<TextItem>,
}

impl Line {
    /// Derives line text by concatenating ordered source facts without inference.
    pub(crate) fn derive_text(items: &[TextItem]) -> String {
        let capacity = items.iter().map(|item| item.raw_text.len()).sum();
        let mut text = String::with_capacity(capacity);
        for item in items {
            text.push_str(&item.raw_text);
        }
        text
    }

    /// Returns whether the final visible source item is an isolated encoded hyphen.
    fn ends_with_encoded_hyphen(&self) -> bool {
        self.text.trim_end().ends_with('-')
            && self
                .text_items
                .iter()
                .rev()
                .find(|item| !item.raw_text.trim().is_empty())
                .is_some_and(|item| {
                    item.raw_text == "-"
                        && item
                            .repair_actions
                            .contains(&RepairAction::EncodedHyphen)
                })
    }
}

/// One final semantic block that exclusively owns its lines.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct Block {
    pub id: BlockId,
    pub label: LayoutLabel,
    pub text: String,
    /// Paragraph presentation with recognized inline formulas; original text and source items remain unchanged.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub markdown: Option<String>,
    #[builder(default)]
    pub raw_label: Option<String>,
    pub label_source: LabelSource,
    #[builder(default)]
    pub confidence: Option<f64>,
    pub bbox: Bbox,
    #[builder(default)]
    pub polygon: Option<Polygon>,
    #[builder(default)]
    pub source_region: Option<SourceRegionEvidence>,
    /// All contributing regions for a merged layout; empty for legacy or unmerged blocks.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_regions: Vec<SourceRegionEvidence>,
    #[builder(default)]
    pub model_region_id: Option<ModelRegionId>,
    #[builder(default)]
    pub model_order: Option<i64>,
    pub final_order: u32,
    #[builder(default)]
    pub evidence: Vec<Evidence>,
    #[builder(default)]
    /// Canonical layout hints retained even when optional evidence is hidden.
    pub semantic_hints: BTreeMap<String, String>,
    pub lines: Vec<Line>,
    /// Non-owning table cells; absent when the layout is not a confidently recovered table.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<crate::Table>,
    /// Original embedded file or page-raster crop for a figure layout.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<FigureImage>,
    /// Page embedded-image index used while the asset is still being attached.
    #[builder(default)]
    #[serde(skip)]
    pub(crate) embedded_image_index: Option<u32>,
    /// Placed image bounds applied only when they do not nest with another block.
    #[builder(default)]
    #[serde(skip)]
    pub(crate) figure_bounds: Option<Bbox>,
}

/// Whether figure bytes came from a PDF image file or from the page raster.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum FigureSource {
    Embedded,
    Raster,
}

/// Image file types PDFium can return without transcoding, plus raster PNG crops.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub enum FigureMediaType {
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/png")]
    Png,
    #[serde(rename = "image/jp2")]
    Jp2,
    #[serde(rename = "image/jpx")]
    Jpx,
}

impl FigureMediaType {
    /// Returns the canonical media type written into JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::Jp2 => "image/jp2",
            Self::Jpx => "image/jpx",
        }
    }
}

/// Exactly one place the figure bytes are delivered.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FigureDelivery {
    File { path: String },
    Inline { data_base64: String },
}

/// Pixel size and bytes for one image, chart, header, footer, or seal block.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct FigureImage {
    pub source: FigureSource,
    pub media_type: FigureMediaType,
    pub width: u32,
    pub height: u32,
    pub delivery: FigureDelivery,
}

/// An embedded PDF image not already delivered by a layout block, including small and page-sized images.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct PageImageAsset {
    pub id: String,
    pub bbox: Bbox,
    pub image: FigureImage,
}

/// Canonical text projection applied between two non-empty physical Lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockTextBoundary {
    Space,
    Newline,
    PreserveHyphen,
}

impl Block {
    /// Marks sparse merged geometry that cannot use rectangular containment for validation.
    pub(crate) const SPARSE_LAYOUT_HINT: &'static str =
        "sparse_layout_envelope";

    /// Identifies annotations kept outside body composition and content overlap diagnostics.
    pub fn is_detached(&self) -> bool {
        matches!(self.label, LayoutLabel::Reference | LayoutLabel::Watermark)
    }

    /// Visits every contributing region without counting the primary source twice.
    pub fn source_regions(
        &self,
    ) -> impl Iterator<Item = &SourceRegionEvidence> {
        self.source_region
            .iter()
            .filter(|_| self.source_regions.is_empty())
            .chain(self.source_regions.iter())
    }

    /// Selects the canonical projection for one non-empty physical-line boundary.
    fn boundary_between(
        policy: LabelPolicy,
        previous: &Line,
        next: &Line,
    ) -> BlockTextBoundary {
        if policy.preserves_line_breaks() {
            return BlockTextBoundary::Newline;
        }
        if !policy.joins_encoded_hyphens() {
            return BlockTextBoundary::Space;
        }
        let line_height =
            previous.bbox.height().max(next.bbox.height()).max(1.0);
        // A real wrapped row advances by at least half a line box; superscripts and
        // subscripts overlap the current row and must not consume its hyphen hint.
        let enters_next_row =
            next.bbox.top - previous.bbox.top >= line_height * 0.5;
        let next_character = next.text.trim().chars().next();
        let continues_hyphenated_word = previous.ends_with_encoded_hyphen()
            && previous.direction == next.direction
            && previous.direction != WritingDirection::Vertical
            && (previous.rotation - next.rotation).abs() <= 2.0
            && enters_next_row
            && next_character.is_some_and(char::is_alphabetic);
        if continues_hyphenated_word {
            BlockTextBoundary::PreserveHyphen
        } else {
            BlockTextBoundary::Space
        }
    }

    /// Derives label-aware summary text while retaining every physical source line.
    pub(crate) fn derive_text(label: &LayoutLabel, lines: &[Line]) -> String {
        let policy = LabelPolicy::from(label);
        // Structured regions require geometric spacing; source items remain unchanged for exact formula mappings.
        if policy.preserves_line_breaks() {
            let mut text = String::with_capacity(
                lines.iter().map(|line| line.text.len()).sum(),
            );
            Line::project_layout(
                lines,
                |line| line.text.as_str(),
                |new_row, spaces, body| {
                    if new_row {
                        text.push('\n');
                    }
                    text.extend(std::iter::repeat_n(' ', spaces));
                    text.push_str(body);
                },
            );
            return text;
        }
        let capacity = lines
            .iter()
            .map(|line| policy.line_text(&line.text).len())
            .sum::<usize>()
            .saturating_add(lines.len().saturating_sub(1));
        let mut text = String::with_capacity(capacity);
        let mut non_empty = lines.iter().filter_map(|line| {
            let line_text = policy.line_text(&line.text);
            (!line_text.is_empty()).then_some((line, line_text))
        });
        if let Some((first_line, first_text)) = non_empty.next() {
            text.push_str(first_text);
            let mut previous = first_line;
            for (line, line_text) in non_empty {
                match Self::boundary_between(policy, previous, line) {
                    BlockTextBoundary::Space => text.push(' '),
                    BlockTextBoundary::Newline => text.push('\n'),
                    BlockTextBoundary::PreserveHyphen => {}
                }
                text.push_str(line_text);
                previous = line;
            }
        }
        text
    }

    /// Checks stored summary text against the canonical label-aware Line projection.
    pub(crate) fn text_matches_lines(&self) -> bool {
        if let Some(table) = &self.table {
            return self.label == LayoutLabel::Table
                && self.text == table.to_text();
        }
        if LabelPolicy::from(&self.label).preserves_line_breaks() {
            // Compare new output as borrowed segments instead of building and discarding the whole summary again.
            let mut remaining = Some(self.text.as_str());
            Line::project_layout(
                &self.lines,
                |line| line.text.as_str(),
                |new_row, spaces, body| {
                    remaining = remaining.and_then(|tail| {
                        let tail = if new_row {
                            tail.strip_prefix('\n')?
                        } else {
                            tail
                        };
                        let (padding, tail) = tail.split_at_checked(spaces)?;
                        if !padding.bytes().all(|byte| byte == b' ') {
                            return None;
                        }
                        tail.strip_prefix(body)
                    });
                },
            );
            if remaining == Some("") {
                return true;
            }
            // Continue accepting stored schema-2 output whose structured text predates geometric whitespace.
            return self.text
                == self
                    .lines
                    .iter()
                    .map(|line| line.text.trim_end())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n")
                || (matches!(
                    self.label,
                    LayoutLabel::Content | LayoutLabel::Table
                ) || LabelPolicy::from(&self.label)
                    == LabelPolicy::Atomic)
                    && self.text
                        == self
                            .lines
                            .iter()
                            .map(|line| line.text.trim())
                            .filter(|text| !text.is_empty())
                            .collect::<Vec<_>>()
                            .join(" ");
        }
        let policy = LabelPolicy::from(&self.label);
        let mut offset = 0_usize;
        let mut non_empty = self
            .lines
            .iter()
            .filter_map(|line| {
                let line_text = policy.line_text(&line.text);
                (!line_text.is_empty()).then_some((line, line_text))
            })
            .peekable();
        while let Some((line, line_text)) = non_empty.next() {
            let boundary = non_empty
                .peek()
                .map(|(next, _)| Self::boundary_between(policy, line, next));
            let segment = line_text;
            let Some(segment_end) = offset.checked_add(segment.len()) else {
                return false;
            };
            if self.text.get(offset..segment_end) != Some(segment) {
                return false;
            }
            offset = segment_end;

            let separator = match boundary {
                Some(BlockTextBoundary::Space) => Some(' '),
                Some(BlockTextBoundary::Newline) => Some('\n'),
                Some(BlockTextBoundary::PreserveHyphen) | None => None,
            };
            if let Some(separator) = separator {
                let mut encoded = [0_u8; 4];
                let separator = separator.encode_utf8(&mut encoded);
                let Some(separator_end) = offset.checked_add(separator.len())
                else {
                    return false;
                };
                if self.text.get(offset..separator_end) != Some(separator) {
                    return false;
                }
                offset = separator_end;
            }
        }
        offset == self.text.len()
    }
}

/// Stable warning emitted for one page without discarding available content.
#[derive(
    Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub struct PageWarning {
    pub code: String,
    pub stage: String,
    pub message: String,
}

/// Stable page failure that never embeds local paths or source bytes.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct PageError {
    pub page_number: u32,
    pub stage: String,
    pub code: String,
    pub message: String,
}

/// Recognized mathematics with non-owning references to its original layout and text.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct FormulaResult {
    /// Actual model/backend identity, including any documented compatibility executor.
    #[builder(default)]
    #[serde(default)]
    pub engine: String,
    /// Stable identity derived from the original layout detection row.
    pub id: ModelRegionId,
    /// Original inline_formula or display_formula classification.
    pub label: LayoutLabel,
    /// Original formula extent in viewport points.
    pub bbox: Bbox,
    /// Refined recognition bounds including measured scripts; absent when the detection box is unchanged.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop_bbox: Option<Bbox>,
    /// Owning display block, prose block, or table block when matched.
    #[builder(default)]
    pub block_id: Option<BlockId>,
    /// Insertion line for inline projection; exact spans may include scripts on adjacent source lines.
    #[builder(default)]
    pub line_id: Option<LineId>,
    /// Bounding item range; text_spans supplies exact byte boundaries for mixed text runs.
    #[builder(default)]
    pub text_item_range: Option<TextItemRange>,
    /// Exact UTF-8 source slices; boundary prose in the same TextItem remains outside the replacement.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub text_spans: Vec<crate::TableTextSpan>,
    /// Zero-based row and column for formulas inside a recovered table.
    #[builder(default)]
    pub table_cell: Option<(usize, usize)>,
    /// Decoded LaTeX without outer Markdown math delimiters; null on failure.
    #[builder(default)]
    pub latex: Option<String>,
    /// LaTeX wrapped with the original inline or display math delimiters.
    #[builder(default)]
    pub markdown: Option<String>,
    /// Explicit recognition failure; original text remains available independently.
    #[builder(default)]
    pub error: Option<String>,
}

/// One page's canonical nested result.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct PageResult {
    pub page_number: u32,
    pub width: f64,
    pub height: f64,
    pub rotation: i32,
    pub blocks: Vec<Block>,
    /// Images are retained even when no layout owns them; each source placement is delivered only once.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<PageImageAsset>,
    /// Formula recognition outputs retain both LaTeX and Markdown under every JSON visibility policy.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub formulas: Vec<FormulaResult>,
    /// Original unusable PDF facts replaced by confident OCR; excluded from reading order, retained for audit.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaced_native_text: Vec<TextItem>,
    #[builder(default)]
    pub warnings: Vec<PageWarning>,
    #[builder(default)]
    pub diagnostics: BTreeMap<String, String>,
}

impl PageResult {
    /// Iterates final lines in block and line reading order.
    pub fn iter_lines(&self) -> impl Iterator<Item = &Line> {
        self.blocks.iter().flat_map(|block| block.lines.iter())
    }

    /// Iterates final text items in block, line, and item reading order.
    pub fn iter_text_items(&self) -> impl Iterator<Item = &TextItem> {
        self.iter_lines().flat_map(|line| line.text_items.iter())
    }
}

/// Immutable document-level facts shared by page analysis.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
#[builder(builder_type(name = DocumentContextDataBuilder))]
pub struct DocumentContext {
    pub page_count: u32,
    #[builder(default)]
    pub body_font_size: Option<f64>,
    #[builder(default)]
    pub repeated_header_fingerprints: Vec<String>,
    #[builder(default)]
    pub repeated_footer_fingerprints: Vec<String>,
    #[builder(default)]
    pub page_number_pattern: Option<String>,
    #[builder(default)]
    pub heading_font_sizes: Vec<f64>,
    #[builder(default)]
    pub model_revision: Option<String>,
    #[builder(default)]
    pub metadata: BTreeMap<String, String>,
}

/// Non-owning reference used by document-level relations.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct NodeRef {
    pub page_number: u32,
    pub block_id: BlockId,
    #[builder(default)]
    pub line_id: Option<LineId>,
}

/// Supported document-level relation categories.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
    utoipa::ToSchema,
)]
pub enum RelationKind {
    RepeatedChrome,
    ParagraphContinuation,
    HeadingHierarchy,
    TableContinuationCandidate,
}

/// One deterministic non-owning document relation.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct DocumentRelation {
    pub kind: RelationKind,
    pub source: NodeRef,
    pub target: NodeRef,
    #[builder(default)]
    pub score: Option<f64>,
    #[builder(default)]
    pub evidence: Vec<Evidence>,
}

/// Sidecar relations that never modify page ownership or reading order.
#[derive(
    Debug, Clone, Default, PartialEq, Serialize, Deserialize, utoipa::ToSchema,
)]
pub struct DocumentRelations {
    pub relations: Vec<DocumentRelation>,
}

/// Canonical complete document aggregate.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Serialize,
    Deserialize,
    TypedBuilder,
    utoipa::ToSchema,
)]
pub struct DocumentResult {
    // SchemaVersion has a custom string serializer rather than exposing its private numeric fields.
    #[schema(value_type = String, pattern = r"^2\.[0-9]+$", example = "2.0")]
    pub schema_version: SchemaVersion,
    pub context: DocumentContext,
    pub pages: Vec<PageResult>,
    #[builder(default)]
    pub relations: DocumentRelations,
    #[builder(default)]
    pub errors: Vec<PageError>,
}

#[cfg(test)]
mod tests {
    use docparse_layout::{Bbox, LayoutLabel};

    use super::{Block, BlockId, Line, LineId, WritingDirection};

    /// Structured labels restore geometric indentation and join fragments on the same physical row.
    #[test]
    fn structured_text_restores_horizontal_spacing() {
        let lines: Vec<_> = [
            ("begin", 10.0, 10.0),
            ("nested", 30.0, 25.0),
            ("2:", 10.0, 40.0),
            ("return", 30.0, 40.0),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (text, left, top))| {
            let mut line = line(index as u32, text);
            line.bbox = Bbox::try_from([
                left,
                top,
                left + text.chars().count() as f64 * 5.0,
                top + 10.0,
            ])
            .expect("bbox");
            line.text_items = vec![
                crate::TextItem::builder()
                    .id(crate::TextItemId::native(1, index as u32))
                    .raw_text(text.to_owned())
                    .bbox(line.bbox)
                    .source(crate::TextSource::Native)
                    .build(),
            ];
            line
        })
        .collect();
        for label in [
            LayoutLabel::Algorithm,
            LayoutLabel::Chart,
            LayoutLabel::Content,
            LayoutLabel::Table,
        ] {
            assert_eq!(
                Block::derive_text(&label, &lines),
                "begin\n    nested\n2:  return",
                "{label:?}"
            );
        }
        assert_eq!(
            Block::derive_text(&LayoutLabel::Text, &lines),
            "begin nested 2: return"
        );
    }

    /// Builds one source line for Block text derivation tests.
    fn line(index: u32, text: &str) -> Line {
        let block_id = BlockId::model(1, 0, 0);
        Line::builder()
            .id(LineId::new(&block_id, index))
            .text(text.to_owned())
            .bbox(
                Bbox::try_from([
                    0.0,
                    f64::from(index),
                    10.0,
                    f64::from(index) + 1.0,
                ])
                .expect("test line bbox must be valid"),
            )
            .direction(WritingDirection::LeftToRight)
            .text_items(Vec::new())
            .build()
    }

    /// Verifies Block summaries trim boundaries, skip blanks, and preserve internal spaces.
    #[test]
    fn block_text_uses_single_space_between_non_empty_lines() {
        let lines = vec![
            line(0, " First line "),
            line(1, "  "),
            line(2, "Second  line"),
        ];

        assert_eq!(
            Block::derive_text(&LayoutLabel::Text, &lines),
            "First line Second  line"
        );
        assert_eq!(Block::derive_text(&LayoutLabel::Text, &[]), "");
    }
}
