use std::io::{self, Write};

use docparse_layout::LayoutLabel;

use crate::label_policy::LabelPolicy;
use crate::render::{hidden_repeated_chrome, render_line};
use crate::{
    Block, DocumentResult, FigureDelivery, FigureImage, RenderView, TextSource,
};

/// A hidden reference definition separates CommonMark lists without requiring HTML support.
const LIST_GROUP_BREAK: &str = "[docparse-list-break]: #";

/// Minimal semantic Markdown renderer that preserves canonical traversal order.
#[derive(Debug, Clone)]
pub struct MarkdownRenderer {
    view: RenderView,
    formula_placeholder: String,
}

/// Keeps image payloads borrowed while one page's text boundaries are resolved.
enum MarkdownPart<'a> {
    Text(String),
    Image(&'a FigureImage),
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
        let mut output = Vec::new();
        self.write_with_images(document, &mut output, |_, path, writer| {
            // Angle destinations allow spaces while preserving the stored file path.
            let path = path
                .replace('\\', "\\\\")
                .replace('<', "\\<")
                .replace('>', "\\>");
            write!(writer, "<{path}>")
        })
        .expect("writing Markdown into memory cannot fail");
        String::from_utf8(output).expect("Markdown output is UTF-8")
    }

    /// Streams page text and image destinations without holding encoded file figures in memory.
    pub fn write_with_images<W: Write>(
        &self,
        document: &DocumentResult,
        writer: &mut W,
        mut file_image: impl FnMut(&FigureImage, &str, &mut W) -> io::Result<()>,
    ) -> io::Result<()> {
        let hidden = if self.view == RenderView::Semantic {
            hidden_repeated_chrome(document)
        } else {
            std::collections::BTreeSet::new()
        };
        let mut wrote_page = false;
        for page in &document.pages {
            let mut parts = Vec::new();
            let mut previous_prose = false;
            let mut previous_list = false;
            if self.view == RenderView::Raw {
                parts.push(MarkdownPart::Text(format!(
                    "<!-- page {} -->",
                    page.page_number
                )));
            }
            for block in &page.blocks {
                if block.label == LayoutLabel::Watermark {
                    previous_prose = false;
                    continue;
                }
                // Page headers and numbers remain visible inside page boundaries even when repeated.
                if hidden.contains(block.id.as_str())
                    && !matches!(
                        block.label,
                        LayoutLabel::Header | LayoutLabel::Number
                    )
                {
                    continue;
                }
                let after_list = std::mem::replace(&mut previous_list, false);
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
                    parts.push(MarkdownPart::Text(display.join("\n\n")));
                    previous_prose = false;
                    continue;
                }
                if let Some(table) = &block.table {
                    parts.push(MarkdownPart::Text(table.to_markdown()));
                    previous_prose = false;
                    continue;
                }
                if let Some(image) = &block.image {
                    parts.push(MarkdownPart::Image(image));
                    previous_prose = false;
                }
                if self.view == RenderView::Semantic
                    && block.label == LayoutLabel::Image
                {
                    // Persisted OCR policy governs new results; legacy results retain the source-based fallback.
                    let show_text = match document
                        .context
                        .metadata
                        .get("ocr_enabled")
                        .map(String::as_str)
                    {
                        Some("true") => true,
                        Some("false") => false,
                        _ => block.lines.iter().any(|line| {
                            line.text_items
                                .iter()
                                .any(|item| item.source == TextSource::Ocr)
                        }),
                    };
                    if !show_text {
                        continue;
                    }
                }
                // Prose cleanup is a presentation projection; source lines and table ranges stay unchanged.
                let prose = self.view == RenderView::Semantic
                    && block.joins_prose_lines();
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
                if prose && !block.list_items.is_empty() {
                    let first = block.lines.first().map(|line| &line.id);
                    let last = block.lines.last().map(|line| &line.id);
                    if after_list
                        && block
                            .list_items
                            .first()
                            .and_then(|item| item.line_ids.first())
                            == first
                    {
                        parts.push(MarkdownPart::Text(
                            LIST_GROUP_BREAK.to_owned(),
                        ));
                    }
                    previous_list = block
                        .list_items
                        .iter()
                        .any(|item| item.line_ids.last() == last);
                }
                // Heal adjacent body blocks using the same boundary rule as physical lines.
                if body
                    && previous_prose
                    && let Some(MarkdownPart::Text(previous)) = parts.last_mut()
                    && is_soft_hyphen_break(previous, &rendered)
                {
                    let _ = previous.pop();
                    previous.push_str(&rendered);
                } else {
                    parts.push(MarkdownPart::Text(rendered));
                }
                previous_prose = body;
            }
            // Unmatched model regions remain visible even when they own no native text.
            parts.extend(
                page.formulas
                    .iter()
                    .filter(|formula| formula.block_id.is_none())
                    .filter_map(|formula| {
                        formula.markdown.clone().map(MarkdownPart::Text)
                    }),
            );
            parts.extend(
                page.images
                    .iter()
                    .map(|asset| MarkdownPart::Image(&asset.image)),
            );
            if parts.is_empty() {
                continue;
            }
            if wrote_page {
                writer.write_all(if self.view == RenderView::Semantic {
                    b"\n\n---\n\n"
                } else {
                    b"\n\n"
                })?;
            }
            for (index, part) in parts.into_iter().enumerate() {
                if index > 0 {
                    writer.write_all(b"\n\n")?;
                }
                match part {
                    MarkdownPart::Text(text) => {
                        writer.write_all(text.as_bytes())?
                    }
                    MarkdownPart::Image(image) => {
                        image.write_markdown(writer, &mut file_image)?
                    }
                }
            }
            wrote_page = true;
        }
        Ok(())
    }
}

impl Block {
    /// Joins physical wraps only for horizontal flow text and titles.
    pub(crate) fn joins_prose_lines(&self) -> bool {
        matches!(
            LabelPolicy::from(&self.label),
            LabelPolicy::FlowText | LabelPolicy::Title
        ) && self
            .lines
            .iter()
            .all(|line| line.direction != crate::WritingDirection::Vertical)
    }

    /// Fences algorithms and charts with literal spacing and otherwise applies shared presentation rules.
    pub(crate) fn render_markdown_text(
        &self,
        placeholder: &str,
        prose: bool,
        formulas: &[&crate::FormulaResult],
    ) -> String {
        if matches!(self.label, LayoutLabel::Algorithm | LayoutLabel::Chart) {
            // Code fences preserve real spaces and newlines; prose escapes and HTML entities would become visible code.
            let text = Self::layout_text(&self.lines, false, |line| {
                line.render_markdown_formulas(placeholder, formulas, false)
            });
            if text.is_empty() {
                return text;
            }
            // A fence longer than every source backtick run cannot be closed by extracted algorithm content.
            let fence = "`".repeat(
                text.split(|ch| ch != '`')
                    .map(str::len)
                    .max()
                    .unwrap_or(0)
                    .saturating_add(1)
                    .max(3),
            );
            return format!("{fence}\n{text}\n{fence}");
        }
        if prose
            && self.label == LayoutLabel::Text
            && !self.list_items.is_empty()
        {
            return self.render_list_markdown(placeholder, formulas);
        }
        // Use one whitespace policy for JSON summaries, Markdown and formula-enriched previews.
        if LabelPolicy::from(&self.label).preserves_line_breaks() {
            return Self::layout_text(&self.lines, true, |line| {
                line.render_markdown_formulas(placeholder, formulas, true)
            });
        }
        let mut text = String::new();
        for line in &self.lines {
            // Detect list structure in source text so formula escaping can preserve only its marker.
            let list_marker =
                prose && crate::text_rules::is_list_marker(line.text.chars());
            let rendered = line.render_markdown_formulas(
                placeholder,
                formulas,
                prose && !formulas.is_empty(),
            );
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
            let mut next =
                rendered.split_whitespace().collect::<Vec<_>>().join(" ");
            if next.is_empty() {
                continue;
            }
            if list_marker && next.starts_with("\\* ") {
                next.remove(0);
            }
            if !text.is_empty() {
                if list_marker {
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

    /// Renders source-backed list items and intervening prose without duplicating their physical lines.
    fn render_list_markdown(
        &self,
        placeholder: &str,
        formulas: &[&crate::FormulaResult],
    ) -> String {
        let mut items = self.list_items.iter().peekable();
        // Active ancestors retain only their next source reference and rendered body indentation.
        let mut active: Vec<(&crate::ListItem, usize, usize)> = Vec::new();
        let mut output = String::new();
        let mut group = None;
        let mut previous_owner = None;
        for line in &self.lines {
            let mut join = false;
            let skip_prefix;
            if items
                .peek()
                .is_some_and(|item| item.line_ids.first() == Some(&line.id))
            {
                let item = items.next().expect("list start");
                if !output.is_empty() {
                    if group == Some(item.group) {
                        output.push('\n');
                    } else if group.is_some() {
                        // A reference definition separates CommonMark lists and stays invisible even with HTML disabled.
                        output.push_str("\n\n");
                        output.push_str(LIST_GROUP_BREAK);
                        output.push_str("\n\n");
                    } else {
                        output.push_str("\n\n");
                    }
                }
                if group != Some(item.group) {
                    active.clear();
                }
                active.truncate(item.level as usize);
                let indent = active.last().map_or(0, |(_, _, indent)| *indent);
                let marker = item.ordinal.map_or_else(
                    || "-".to_owned(),
                    |ordinal| format!("{ordinal}."),
                );
                output.extend(std::iter::repeat_n(' ', indent));
                output.push_str(&marker);
                output.push(' ');
                active.push((item, 1, indent + marker.len() + 1));
                skip_prefix = item.marker_end;
                group = Some(item.group);
                previous_owner = item.line_ids.first();
            } else if let Some(level) =
                active.iter().rposition(|(item, next, _)| {
                    item.line_ids.get(*next) == Some(&line.id)
                })
            {
                active.truncate(level + 1);
                let (item, next, indent) =
                    active.last_mut().expect("matched ancestor");
                if previous_owner == item.line_ids.first() {
                    join = true;
                } else {
                    output.push_str("\n\n");
                    output.extend(std::iter::repeat_n(' ', *indent));
                }
                *next += 1;
                skip_prefix = 0;
                previous_owner = item.line_ids.first();
            } else {
                if previous_owner.is_some() {
                    output.push_str("\n\n");
                } else {
                    join = !output.is_empty();
                }
                active.clear();
                group = None;
                previous_owner = None;
                skip_prefix = 0;
            }
            let rendered = line.render_markdown_formulas_after(
                placeholder,
                formulas,
                true,
                skip_prefix,
            );
            let mut words = rendered.split_whitespace();
            let Some(first) = words.next() else {
                continue;
            };
            if join {
                if is_soft_hyphen_break(&output, first) {
                    let _ = output.pop();
                } else {
                    output.push(' ');
                }
            }
            output.push_str(first);
            for word in words {
                output.push(' ');
                output.push_str(word);
            }
        }
        output
    }

    /// Projects lists and anchored formulas through the same renderer while preserving source ownership.
    pub(crate) fn project_markdown(
        &mut self,
        formulas: &[crate::FormulaResult],
        placeholder: &str,
    ) {
        self.markdown = None;
        let inline: Vec<_> = formulas.iter()
            .filter(|formula| {
                formula.block_id.as_ref() == Some(&self.id)
                    && formula.label == LayoutLabel::InlineFormula
                && formula.line_id.is_some()
                && formula.markdown.is_some()
                && (!formula.text_spans.is_empty()
                    || formula.text_item_range.is_some_and(|range| range.start < range.end)
                    || self.lines.iter().any(|line| {
                        formula.line_id.as_ref() == Some(&line.id)
                            && line.inline_spans.iter().any(|span| {
                                span.bbox == formula.bbox
                                    && span.content_status == crate::InlineContentStatus::Missing
                            })
                    }))
            })
            .collect();
        if !inline.is_empty() || !self.list_items.is_empty() {
            // Formula replacement must not collapse algorithm/TOC indentation in the browser projection.
            if crate::label_policy::LabelPolicy::from(&self.label)
                .preserves_line_breaks()
            {
                // Keep block Markdown consistent with complete-document algorithm fences.
                self.markdown = Some(self.render_markdown_text(
                    placeholder,
                    false,
                    &inline,
                ));
                return;
            }

            if self.joins_prose_lines() {
                // Keep browser Markdown and complete-document export on the same prose policy.
                self.markdown =
                    Some(self.render_markdown_text(placeholder, true, &inline));
                return;
            }

            // Reuse UTF-8-aware range replacement; prose must remain literal in browser Markdown.
            self.markdown = Some(
                self.lines
                    .iter()
                    .map(|line| {
                        line.render_markdown_formulas(
                            placeholder,
                            &inline,
                            true,
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            );
        }
    }
}

impl FigureImage {
    /// Writes inline bytes directly and lets the caller resolve file destinations.
    fn write_markdown<W: Write>(
        &self,
        writer: &mut W,
        file_image: &mut impl FnMut(&FigureImage, &str, &mut W) -> io::Result<()>,
    ) -> io::Result<()> {
        writer.write_all(b"![image](")?;
        match &self.delivery {
            FigureDelivery::Inline { data_base64 } => {
                write!(writer, "data:{};base64,", self.media_type.as_str())?;
                writer.write_all(data_base64.as_bytes())?;
            }
            FigureDelivery::File { path } => file_image(self, path, writer)?,
        }
        writer.write_all(b")")
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
        self.render_markdown_formulas_after(
            placeholder,
            formulas,
            escape_prose,
            0,
        )
    }

    /// Removes a list prefix as a source-byte replacement so formula anchors and UTF-8 offsets stay unchanged.
    fn render_markdown_formulas_after(
        &self,
        placeholder: &str,
        formulas: &[&crate::FormulaResult],
        escape_prose: bool,
        skip_prefix: usize,
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
        if skip_prefix > 0 && source.get(..skip_prefix).is_some() {
            replacements.push((std::iter::once(0..skip_prefix).collect(), ""));
        }
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
