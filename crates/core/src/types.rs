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
    #[serde(default)]
    pub underline: bool,
    #[builder(default)]
    #[serde(default)]
    pub strikeout: bool,
    #[builder(default)]
    #[serde(default)]
    pub superscript: bool,
    #[builder(default)]
    #[serde(default)]
    pub subscript: bool,
    #[builder(default)]
    #[serde(default)]
    pub baseline_shift: Option<f64>,
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

    /// Reports whether this style has an underline.
    pub fn is_underline(&self) -> bool {
        self.underline
    }

    /// Reports whether this style has a strikeout line.
    pub fn is_strikeout(&self) -> bool {
        self.strikeout
    }

    /// Reports whether this style is a superscript run.
    pub fn is_superscript(&self) -> bool {
        self.superscript
    }

    /// Reports whether this style is a subscript run.
    pub fn is_subscript(&self) -> bool {
        self.subscript
    }

    /// Reports whether this style is either a superscript or subscript run.
    pub fn is_script(&self) -> bool {
        self.superscript || self.subscript
    }

    /// Returns the effective physical font size, preferring scaled font height over unscaled base size.
    pub fn effective_font_size(&self) -> Option<f64> {
        self.font_height.or(self.font_size)
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
    #[serde(default)]
    pub char_codes: Vec<u32>,
    #[builder(default)]
    #[serde(default)]
    pub mcid: Option<i32>,
    #[builder(default)]
    #[serde(default)]
    pub text_object_index: Option<u32>,
    pub unicode_mapping: UnicodeMappingStatus,
    #[builder(default)]
    #[serde(default)]
    pub generated_space: bool,
    #[builder(default)]
    #[serde(default)]
    pub link: Option<String>,
    #[builder(default)]
    #[serde(default)]
    pub strike: bool,
    #[builder(default)]
    #[serde(default)]
    pub underline: bool,
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

    /// Reports whether this text item has underline decoration.
    pub fn is_underline(&self) -> bool {
        self.style.as_ref().is_some_and(|s| s.underline)
            || self.provenance.as_ref().is_some_and(|p| p.underline)
    }

    /// Sets the underline decoration flag on this item's style and provenance.
    pub fn set_underline(&mut self, underline: bool) {
        if let Some(style) = &mut self.style {
            style.underline = underline;
        } else if underline {
            self.style = Some(TextStyle::builder().underline(true).build());
        }
        if let Some(prov) = &mut self.provenance {
            prov.underline = underline;
        }
    }

    /// Reports whether this text item has strikeout decoration.
    pub fn is_strikeout(&self) -> bool {
        self.style.as_ref().is_some_and(|s| s.strikeout)
            || self.provenance.as_ref().is_some_and(|p| p.strike)
    }

    /// Sets the strikeout decoration flag on this item's style and provenance.
    pub fn set_strikeout(&mut self, strikeout: bool) {
        if let Some(style) = &mut self.style {
            style.strikeout = strikeout;
        } else if strikeout {
            self.style = Some(TextStyle::builder().strikeout(true).build());
        }
        if let Some(prov) = &mut self.provenance {
            prov.strike = strikeout;
        }
    }

    /// Reports whether this text item is a superscript run.
    pub fn is_superscript(&self) -> bool {
        self.style.as_ref().is_some_and(|s| s.superscript)
    }

    /// Reports whether this text item is a subscript run.
    pub fn is_subscript(&self) -> bool {
        self.style.as_ref().is_some_and(|s| s.subscript)
    }

    /// Reports whether this text item is either a superscript or subscript run.
    pub fn is_script(&self) -> bool {
        self.style.as_ref().is_some_and(|s| s.is_script())
    }

    /// Reports whether this text item has bold styling.
    pub fn is_bold(&self) -> bool {
        self.style.as_ref().is_some_and(TextStyle::is_bold)
    }

    /// Reports whether this text item has italic styling.
    pub fn is_italic(&self) -> bool {
        self.style.as_ref().is_some_and(|s| s.italic)
    }

    /// Returns the baseline shift in points if this item is a super/subscript run.
    pub fn baseline_shift(&self) -> Option<f64> {
        self.style.as_ref().and_then(|s| s.baseline_shift)
    }

    /// Returns the effective physical font size of this item, falling back to the bounding box height.
    pub fn effective_font_size(&self) -> f64 {
        self.style
            .as_ref()
            .and_then(TextStyle::effective_font_size)
            .unwrap_or_else(|| self.bbox.height().max(1.0))
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

    /// Returns the dominant font size among items in this line.
    pub fn dominant_font_size(&self) -> f64 {
        self.text_items
            .iter()
            .map(TextItem::effective_font_size)
            .fold(0.0_f64, f64::max)
    }

    /// Evaluates whether an item acts as a superscript or subscript using precalculated line metrics.
    pub(crate) fn evaluate_script_tag(
        &self,
        item: &TextItem,
        dominant_size: f64,
        line_base_y: f64,
    ) -> Option<&'static str> {
        if item.is_superscript() {
            return Some("sup");
        }
        if item.is_subscript() {
            return Some("sub");
        }
        if dominant_size <= 0.0 {
            return None;
        }
        let font_size = item.effective_font_size();
        if font_size / dominant_size > 0.85 {
            return None;
        }
        let item_base_y = item
            .baseline
            .map(|b| (b.start.y + b.end.y) * 0.5)
            .unwrap_or(item.bbox.bottom);
        let shift = line_base_y - item_base_y;
        let threshold = dominant_size * 0.05;
        if shift > threshold {
            Some("sup")
        } else if shift < -threshold {
            Some("sub")
        } else {
            None
        }
    }

    /// Checks whether an item at `index` acts as a superscript or subscript within this line.
    pub fn item_script_tag(&self, index: usize) -> Option<&'static str> {
        let item = self.text_items.get(index)?;
        let dominant_size = self.dominant_font_size();
        let line_base_y = self
            .baseline
            .map(|b| (b.start.y + b.end.y) * 0.5)
            .unwrap_or(self.bbox.bottom);
        self.evaluate_script_tag(item, dominant_size, line_base_y)
    }

    /// Formats line text with optional inline markdown and HTML formatting for styles and decorations.
    ///
    /// Following the decoration and formatting hierarchy from `pdf-inspector`:
    /// - Underline: `<u>...</u>`
    /// - Strikeout: `<s>...</s>` (exclusive with underline; strikeout takes priority if both are present)
    /// - Superscript: `<sup>...</sup>`
    /// - Subscript: `<sub>...</sub>`
    /// - Bold: `**...**` (omitted if inside underline or strikeout for clean tag nesting)
    /// - Italic: `*...*` (omitted if inside underline or strikeout for clean tag nesting)
    pub fn text_with_formatting(
        &self,
        format_bold: bool,
        format_italic: bool,
        format_decorations: bool,
        format_scripts: bool,
    ) -> String {
        if !format_bold
            && !format_italic
            && !format_decorations
            && !format_scripts
        {
            return self.text.clone();
        }

        let (dominant_size, line_base_y) = if format_scripts {
            let size = self.dominant_font_size();
            let base_y = self
                .baseline
                .map(|b| (b.start.y + b.end.y) * 0.5)
                .unwrap_or(self.bbox.bottom);
            (size, base_y)
        } else {
            (0.0, 0.0)
        };

        let capacity = self
            .text_items
            .iter()
            .map(|item| item.raw_text.len().saturating_add(8))
            .sum();
        let mut result = String::with_capacity(capacity);
        let mut current_bold = false;
        let mut current_italic = false;
        let mut current_underline = false;
        let mut current_strikeout = false;

        for item in &self.text_items {
            let text = &item.raw_text;
            if text.is_empty() {
                continue;
            }

            let script_tag = if format_scripts {
                self.evaluate_script_tag(item, dominant_size, line_base_y)
            } else {
                None
            };
            let is_script = script_tag.is_some();
            let own_strikeout = format_decorations && item.is_strikeout();
            let own_underline =
                format_decorations && item.is_underline() && !own_strikeout;
            let own_bold = format_bold
                && item.is_bold()
                && !own_underline
                && !own_strikeout;
            let own_italic = format_italic
                && item.is_italic()
                && !own_underline
                && !own_strikeout;

            // Script items inherit whatever font style (bold/italic) is open around them,
            // but use their own drawn ink decorations (underline/strikeout).
            let (item_strikeout, item_underline, item_bold, item_italic) =
                if is_script {
                    (own_strikeout, own_underline, current_bold, current_italic)
                } else {
                    (own_strikeout, own_underline, own_bold, own_italic)
                };

            // Close previous styles if they change
            if current_italic && !item_italic {
                result.push('*');
                current_italic = false;
            }
            if current_bold && !item_bold {
                result.push_str("**");
                current_bold = false;
            }
            if current_underline && !item_underline {
                result.push_str("</u>");
                current_underline = false;
            }
            if current_strikeout && !item_strikeout {
                result.push_str("</s>");
                current_strikeout = false;
            }

            // Open new styles
            if item_underline && !current_underline {
                result.push_str("<u>");
                current_underline = true;
            }
            if item_strikeout && !current_strikeout {
                result.push_str("<s>");
                current_strikeout = true;
            }
            if item_bold && !current_bold {
                result.push_str("**");
                current_bold = true;
            }
            if item_italic && !current_italic {
                result.push('*');
                current_italic = true;
            }

            if let Some(tag) = script_tag {
                result.push('<');
                result.push_str(tag);
                result.push('>');
                result.push_str(text);
                result.push_str("</");
                result.push_str(tag);
                result.push('>');
            } else {
                result.push_str(text);
            }
        }

        // Close any remaining open styles
        if current_italic {
            result.push('*');
        }
        if current_bold {
            result.push_str("**");
        }
        if current_underline {
            result.push_str("</u>");
        }
        if current_strikeout {
            result.push_str("</s>");
        }

        result
    }

    /// Formats line text with full inline markdown and HTML formatting tags.
    pub fn formatted_text(&self) -> String {
        self.text_with_formatting(true, true, true, true)
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

    /// Verifies line text formatting correctly applies HTML/Markdown tags for styles, decorations, and scripts.
    #[test]
    fn line_text_with_formatting_applies_tags() {
        use crate::{TextItem, TextItemId, TextSource, TextStyle};

        let block_id = BlockId::model(1, 0, 0);
        let bbox = Bbox::try_from([0.0, 0.0, 100.0, 10.0]).expect("bbox");

        let items = vec![
            TextItem::builder()
                .id(TextItemId::native(1, 0))
                .raw_text("Normal ".to_owned())
                .bbox(bbox)
                .source(TextSource::Native)
                .build(),
            TextItem::builder()
                .id(TextItemId::native(1, 1))
                .raw_text("Bold ".to_owned())
                .bbox(bbox)
                .source(TextSource::Native)
                .style(Some(TextStyle::builder().bold(true).build()))
                .build(),
            TextItem::builder()
                .id(TextItemId::native(1, 2))
                .raw_text("Underline ".to_owned())
                .bbox(bbox)
                .source(TextSource::Native)
                .style(Some(TextStyle::builder().underline(true).build()))
                .build(),
            TextItem::builder()
                .id(TextItemId::native(1, 3))
                .raw_text("Struck ".to_owned())
                .bbox(bbox)
                .source(TextSource::Native)
                .style(Some(TextStyle::builder().strikeout(true).build()))
                .build(),
            TextItem::builder()
                .id(TextItemId::native(1, 4))
                .raw_text("Super".to_owned())
                .bbox(bbox)
                .source(TextSource::Native)
                .style(Some(TextStyle::builder().superscript(true).build()))
                .build(),
            TextItem::builder()
                .id(TextItemId::native(1, 5))
                .raw_text(" and ".to_owned())
                .bbox(bbox)
                .source(TextSource::Native)
                .build(),
            TextItem::builder()
                .id(TextItemId::native(1, 6))
                .raw_text("Sub".to_owned())
                .bbox(bbox)
                .source(TextSource::Native)
                .style(Some(TextStyle::builder().subscript(true).build()))
                .build(),
        ];

        let line = Line::builder()
            .id(LineId::new(&block_id, 0))
            .text("Normal Bold Underline Struck Super and Sub".to_owned())
            .bbox(bbox)
            .direction(WritingDirection::LeftToRight)
            .text_items(items)
            .build();

        // Plain text when all formatting is disabled
        assert_eq!(
            line.text_with_formatting(false, false, false, false),
            "Normal Bold Underline Struck Super and Sub"
        );

        // Decorations only
        assert_eq!(
            line.text_with_formatting(false, false, true, false),
            "Normal Bold <u>Underline </u><s>Struck </s>Super and Sub"
        );

        // Scripts only
        assert_eq!(
            line.text_with_formatting(false, false, false, true),
            "Normal Bold Underline Struck <sup>Super</sup> and <sub>Sub</sub>"
        );

        // All formatting enabled
        assert_eq!(
            line.formatted_text(),
            "Normal **Bold **<u>Underline </u><s>Struck </s><sup>Super</sup> and <sub>Sub</sub>"
        );
    }
}
