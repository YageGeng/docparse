// SPDX-License-Identifier: Apache-2.0
// Segmentation behavior is derived from LiteParse revision
// b2e76ec5b0c1cb4eb11d67296e916792f4fb5858 and adapted to DocParse facts.

use docparse_layout::{Bbox, Point};
use pdfium::{Page, RectF, TextPage};
use typed_builder::TypedBuilder;

use crate::line::TextAxes;

use crate::{
    ExtractError, PdfProvenance, RepairAction, TextItem, TextItemId,
    TextSource, TextStyle, UnicodeMappingStatus,
};

const MAX_INLINE_GAP: f64 = 15.0;
const ROTATION_TOLERANCE_DEGREES: f64 = 2.0;

/// One immutable PDFium character fact in canonical viewport coordinates.
#[derive(Debug, Clone, TypedBuilder)]
pub(crate) struct TextCharFact {
    pub(crate) character: char,
    pub(crate) bbox: Bbox,
    pub(crate) loose_bbox: Bbox,
    #[builder(default)]
    pub(crate) origin: Option<Point>,
    #[builder(default)]
    pub(crate) font_name: Option<String>,
    #[builder(default)]
    pub(crate) font_size: f64,
    #[builder(default)]
    pub(crate) font_height: Option<f64>,
    #[builder(default)]
    pub(crate) font_ascent: Option<f64>,
    #[builder(default)]
    pub(crate) font_descent: Option<f64>,
    #[builder(default)]
    pub(crate) font_weight: Option<u16>,
    #[builder(default)]
    pub(crate) font_flags: Option<u32>,
    #[builder(default)]
    pub(crate) text_matrix: Option<[f64; 6]>,
    #[builder(default)]
    pub(crate) fill_color: Option<[u8; 4]>,
    #[builder(default)]
    pub(crate) stroke_color: Option<[u8; 4]>,
    #[builder(default)]
    pub(crate) rotation: f64,
    #[builder(default)]
    pub(crate) char_code: u32,
    #[builder(default)]
    pub(crate) generated: bool,
    #[builder(default)]
    pub(crate) unicode_map_error: bool,
    #[builder(default)]
    pub(crate) explicit_break: bool,
    #[builder(default)]
    pub(crate) mcid: Option<i32>,
    #[builder(default)]
    pub(crate) text_object_index: Option<u32>,
    #[builder(default)]
    pub(crate) link: Option<String>,
    #[builder(default)]
    pub(crate) strike: bool,
}

/// Intermediate text fact before conversion to the public nested result type.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct TextItemDraft {
    pub(crate) id: TextItemId,
    pub(crate) raw_text: String,
    pub(crate) bbox: Bbox,
    #[builder(default)]
    pub(crate) baseline: Option<crate::Baseline>,
    pub(crate) rotation: f64,
    #[builder(default)]
    pub(crate) font_name: Option<String>,
    #[builder(default)]
    pub(crate) font_size: Option<f64>,
    #[builder(default)]
    pub(crate) font_height: Option<f64>,
    #[builder(default)]
    pub(crate) font_ascent: Option<f64>,
    #[builder(default)]
    pub(crate) font_descent: Option<f64>,
    #[builder(default)]
    pub(crate) font_weight: Option<u16>,
    #[builder(default)]
    pub(crate) font_flags: Option<u32>,
    #[builder(default)]
    pub(crate) text_matrix: Option<[f64; 6]>,
    #[builder(default)]
    pub(crate) fill_color: Option<[u8; 4]>,
    #[builder(default)]
    pub(crate) stroke_color: Option<[u8; 4]>,
    #[builder(default)]
    pub(crate) char_codes: Vec<u32>,
    pub(crate) unicode_mapping: UnicodeMappingStatus,
    #[builder(default)]
    pub(crate) generated_space: bool,
    #[builder(default)]
    pub(crate) mcid: Option<i32>,
    #[builder(default)]
    pub(crate) text_object_index: Option<u32>,
    #[builder(default)]
    pub(crate) link: Option<String>,
    #[builder(default)]
    pub(crate) strike: bool,
    #[builder(default)]
    pub(crate) repair_actions: Vec<RepairAction>,
    pub(crate) extraction_order: u32,
}

impl TryFrom<TextItemDraft> for TextItem {
    type Error = ExtractError;

    /// Converts a validated draft without overwriting raw text or geometry facts.
    fn try_from(draft: TextItemDraft) -> Result<Self, Self::Error> {
        let style = (draft.font_name.is_some()
            || draft.font_size.is_some()
            || draft.font_flags.is_some()
            || draft.fill_color.is_some()
            || draft.stroke_color.is_some()
            || draft.text_matrix.is_some())
        .then(|| {
            TextStyle::builder()
                .font_name(draft.font_name)
                .font_size(draft.font_size)
                .font_height(draft.font_height)
                .font_ascent(draft.font_ascent)
                .font_descent(draft.font_descent)
                .weight(draft.font_weight)
                .flags(draft.font_flags)
                .fill_color(draft.fill_color)
                .stroke_color(draft.stroke_color)
                .text_matrix(draft.text_matrix)
                .build()
        });
        let provenance = PdfProvenance::builder()
            .char_codes(draft.char_codes)
            .mcid(draft.mcid)
            .text_object_index(draft.text_object_index)
            .unicode_mapping(draft.unicode_mapping)
            .generated_space(draft.generated_space)
            .link(draft.link)
            .strike(draft.strike)
            .build();
        Ok(TextItem::builder()
            .id(draft.id)
            .raw_text(draft.raw_text)
            .raw_bbox(Some(draft.bbox))
            .bbox(draft.bbox)
            .baseline(draft.baseline)
            .rotation(draft.rotation)
            .source(TextSource::Native)
            .extraction_order(draft.extraction_order)
            .final_order(draft.extraction_order)
            .style(style)
            .provenance(Some(provenance))
            .repair_actions(draft.repair_actions)
            .build())
    }
}

#[derive(Debug, Clone, TypedBuilder)]
pub(crate) struct CurrentSegment {
    raw_text: String,
    bbox: Bbox,
    last_bbox: Bbox,
    #[builder(default)]
    baseline: Option<crate::Baseline>,
    width_sum: f64,
    character_count: usize,
    rotation: f64,
    #[builder(default)]
    font_name: Option<String>,
    #[builder(default)]
    font_size: Option<f64>,
    #[builder(default)]
    font_height: Option<f64>,
    #[builder(default)]
    font_ascent: Option<f64>,
    #[builder(default)]
    font_descent: Option<f64>,
    #[builder(default)]
    font_weight: Option<u16>,
    #[builder(default)]
    font_flags: Option<u32>,
    #[builder(default)]
    text_matrix: Option<[f64; 6]>,
    #[builder(default)]
    fill_color: Option<[u8; 4]>,
    #[builder(default)]
    stroke_color: Option<[u8; 4]>,
    #[builder(default)]
    char_codes: Vec<u32>,
    #[builder(default)]
    unicode_error_count: usize,
    #[builder(default)]
    generated_space: bool,
    #[builder(default)]
    mcid: Option<i32>,
    #[builder(default)]
    text_object_index: Option<u32>,
    #[builder(default)]
    link: Option<String>,
    #[builder(default)]
    strike: bool,
    #[builder(default)]
    repair_actions: Vec<RepairAction>,
}

impl CurrentSegment {
    /// Starts one segment from its first visible character fact.
    fn from_fact(fact: TextCharFact) -> Self {
        let character = fact.character.to_string();
        let baseline = fact.oblique_baseline();
        Self::builder()
            .raw_text(character)
            .bbox(fact.loose_bbox)
            .last_bbox(fact.bbox)
            .baseline(baseline)
            .width_sum(fact.bbox.width())
            .character_count(1)
            .rotation(fact.rotation)
            .font_name(fact.font_name)
            .font_size((fact.font_size > 0.0).then_some(fact.font_size))
            .font_height(fact.font_height)
            .font_ascent(fact.font_ascent)
            .font_descent(fact.font_descent)
            .font_weight(fact.font_weight)
            .font_flags(fact.font_flags)
            .text_matrix(fact.text_matrix)
            .fill_color(fact.fill_color)
            .stroke_color(fact.stroke_color)
            .char_codes(vec![fact.char_code])
            .unicode_error_count(usize::from(fact.unicode_map_error))
            .mcid(fact.mcid)
            .text_object_index(fact.text_object_index)
            .link(fact.link)
            .strike(fact.strike)
            .build()
    }

    /// Returns whether an incoming visible character must start a new segment.
    fn must_split(&self, fact: &TextCharFact) -> bool {
        let axes = TextAxes::from(self.rotation);
        let (previous, incoming) = if axes.is_oblique() {
            let (Ok(previous), Ok(incoming)) = (
                axes.project_bbox(self.last_bbox),
                axes.project_bbox(fact.bbox),
            ) else {
                return true;
            };
            (previous, incoming)
        } else {
            (self.last_bbox, fact.bbox)
        };
        // Oblique glyphs advance on a tilted baseline; page-y motion is not a line break.
        let vertical_shift = if axes.is_oblique() {
            match (self.baseline, fact.origin) {
                (Some(baseline), Some(origin)) => (axes.project(origin).y
                    - axes.project(baseline.start).y)
                    .abs(),
                _ => (incoming.center().y - previous.center().y).abs(),
            }
        } else {
            (incoming.top - previous.top).abs()
        };
        let line_threshold = if axes.is_oblique()
            && self.baseline.is_some()
            && fact.origin.is_some()
        {
            // Measured origins share a baseline within PDFium's sub-point rounding; an
            // inflated rotated glyph box must not join a neighboring parallel line.
            2.0
        } else {
            (previous.height().max(incoming.height()) * 0.5).max(2.0)
        };
        let gap = incoming.left - previous.right;
        let average_width = if axes.is_oblique() {
            previous.width()
        } else {
            self.average_width()
        };
        let backtrack = incoming.left + average_width * 0.5 < previous.left;
        let style_changed = self.font_name != fact.font_name
            || self.font_flags != fact.font_flags;
        vertical_shift > line_threshold
            || gap > MAX_INLINE_GAP
            || backtrack
            || (self.rotation - fact.rotation).abs()
                > ROTATION_TOLERANCE_DEGREES
            || style_changed
    }

    /// Appends one source whitespace without changing visible geometry.
    fn push_source_space(&mut self, fact: &TextCharFact) {
        if !self.raw_text.ends_with(' ') {
            self.raw_text.push(' ');
        }
        self.generated_space |= fact.generated;
        self.char_codes.push(fact.char_code);
    }

    /// Appends one visible source character without inferring missing text.
    fn push_visible(&mut self, fact: TextCharFact) {
        match (&mut self.baseline, fact.oblique_baseline()) {
            (Some(baseline), Some(next)) => baseline.end = next.end,
            _ => self.baseline = None,
        }
        self.raw_text.push(fact.character);
        self.bbox = Self::union(self.bbox, fact.loose_bbox);
        self.last_bbox = fact.bbox;
        self.width_sum += fact.bbox.width();
        self.font_size = Self::merge_metric(
            self.font_size,
            (fact.font_size > 0.0).then_some(fact.font_size),
            self.character_count,
        );
        self.font_height = Self::merge_metric(
            self.font_height,
            fact.font_height,
            self.character_count,
        );
        self.font_ascent = Self::merge_metric(
            self.font_ascent,
            fact.font_ascent,
            self.character_count,
        );
        self.font_descent = Self::merge_metric(
            self.font_descent,
            fact.font_descent,
            self.character_count,
        );
        self.character_count += 1;
        self.char_codes.push(fact.char_code);
        self.unicode_error_count += usize::from(fact.unicode_map_error);
        if self.text_object_index != fact.text_object_index {
            self.text_object_index = None;
        }
        if self.link.is_none() {
            self.link = fact.link;
        }
        self.strike |= fact.strike;
    }

    /// Returns the average strict glyph width for gap recovery.
    fn average_width(&self) -> f64 {
        self.width_sum / self.character_count.max(1) as f64
    }

    /// Updates one character-weighted optional metric.
    fn merge_metric(
        current: Option<f64>,
        incoming: Option<f64>,
        count: usize,
    ) -> Option<f64> {
        match (current, incoming) {
            (Some(current), Some(incoming)) => {
                Some((current * count as f64 + incoming) / (count + 1) as f64)
            }
            (Some(current), None) => Some(current),
            (None, Some(incoming)) => Some(incoming),
            (None, None) => None,
        }
    }

    /// Computes the finite union of two validated boxes.
    fn union(left: Bbox, right: Bbox) -> Bbox {
        Bbox::try_from([
            left.left.min(right.left),
            left.top.min(right.top),
            left.right.max(right.right),
            left.bottom.max(right.bottom),
        ])
        .expect("the union of validated bboxes must remain valid")
    }

    /// Converts the completed segment into an indexed draft.
    fn finish(self, page_number: u32, extraction_order: u32) -> TextItemDraft {
        let unicode_mapping = if self.unicode_error_count == 0 {
            UnicodeMappingStatus::Complete
        } else if self.unicode_error_count >= self.character_count {
            UnicodeMappingStatus::Missing
        } else {
            UnicodeMappingStatus::Partial
        };
        // Keep trailing source whitespace because a style or geometry boundary may
        // start the next visible glyph in a separate TextItem on the same line.
        TextItemDraft::builder()
            .id(TextItemId::native(page_number, extraction_order))
            .raw_text(self.raw_text)
            .bbox(self.bbox)
            .baseline(self.baseline)
            .rotation(self.rotation)
            .font_name(self.font_name)
            .font_size(self.font_size)
            .font_height(self.font_height)
            .font_ascent(self.font_ascent)
            .font_descent(self.font_descent)
            .font_weight(self.font_weight)
            .font_flags(self.font_flags)
            .text_matrix(self.text_matrix)
            .fill_color(self.fill_color)
            .stroke_color(self.stroke_color)
            .char_codes(self.char_codes)
            .unicode_mapping(unicode_mapping)
            .generated_space(self.generated_space)
            .mcid(self.mcid)
            .text_object_index(self.text_object_index)
            .link(self.link)
            .strike(self.strike)
            .repair_actions(self.repair_actions)
            .extraction_order(extraction_order)
            .build()
    }
}

impl TextCharFact {
    /// Extends the measured oblique origin to the glyph's projected visible extent.
    fn oblique_baseline(&self) -> Option<crate::Baseline> {
        let axes = TextAxes::from(self.rotation);
        if !axes.is_oblique() {
            return None;
        }
        let origin = self.origin?;
        let local = axes.project(origin);
        let bounds = axes.project_bbox(self.bbox).ok()?;
        Some(crate::Baseline {
            start: origin,
            end: axes.unproject(Point::new(bounds.right.max(local.x), local.y)),
        })
    }
}

/// Accumulates ordered character facts into deterministic text item drafts.
#[derive(Debug, TypedBuilder)]
pub(crate) struct SegmentBuilder {
    page_number: u32,
    next_extraction_index: u32,
    #[builder(default)]
    current: Option<CurrentSegment>,
    #[builder(default)]
    completed: Vec<TextItemDraft>,
}

impl SegmentBuilder {
    /// Creates an empty builder for one public one-based page number.
    pub(crate) fn new(page_number: u32) -> Self {
        Self::builder()
            .page_number(page_number)
            .next_extraction_index(0)
            .build()
    }

    /// Consumes one character fact, flushing only at explicit discontinuities.
    pub(crate) fn push(
        &mut self,
        mut fact: TextCharFact,
    ) -> Result<(), ExtractError> {
        if fact.explicit_break || matches!(fact.character, '\n' | '\r') {
            self.flush()?;
            return Ok(());
        }
        // Some embedded subset fonts expose a visible space or encoded hyphen
        // through PDFium as SOH/STX. Decode only these established
        // mappings before the generic control-character rejection path.
        if fact.character == '\u{0001}' {
            if let Some(current) = &mut self.current {
                current.push_source_space(&fact);
            }
            return Ok(());
        }
        if fact.character == '\u{0002}' {
            fact.character = '-';
            // Isolate the mapped glyph so its repair evidence identifies the exact
            // trailing character instead of ambiguously annotating a whole text run.
            self.flush()?;
            let mut hyphen = CurrentSegment::from_fact(fact);
            hyphen.repair_actions.push(RepairAction::EncodedHyphen);
            self.current = Some(hyphen);
            self.flush()?;
            return Ok(());
        }
        if fact.character.is_control() {
            if let Some(current) = &mut self.current {
                current.repair_actions.push(RepairAction::RemovedControl);
            }
            return Ok(());
        }
        if fact.character.is_whitespace() {
            if let Some(current) = &mut self.current {
                current.push_source_space(&fact);
            }
            return Ok(());
        }

        let split = self
            .current
            .as_ref()
            .is_some_and(|current| current.must_split(&fact));
        if split {
            self.flush()?;
        }
        if let Some(current) = &mut self.current {
            current.push_visible(fact);
        } else {
            self.current = Some(CurrentSegment::from_fact(fact));
        }
        Ok(())
    }

    /// Flushes the final segment and returns drafts in extraction order.
    pub(crate) fn finish(mut self) -> Result<Vec<TextItemDraft>, ExtractError> {
        self.flush()?;
        Ok(self.completed)
    }

    /// Finalizes the current non-empty segment and advances the stable index.
    fn flush(&mut self) -> Result<(), ExtractError> {
        let Some(current) = self.current.take() else {
            return Ok(());
        };
        let extraction_order = self.next_extraction_index;
        self.next_extraction_index = self
            .next_extraction_index
            .checked_add(1)
            .ok_or(ExtractError::ExtractionIndexOverflow)?;
        let draft = current.finish(self.page_number, extraction_order);
        if !draft.raw_text.is_empty() {
            self.completed.push(draft);
        }
        Ok(())
    }
}

/// Extracts stable native text items from one live PDFium page and text page.
pub(crate) fn extract_page_text_items(
    page: &Page<'_, '_>,
    text_page: &TextPage<'_, '_>,
    view_box: &RectF,
    page_number: u32,
) -> Result<Vec<TextItem>, ExtractError> {
    let mut builder = SegmentBuilder::new(page_number);
    let viewport = page.viewport_transform(view_box);
    let object_indices: std::collections::HashMap<_, _> = page
        .text_object_identities()
        .into_iter()
        .enumerate()
        .filter_map(|(index, identity)| {
            u32::try_from(index).ok().map(|index| (identity, index))
        })
        .collect();
    let links = page.links(view_box);
    for index in 0..text_page.char_count() {
        let Some(character) = text_page.char_at(index) else {
            continue;
        };
        let unicode = character.unicode();
        let Some(value) = char::from_u32(unicode) else {
            continue;
        };
        if matches!(unicode, 0 | 0xFFFE | 0xFFFF) {
            continue;
        }
        if matches!(value, '\n' | '\r') {
            let dummy = Bbox::try_from([0.0, 0.0, 1.0, 1.0])?;
            builder.push(
                TextCharFact::builder()
                    .character(value)
                    .bbox(dummy)
                    .loose_bbox(dummy)
                    .explicit_break(true)
                    .build(),
            )?;
            continue;
        }
        if value.is_whitespace() {
            let dummy = Bbox::try_from([0.0, 0.0, 1.0, 1.0])?;
            builder.push(
                TextCharFact::builder()
                    .character(value)
                    .bbox(dummy)
                    .loose_bbox(dummy)
                    .char_code(character.char_code())
                    .generated(character.is_generated())
                    .unicode_map_error(character.has_unicode_map_error())
                    .build(),
            )?;
            continue;
        }

        let strict = character
            .char_box()
            .map(|bbox| RectF {
                left: bbox.left as f32,
                top: bbox.top as f32,
                right: bbox.right as f32,
                bottom: bbox.bottom as f32,
            })
            .map(|bbox| page.bounds_to_viewport(view_box, &bbox));
        let loose = character
            .loose_char_box()
            .map(|bbox| page.bounds_to_viewport(view_box, &bbox));
        let strict = strict
            .and_then(rect_to_bbox)
            .or_else(|| loose.and_then(rect_to_bbox))
            .ok_or(ExtractError::MissingCharacterGeometry { index })?;
        let loose = loose.and_then(rect_to_bbox).unwrap_or(strict);
        let (font_name, font_flags) =
            character.font_info().map_or((None, None), |(name, flags)| {
                (Some(name), u32::try_from(flags).ok())
            });
        let angle = character.angle();
        let rotation = if angle >= 0.0 {
            // PDFium reports clockwise character angles before the page's /Rotate.
            // Recover a direction in PDF's y-up coordinates, then use the same
            // affine map as the glyph bounds. Applying translation to a direction
            // would make the result depend on the CropBox's position.
            let (sine, cosine) = angle.sin_cos();
            let (x, y) = viewport.transform_vector(cosine, -sine);
            f64::from(y.atan2(x).to_degrees()).rem_euclid(360.0)
        } else {
            0.0
        };
        let origin = if TextAxes::from(rotation).is_oblique() {
            character.origin().map(|origin| {
                let (x, y) = page.page_to_viewport(
                    view_box,
                    origin.x as f32,
                    origin.y as f32,
                );
                Point::new(f64::from(x), f64::from(y))
            })
        } else {
            None
        };
        let font_size = character.font_size();
        let font = character.font();
        let font_height = character
            .matrix()
            .map(|matrix| font_size * f64::from(matrix.c.hypot(matrix.d)));
        let font_ascent = font
            .as_ref()
            .and_then(|font| font.ascent(font_size as f32))
            .map(f64::from);
        let font_descent = font
            .as_ref()
            .and_then(|font| font.descent(font_size as f32))
            .map(f64::from);
        let text_matrix = character.matrix().map(|matrix| {
            [
                f64::from(matrix.a),
                f64::from(matrix.b),
                f64::from(matrix.c),
                f64::from(matrix.d),
                f64::from(matrix.e),
                f64::from(matrix.f),
            ]
        });
        let fill_color = character
            .fill_color()
            .map(|color| [color.r, color.g, color.b, color.a]);
        let stroke_color = character
            .stroke_color()
            .map(|color| [color.r, color.g, color.b, color.a]);
        let text_object_index = character
            .text_object_identity()
            .and_then(|identity| object_indices.get(&identity).copied());
        let center = strict.center();
        let link = links
            .iter()
            .find(|link| {
                center.x >= f64::from(link.rect.left)
                    && center.x <= f64::from(link.rect.right)
                    && center.y >= f64::from(link.rect.top)
                    && center.y <= f64::from(link.rect.bottom)
            })
            .map(|link| link.uri.clone());
        builder.push(
            TextCharFact::builder()
                .character(value)
                .bbox(strict)
                .loose_bbox(loose)
                .origin(origin)
                .font_name(font_name)
                .font_size(font_size)
                .font_height(font_height)
                .font_ascent(font_ascent)
                .font_descent(font_descent)
                .font_weight(u16::try_from(character.font_weight()).ok())
                .font_flags(font_flags)
                .text_matrix(text_matrix)
                .fill_color(fill_color)
                .stroke_color(stroke_color)
                .rotation(rotation)
                .char_code(character.char_code())
                .generated(character.is_generated())
                .unicode_map_error(character.has_unicode_map_error())
                .mcid(character.marked_content_id())
                .text_object_index(text_object_index)
                .link(link)
                .build(),
        )?;
    }
    let drafts = builder.finish()?;
    let oblique_count = drafts
        .iter()
        .filter(|item| TextAxes::from(item.rotation).is_oblique())
        .count();
    if oblique_count > 0 {
        tracing::debug!(
            "extracted {} oblique text runs on page {}",
            oblique_count,
            page_number
        );
    }
    drafts.into_iter().map(TextItem::try_from).collect()
}

/// Converts a PDFium viewport rectangle into a validated canonical box.
fn rect_to_bbox(rect: RectF) -> Option<Bbox> {
    Bbox::try_from([
        f64::from(rect.left),
        f64::from(rect.top),
        f64::from(rect.right),
        f64::from(rect.bottom),
    ])
    .ok()
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use docparse_layout::Bbox;
    use pdfium::Library;

    use super::{SegmentBuilder, TextCharFact, extract_page_text_items};
    use crate::UnicodeMappingStatus;

    /// Creates one character fact with a five-point glyph box.
    fn fact(character: char, x: f64, y: f64) -> TextCharFact {
        let bbox = Bbox::try_from([x, y, x + 5.0, y + 10.0])
            .expect("the test glyph bbox must be valid");
        TextCharFact::builder()
            .character(character)
            .bbox(bbox)
            .loose_bbox(bbox)
            .font_size(10.0)
            .char_code(u32::from(character))
            .build()
    }

    /// Resolves a repository fixture path from the core crate root.
    fn fixture_path(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pdf")
            .join(name)
    }

    /// Pushes a sequence of facts and returns all final drafts.
    fn build(
        facts: impl IntoIterator<Item = TextCharFact>,
    ) -> Vec<super::TextItemDraft> {
        let mut builder = SegmentBuilder::new(1);
        for fact in facts {
            builder.push(fact).expect("test facts must be accepted");
        }
        builder.finish().expect("test segments must finish")
    }

    /// Verifies explicit newlines and vertical jumps end the current segment.
    #[test]
    fn explicit_and_geometric_line_breaks_split_segments() {
        let mut newline = fact('\n', 10.0, 0.0);
        newline.explicit_break = true;
        let items = build([
            fact('A', 0.0, 0.0),
            newline,
            fact('B', 0.0, 0.0),
            fact('C', 5.0, 25.0),
        ]);
        let texts: Vec<_> =
            items.iter().map(|item| item.raw_text.as_str()).collect();

        assert_eq!(texts, vec!["A", "B", "C"]);
    }

    /// Verifies backtracking, large gaps, rotations, and style changes split segments.
    #[test]
    fn discontinuities_split_segments() {
        let mut rotated = fact('D', 30.0, 0.0);
        rotated.rotation = 90.0;
        let mut styled = fact('E', 35.0, 0.0);
        styled.rotation = 90.0;
        styled.font_flags = Some(32);
        let items = build([
            fact('A', 20.0, 0.0),
            fact('B', 0.0, 0.0),
            fact('C', 25.0, 0.0),
            rotated,
            styled,
        ]);
        let texts: Vec<_> =
            items.iter().map(|item| item.raw_text.as_str()).collect();

        assert_eq!(texts, vec!["A", "B", "C", "D", "E"]);
    }

    /// Verifies a visual glyph gap never invents a source-space character.
    #[test]
    fn visual_gap_does_not_invent_source_space() {
        let items = build([fact('A', 0.0, 0.0), fact('B', 9.0, 0.0)]);
        let item = items.first().expect("one item must be produced");

        assert_eq!(item.raw_text, "AB");
        assert!(item.repair_actions.is_empty());
    }

    /// Keeps slanted glyphs on their measured baseline despite changes in page-axis box tops.
    #[test]
    fn oblique_glyphs_follow_the_text_axis() {
        let mut first = fact('A', 0.0, 0.0);
        first.rotation = 315.0;
        first.font_size = 48.0;
        first.bbox = Bbox::try_from([8.435, 645.337, 55.410, 692.312])
            .expect("valid first oblique glyph");
        first.loose_bbox = first.bbox;
        first.origin = Some(docparse_layout::Point::new(32.737, 692.312));
        let mut next = fact('C', 0.0, 0.0);
        next.rotation = 315.0;
        next.font_size = 48.0;
        next.bbox = Bbox::try_from([32.364, 621.782, 78.931, 668.349])
            .expect("valid second oblique glyph");
        next.loose_bbox = next.bbox;
        next.origin = Some(docparse_layout::Point::new(55.376, 669.673));

        let items = build([first, next.clone()]);

        assert_eq!(items.len(), 1);
        let item = items.first().expect("one continuous source run");
        assert_eq!(item.raw_text, "AC");
        assert_eq!(item.rotation.to_bits(), 315.0_f64.to_bits());
        let baseline = item.baseline.expect("measured oblique baseline");
        assert!(baseline.end.x > baseline.start.x);
        assert!(baseline.end.y < baseline.start.y);

        // Parallel baselines must remain separate even when their rotated page boxes overlap.
        let mut parallel = next.clone();
        let shift = 30.0 / 2.0_f64.sqrt();
        parallel.bbox = Bbox::try_from([
            next.bbox.left + shift,
            next.bbox.top + shift,
            next.bbox.right + shift,
            next.bbox.bottom + shift,
        ])
        .expect("valid parallel glyph");
        parallel.loose_bbox = parallel.bbox;
        parallel.origin = next.origin.map(|origin| {
            docparse_layout::Point::new(origin.x + shift, origin.y + shift)
        });
        assert_eq!(build([next, parallel]).len(), 2);
    }

    /// Verifies source spaces and dot leaders remain literal extraction facts.
    #[test]
    fn source_spaces_and_dot_leaders_are_preserved() {
        let items = build([
            fact('A', 0.0, 0.0),
            fact(' ', 5.0, 0.0),
            fact('.', 10.0, 0.0),
            fact('.', 15.0, 0.0),
            fact('2', 20.0, 0.0),
        ]);
        let item = items.first().expect("one item must be produced");

        assert_eq!(item.raw_text, "A ..2");
        assert!(item.repair_actions.is_empty());
    }

    /// Verifies a source space survives when the next visible glyph changes style.
    #[test]
    fn source_space_survives_segment_boundary() {
        let mut source_space = fact(' ', 5.0, 0.0);
        source_space.char_code = 32;
        let mut styled = fact('B', 9.0, 0.0);
        styled.font_flags = Some(1);

        let items = build([fact('A', 0.0, 0.0), source_space, styled]);
        let texts = items
            .iter()
            .map(|item| item.raw_text.as_str())
            .collect::<Vec<_>>();

        assert_eq!(texts, vec!["A ", "B"]);
    }

    /// Verifies a subset-font SOH glyph is decoded as source whitespace.
    #[test]
    fn subset_font_space_control_is_restored() {
        let items = build([
            fact('A', 0.0, 0.0),
            fact('\u{0001}', 5.0, 0.0),
            fact('B', 10.0, 0.0),
        ]);
        let item = items.first().expect("one item must be produced");

        assert_eq!(item.raw_text, "A B");
        assert_eq!(item.char_codes, vec![u32::from('A'), 0x01, u32::from('B')]);
        assert!(item.repair_actions.is_empty());
    }

    /// Verifies a subset-font STX glyph remains a positionally isolated hyphen fact.
    #[test]
    fn subset_font_hyphen_control_is_restored() {
        let items = build([
            fact('A', 0.0, 0.0),
            fact('\u{0002}', 5.0, 0.0),
            fact('b', 10.0, 0.0),
        ]);
        let texts = items
            .iter()
            .map(|item| item.raw_text.as_str())
            .collect::<Vec<_>>();
        let hyphen = items.get(1).expect("the hyphen item must exist");

        assert_eq!(texts, vec!["A", "-", "b"]);
        assert_eq!(hyphen.char_codes, vec![0x02]);
        assert_eq!(
            hyphen.repair_actions,
            vec![crate::RepairAction::EncodedHyphen]
        );
    }

    /// Verifies mapping failures remain metadata rather than replacement characters.
    #[test]
    fn unicode_mapping_failure_is_retained() {
        let mut failed = fact('?', 0.0, 0.0);
        failed.unicode_map_error = true;

        let item = build([failed]).remove(0);

        assert_eq!(item.unicode_mapping, UnicodeMappingStatus::Missing);
        assert_eq!(item.raw_text, "?");
    }

    /// Builds a valid in-memory PDF with independent text and page rotations.
    fn oblique_pdf(page_rotation: i32, text_angle: f64) -> Vec<u8> {
        let (sine, cosine) = text_angle.to_radians().sin_cos();
        let content = format!(
            "BT /F1 20 Tf {cosine} {sine} {} {cosine} 200 250 Tm (ACME) Tj ET",
            -sine
        );
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /CropBox [10 20 590 780] /UserUnit 2 /Rotate {page_rotation} /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len()
            ),
        ];
        let mut pdf = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(
                format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes(),
            );
        }
        let xref = pdf.len();
        pdf.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
        for offset in offsets {
            pdf.extend_from_slice(
                format!("{offset:010} 00000 n \n").as_bytes(),
            );
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        pdf
    }

    /// Preserves a complete oblique run and its measured direction under every page rotation.
    #[test]
    fn oblique_extraction_composes_page_rotation() {
        let library = Library::init();
        for text_angle in [15.0, 45.0, 135.0, 225.0, 315.0] {
            for page_rotation in [0, 90, 180, 270] {
                let bytes = oblique_pdf(page_rotation, text_angle);
                let document = library
                    .load_document_from_bytes(&bytes, None)
                    .expect("synthetic rotated PDF must open");
                let page = document.page(0).expect("page must open");
                let view_box = page.view_box().expect("crop box must exist");
                let text_page = page.text().expect("text must load");
                let items =
                    extract_page_text_items(&page, &text_page, &view_box, 1)
                        .expect("rotated extraction must succeed");
                assert_eq!(
                    items.len(),
                    1,
                    "text angle {text_angle}, page rotation {page_rotation}"
                );
                let item = items.first().expect("one complete oblique run");
                assert_eq!(item.raw_text, "ACME");
                let expected =
                    (f64::from(page_rotation) - text_angle).rem_euclid(360.0);
                assert!(
                    (item.rotation - expected).abs() < 0.01,
                    "expected {expected}, got {}",
                    item.rotation
                );
                let baseline =
                    item.baseline.expect("measured baseline must exist");
                let axes = crate::line::TextAxes::from(expected);
                let start = axes.project(baseline.start);
                let end = axes.project(baseline.end);
                assert!(end.x > start.x);
                assert!((end.y - start.y).abs() < 0.01);
            }
        }
    }

    /// Verifies repeated real PDF extraction preserves IDs, text, and geometry order.
    #[test]
    fn real_fixture_extraction_is_stable() {
        let mut runs = Vec::new();
        for _ in 0..3 {
            let library = Library::init();
            let document = library
                .load_document(
                    fixture_path("extraction_metadata.pdf")
                        .to_str()
                        .expect("fixture path must be UTF-8"),
                    None,
                )
                .expect("the PDF fixture must open");
            let page = document.page(0).expect("the fixture page must open");
            let view_box =
                page.view_box().expect("the fixture must have a view box");
            let text_page = page.text().expect("fixture text must load");
            let items =
                extract_page_text_items(&page, &text_page, &view_box, 1)
                    .expect("fixture extraction must succeed");
            runs.push(
                items
                    .into_iter()
                    .map(|item| (item.id, item.raw_text, item.bbox))
                    .collect::<Vec<_>>(),
            );
        }

        let first = runs.first().expect("the first extraction must exist");
        assert!(!first.is_empty());
        assert!(runs.iter().all(|run| run == first));
    }
}
