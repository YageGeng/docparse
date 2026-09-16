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
                let formulas: Vec<_> = page
                    .formulas
                    .iter()
                    .filter(|formula| {
                        formula.block_id.as_ref() == Some(&block.id)
                            && formula.markdown.is_some()
                    })
                    .collect();
                let display: Vec<_> = formulas
                    .iter()
                    .filter(|formula| {
                        formula.line_id.is_none()
                            && formula.table_cell.is_none()
                    })
                    .filter_map(|formula| formula.markdown.as_deref())
                    .collect();
                if !display.is_empty()
                    && matches!(
                        block.label,
                        LayoutLabel::DisplayFormula
                            | LayoutLabel::InlineFormula
                    )
                {
                    output.push(display.join("\n\n"));
                    previous_prose = false;
                    continue;
                }
                if let Some(table) = &block.table {
                    output.push(table.to_markdown());
                    previous_prose = false;
                    continue;
                }
                // Prose cleanup is a presentation projection; source lines and table ranges stay unchanged.
                let prose = self.view == RenderView::Semantic
                    && formulas.is_empty()
                    && matches!(
                        LabelPolicy::from(&block.label),
                        LabelPolicy::FlowText | LabelPolicy::Title
                    )
                    && block.lines.iter().all(|line| {
                        line.inline_spans.is_empty()
                            && line.direction
                                != crate::WritingDirection::Vertical
                    });
                let text = block.render_markdown_text(
                    &self.formula_placeholder,
                    prose,
                    &formulas,
                );
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
            // Unmatched model regions remain visible even when they own no native text.
            output.extend(
                page.formulas
                    .iter()
                    .filter(|formula| formula.block_id.is_none())
                    .filter_map(|formula| formula.markdown.clone()),
            );
        }
        output.join("\n\n")
    }
}

impl Block {
    /// Joins prose with LiteParse's whitespace and lowercase continuation rule while retaining list boundaries.
    fn render_markdown_text(
        &self,
        placeholder: &str,
        prose: bool,
        formulas: &[&crate::FormulaResult],
    ) -> String {
        let mut text = String::new();
        for line in &self.lines {
            let rendered =
                line.render_markdown_formulas(placeholder, formulas, false);
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

impl crate::Line {
    /// Replaces matched item ranges only in presentation, retaining adjacent text and missing-formula fallbacks.
    pub(crate) fn render_markdown_formulas(
        &self,
        placeholder: &str,
        formulas: &[&crate::FormulaResult],
        escape_prose: bool,
    ) -> String {
        let source: String = self
            .text_items
            .iter()
            .map(|item| item.raw_text.as_str())
            .collect();
        let mut offsets = Vec::with_capacity(self.text_items.len() + 1);
        offsets.push(0);
        for item in &self.text_items {
            offsets.push(
                offsets.last().copied().unwrap_or(0) + item.raw_text.len(),
            );
        }
        let mut replacements = Vec::new();
        for formula in
            formulas.iter().filter(|formula| formula.line_id.is_some())
        {
            let Some(markdown) = formula.markdown.as_deref() else {
                continue;
            };
            let insert_here = formula.line_id.as_ref() == Some(&self.id);
            let ranges = if formula.text_spans.is_empty() {
                if !insert_here {
                    continue;
                }
                formula
                    .text_item_range
                    .and_then(|range| {
                        let start = *offsets.get(range.start)?;
                        let safe = self
                            .text_items
                            .get(range.start..range.end)?
                            .iter()
                            .all(|item| formula.bbox.contains_bbox(item.bbox));
                        Some(
                            start..if safe {
                                *offsets.get(range.end)?
                            } else {
                                start
                            },
                        )
                    })
                    .into_iter()
                    .collect::<Vec<_>>()
            } else {
                formula
                    .text_spans
                    .iter()
                    .filter_map(|span| {
                        let index = self
                            .text_items
                            .iter()
                            .position(|item| item.id == span.text_item_id)?;
                        let offset = *offsets.get(index)?;
                        Some(
                            offset + span.byte_range.start
                                ..offset + span.byte_range.end,
                        )
                    })
                    .collect()
            };
            if !ranges.is_empty() {
                // Emit the equation on its anchor line; remove only its measured slices on other lines.
                replacements
                    .push((ranges, if insert_here { markdown } else { "" }));
            }
        }
        if replacements.is_empty() {
            let source = render_line(self, placeholder);
            return if escape_prose {
                crate::render::replace_formula_ranges(&source, Vec::new(), true)
            } else {
                source
            };
        }
        for span in self.inline_spans.iter().filter(|span| {
            span.content_status == crate::InlineContentStatus::Missing
        }) {
            if !formulas.iter().any(|formula| {
                formula.line_id.as_ref() == Some(&self.id)
                    && formula.bbox == span.bbox
                    && formula.markdown.is_some()
            }) && let Some(offset) = offsets.get(span.text_item_range.start)
            {
                replacements.push((
                    std::iter::once(*offset..*offset).collect(),
                    placeholder,
                ));
            }
        }
        crate::render::replace_formula_ranges(
            &source,
            replacements,
            escape_prose,
        )
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
