use docparse_layout::LayoutLabel;

/// Structural behavior selected from a model label without replacing that label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LabelPolicy {
    FlowText,
    Title,
    Algorithm,
    Atomic,
    Formula,
    Chrome,
    Structured,
    /// Annotation geometry without text ownership or participation in body composition.
    VisualOnly,
    Unknown,
}

impl LabelPolicy {
    /// Returns whether canonical Block text should retain physical line breaks.
    pub(crate) const fn preserves_line_breaks(self) -> bool {
        matches!(self, Self::Algorithm)
    }

    /// Returns whether canonical Block text may join an encoded line-end hyphen.
    pub(crate) const fn joins_encoded_hyphens(self) -> bool {
        matches!(self, Self::FlowText | Self::Title)
    }

    /// Trims one physical Line according to its canonical Block text policy.
    pub(crate) fn line_text(self, text: &str) -> &str {
        if self.preserves_line_breaks() {
            text.trim_end()
        } else {
            text.trim()
        }
    }
}

impl From<&LayoutLabel> for LabelPolicy {
    /// Maps every fixed PP-DocLayoutV3 label to its canonical fusion policy.
    fn from(label: &LayoutLabel) -> Self {
        match label {
            LayoutLabel::Abstract
            | LayoutLabel::AsideText
            | LayoutLabel::Content
            | LayoutLabel::Footnote
            | LayoutLabel::ReferenceContent
            | LayoutLabel::Text
            | LayoutLabel::VerticalText
            | LayoutLabel::VisionFootnote => Self::FlowText,
            LayoutLabel::DocTitle
            | LayoutLabel::FigureTitle
            | LayoutLabel::ParagraphTitle => Self::Title,
            LayoutLabel::Algorithm => Self::Algorithm,
            LayoutLabel::Chart
            | LayoutLabel::FooterImage
            | LayoutLabel::HeaderImage
            | LayoutLabel::Image
            | LayoutLabel::Seal
            | LayoutLabel::Watermark => Self::Atomic,
            LayoutLabel::DisplayFormula
            | LayoutLabel::FormulaNumber
            | LayoutLabel::InlineFormula => Self::Formula,
            LayoutLabel::Footer | LayoutLabel::Header | LayoutLabel::Number => {
                Self::Chrome
            }
            LayoutLabel::Reference => Self::VisualOnly,
            LayoutLabel::Table => Self::Structured,
            LayoutLabel::Unknown(_) => Self::Unknown,
        }
    }
}

#[cfg(test)]
mod tests {
    use docparse_layout::LayoutLabel;

    use super::LabelPolicy;

    /// Verifies every fixed model label maps to its documented canonical policy.
    #[test]
    fn fixed_labels_map_to_complete_policy_table() {
        let cases = [
            (LayoutLabel::Abstract, LabelPolicy::FlowText),
            (LayoutLabel::Algorithm, LabelPolicy::Algorithm),
            (LayoutLabel::AsideText, LabelPolicy::FlowText),
            (LayoutLabel::Chart, LabelPolicy::Atomic),
            (LayoutLabel::Content, LabelPolicy::FlowText),
            (LayoutLabel::DisplayFormula, LabelPolicy::Formula),
            (LayoutLabel::DocTitle, LabelPolicy::Title),
            (LayoutLabel::FigureTitle, LabelPolicy::Title),
            (LayoutLabel::Footer, LabelPolicy::Chrome),
            (LayoutLabel::FooterImage, LabelPolicy::Atomic),
            (LayoutLabel::Footnote, LabelPolicy::FlowText),
            (LayoutLabel::FormulaNumber, LabelPolicy::Formula),
            (LayoutLabel::Header, LabelPolicy::Chrome),
            (LayoutLabel::HeaderImage, LabelPolicy::Atomic),
            (LayoutLabel::Image, LabelPolicy::Atomic),
            (LayoutLabel::InlineFormula, LabelPolicy::Formula),
            (LayoutLabel::Number, LabelPolicy::Chrome),
            (LayoutLabel::ParagraphTitle, LabelPolicy::Title),
            (LayoutLabel::Reference, LabelPolicy::VisualOnly),
            (LayoutLabel::ReferenceContent, LabelPolicy::FlowText),
            (LayoutLabel::Seal, LabelPolicy::Atomic),
            (LayoutLabel::Table, LabelPolicy::Structured),
            (LayoutLabel::Text, LabelPolicy::FlowText),
            (LayoutLabel::VerticalText, LabelPolicy::FlowText),
            (LayoutLabel::VisionFootnote, LabelPolicy::FlowText),
        ];

        for (label, expected) in cases {
            assert_eq!(LabelPolicy::from(&label), expected);
        }
        assert_eq!(
            LabelPolicy::from(&LayoutLabel::Unknown("future".to_owned())),
            LabelPolicy::Unknown
        );
    }
}
