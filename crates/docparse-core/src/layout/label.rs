//! Layout label definitions.

/// Semantic label emitted by a PP-StructureV3 layout model.
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    serde::Serialize,
    serde::Deserialize,
)]
pub enum LayoutLabel {
    /// Abstract section.
    Abstract,
    /// Algorithm block.
    Algorithm,
    /// Aside text.
    AsideText,
    /// Chart region.
    Chart,
    /// Main content section.
    Content,
    /// Display formula region.
    DisplayFormula,
    /// Document title.
    DocTitle,
    /// Paragraph title.
    ParagraphTitle,
    /// Figure title.
    FigureTitle,
    /// Page footer.
    Footer,
    /// Footer image.
    FooterImage,
    /// Footnote region.
    Footnote,
    /// Formula number.
    FormulaNumber,
    /// Page header.
    Header,
    /// Header image.
    HeaderImage,
    /// Embedded image region.
    Image,
    /// Inline formula region.
    InlineFormula,
    /// Page number or numeric marker.
    Number,
    /// Reference section.
    Reference,
    /// Reference content.
    ReferenceContent,
    /// Seal or stamp.
    Seal,
    /// Table region.
    Table,
    /// Body text region.
    Text,
    /// Vertical text region.
    VerticalText,
    /// Vision footnote region.
    VisionFootnote,
    /// Unknown or unsupported label.
    Unknown,
}

impl LayoutLabel {
    /// Converts a model class ID into a typed layout label.
    #[must_use]
    pub fn from_class_id(class_id: i64) -> Self {
        match class_id {
            0 => Self::Abstract,
            1 => Self::Algorithm,
            2 => Self::AsideText,
            3 => Self::Chart,
            4 => Self::Content,
            5 => Self::DisplayFormula,
            6 => Self::DocTitle,
            7 => Self::FigureTitle,
            8 => Self::Footer,
            9 => Self::FooterImage,
            10 => Self::Footnote,
            11 => Self::FormulaNumber,
            12 => Self::Header,
            13 => Self::HeaderImage,
            14 => Self::Image,
            15 => Self::InlineFormula,
            16 => Self::Number,
            17 => Self::ParagraphTitle,
            18 => Self::Reference,
            19 => Self::ReferenceContent,
            20 => Self::Seal,
            21 => Self::Table,
            22 => Self::Text,
            23 => Self::VerticalText,
            24 => Self::VisionFootnote,
            _ => Self::Unknown,
        }
    }

    /// Returns the stable display color used when drawing this label in clients.
    #[must_use]
    pub fn color(self) -> &'static str {
        match self {
            Self::Abstract => "#7c3aed",
            Self::Algorithm => "#0891b2",
            Self::AsideText => "#64748b",
            Self::Chart => "#db2777",
            Self::Content => "#475569",
            Self::DisplayFormula => "#9333ea",
            Self::DocTitle => "#dc2626",
            Self::ParagraphTitle => "#c2410c",
            Self::FigureTitle => "#f97316",
            Self::Footer => "#6b7280",
            Self::FooterImage => "#a16207",
            Self::Footnote => "#4b5563",
            Self::FormulaNumber => "#8b5cf6",
            Self::Header => "#374151",
            Self::HeaderImage => "#ca8a04",
            Self::Image => "#ea580c",
            Self::InlineFormula => "#a855f7",
            Self::Number => "#0f766e",
            Self::Reference => "#0369a1",
            Self::ReferenceContent => "#0284c7",
            Self::Seal => "#e11d48",
            Self::Table => "#16a34a",
            Self::Text => "#2563eb",
            Self::VerticalText => "#4f46e5",
            Self::VisionFootnote => "#52525b",
            Self::Unknown => "#111827",
        }
    }
}

impl TryFrom<&str> for LayoutLabel {
    type Error = String;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        // Keep this parser aligned with serde's unit enum names so the WASM UI
        // can turn JSON layout labels back into typed Rust labels for coloring.
        match value {
            "Abstract" => Ok(Self::Abstract),
            "Algorithm" => Ok(Self::Algorithm),
            "AsideText" => Ok(Self::AsideText),
            "Chart" => Ok(Self::Chart),
            "Content" => Ok(Self::Content),
            "DisplayFormula" => Ok(Self::DisplayFormula),
            "DocTitle" => Ok(Self::DocTitle),
            "ParagraphTitle" => Ok(Self::ParagraphTitle),
            "FigureTitle" => Ok(Self::FigureTitle),
            "Footer" => Ok(Self::Footer),
            "FooterImage" => Ok(Self::FooterImage),
            "Footnote" => Ok(Self::Footnote),
            "FormulaNumber" => Ok(Self::FormulaNumber),
            "Header" => Ok(Self::Header),
            "HeaderImage" => Ok(Self::HeaderImage),
            "Image" => Ok(Self::Image),
            "InlineFormula" => Ok(Self::InlineFormula),
            "Number" => Ok(Self::Number),
            "Reference" => Ok(Self::Reference),
            "ReferenceContent" => Ok(Self::ReferenceContent),
            "Seal" => Ok(Self::Seal),
            "Table" => Ok(Self::Table),
            "Text" => Ok(Self::Text),
            "VerticalText" => Ok(Self::VerticalText),
            "VisionFootnote" => Ok(Self::VisionFootnote),
            "Unknown" => Ok(Self::Unknown),
            _ => Err(format!("unknown layout label: {value}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::LayoutLabel;

    #[test]
    fn known_class_ids_map_to_labels() {
        assert_eq!(LayoutLabel::from_class_id(0), LayoutLabel::Abstract);
        assert_eq!(LayoutLabel::from_class_id(14), LayoutLabel::Image);
        assert_eq!(LayoutLabel::from_class_id(24), LayoutLabel::VisionFootnote);
    }

    #[test]
    fn unknown_class_ids_map_to_unknown() {
        assert_eq!(LayoutLabel::from_class_id(25), LayoutLabel::Unknown);
        assert_eq!(LayoutLabel::from_class_id(999), LayoutLabel::Unknown);
    }

    #[test]
    fn labels_have_stable_distinct_display_colors() {
        assert_eq!(LayoutLabel::Text.color(), "#2563eb");
        assert_eq!(LayoutLabel::Table.color(), "#16a34a");
        assert_eq!(LayoutLabel::Image.color(), "#ea580c");
        assert_ne!(LayoutLabel::Text.color(), LayoutLabel::Table.color());
    }

    #[test]
    fn labels_can_be_parsed_from_serialized_names() {
        assert_eq!("Text".try_into(), Ok(LayoutLabel::Text));
        assert_eq!("Table".try_into(), Ok(LayoutLabel::Table));
        let parsed: Result<LayoutLabel, _> = "NotALabel".try_into();
        parsed.expect_err("unknown label should be rejected");
    }
}
