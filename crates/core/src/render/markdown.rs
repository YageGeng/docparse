use docparse_layout::LayoutLabel;

use crate::render::{hidden_repeated_chrome, render_line};
use crate::{DocumentResult, RenderView};

/// Minimal semantic Markdown renderer that preserves canonical traversal order.
#[derive(Debug, Clone)]
pub struct MarkdownRenderer {
    view: RenderView,
    formula_placeholder: String,
}

impl MarkdownRenderer {
    /// Creates a Markdown renderer with one immutable presentation policy.
    pub fn new(
        view: RenderView,
        formula_placeholder: impl Into<String>,
    ) -> Self {
        Self {
            view,
            formula_placeholder: formula_placeholder.into(),
        }
    }

    /// Projects final blocks into Markdown without editing canonical labels or relations.
    pub fn render(&self, document: &DocumentResult) -> String {
        let hidden = if self.view == RenderView::Semantic {
            hidden_repeated_chrome(document)
        } else {
            std::collections::BTreeSet::new()
        };
        let mut output = Vec::new();
        for page in &document.pages {
            if self.view == RenderView::Raw {
                output.push(format!("<!-- page {} -->", page.page_number));
            }
            for block in &page.blocks {
                if hidden.contains(block.id.as_str()) {
                    continue;
                }
                let text = block
                    .lines
                    .iter()
                    .map(|line| render_line(line, &self.formula_placeholder))
                    .filter(|line| !line.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    continue;
                }
                let rendered = match block.label {
                    LayoutLabel::DocTitle => format!("# {text}"),
                    LayoutLabel::ParagraphTitle => format!("## {text}"),
                    LayoutLabel::FigureTitle => format!("*{text}*"),
                    _ => text,
                };
                output.push(rendered);
            }
        }
        output.join("\n\n")
    }
}
