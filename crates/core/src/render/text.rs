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
            pages.push(lines.join("\n"));
        }
        pages.join("\n\u{000c}\n")
    }
}
