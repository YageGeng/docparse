use crate::render::{hidden_repeated_chrome, render_line};
use crate::{DocumentResult, RenderView};

/// Plain-text renderer over the existing canonical block and line order.
#[derive(Debug, Clone)]
pub struct TextRenderer {
    view: RenderView,
    formula_placeholder: String,
}

impl TextRenderer {
    /// Creates a plain-text renderer with one immutable presentation policy.
    pub fn new(
        view: RenderView,
        formula_placeholder: impl Into<String>,
    ) -> Self {
        Self {
            view,
            formula_placeholder: formula_placeholder.into(),
        }
    }

    /// Renders pages and blocks without reordering or modifying the source document.
    pub fn render(&self, document: &DocumentResult) -> String {
        let hidden = if self.view == RenderView::Semantic {
            hidden_repeated_chrome(document)
        } else {
            std::collections::BTreeSet::new()
        };
        let mut pages = Vec::new();
        for page in &document.pages {
            let mut lines = Vec::new();
            for block in &page.blocks {
                if hidden.contains(block.id.as_str()) {
                    continue;
                }
                if let Some(table) = &block.table {
                    lines.push(table.to_text());
                    continue;
                }
                // Spatial labels share the same median-width projection as canonical JSON and Markdown.
                if crate::label_policy::LabelPolicy::from(&block.label)
                    .preserves_line_breaks()
                {
                    lines.push(crate::Block::layout_text(
                        &block.lines,
                        false,
                        |line| render_line(line, &self.formula_placeholder),
                    ));
                    continue;
                }
                if block.label == docparse_layout::LayoutLabel::Watermark
                    && !lines.is_empty()
                {
                    lines.push(String::new());
                }
                lines.extend(
                    block
                        .lines
                        .iter()
                        .map(|line| {
                            render_line(line, &self.formula_placeholder)
                        })
                        .filter(|line| !line.is_empty()),
                );
            }
            // Remove page margins after layout is rendered, preserving internal alignment.
            pages.push(clean_rendered_text(&lines.join("\n")));
        }
        pages.join("\n\u{000c}\n")
    }
}

/// Removes NUL placeholders and common page margins without slicing through UTF-8 indentation.
fn clean_rendered_text(text: &str) -> String {
    let text = text.replace('\0', " ");
    let lines: Vec<_> = text.split('\n').collect();
    let Some(first) = lines.iter().position(|line| !line.trim().is_empty())
    else {
        return String::new();
    };
    let last = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .unwrap_or(first);
    let content = lines.get(first..=last).unwrap_or_default();
    let indent = content
        .iter()
        .filter(|line| !line.trim().is_empty())
        .map(|line| line.chars().take_while(|c| c.is_whitespace()).count())
        .min()
        .unwrap_or(0);
    content
        .iter()
        .map(|line| line.chars().skip(indent).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::clean_rendered_text;

    /// Mixed Unicode indentation and empty pages cannot panic or retain NUL sentinels.
    #[test]
    fn trims_only_common_layout_margins() {
        assert_eq!(clean_rendered_text("\n　 a\0b\n  c\n\n"), "a b\nc");
        assert_eq!(clean_rendered_text("\n \n"), "");
        assert_eq!(clean_rendered_text("  a\n    b\n"), "a\n  b");
    }
}
