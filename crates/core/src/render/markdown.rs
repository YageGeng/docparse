use docparse_layout::LayoutLabel;

use crate::label_policy::LabelPolicy;
use crate::render::{hidden_repeated_chrome, render_line};
use crate::{Block, DocumentResult, RenderView};

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
        let mut output: Vec<String> = Vec::new();
        for page in &document.pages {
            let mut previous_prose = false;
            if self.view == RenderView::Raw {
                output.push(format!("<!-- page {} -->", page.page_number));
            }
            for block in &page.blocks {
                if hidden.contains(block.id.as_str()) {
                    continue;
                }
                if let Some(table) = &block.table {
                    output.push(table.to_markdown());
                    previous_prose = false;
                    continue;
                }
                // Prose cleanup is a presentation projection; source lines and table ranges stay unchanged.
                let prose = self.view == RenderView::Semantic
                    && matches!(
                        LabelPolicy::from(&block.label),
                        LabelPolicy::FlowText | LabelPolicy::Title
                    )
                    && block.lines.iter().all(|line| {
                        line.inline_spans.is_empty()
                            && line.direction
                                != crate::WritingDirection::Vertical
                    });
                let text = block
                    .render_markdown_text(&self.formula_placeholder, prose);
                if text.is_empty() {
                    previous_prose = false;
                    continue;
                }
                let rendered = match block.label {
                    LayoutLabel::DocTitle => format!("# {text}"),
                    LayoutLabel::ParagraphTitle => format!("## {text}"),
                    LayoutLabel::FigureTitle => format!("*{text}*"),
                    _ => text,
                };
                let body = prose
                    && matches!(
                        LabelPolicy::from(&block.label),
                        LabelPolicy::FlowText
                    );
                // Heal adjacent body blocks using the same boundary rule as physical lines.
                if body
                    && previous_prose
                    && let Some(previous) = output.last_mut()
                    && is_soft_hyphen_break(previous, &rendered)
                {
                    let _ = previous.pop();
                    previous.push_str(&rendered);
                } else {
                    output.push(rendered);
                }
                previous_prose = body;
            }
        }
        output.join("\n\n")
    }
}

impl Block {
    /// Joins prose with LiteParse's whitespace and lowercase continuation rule while retaining list boundaries.
    fn render_markdown_text(&self, placeholder: &str, prose: bool) -> String {
        let mut text = String::new();
        for line in &self.lines {
            let rendered = render_line(line, placeholder);
            if rendered.is_empty() {
                continue;
            }
            if !prose {
                if !text.is_empty() {
                    text.push('\n');
                }
                text.push_str(&rendered);
                continue;
            }
            let next =
                rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            if next.is_empty() {
                continue;
            }
            if !text.is_empty() {
                if crate::text_rules::is_list_marker(next.chars()) {
                    text.push('\n');
                } else if is_soft_hyphen_break(&text, &next) {
                    let _ = text.pop();
                } else {
                    text.push(' ');
                }
            }
            text.push_str(&next);
        }
        text
    }
}

/// Recognizes an alphabetic line-end hyphen followed by lowercase continuation, not a numeric range.
fn is_soft_hyphen_break(previous: &str, next: &str) -> bool {
    previous.ends_with('-')
        && previous
            .chars()
            .rev()
            .nth(1)
            .is_some_and(char::is_alphabetic)
        && next.chars().next().is_some_and(char::is_lowercase)
}
