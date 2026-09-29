//! Body-only list recovery over final, exclusively owned source lines.
use crate::{Block, Line, ListItem, ListKind, WritingDirection};
use docparse_layout::LayoutLabel;
use typed_builder::TypedBuilder;

/// One lexical candidate; weak families still require page-local sequence evidence.
#[derive(TypedBuilder)]
struct Marker {
    kind: ListKind,
    text: String,
    end: usize,
    #[builder(default)]
    ordinal: Option<u32>,
}

impl Marker {
    /// Interprets ambiguous single letters as Roman numerals only when their sequence confirms that family.
    fn value_as(&self, kind: ListKind) -> Option<u32> {
        if self.kind == kind {
            return self.ordinal;
        }
        let matching_case = matches!(
            (self.kind, kind),
            (ListKind::AlphaLower, ListKind::RomanLower)
                | (ListKind::AlphaUpper, ListKind::RomanUpper)
        );
        matching_case.then(|| {
            Self::roman_value(self.text.trim_matches(['(', ')', '.']))
        })?
    }

    /// Accepts canonical Roman numerals instead of interpreting arbitrary letter combinations as numbers.
    fn roman_value(text: &str) -> Option<u32> {
        let text = text.to_ascii_uppercase();
        let mut rest = text.as_str();
        let mut value = 0;
        let digits = [
            ("M", 1000),
            ("CM", 900),
            ("D", 500),
            ("CD", 400),
            ("C", 100),
            ("XC", 90),
            ("L", 50),
            ("XL", 40),
            ("X", 10),
            ("IX", 9),
            ("V", 5),
            ("IV", 4),
            ("I", 1),
        ];
        for (token, amount) in digits {
            let mut count = 0;
            while let Some(suffix) = rest.strip_prefix(token) {
                count += 1;
                if count
                    > if token.len() == 2 || matches!(token, "D" | "L" | "V") {
                        1
                    } else {
                        3
                    }
                {
                    return None;
                }
                value += amount;
                rest = suffix;
            }
        }
        if !rest.is_empty() || value == 0 || value >= 4000 {
            return None;
        }
        let mut number = value;
        let mut canonical = String::new();
        for (token, amount) in digits {
            while number >= amount {
                canonical.push_str(token);
                number -= amount;
            }
        }
        (canonical == text).then_some(value)
    }
}

impl Line {
    /// Finds a source prefix, using a physical gap when PDF style runs omit the separator character.
    fn list_marker(&self) -> Option<Marker> {
        if self.direction != WritingDirection::LeftToRight
            || self.rotation.abs() > 2.0
        {
            return None;
        }
        let source = self.text.trim_start();
        let leading = self.text.len() - source.len();
        let first = source.chars().next()?;
        let (kind, ordinal, end) = if crate::text_rules::LIST_BULLETS
            .contains(&first)
        {
            (ListKind::Unordered, None, first.len_utf8())
        } else {
            let enclosed = source.starts_with('(');
            let start = usize::from(enclosed);
            let body = source.get(start..)?;
            // Stop a numeric prefix before letters; the separator check below still requires whitespace or a measured run gap.
            let numeric = body.starts_with(|ch: char| ch.is_ascii_digit());
            let len = body
                .bytes()
                .take_while(|ch| {
                    if numeric {
                        ch.is_ascii_digit()
                    } else {
                        ch.is_ascii_alphabetic()
                    }
                })
                .count();
            let token = body.get(..len)?;
            if token.is_empty() {
                return None;
            }
            let suffix = body.get(len..)?;
            let punctuation = suffix.chars().next()?;
            let explicit = matches!(punctuation, '.' | ')' | '、');
            if enclosed && punctuation != ')' {
                return None;
            }
            let (kind, ordinal) = if token.bytes().all(|c| c.is_ascii_digit()) {
                if len > 3 {
                    return None;
                }
                (
                    if explicit {
                        ListKind::Decimal
                    } else {
                        ListKind::BareDecimal
                    },
                    token.parse().ok()?,
                )
            } else if len == 1 {
                let letter = token.chars().next()?;
                if !letter.is_ascii_alphabetic() {
                    return None;
                }
                (
                    if letter.is_ascii_lowercase() {
                        ListKind::AlphaLower
                    } else {
                        ListKind::AlphaUpper
                    },
                    u32::from(letter.to_ascii_lowercase()) - u32::from('a') + 1,
                )
            } else {
                if !(token.chars().all(|c| c.is_ascii_lowercase())
                    || token.chars().all(|c| c.is_ascii_uppercase()))
                {
                    return None;
                }
                (
                    if token.starts_with(char::is_lowercase) {
                        ListKind::RomanLower
                    } else {
                        ListKind::RomanUpper
                    },
                    Marker::roman_value(token)?,
                )
            };
            if !explicit && (enclosed || kind != ListKind::BareDecimal) {
                return None;
            }
            (
                kind,
                Some(ordinal),
                start + len + if explicit { punctuation.len_utf8() } else { 0 },
            )
        };
        let rest = source.get(end..)?;
        let end_in_line = leading + end;
        let mut offset = 0;
        let separated = self.text_items.windows(2).any(|pair| {
            let Some((left, right)) = pair.first().zip(pair.get(1)) else {
                return false;
            };
            offset += left.raw_text.len();
            offset == end_in_line
                && right.bbox.left - left.bbox.right
                    >= self.dominant_font_size() * 0.2
        });
        if !rest.starts_with(char::is_whitespace)
            && !separated
            && !source.get(..end)?.ends_with('、')
        {
            return None;
        }
        let body = rest.trim_start();
        if body.is_empty()
            || kind == ListKind::BareDecimal
                && !body.starts_with(char::is_alphabetic)
        {
            return None;
        }
        Some(
            Marker::builder()
                .kind(kind)
                .ordinal(ordinal)
                .text(source.get(..end)?.to_owned())
                .end(self.text.len() - body.len())
                .build(),
        )
    }

    /// Locates the list body's source start without copying or rewriting the underlying text items.
    fn list_body_x(&self, prefix: usize) -> f64 {
        let mut offset = 0;
        for item in &self.text_items {
            let end = offset + item.raw_text.len();
            if prefix < end {
                let before = item
                    .raw_text
                    .get(..prefix.saturating_sub(offset))
                    .unwrap_or("")
                    .chars()
                    .count();
                return item.bbox.left
                    + item.bbox.width() * before as f64
                        / item.raw_text.chars().count().max(1) as f64;
            }
            offset = end;
        }
        self.bbox.left
    }

    /// Limits joins to a forward local flow with compatible body typography and no column restart.
    fn follows_list_line(&self, previous: &Self, distance: usize) -> bool {
        let size = self.dominant_font_size().max(1.0);
        let previous_size = previous.dominant_font_size().max(1.0);
        let gap = self.bbox.top - previous.bbox.bottom;
        self.direction == WritingDirection::LeftToRight
            && self.rotation.abs() <= 2.0
            && (size - previous_size).abs() <= size.max(previous_size) * 0.2
            && gap >= -size * 0.25
            && gap <= size.max(previous_size) * 2.0 * distance.max(1) as f64
    }
}

/// A short-lived numbering run; stale runs expire after twelve physical lines.
struct Sequence {
    indices: Vec<usize>,
    next: u32,
    indent: f64,
}

/// Positional annotations survive owner merging without cloning temporary Line IDs.
#[derive(TypedBuilder)]
struct RecoveredItem {
    group: u32,
    level: u32,
    marker: Marker,
    lines: Vec<usize>,
}

impl RecoveredItem {
    /// Classifies a borrowed reading-order view, including rows split across adjacent body layouts.
    fn recover(lines: &[&Line]) -> Vec<Self> {
        let mut items: Vec<Self> = Vec::new();
        let mut markers: Vec<_> =
            lines.iter().map(|line| line.list_marker()).collect();
        let mut confirmed = vec![None; lines.len()];
        for kind in [
            ListKind::BareDecimal,
            ListKind::AlphaLower,
            ListKind::AlphaUpper,
            ListKind::RomanLower,
            ListKind::RomanUpper,
        ] {
            let mut runs: Vec<Sequence> = Vec::new();
            for (index, (line, marker)) in
                lines.iter().zip(&markers).enumerate()
            {
                let Some(value) =
                    marker.as_ref().and_then(|marker| marker.value_as(kind))
                else {
                    continue;
                };
                // Only twelve lines can own live candidates, bounding this scan even for long documents.
                runs.retain(|run| {
                    run.indices.last().is_some_and(|last| index - last <= 12)
                });
                let found = runs.iter_mut().rev().find(|run| {
                    run.next == value
                        && (run.indent - line.bbox.left).abs()
                            <= line.dominant_font_size().max(1.0) * 0.6
                        && run
                            .indices
                            .last()
                            .and_then(|last| {
                                lines.get(*last).map(|previous| {
                                    line.follows_list_line(
                                        previous,
                                        index - last,
                                    )
                                })
                            })
                            .unwrap_or(false)
                });
                if let Some(run) = found {
                    run.indices.push(index);
                    run.next += 1;
                    let minimum =
                        if kind == ListKind::BareDecimal { 3 } else { 2 };
                    if run.indices.len() == minimum {
                        for (ordinal, member) in run.indices.iter().enumerate()
                        {
                            if let Some(slot) = confirmed.get_mut(*member) {
                                *slot = Some((kind, ordinal as u32 + 1));
                            }
                        }
                    } else if run.indices.len() > minimum
                        && let Some(slot) = confirmed.get_mut(index)
                    {
                        *slot = Some((kind, value));
                    }
                } else if value == 1 {
                    runs.push(Sequence {
                        indices: vec![index],
                        next: 2,
                        indent: line.bbox.left,
                    });
                }
            }
        }
        for (marker, confirmation) in markers.iter_mut().zip(confirmed) {
            if let Some(candidate) = marker {
                if matches!(
                    candidate.kind,
                    ListKind::Unordered | ListKind::Decimal
                ) {
                    continue;
                }
                if let Some((kind, ordinal)) = confirmation {
                    candidate.kind = kind;
                    candidate.ordinal = Some(ordinal);
                } else {
                    *marker = None;
                }
            }
        }
        let mut indents: Vec<f64> = Vec::new();
        let mut previous_line: Option<usize> = None;
        // Each active indentation retains its item and first line so a parent can resume after children.
        let mut active: Vec<(usize, usize)> = Vec::new();
        let mut group = 0;
        for (index, (line, marker)) in lines.iter().zip(markers).enumerate() {
            if let Some(marker) = marker {
                let tolerance = line.dominant_font_size().max(1.0) * 0.6;
                let connected =
                    previous_line.and_then(|i| lines.get(i)).is_some_and(
                        |previous| line.follows_list_line(previous, 1),
                    );
                let root_changed = indents.first().is_some_and(|indent| {
                    (line.bbox.left - indent).abs() <= tolerance
                }) && active
                    .first()
                    .and_then(|(item, _)| items.get(*item))
                    .is_some_and(|item| {
                        item.marker.kind != marker.kind
                            || marker.ordinal == Some(1)
                                && item
                                    .marker
                                    .ordinal
                                    .is_some_and(|value| value > 1)
                    });
                if active.is_empty()
                    || !connected
                    || root_changed
                    || indents.first().is_some_and(|indent| {
                        line.bbox.left < indent - tolerance
                    })
                {
                    if !items.is_empty() {
                        group += 1;
                    }
                    indents.clear();
                    active.clear();
                }
                while indents
                    .last()
                    .is_some_and(|indent| line.bbox.left < indent - tolerance)
                {
                    indents.pop();
                }
                if indents
                    .last()
                    .is_none_or(|indent| line.bbox.left > indent + tolerance)
                {
                    indents.push(line.bbox.left);
                }
                let level = indents.len().saturating_sub(1) as u32;
                items.push(
                    RecoveredItem::builder()
                        .group(group)
                        .level(level)
                        .marker(marker)
                        .lines(vec![index])
                        .build(),
                );
                active.truncate(level as usize);
                active.push((items.len() - 1, index));
                previous_line = Some(index);
                continue;
            }
            let continuation = active.iter().rposition(|(item, first)| {
                let Some(item) = items.get(*item) else {
                    return false;
                };
                let Some(first) = lines.get(*first) else {
                    return false;
                };
                let Some(previous) =
                    previous_line.and_then(|previous| lines.get(previous))
                else {
                    return false;
                };
                let size = line.dominant_font_size().max(1.0);
                let body_x = first.list_body_x(item.marker.end);
                let hanging = (line.bbox.left - body_x).abs() <= size * 0.6;
                let flush_wrap = (line.bbox.left - first.bbox.left).abs()
                    <= size * 0.4
                    && !previous.text.trim_end().ends_with([
                        '.', '!', '?', '。', '！', '？', ':', '：', ';', '；',
                    ]);
                line.follows_list_line(previous, 1)
                    && (hanging || flush_wrap)
                    && !line.text_items.iter().all(crate::TextItem::is_bold)
            });
            if let Some(level) = continuation {
                active.truncate(level + 1);
                indents.truncate(level + 1);
                let (item, _) = active.last().expect("matched ancestor");
                items.get_mut(*item).expect("active item").lines.push(index);
                previous_line = Some(index);
            } else {
                active.clear();
                previous_line = None;
                indents.clear();
            }
        }
        items
    }
}

impl ListItem {
    /// Confirms that a serialized marker and ordinal still describe the unmodified source prefix.
    pub(crate) fn matches_source(&self, line: &Line) -> bool {
        line.list_marker().is_some_and(|marker| {
            marker.text == self.marker
                && marker.end == self.marker_end
                && if self.kind == ListKind::Unordered {
                    marker.kind == ListKind::Unordered && self.ordinal.is_none()
                } else {
                    self.ordinal.is_some()
                        && marker.value_as(self.kind) == self.ordinal
                }
        })
    }
}

impl super::SemanticAssembler<'_> {
    /// Recovers each body run once and remaps positional annotations after merging only image-free owners.
    pub(crate) fn recover_list_layouts(
        blocks: Vec<Block>,
    ) -> Result<Vec<Block>, crate::SemanticError> {
        let mut output = Vec::with_capacity(blocks.len());
        let mut input = blocks.into_iter().peekable();
        while let Some(mut first) = input.next() {
            first.list_items.clear();
            if first.label != LayoutLabel::Text || first.table.is_some() {
                first.final_order = output.len() as u32;
                output.push(first);
                continue;
            }
            let mut run = vec![first];
            while let Some(next) = input.peek() {
                let previous = run.last().expect("nonempty body run");
                // A block has only one image slot. Keep image owners as hard boundaries instead of relocating their assets.
                if next.label != LayoutLabel::Text
                    || next.table.is_some()
                    || previous.embedded_image_index.is_some()
                    || previous.image.is_some()
                    || next.embedded_image_index.is_some()
                    || next.image.is_some()
                {
                    break;
                }
                let connected = previous
                    .lines
                    .last()
                    .zip(next.lines.first())
                    .is_some_and(|(previous, next)| {
                        let overlap = previous.bbox.right.min(next.bbox.right)
                            - previous.bbox.left.max(next.bbox.left);
                        next.follows_list_line(previous, 1)
                            && (overlap > 0.0
                                || (next.bbox.left - previous.bbox.left).abs()
                                    <= next.dominant_font_size().max(1.0) * 3.0)
                    });
                if !connected {
                    break;
                }
                run.push(input.next().expect("peeked body block"));
            }
            let lines: Vec<_> =
                run.iter().flat_map(|block| &block.lines).collect();
            let items = RecoveredItem::recover(&lines);
            // Numeric owner positions replace temporary LineId maps and avoid reclassifying the merged source.
            let owners: Vec<_> = if items.is_empty() {
                Vec::new()
            } else {
                run.iter()
                    .enumerate()
                    .flat_map(|(index, block)| {
                        std::iter::repeat_n(index, block.lines.len())
                    })
                    .collect()
            };
            let mut groups: Vec<(usize, usize)> = Vec::new();
            let mut group = None;
            for item in &items {
                let first = *owners
                    .get(*item.lines.first().expect("list start"))
                    .expect("line owner");
                let last = *owners
                    .get(*item.lines.last().expect("list end"))
                    .expect("line owner");
                if group == Some(item.group) {
                    let bounds = groups.last_mut().expect("current group");
                    bounds.1 = bounds.1.max(last);
                } else {
                    groups.push((first, last));
                    group = Some(item.group);
                }
            }
            let mut ranges: Vec<(usize, usize)> = Vec::new();
            for (start, end) in groups {
                if let Some(previous) = ranges.last_mut()
                    && start <= previous.1
                {
                    previous.1 = previous.1.max(end);
                } else {
                    ranges.push((start, end));
                }
            }
            let mut ranges = ranges.into_iter().peekable();
            let mut items = items.into_iter().peekable();
            let mut blocks = run.into_iter().enumerate();
            let mut line_offset = 0;
            while let Some((index, mut block)) = blocks.next() {
                let end = if ranges
                    .peek()
                    .is_some_and(|(start, _)| *start == index)
                {
                    ranges.next().expect("merge range").1
                } else {
                    index
                };
                if end > index {
                    let mut sources: Vec<_> =
                        block.source_regions().cloned().collect();
                    for (_, other) in blocks.by_ref().take(end - index) {
                        block.bbox = docparse_layout::Bbox::try_from([
                            block.bbox.left.min(other.bbox.left),
                            block.bbox.top.min(other.bbox.top),
                            block.bbox.right.max(other.bbox.right),
                            block.bbox.bottom.max(other.bbox.bottom),
                        ])?;
                        for source in other.source_regions() {
                            if !sources.contains(source) {
                                sources.push(source.clone());
                            }
                        }
                        block.lines.extend(other.lines);
                        block.evidence.extend(other.evidence);
                        for (key, value) in other.semantic_hints {
                            block.semantic_hints.entry(key).or_insert(value);
                        }
                    }
                    // Only parent-qualified Line IDs change; canonical TextItem IDs and byte ranges remain intact.
                    for (ordinal, line) in block.lines.iter_mut().enumerate() {
                        line.id = crate::LineId::new(&block.id, ordinal as u32);
                    }
                    block.text = Block::derive_text(&block.label, &block.lines);
                    block.polygon = Self::content_polygon(&block.lines);
                    block.label_source = crate::LabelSource::Heuristic;
                    block.model_order = sources
                        .iter()
                        .filter_map(|source| source.model_order)
                        .min();
                    block.source_regions = sources;
                    block.semantic_hints.insert(
                        Block::SPARSE_LAYOUT_HINT.to_owned(),
                        "true".to_owned(),
                    );
                    block.evidence.push(
                        crate::Evidence::builder()
                            .kind("body_list_layout_merge".to_owned())
                            .build(),
                    );
                    tracing::debug!(
                        "merged {} body layouts into list block {}",
                        end - index + 1,
                        block.id.as_str()
                    );
                }
                block.list_items.clear();
                let mut first_group = None;
                while items.peek().is_some_and(|item| {
                    item.lines.first().is_some_and(|line| {
                        *line < line_offset + block.lines.len()
                    })
                }) {
                    let item = items.next().expect("list item in owner");
                    let first_group = *first_group.get_or_insert(item.group);
                    block.list_items.push(
                        ListItem::builder()
                            .group(item.group - first_group)
                            .level(item.level)
                            .kind(item.marker.kind)
                            .marker(item.marker.text)
                            .ordinal(item.marker.ordinal)
                            .marker_end(item.marker.end)
                            .line_ids(
                                item.lines
                                    .into_iter()
                                    .map(|line| {
                                        block
                                            .lines
                                            .get(line - line_offset)
                                            .expect("merged source line")
                                            .id
                                            .clone()
                                    })
                                    .collect(),
                            )
                            .build(),
                    );
                }
                if !block.list_items.is_empty() {
                    tracing::debug!(
                        "recovered {} list items in body block {}",
                        block.list_items.len(),
                        block.id.as_str()
                    );
                }
                line_offset += block.lines.len();
                block.final_order = output.len() as u32;
                output.push(block);
            }
        }
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing)] // Tests assert fixture sizes and serialized shapes before indexing.

    use crate::page::{OcrCompletion, PageAnalyzer};
    use crate::{
        DocumentContextBuilder, DocumentResult, ExtractedPage, JsonRenderer,
        MarkdownRenderer, PageProbe, RenderView, SchemaVersion, TextItem,
        TextItemId, TextSource, TextStyle,
    };
    use docparse_config::{RawConfig, ValidatedConfig};
    use docparse_layout::{Bbox, GeometrySource, LayoutDetection, LayoutLabel};
    use std::{collections::BTreeMap, sync::Arc};

    /// Bare numbers split into separate PDF runs must use physical gaps without changing source bytes.
    #[test]
    fn body_lists_accept_bare_number_gaps() {
        let document = document(
            LayoutLabel::Text,
            &[
                ("1", 40.0, 30.0),
                ("alpha", 55.0, 30.0),
                ("2", 40.0, 44.0),
                ("beta", 55.0, 44.0),
                ("3", 40.0, 58.0),
                ("gamma", 55.0, 58.0),
            ],
        );
        assert_eq!(document.pages[0].blocks[0].list_items.len(), 3);
        assert_eq!(document.pages[0].blocks[0].lines[0].text, "1alpha");
        assert_eq!(
            MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                .render(&document),
            "1. alpha\n2. beta\n3. gamma"
        );
        let glued = document_with_layouts(
            &[LayoutLabel::Text],
            &[
                ("1alpha", 40.0, 30.0),
                ("2beta", 40.0, 44.0),
                ("3gamma", 40.0, 58.0),
            ],
        );
        assert!(glued.pages[0].blocks[0].list_items.is_empty());
    }

    /// Numbering restarts use an invisible CommonMark boundary instead of joining into one loose list.
    #[test]
    fn body_lists_separate_restarted_numbering() {
        let document = document(
            LayoutLabel::Text,
            &[
                ("1. first", 40.0, 30.0),
                ("2. second", 40.0, 44.0),
                ("1. third", 40.0, 58.0),
                ("2. fourth", 40.0, 72.0),
            ],
        );
        assert_eq!(
            MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                .render(&document),
            "1. first\n2. second\n\n[docparse-list-break]: #\n\n1. third\n2. fourth"
        );
        let separate = document_with_layouts(
            &[LayoutLabel::Text, LayoutLabel::Text],
            &[("1. first", 40.0, 30.0), ("1. separate", 40.0, 100.0)],
        );
        assert_eq!(separate.pages[0].blocks.len(), 2);
        assert_eq!(
            MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                .render(&separate),
            "1. first\n\n[docparse-list-break]: #\n\n1. separate"
        );
    }

    /// Parent continuations resume after children without changing source order or starting a new root group.
    #[test]
    fn body_lists_resume_parent_after_children() {
        let lines = [
            ("1. parent", 40.0, 30.0),
            ("a. child", 65.0, 44.0),
            ("b. child two", 65.0, 58.0),
            ("parent continues", 55.0, 72.0),
            ("2. next", 40.0, 86.0),
        ];
        for labels in [
            vec![LayoutLabel::Text],
            vec![LayoutLabel::Text; lines.len()],
        ] {
            let document = document_with_layouts(&labels, &lines);
            let block = &document.pages[0].blocks[0];
            assert_eq!(document.pages[0].blocks.len(), 1);
            assert_eq!(
                block.list_items[0].line_ids,
                [block.lines[0].id.clone(), block.lines[3].id.clone()]
            );
            assert!(block.list_items.iter().all(|item| item.group == 0));
            assert_eq!(
                MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                    .render(&document),
                "1. parent\n   1. child\n   2. child two\n\n   parent continues\n2. next"
            );
            crate::ResultValidator::validate(&document)
                .expect("valid ancestor continuation");
        }
        let document = document(LayoutLabel::Text, &lines);
        let mut invalid = document.clone();
        let block = &mut invalid.pages[0].blocks[0];
        let after_sibling = block.lines[4].id.clone();
        block.list_items[0].line_ids[1] = after_sibling;
        block.list_items.pop();
        assert!(
            crate::ResultValidator::validate(&invalid).is_err(),
            "an ancestor cannot resume across unrelated prose"
        );
    }

    /// Noncontiguous references may cross descendants only, never a later sibling's scope.
    #[test]
    fn body_lists_reject_continuations_after_siblings() {
        let mut document = document(
            LayoutLabel::Text,
            &[
                ("1. parent", 40.0, 30.0),
                ("2. sibling", 40.0, 44.0),
                ("wrap", 55.0, 58.0),
            ],
        );
        let block = &mut document.pages[0].blocks[0];
        let continuation =
            block.list_items[1].line_ids.pop().expect("sibling wrap");
        block.list_items[0].line_ids.push(continuation);
        assert!(crate::ResultValidator::validate(&document).is_err());
    }

    /// Image-bearing list regions retain their own identity instead of losing image ownership during merging.
    #[test]
    fn body_lists_keep_image_owners_separate() {
        let document = document(
            LayoutLabel::Text,
            &[("1. first", 40.0, 30.0), ("2. second", 40.0, 44.0)],
        );
        let mut first = document.pages[0].blocks[0].clone();
        let mut second = first.clone();
        second.lines = first.lines.split_off(1);
        second.id = crate::BlockId::model(1, 1, 0);
        second.lines[0].id = crate::LineId::new(&second.id, 0);
        first.list_items.clear();
        second.list_items.clear();
        for owner in 0..2 {
            for delivered in [false, true] {
                let mut blocks = vec![first.clone(), second.clone()];
                if delivered {
                    blocks[owner].image = Some(
                        crate::FigureImage::builder()
                            .source(crate::FigureSource::Embedded)
                            .media_type(crate::FigureMediaType::Png)
                            .width(1)
                            .height(1)
                            .delivery(crate::FigureDelivery::Inline {
                                data_base64: "iVBORw==".into(),
                            })
                            .build(),
                    );
                } else {
                    blocks[owner].embedded_image_index = Some(7);
                }
                let original = blocks[owner].clone();
                let blocks =
                    crate::semantic::SemanticAssembler::recover_list_layouts(
                        blocks,
                    )
                    .expect("merge");
                assert_eq!(blocks.len(), 2);
                assert_eq!(blocks[owner].id, original.id);
                assert_eq!(
                    blocks[owner].embedded_image_index,
                    original.embedded_image_index
                );
                assert_eq!(blocks[owner].image, original.image);
            }
        }
    }

    /// Exercises the real semantic pipeline with deterministic source lines and one model region.
    fn document(
        label: LayoutLabel,
        lines: &[(&str, f64, f64)],
    ) -> DocumentResult {
        document_with_layouts(&[label], lines)
    }

    /// Reproduces model output that assigns separate layouts to adjacent list rows.
    fn document_with_layouts(
        labels: &[LayoutLabel],
        lines: &[(&str, f64, f64)],
    ) -> DocumentResult {
        let text_items = lines
            .iter()
            .enumerate()
            .map(|(index, (text, x, y))| {
                TextItem::builder()
                    .id(TextItemId::native(1, index as u32))
                    .raw_text((*text).to_owned())
                    .source(TextSource::Native)
                    .bbox(
                        Bbox::try_from([
                            *x,
                            *y,
                            x + text.chars().count() as f64 * 5.0,
                            y + 10.0,
                        ])
                        .expect("text bounds"),
                    )
                    .style(Some(
                        TextStyle::builder().font_size(Some(10.0)).build(),
                    ))
                    .extraction_order(index as u32)
                    .build()
            })
            .collect();
        let page = ExtractedPage::builder()
            .page_number(1)
            .width(612.0)
            .height(792.0)
            .rotation(0)
            .text_items(text_items)
            .build();
        let mut context = DocumentContextBuilder::new(1);
        context.push_page(PageProbe::from(&page)).expect("probe");
        let context = context.build().expect("context");
        let detections = labels
            .iter()
            .enumerate()
            .map(|(index, label)| {
                let bounds = if labels.len() == 1 {
                    [20.0, 10.0, 570.0, 760.0]
                } else {
                    let (text, x, y) = lines[index];
                    [
                        x - 1.0,
                        y - 1.0,
                        x + text.chars().count() as f64 * 5.0 + 1.0,
                        y + 11.0,
                    ]
                };
                LayoutDetection::builder()
                    .source_detection_index(index as u32)
                    .raw_label(label.to_str().to_owned())
                    .class_id(22)
                    .label(label.clone())
                    .confidence(1.0)
                    .bbox(Bbox::try_from(bounds).expect("region"))
                    .polygon(None)
                    .geometry_source(GeometrySource::DerivedFromBbox)
                    .model_order(index as i64)
                    .metadata(BTreeMap::new())
                    .build()
            })
            .collect();
        let config = Arc::new(
            ValidatedConfig::try_from(RawConfig::default()).expect("config"),
        );
        let analyzer = PageAnalyzer::new(config);
        let draft = analyzer
            .prepare(page, detections, Arc::clone(&context))
            .expect("prepare");
        let page = analyzer
            .finish(draft, OcrCompletion::NotRequested)
            .expect("finish");
        DocumentResult::builder()
            .schema_version(SchemaVersion::V2_0)
            .context((*context).clone())
            .pages(vec![page])
            .build()
    }

    /// A list must preserve raw source facts while grouping its hanging wrap and nested child.
    #[test]
    fn body_lists_recover_items_nesting_and_continuations() {
        let document = document(
            LayoutLabel::Text,
            &[
                ("Intro paragraph.", 40.0, 20.0),
                ("• first wrap", 40.0, 50.0),
                ("continues here", 50.0, 63.0),
                ("◦ nested", 65.0, 76.0),
                ("• last", 40.0, 89.0),
                ("After.", 40.0, 124.0),
            ],
        );
        let value = serde_json::to_value(&document).expect("JSON");
        let items = value
            .pointer("/pages/0/blocks/0/list_items")
            .and_then(serde_json::Value::as_array)
            .expect("recovered list items");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0]["line_ids"].as_array().expect("lines").len(), 2);
        assert_eq!(
            items
                .iter()
                .map(|item| item["level"].as_u64().expect("level"))
                .collect::<Vec<_>>(),
            [0, 1, 0]
        );
        let markdown = MarkdownRenderer::new(RenderView::Semantic, "[formula]")
            .render(&document);
        assert_eq!(
            markdown,
            "Intro paragraph.\n\n- first wrap continues here\n  - nested\n- last\n\nAfter."
        );
        assert_eq!(
            document.pages[0].blocks[0].markdown.as_deref(),
            Some(markdown.as_str())
        );
        assert!(
            document.pages[0].blocks[0]
                .lines
                .iter()
                .any(|line| line.text == "• first wrap")
        );
        let filtered = JsonRenderer::render_with_config(
            &document,
            &docparse_config::OutputConfig::default(),
        )
        .expect("configured JSON");
        let filtered: serde_json::Value =
            serde_json::from_str(&filtered).expect("wire JSON");
        assert_eq!(
            filtered.pointer("/pages/0/blocks/0/list_items"),
            value.pointer("/pages/0/blocks/0/list_items")
        );
    }

    /// Decimal punctuation, confirmed letters/romans and bare runs produce actual Markdown lists.
    #[test]
    fn body_lists_recognize_numbering_families() {
        for prefixes in [
            ["1.", "2)", "(3)"],
            ["a.", "b.", "c."],
            ["i.", "ii.", "iii."],
            ["1", "2", "3"],
            ["1、", "2、", "3、"],
        ] {
            let lines: Vec<_> = prefixes
                .into_iter()
                .enumerate()
                .map(|(i, prefix)| {
                    (
                        format!("{prefix} item {}", i + 1),
                        40.0,
                        30.0 + i as f64 * 14.0,
                    )
                })
                .collect();
            let borrowed: Vec<_> = lines
                .iter()
                .map(|(text, x, y)| (text.as_str(), *x, *y))
                .collect();
            let document = document(LayoutLabel::Text, &borrowed);
            assert_eq!(
                MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                    .render(&document),
                "1. item 1\n2. item 2\n3. item 3",
                "{prefixes:?}"
            );
        }
    }

    /// Only Text can carry list semantics, regardless of how list-like another model region looks.
    #[test]
    fn body_lists_exclude_directory_algorithm_chart_and_other_layouts() {
        for label in [
            LayoutLabel::Content,
            LayoutLabel::Algorithm,
            LayoutLabel::Chart,
            LayoutLabel::Abstract,
            LayoutLabel::ParagraphTitle,
            LayoutLabel::Header,
            LayoutLabel::Number,
            LayoutLabel::Footnote,
        ] {
            let document = document(
                label.clone(),
                &[("• first", 40.0, 30.0), ("• second", 40.0, 44.0)],
            );
            let value = serde_json::to_value(&document).expect("JSON");
            assert!(
                value["pages"][0]["blocks"]
                    .as_array()
                    .expect("blocks")
                    .iter()
                    .all(|block| block.get("list_items").is_none()),
                "{label:?}"
            );
        }
    }

    /// Numbers, initials and sparse marker-like sentences cannot establish weak lists by themselves.
    #[test]
    fn body_lists_reject_ambiguous_prose() {
        for lines in [
            vec![
                ("1 ordinary sentence", 40.0, 30.0),
                ("2 another sentence", 40.0, 44.0),
            ],
            vec![("2026 was a year", 40.0, 30.0)],
            vec![("1.5x growth", 40.0, 30.0)],
            vec![
                ("J. Smith reported", 40.0, 30.0),
                ("M. Jones agreed", 40.0, 44.0),
            ],
        ] {
            let document = document(LayoutLabel::Text, &lines);
            let value = serde_json::to_value(&document).expect("JSON");
            assert!(value.pointer("/pages/0/blocks/0/list_items").is_none());
        }
    }
    /// Nested ordered lists must indent to the parent's body, not by a fixed two-space shortcut.
    #[test]
    fn body_lists_render_nested_numbering_and_preserve_raw_output() {
        let document = document(
            LayoutLabel::Text,
            &[
                ("1. parent", 40.0, 30.0),
                ("a. child", 65.0, 44.0),
                ("b. child two", 65.0, 58.0),
                ("2. next", 40.0, 72.0),
            ],
        );
        assert_eq!(
            MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                .render(&document),
            "1. parent\n   1. child\n   2. child two\n2. next"
        );
        let raw = MarkdownRenderer::new(RenderView::Raw, "[formula]")
            .render(&document);
        assert!(raw.contains("a. child") && raw.contains("b. child two"));
    }

    /// Removing marker bytes must preserve exact formula anchors and literal Markdown metacharacters.
    #[test]
    fn body_lists_preserve_formula_ranges() {
        let mut document =
            document(LayoutLabel::Text, &[("• value 7 [item]", 40.0, 30.0)]);
        let block = &document.pages[0].blocks[0];
        let line = &block.lines[0];
        let source = &line.text_items[0];
        let start = source.raw_text.find('7').expect("formula source");
        let formula = crate::FormulaResult::builder()
            .id(crate::ModelRegionId::detected(1, 1))
            .label(LayoutLabel::InlineFormula)
            .bbox(source.bbox)
            .block_id(Some(block.id.clone()))
            .line_id(Some(line.id.clone()))
            .text_item_range(Some(crate::TextItemRange::new(0, 1)))
            .text_spans(vec![crate::TableTextSpan {
                text_item_id: source.id.clone(),
                byte_range: start..start + 1,
                bbox: source.bbox,
            }])
            .latex(Some("7".to_owned()))
            .markdown(Some("$7$".to_owned()))
            .build();
        document.pages[0].formulas.push(formula);
        assert_eq!(
            MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                .render(&document),
            "- value $7$ \\[item\\]"
        );
        crate::ResultValidator::validate(&document)
            .expect("list and formula source conservation");
    }

    /// Serialized references reject invalid UTF-8 cuts and duplicated ownership while older JSON stays readable.
    #[test]
    fn body_lists_validate_references_and_legacy_json() {
        let document = document(
            LayoutLabel::Text,
            &[("• first", 40.0, 30.0), ("• second", 40.0, 44.0)],
        );
        let mut invalid = document.clone();
        invalid.pages[0].blocks[0].list_items[0].marker_end = 1;
        assert!(crate::ResultValidator::validate(&invalid).is_err());
        let mut invalid = document.clone();
        let item = &mut invalid.pages[0].blocks[0].list_items[0];
        item.line_ids.push(item.line_ids[0].clone());
        assert!(crate::ResultValidator::validate(&invalid).is_err());
        for label in [
            LayoutLabel::Content,
            LayoutLabel::Algorithm,
            LayoutLabel::Chart,
        ] {
            let mut invalid = document.clone();
            invalid.pages[0].blocks[0].label = label;
            assert!(crate::ResultValidator::validate(&invalid).is_err());
            invalid.pages[0].blocks =
                crate::semantic::SemanticAssembler::recover_list_layouts(
                    std::mem::take(&mut invalid.pages[0].blocks),
                )
                .expect("excluded layout");
            assert!(invalid.pages[0].blocks[0].list_items.is_empty());
        }
        let mut old = serde_json::to_value(&document).expect("JSON");
        old["pages"][0]["blocks"][0]
            .as_object_mut()
            .expect("block")
            .remove("list_items");
        let restored: DocumentResult =
            serde_json::from_value(old).expect("legacy JSON");
        assert!(restored.pages[0].blocks[0].list_items.is_empty());
    }

    /// Noncanonical Roman strings must not be assigned arithmetic values and promoted into a list.
    #[test]
    fn body_lists_reject_noncanonical_roman_markers() {
        for marker in ["IXI", "IIV", "IIII", "VX", "IC"] {
            assert_eq!(super::Marker::roman_value(marker), None, "{marker}");
        }
        assert_eq!(super::Marker::roman_value("xiv"), Some(14));
    }
    /// A valid source reference cannot justify a fabricated marker family or numeric value.
    #[test]
    fn body_lists_reject_inconsistent_marker_metadata() {
        let mut document = document(
            LayoutLabel::Text,
            &[("1. first", 40.0, 30.0), ("2. second", 40.0, 44.0)],
        );
        document.pages[0].blocks[0].list_items[0].ordinal = Some(9);
        assert!(crate::ResultValidator::validate(&document).is_err());
    }

    /// Marker-only style runs may use geometry for separation without adding spaces to canonical source text.
    #[test]
    fn body_lists_accept_style_split_markers() {
        let document = document(
            LayoutLabel::Text,
            &[("12)", 40.0, 30.0), ("item", 65.0, 30.0)],
        );
        assert_eq!(
            MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                .render(&document),
            "12. item"
        );
        assert_eq!(document.pages[0].blocks[0].lines[0].text, "12)item");
    }
    /// Real layout models split list rows; confirmed groups must regain one owner before presentation.
    #[test]
    fn body_lists_recover_across_adjacent_text_layouts() {
        let lines = [
            ("1. parent", 40.0, 30.0),
            ("wrapped body", 55.0, 44.0),
            ("a. child", 65.0, 58.0),
            ("b. child two", 65.0, 72.0),
            ("2. next", 40.0, 86.0),
        ];
        let document = document_with_layouts(
            &vec![LayoutLabel::Text; lines.len()],
            &lines,
        );
        assert_eq!(
            document.pages[0].blocks.len(),
            1,
            "one complete list layout"
        );
        assert_eq!(
            MarkdownRenderer::new(RenderView::Semantic, "[formula]")
                .render(&document),
            "1. parent wrapped body\n   1. child\n   2. child two\n2. next"
        );
        crate::ResultValidator::validate(&document)
            .expect("source conservation");
        let value: serde_json::Value = serde_json::from_str(
            &JsonRenderer::render_with_config(
                &document,
                &docparse_config::OutputConfig::default(),
            )
            .expect("wire JSON"),
        )
        .expect("JSON");
        assert!(
            value.pointer("/pages/0/blocks/0/source_regions").is_none(),
            "absorbed layout boxes must stay hidden"
        );
    }

    /// A directory, algorithm, or chart layout must break cross-layout list recovery.
    #[test]
    fn body_lists_never_merge_across_excluded_layouts() {
        for middle in [
            LayoutLabel::Content,
            LayoutLabel::Algorithm,
            LayoutLabel::Chart,
        ] {
            let document = document_with_layouts(
                &[LayoutLabel::Text, middle.clone(), LayoutLabel::Text],
                &[
                    ("1. first", 40.0, 30.0),
                    ("2. middle", 40.0, 44.0),
                    ("3. last", 40.0, 58.0),
                ],
            );
            assert_eq!(document.pages[0].blocks.len(), 3);
            assert_eq!(document.pages[0].blocks[1].label, middle);
            assert!(document.pages[0].blocks[1].list_items.is_empty());
        }
    }
}
