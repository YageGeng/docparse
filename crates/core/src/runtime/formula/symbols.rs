//! Structured mapping table translating Unicode mathematical symbols to LaTeX syntax.

/// Category of a mathematical or typographical symbol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MathSymbolCategory {
    /// Historical TeX Computer Modern font encoding anomalies.
    EncodingAnomaly,
    /// Greek alphabetic letter.
    GreekLetter,
    /// Mathematical operator or calculus symbol.
    Operator,
    /// Comparison, set, or relational symbol.
    Relation,
}

/// A structured entry mapping a Unicode character to its LaTeX representation and category.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct MathCharMapping {
    pub(crate) character: char,
    pub(crate) latex: &'static str,
    pub(crate) category: MathSymbolCategory,
}

impl MathCharMapping {
    /// Creates a new math character mapping entry.
    pub(crate) const fn new(
        character: char,
        latex: &'static str,
        category: MathSymbolCategory,
    ) -> Self {
        Self {
            character,
            latex,
            category,
        }
    }

    /// Finds the structured mapping entry for a Unicode character via binary search.
    pub(crate) fn find(c: char) -> Option<&'static Self> {
        MATH_CHAR_MAPPINGS
            .binary_search_by_key(&c, |entry| entry.character)
            .ok()
            .and_then(|idx| MATH_CHAR_MAPPINGS.get(idx))
    }

    /// Translates a single Unicode math symbol, TeXCM bugged character, or Greek letter to LaTeX syntax.
    pub(crate) fn to_latex(c: char) -> Option<&'static str> {
        Self::find(c).map(|mapping| mapping.latex)
    }
}

/// Static sorted table of Unicode math symbol to LaTeX translations.
pub(crate) static MATH_CHAR_MAPPINGS: &[MathCharMapping] = &[
    MathCharMapping::new('±', "\\pm ", MathSymbolCategory::Operator),
    MathCharMapping::new('·', "\\cdot ", MathSymbolCategory::Operator),
    MathCharMapping::new('¼', "=", MathSymbolCategory::EncodingAnomaly),
    MathCharMapping::new('½', "-", MathSymbolCategory::EncodingAnomaly),
    MathCharMapping::new('×', "\\times ", MathSymbolCategory::Operator),
    MathCharMapping::new('Þ', ")", MathSymbolCategory::EncodingAnomaly),
    MathCharMapping::new('ð', "(", MathSymbolCategory::EncodingAnomaly),
    MathCharMapping::new('÷', "\\div ", MathSymbolCategory::Operator),
    MathCharMapping::new('þ', "+", MathSymbolCategory::EncodingAnomaly),
    MathCharMapping::new('Γ', "\\Gamma ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('Δ', "\\Delta ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('Θ', "\\Theta ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('Λ', "\\Lambda ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('Σ', "\\Sigma ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('Φ', "\\Phi ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('Ω', "\\Omega ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('α', "\\alpha ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('β', "\\beta ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('γ', "\\gamma ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('δ', "\\delta ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('ε', "\\epsilon ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('θ', "\\theta ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('λ', "\\lambda ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('μ', "\\mu ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('π', "\\pi ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('σ', "\\sigma ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('τ', "\\tau ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('φ', "\\phi ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('ω', "\\omega ", MathSymbolCategory::GreekLetter),
    MathCharMapping::new('←', "\\gets ", MathSymbolCategory::Relation),
    MathCharMapping::new('→', "\\to ", MathSymbolCategory::Relation),
    MathCharMapping::new('⇒', "\\Rightarrow ", MathSymbolCategory::Relation),
    MathCharMapping::new('∂', "\\partial ", MathSymbolCategory::Operator),
    MathCharMapping::new('∇', "\\nabla ", MathSymbolCategory::Operator),
    MathCharMapping::new('∈', "\\in ", MathSymbolCategory::Relation),
    MathCharMapping::new('∉', "\\notin ", MathSymbolCategory::Relation),
    MathCharMapping::new('∏', "\\prod ", MathSymbolCategory::Operator),
    MathCharMapping::new('∑', "\\sum ", MathSymbolCategory::Operator),
    MathCharMapping::new('−', "-", MathSymbolCategory::Operator),
    MathCharMapping::new('√', "\\sqrt ", MathSymbolCategory::Operator),
    MathCharMapping::new('∞', "\\infty ", MathSymbolCategory::Relation),
    MathCharMapping::new('∩', "\\cap ", MathSymbolCategory::Relation),
    MathCharMapping::new('∪', "\\cup ", MathSymbolCategory::Relation),
    MathCharMapping::new('∫', "\\int ", MathSymbolCategory::Operator),
    MathCharMapping::new('≈', "\\approx ", MathSymbolCategory::Relation),
    MathCharMapping::new('≠', "\\neq ", MathSymbolCategory::Relation),
    MathCharMapping::new('≡', "\\equiv ", MathSymbolCategory::Relation),
    MathCharMapping::new('≤', "\\le ", MathSymbolCategory::Relation),
    MathCharMapping::new('≥', "\\ge ", MathSymbolCategory::Relation),
    MathCharMapping::new('⊂', "\\subset ", MathSymbolCategory::Relation),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies math character mappings are strictly sorted by character and contain no duplicates.
    #[test]
    fn math_char_mappings_are_sorted_and_unique() {
        for (left, right) in MATH_CHAR_MAPPINGS
            .iter()
            .zip(MATH_CHAR_MAPPINGS.iter().skip(1))
        {
            assert!(
                left.character < right.character,
                "mappings must be strictly sorted: {:?} (U+{:04X}) >= {:?} (U+{:04X})",
                left.character,
                left.character as u32,
                right.character,
                right.character as u32
            );
        }
    }

    /// Verifies accurate LaTeX translations for representative symbols across categories.
    #[test]
    fn maps_symbols_accurately() {
        assert_eq!(MathCharMapping::to_latex('α'), Some("\\alpha "));
        assert_eq!(MathCharMapping::to_latex('Ω'), Some("\\Omega "));
        assert_eq!(MathCharMapping::to_latex('≤'), Some("\\le "));
        assert_eq!(MathCharMapping::to_latex('∑'), Some("\\sum "));
        assert_eq!(MathCharMapping::to_latex('¼'), Some("="));
        assert_eq!(MathCharMapping::to_latex('x'), None);
    }
}
