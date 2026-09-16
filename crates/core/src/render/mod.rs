mod json;
mod markdown;
mod overlay;
mod text;

pub use json::JsonRenderer;
pub use markdown::MarkdownRenderer;
pub use overlay::{OverlayArtifacts, OverlayRenderer};
pub use text::TextRenderer;

/// Selects literal page content or a relation-aware presentation view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderView {
    Raw,
    Semantic,
}

/// Failures produced while serializing canonical or diagnostic output.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// Canonical JSON serialization failed.
    #[error("failed to serialize document JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// RGB pixels could not be encoded as PNG.
    #[error("failed to encode overlay PNG: {0}")]
    Image(#[from] image::ImageError),
    /// An output buffer rejected encoded PNG bytes.
    #[error("failed to write overlay bytes: {0}")]
    Io(#[from] std::io::Error),
    /// In-memory SVG formatting failed unexpectedly.
    #[error("failed to format overlay SVG: {0}")]
    Format(#[from] std::fmt::Error),
    /// The PDFium overlay source failed before model-independent diagnostics rendered.
    #[error("failed to render PDF overlay source: {0}")]
    Pdfium(String),
    /// The canonical result and source PDF do not describe the same page set.
    #[error("overlay page mismatch: {0}")]
    PageMismatch(String),
}

/// Collects block IDs hidden only in the semantic rendering projection.
pub(crate) fn hidden_repeated_chrome(
    document: &crate::DocumentResult,
) -> std::collections::BTreeSet<String> {
    document
        .relations
        .relations
        .iter()
        .filter(|relation| relation.kind == crate::RelationKind::RepeatedChrome)
        .flat_map(|relation| {
            [&relation.source.block_id, &relation.target.block_id]
        })
        .map(|id| id.as_str().to_owned())
        .collect()
}

/// Renders one line in item order and inserts placeholders only for missing formulas.
pub(crate) fn render_line(
    line: &crate::Line,
    formula_placeholder: &str,
) -> String {
    let mut rendered = String::new();
    for ordinal in 0..=line.text_items.len() {
        for span in line.inline_spans.iter().filter(|span| {
            span.content_status == crate::InlineContentStatus::Missing
                && span.text_item_range.start == ordinal
        }) {
            let _ = span;
            rendered.push_str(formula_placeholder);
        }
        if let Some(item) = line.text_items.get(ordinal) {
            rendered.push_str(&item.raw_text);
        }
    }
    rendered
}

/// Applies validated byte replacements without erasing surrounding source text or splitting UTF-8.
pub(crate) fn replace_formula_ranges(
    source: &str,
    replacements: Vec<(Vec<std::ops::Range<usize>>, &str)>,
    escape_prose: bool,
) -> String {
    let mut ordered = Vec::new();
    for (mut spans, markdown) in replacements {
        spans.retain(|range| source.get(range.clone()).is_some());
        spans.sort_by_key(|range| (range.start, range.end));
        let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
        for span in spans {
            if let Some(last) = merged.last_mut()
                && (span.start <= last.end
                    || source.get(last.end..span.start).is_some_and(|gap| {
                        gap.chars().all(char::is_whitespace)
                    }))
            {
                last.end = last.end.max(span.end);
            } else {
                merged.push(span);
            }
        }
        for (index, span) in merged.into_iter().enumerate() {
            ordered.push((span, if index == 0 { markdown } else { "" }));
        }
    }
    ordered.sort_by_key(|(range, _)| (range.start, range.end));
    let mut result = String::new();
    let mut consumed = 0;
    let mut after_formula = false;
    let append_source = |output: &mut String,
                         text: &str,
                         after_formula: bool| {
        // Dollar math cannot close immediately before an ASCII word, including a PDF run with no space.
        if after_formula
            && text
                .starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '_')
        {
            output.push(' ');
        }
        for ch in text.chars() {
            if ch == '$'
                || (escape_prose
                    && matches!(ch, '\\' | '*' | '_' | '[' | ']' | '`'))
            {
                output.push('\\');
            }
            output.push(ch);
        }
    };
    for (range, markdown) in ordered {
        if range.start > range.end || source.get(range.clone()).is_none() {
            continue;
        }
        if range.start > consumed
            && let Some(prefix) = source.get(consumed..range.start)
        {
            append_source(&mut result, prefix, after_formula);
            after_formula = false;
        }
        if !markdown.is_empty() {
            // Separate opening delimiters from words and neighboring formulas without changing punctuation or CJK spacing.
            if result.ends_with(|ch: char| {
                ch.is_ascii_alphanumeric() || matches!(ch, '_' | '$')
            }) {
                result.push(' ');
            }
            result.push_str(markdown);
            after_formula = true;
        }
        consumed = consumed.max(range.end);
    }
    if let Some(suffix) = source.get(consumed..) {
        append_source(&mut result, suffix, after_formula);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::replace_formula_ranges;

    /// Inline math remains parseable beside PDF runs that omit spaces, without separating punctuation or CJK prose.
    #[test]
    #[expect(
        clippy::single_range_in_vec_init,
        reason = "Each replacement contains source byte ranges, not byte positions."
    )]
    fn inline_formula_delimiters_have_word_boundaries() {
        for (source, ranges, expected) in [
            ("useSito", vec![(vec![3..5], "$S^{i}$")], "use $S^{i}$ to"),
            ("2x3", vec![(vec![1..2], "$x$")], "2 $x$ 3"),
            ("(x),", vec![(vec![1..2], "$x$")], "($x$),"),
            ("中x文", vec![(vec![3..4], "$x$")], "中$x$文"),
            ("use x next", vec![(vec![4..5], "$x$")], "use $x$ next"),
            (
                "xy",
                vec![(vec![0..1], "$x$"), (vec![1..2], "$y$")],
                "$x$ $y$",
            ),
            ("xi.next", vec![(vec![0..1, 1..2], "$x_i$")], "$x_i$.next"),
        ] {
            assert_eq!(replace_formula_ranges(source, ranges, true), expected);
        }
    }
}
