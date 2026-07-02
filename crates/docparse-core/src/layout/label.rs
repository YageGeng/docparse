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
}
