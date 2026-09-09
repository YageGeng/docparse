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
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
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
}

impl ModelRegionId {
    /// Builds a model region ID from the unfiltered source row index.
    pub fn detected(page_number: u32, source_detection_index: u32) -> Self {
        Self(format!("p{page_number}:m{source_detection_index}"))
    }
}

/// Stable recursive XY-cut path independent of temporary vector positions.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LabelSource {
    Model,
    Pdf,
    Heuristic,
    Fallback,
}

/// Origin of one text fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextSource {
    Native,
    Ocr,
}

/// Positive watermark evidence; absence leaves an ordinary text fact eligible for fusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WatermarkSource {
    PdfMarkedContent,
    TextPattern,
}

/// Final inline ordering direction for a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WritingDirection {
    LeftToRight,
    RightToLeft,
    Vertical,
}

/// Explicit text repair evidence retained beside the original text fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepairAction {
    RemovedControl,
    EncodedHyphen,
    MergedFragment,
}

/// Completeness of text located beneath one inline formula region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InlineContentStatus {
    Complete,
    Partial,
    Missing,
}

/// Half-open text-item ordinal range inside one final line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Baseline {
    pub start: Point,
    pub end: Point,
}

/// Generic deterministic evidence attached to a result decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct Evidence {
    pub kind: String,
    #[builder(default)]
    pub score: Option<f64>,
    #[builder(default)]
    pub details: BTreeMap<String, String>,
}

/// Original model or fallback region geometry retained beside final content bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
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

/// Rich font and paint facts aggregated over one text item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
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

/// Whether PDFium could map source character codes to Unicode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnicodeMappingStatus {
    Complete,
    Partial,
    Missing,
}

/// Stable PDF provenance that excludes temporary handles and pointer values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
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

/// A non-owning inline formula annotation attached to one line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct Block {
    pub id: BlockId,
    pub label: LayoutLabel,
    pub text: String,
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
    pub semantic_hints: BTreeMap<String, String>,
    pub lines: Vec<Line>,
    /// Non-owning table cells; absent when the layout is not a confidently recovered table.
    #[builder(default)]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<crate::Table>,
}

/// Canonical text projection applied between two non-empty physical Lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockTextBoundary {
    Space,
    Newline,
    PreserveHyphen,
}

impl Block {
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
        // Schema-2 results produced before cell reconstruction joined table fragments
        // with spaces. Keep those stored documents valid while new unresolved tables
        // retain line breaks and structured tables use their explicit cell projection.
        if self.label == LayoutLabel::Table
            && self.text
                == self
                    .lines
                    .iter()
                    .map(|line| line.text.trim())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ")
        {
            return true;
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageWarning {
    pub code: String,
    pub stage: String,
    pub message: String,
}

/// Stable page failure that never embeds local paths or source bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
pub struct PageError {
    pub page_number: u32,
    pub stage: String,
    pub code: String,
    pub message: String,
}

/// One page's canonical nested result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct PageResult {
    pub page_number: u32,
    pub width: f64,
    pub height: f64,
    pub rotation: i32,
    pub blocks: Vec<Block>,
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TypedBuilder)]
pub struct NodeRef {
    pub page_number: u32,
    pub block_id: BlockId,
    #[builder(default)]
    pub line_id: Option<LineId>,
}

/// Supported document-level relation categories.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize,
)]
pub enum RelationKind {
    RepeatedChrome,
    ParagraphContinuation,
    HeadingHierarchy,
    TableContinuationCandidate,
}

/// One deterministic non-owning document relation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocumentRelations {
    pub relations: Vec<DocumentRelation>,
}

/// Canonical complete document aggregate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TypedBuilder)]
pub struct DocumentResult {
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
