//! Formula enrichment retains native ownership and one result for every original model region.
use super::RenderedPage;
use crate::{FormulaResult, ModelRegionId, PageResult, PageWarning};
use docparse_common::timing::Timings;
use docparse_config::ValidatedConfig;
use docparse_formula::{FormulaEngine, FormulaError};
use docparse_layout::{Bbox, LayoutDetection, LayoutLabel, PageImage};
use futures_util::{StreamExt, stream};
use std::sync::Arc;

mod crop;
use crop::FormulaCrop;

impl FormulaResult {
    /// Stops raster expansion before unrelated text rows, including rows already grazing the detector edge.
    fn background_limits(&self, page: &PageResult) -> Option<Bbox> {
        if self.label != LayoutLabel::InlineFormula {
            return None;
        }
        let seed = self.crop_bbox.unwrap_or(self.bbox);
        let mut limit = match Bbox::try_from([
            0.0,
            0.0,
            page.width,
            page.height,
        ]) {
            Ok(limit) => limit,
            Err(error) => {
                tracing::warn!(
                    "cannot bound inline formula {} background search on page {}: {}",
                    self.id.as_str(),
                    page.page_number,
                    error
                );
                return None;
            }
        };
        for line in page.blocks.iter().flat_map(|block| &block.lines) {
            if self.line_id.as_ref() == Some(&line.id) {
                continue;
            }
            // Reject geometrically irrelevant rows before scanning their text-item ownership.
            let mut candidate = limit;
            let neighbor = line.bbox;
            if neighbor.left < seed.right && neighbor.right > seed.left {
                if (neighbor.top + neighbor.bottom) * 0.5 < seed.top {
                    candidate.top =
                        candidate.top.max(neighbor.bottom.min(seed.top));
                } else if (neighbor.top + neighbor.bottom) * 0.5 > seed.bottom {
                    candidate.bottom =
                        candidate.bottom.min(neighbor.top.max(seed.bottom));
                }
            }
            if neighbor.top < seed.bottom && neighbor.bottom > seed.top {
                if (neighbor.left + neighbor.right) * 0.5 < seed.left {
                    candidate.left =
                        candidate.left.max(neighbor.right.min(seed.left));
                } else if (neighbor.left + neighbor.right) * 0.5 > seed.right {
                    candidate.right =
                        candidate.right.min(neighbor.left.max(seed.right));
                }
            }
            // Confirmed cross-line scripts cannot exclude their own formula, even when their row would tighten a limit.
            if candidate != limit
                && !line.text_items.iter().any(|item| {
                    self.text_spans
                        .iter()
                        .any(|span| span.text_item_id == item.id)
                })
            {
                limit = candidate;
            }
        }
        Some(limit)
    }

    /// Completes measured glyphs and their unambiguous scripts without adding unrelated neighboring prose.
    fn refine_crop(&mut self, page: &PageResult) {
        if self.line_id.is_none() || self.text_spans.is_empty() {
            return;
        }
        let Some(block) = page
            .blocks
            .iter()
            .find(|block| self.block_id.as_ref() == Some(&block.id))
        else {
            return;
        };
        let mut fragments = Vec::new();
        let mut spans = Vec::new();
        let mut included = std::collections::BTreeSet::new();
        for item in block
            .lines
            .iter()
            .flat_map(|line| &line.text_items)
            .filter(|item| item.source == crate::TextSource::Native)
        {
            // Split an already selected mixed text run at its exact source boundaries.
            let mut parts: Vec<_> = self
                .text_spans
                .iter()
                .filter(|span| span.text_item_id == item.id)
                .cloned()
                .collect();
            let selected = !parts.is_empty();
            if !selected {
                let start =
                    item.raw_text.len() - item.raw_text.trim_start().len();
                let end = item.raw_text.trim_end().len();
                if start >= end {
                    continue;
                }
                parts.push(crate::TableTextSpan {
                    text_item_id: item.id.clone(),
                    byte_range: start..end,
                    bbox: item.bbox,
                });
            }
            for span in parts {
                let Some(text) = item.raw_text.get(span.byte_range.clone())
                else {
                    continue;
                };
                let mut glyph = item.clone();
                glyph.raw_text = text.to_owned();
                glyph.bbox = span.bbox;
                if let Some(baseline) = &mut glyph.baseline {
                    baseline.start.x = span.bbox.left;
                    baseline.end.x = span.bbox.right;
                }
                match crate::line::LineFragment::from_items(
                    vec![glyph],
                    page.width,
                ) {
                    Ok(fragment) => {
                        if selected {
                            included.insert(fragments.len());
                        }
                        fragments.push(fragment);
                        spans.push(span);
                    }
                    Err(error) => tracing::debug!(
                        "cannot refine formula {} from text item {}: {}",
                        self.id.as_str(),
                        item.id.as_str(),
                        error
                    ),
                }
            }
        }
        // Reuse the line assembler's font/baseline/distance rules and ambiguity guard.
        // Compute parents before growing the crop so expansion cannot recruit an unrelated text row.
        let parents: Vec<_> = fragments
            .iter()
            .map(|fragment| fragment.script_parent(&fragments))
            .collect();
        let original_count = included.len();
        loop {
            let previous_count = included.len();
            for (index, parent) in parents.iter().enumerate() {
                if parent.is_some_and(|parent| included.contains(&parent)) {
                    included.insert(index);
                }
            }
            if previous_count == included.len() {
                break;
            }
        }
        let mut crop = self.bbox;
        let mut refined = self.text_spans.clone();
        for (index, span) in spans.into_iter().enumerate() {
            if !included.contains(&index) {
                continue;
            }
            crop.left = crop.left.min(span.bbox.left);
            crop.top = crop.top.min(span.bbox.top);
            crop.right = crop.right.max(span.bbox.right);
            crop.bottom = crop.bottom.max(span.bbox.bottom);
            if !refined.contains(&span) {
                refined.push(span);
            }
        }
        if crop != self.bbox {
            tracing::debug!(
                "refined formula {} crop from {:?} to {:?}, adding {} script slices",
                self.id.as_str(),
                self.bbox,
                crop,
                included.len() - original_count
            );
            self.crop_bbox = Some(crop);
        }
        self.text_spans = refined;
        if let Some(line) = block
            .lines
            .iter()
            .find(|line| self.line_id.as_ref() == Some(&line.id))
        {
            for (index, item) in line.text_items.iter().enumerate() {
                if self
                    .text_spans
                    .iter()
                    .any(|span| span.text_item_id == item.id)
                    && let Some(range) = &mut self.text_item_range
                {
                    range.start = range.start.min(index);
                    range.end = range.end.max(index + 1);
                }
            }
        }
    }

    /// Uses existing measured words to preserve prose sharing a PDF text item with a formula.
    fn bind_text_spans(
        &mut self,
        page: &PageResult,
        words: &std::collections::BTreeMap<
            crate::TextItemId,
            Vec<crate::TableWord>,
        >,
    ) {
        for block in page
            .blocks
            .iter()
            .filter(|block| self.block_id.as_ref() == Some(&block.id))
        {
            // Superscripts and subscripts may have separate source lines; geometry still bounds every slice.
            for line in &block.lines {
                for (item_index, item) in line.text_items.iter().enumerate() {
                    if let Some(measured) = words.get(&item.id) {
                        for word in measured {
                            let Some(text) =
                                item.raw_text.get(word.byte_range.clone())
                            else {
                                continue;
                            };
                            let overlap =
                                self.bbox.intersection_area(word.bbox);
                            // Only the anchor line may contribute a lightly clipped delimiter; neighboring rows still need majority coverage.
                            let anchor_line =
                                self.line_id.as_ref() == Some(&line.id);
                            let punctuation = anchor_line
                                && !text.trim().is_empty()
                                && text
                                    .trim()
                                    .chars()
                                    .all(|ch| ch.is_ascii_punctuation());
                            // TeX overbars are separate, full-size glyphs whose thin ink can lie above the detector box.
                            // Require the existing inline ownership range and measured nearby ink, never neighboring rows or prose.
                            let overbar = anchor_line
                                && item.source == crate::TextSource::Native
                                && matches!(
                                    text.trim(),
                                    "¯" | "\u{0304}" | "\u{0305}"
                                )
                                && self.text_item_range.is_some_and(|range| {
                                    (range.start..range.end)
                                        .contains(&item_index)
                                })
                                && word.bbox.left >= self.bbox.left
                                && word.bbox.right <= self.bbox.right
                                && item.style.as_ref().is_some_and(|style| {
                                    !style.font_size_estimated
                                        && style.font_size.is_some_and(|size| {
                                            size.is_finite()
                                                && size > 0.0
                                                && word.bbox.height()
                                                    <= size * 0.2
                                                && word.bbox.top < self.bbox.top
                                                && word.bbox.bottom
                                                    <= self.bbox.top
                                                        + size * 0.2
                                                && self.bbox.top
                                                    - word.bbox.bottom
                                                    <= size * 0.5
                                        })
                                });
                            if !overbar
                                && (overlap <= 0.0
                                    || (overlap
                                        / word.bbox.area().max(f64::EPSILON)
                                        < 0.5
                                        && !punctuation))
                            {
                                continue;
                            }
                            if overbar {
                                tracing::debug!(
                                    "including clipped overbar {} in formula {} on page {}",
                                    item.id.as_str(),
                                    self.id.as_str(),
                                    page.page_number
                                );
                            }
                            let start = word.byte_range.start + text.len()
                                - text.trim_start().len();
                            let end = word.byte_range.end
                                - (text.len() - text.trim_end().len());
                            if start < end {
                                self.text_spans.push(crate::TableTextSpan {
                                    text_item_id: item.id.clone(),
                                    byte_range: start..end,
                                    bbox: word.bbox,
                                });
                            }
                        }
                    } else if self.bbox.contains_bbox(item.bbox) {
                        self.text_spans.push(crate::TableTextSpan {
                            text_item_id: item.id.clone(),
                            byte_range: 0..item.raw_text.len(),
                            bbox: item.bbox,
                        });
                    }
                }
            }
        }
        // Cross-line scripts are removable only when the primary line has a real insertion anchor.
        if let Some(line_id) = &self.line_id {
            let anchored = page
                .blocks
                .iter()
                .flat_map(|block| &block.lines)
                .find(|line| &line.id == line_id)
                .is_some_and(|line| {
                    line.text_items.iter().any(|item| {
                        self.text_spans
                            .iter()
                            .any(|span| span.text_item_id == item.id)
                    }) || line.inline_spans.iter().any(|span| {
                        span.bbox == self.bbox
                            && span.content_status
                                == crate::InlineContentStatus::Missing
                    })
                });
            if !anchored {
                self.text_spans.clear();
            }
        }
        // An ambiguous mixed run must never be erased merely because its broad item box overlaps.
        if self.text_spans.is_empty()
            && let Some(range) = &mut self.text_item_range
        {
            range.end = range.start;
        }
    }

    /// Leaves sentence punctuation in the source when the recognizer did not include it in LaTeX.
    fn retain_unrecognized_punctuation(&mut self, page: &PageResult) {
        let Some(latex) = &self.latex else {
            return;
        };
        while let Some(span) = self.text_spans.last_mut() {
            let Some(item) = page
                .iter_text_items()
                .find(|item| item.id == span.text_item_id)
            else {
                break;
            };
            while let Some(text) = item.raw_text.get(span.byte_range.clone()) {
                let Some(ch) = text.chars().last() else {
                    break;
                };
                if ch.is_whitespace()
                    || (matches!(ch, ',' | ';' | '.')
                        && !latex.trim_end().ends_with(ch))
                {
                    span.byte_range.end -= ch.len_utf8();
                } else {
                    break;
                }
            }
            if span.byte_range.is_empty() {
                self.text_spans.pop();
            } else {
                break;
            }
        }
    }
    /// Attaches to a recovered cell, existing inline span, or source model block without taking text ownership.
    fn attach(&mut self, page: &PageResult) {
        let formula_bbox = self.bbox;
        let cell = page
            .blocks
            .iter()
            .filter_map(|block| {
                block.table.as_ref().map(|table| (block, table))
            })
            .flat_map(|(block, table)| {
                table.cells.iter().filter_map(move |cell| {
                    let bbox = cell.bbox?;
                    let overlap = bbox.intersection_area(formula_bbox);
                    (overlap / formula_bbox.area() >= 0.5)
                        .then_some((block, cell, overlap))
                })
            })
            .max_by(|a, b| a.2.total_cmp(&b.2));
        if let Some((block, cell, _)) = cell {
            self.block_id = Some(block.id.clone());
            self.table_cell = Some((cell.row, cell.column));
            return;
        }
        if self.label == LayoutLabel::InlineFormula {
            for block in &page.blocks {
                for line in &block.lines {
                    if let Some(span) = line
                        .inline_spans
                        .iter()
                        .find(|span| span.bbox == self.bbox)
                    {
                        self.block_id = Some(block.id.clone());
                        self.line_id = Some(line.id.clone());
                        self.text_item_range = Some(span.text_item_range);
                        return;
                    }
                }
            }
        }
        if let Some(block) = page.blocks.iter().find(|block| {
            block.model_region_id.as_ref() == Some(&self.id)
                || block.source_regions().any(|source| {
                    source.model_region_id.as_ref() == Some(&self.id)
                })
        }) {
            self.block_id = Some(block.id.clone());
        }
    }
}

impl PageResult {
    /// Continuously submits independent crops to shared engines and isolates failures by region.
    pub(crate) async fn recognize_formulas(
        &mut self,
        mut detections: Vec<LayoutDetection>,
        rendered: &RenderedPage,
        engine: Option<&dyn FormulaEngine>,
        config: &ValidatedConfig,
        timings: &Timings,
        words: &std::collections::BTreeMap<
            crate::TextItemId,
            Vec<crate::TableWord>,
        >,
    ) {
        if !config.formula().inline_enabled || !config.formula().display_enabled
        {
            let count = detections.len();
            // Keep native formula geometry for text assembly, but skip disabled crops and model calls entirely.
            detections.retain(|detection| match detection.label {
                LayoutLabel::InlineFormula => config.formula().inline_enabled,
                LayoutLabel::DisplayFormula => config.formula().display_enabled,
                _ => false,
            });
            tracing::debug!(
                "skipping {} disabled formula regions on page {}",
                count - detections.len(),
                self.page_number
            );
        }
        if detections.is_empty() {
            return;
        }
        let mut formulas: Vec<_> = detections
            .into_iter()
            .map(|detection| {
                let mut formula = FormulaResult::builder()
                    .engine(
                        engine
                            .map_or("unavailable", FormulaEngine::name)
                            .to_owned(),
                    )
                    .id(ModelRegionId::detected(
                        self.page_number,
                        detection.source_detection_index,
                    ))
                    .label(detection.label)
                    .bbox(detection.bbox)
                    .build();
                formula.attach(self);
                formula.bind_text_spans(self, words);
                formula.refine_crop(self);
                formula
            })
            .collect();
        let batch_size = config.formula().batch_size;
        let parallelism = match &config.formula().engine {
            docparse_config::FormulaEngineConfig::Texo(texo) => {
                batch_size * texo.session_size
            }
            docparse_config::FormulaEngineConfig::Pp(pp) => {
                batch_size * pp.session_size
            }
            docparse_config::FormulaEngineConfig::Mineru(mineru) => {
                mineru.concurrency
            }
        };
        // Bound crop preparation by active execution plus the explicitly configured pending queue.
        let window = (parallelism + config.formula().queue_size)
            .min(formulas.len())
            .max(1);
        tracing::info!(
            "submitting {} formula regions on page {} to shared queues with window {}",
            formulas.len(),
            self.page_number,
            window
        );
        {
            let page = &*self;
            let requests: Vec<_> = formulas.iter_mut().map(|formula| async move {
                let result = crate::wasm_compat::timeout(std::time::Duration::from_millis(config.formula().timeout_ms), async {
                let engine = engine.ok_or_else(|| FormulaError::Invalid("formula recognizer unavailable".into()))?;
                let _admission = if let Some(admission) = engine.admission() {
                    let queued = timings.start(docparse_common::timing::TimingStage::FormulaQueue);
                    let permit = admission.acquire_owned().await.map_err(|error| FormulaError::Invalid(format!("formula admission closed: {error}")))?;
                    drop(queued);
                    Some(permit)
                } else { None };
                let initial = formula.crop_bbox.unwrap_or(formula.bbox);
                let mut crop = FormulaCrop { bbox: initial, rendered, expansion: formula.background_limits(page) };
                let image = match PageImage::try_from(&mut crop) {
                    Ok(image) => {
                        if crop.bbox != initial {
                            tracing::debug!("aligned inline formula {} crop from {:?} to background boundaries {:?}", formula.id.as_str(), initial, crop.bbox);
                        }
                        formula.crop_bbox = (crop.bbox != formula.bbox).then_some(crop.bbox);
                        Arc::new(image)
                    }
                    Err(error) => {
                        tracing::warn!("formula {} crop failed: {}", formula.id.as_str(), error);
                        return Err(error);
                    }
                };
                engine.recognize(vec![image], timings.clone()).await
                }).await.unwrap_or_else(|_elapsed| Err(FormulaError::Invalid(format!("formula request exceeded {} ms", config.formula().timeout_ms))));
                let result = result.and_then(|output| {
                    let [latex]: [String; 1] = output.try_into().map_err(|output: Vec<String>| FormulaError::Invalid(format!("request returned {} formulas for one crop", output.len())))?;
                    Ok(latex)
                });
                match result {
                    Ok(latex) => {
                        let latex = latex.trim();
                        let latex = latex.strip_prefix("$$").and_then(|s| s.strip_suffix("$$"))
                            .or_else(|| latex.strip_prefix('$').and_then(|s| s.strip_suffix('$'))).unwrap_or(latex).trim();
                        if latex.is_empty() {
                            formula.error = Some("formula model returned empty LaTeX".into());
                            tracing::warn!("formula {} returned empty LaTeX", formula.id.as_str());
                        } else {
                            formula.latex = Some(latex.to_owned());
                            formula.markdown = Some(if formula.label == LayoutLabel::InlineFormula { format!("${latex}$") } else { format!("$$\n{latex}\n$$") });
                            formula.retain_unrecognized_punctuation(page);
                        }
                    }
                    Err(error) => {
                        tracing::warn!("formula {} failed on page {}: {}", formula.id.as_str(), page.page_number, error);
                        formula.error = Some(error.to_string());
                    }
                }
            }).collect();
            let requests = stream::iter(requests).buffer_unordered(window);
            tokio::pin!(requests);
            while requests.next().await.is_some() {}
        }
        for formula in &formulas {
            if let Some(error) = &formula.error {
                self.warnings.push(PageWarning {
                    code: "FormulaRecognitionFailed".into(),
                    stage: "formula".into(),
                    message: format!("{}: {error}", formula.id.as_str()),
                });
            }
        }
        tracing::info!(
            "completed formula recognition on page {}: {} succeeded, {} failed",
            self.page_number,
            formulas.iter().filter(|f| f.latex.is_some()).count(),
            formulas.iter().filter(|f| f.error.is_some()).count()
        );
        self.formulas = formulas;
        self.project_formulas(&config.output().formula_placeholder);
        self.warnings.sort_by(|a, b| {
            (&a.stage, &a.code, &a.message)
                .cmp(&(&b.stage, &b.code, &b.message))
        });
    }

    /// Enriches paragraph and cell presentation while keeping canonical source spans and text unchanged.
    fn project_formulas(&mut self, placeholder: &str) {
        for block in &mut self.blocks {
            block.markdown = None;
            let Some(table) = &mut block.table else {
                let inline: Vec<_> = self
                    .formulas
                    .iter()
                    .filter(|formula| {
                        formula.block_id.as_ref() == Some(&block.id)
                            && formula.label == LayoutLabel::InlineFormula
                        && formula.line_id.is_some()
                        && formula.markdown.is_some()
                        && (!formula.text_spans.is_empty()
                            || formula.text_item_range.is_some_and(|range| range.start < range.end)
                            || block.lines.iter().any(|line| {
                                formula.line_id.as_ref() == Some(&line.id)
                                    && line.inline_spans.iter().any(|span| {
                                        span.bbox == formula.bbox
                                            && span.content_status == crate::InlineContentStatus::Missing
                                    })
                            }))
                    })
                    .collect();
                if !inline.is_empty() {
                    // Reuse UTF-8-aware range replacement; prose must remain literal in browser Markdown.
                    block.markdown = Some(
                        block
                            .lines
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
                continue;
            };
            let sources: std::collections::BTreeMap<_, _> = block
                .lines
                .iter()
                .flat_map(|line| &line.text_items)
                .map(|item| (&item.id, item.raw_text.as_str()))
                .collect();
            for cell in &mut table.cells {
                let mut formulas: Vec<_> = self
                    .formulas
                    .iter()
                    .filter(|formula| {
                        formula.block_id.as_ref() == Some(&block.id)
                            && formula.table_cell
                                == Some((cell.row, cell.column))
                            && formula.latex.is_some()
                    })
                    .collect();
                if formulas.is_empty() {
                    continue;
                }
                formulas.sort_by(|a, b| {
                    a.bbox
                        .top
                        .total_cmp(&b.bbox.top)
                        .then_with(|| a.bbox.left.total_cmp(&b.bbox.left))
                });
                let source = cell
                    .lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut mapped = Vec::new();
                let mut line_offset = 0;
                for line in &cell.lines {
                    let mut searched = 0;
                    for span in &line.spans {
                        let Some(raw) = sources
                            .get(&span.text_item_id)
                            .and_then(|text| text.get(span.byte_range.clone()))
                        else {
                            continue;
                        };
                        let token = raw.trim();
                        if token.is_empty() {
                            continue;
                        }
                        let Some(found) = line
                            .text
                            .get(searched..)
                            .and_then(|text| text.find(token))
                        else {
                            continue;
                        };
                        let position = searched + found;
                        let start = span.byte_range.start + raw.len()
                            - raw.trim_start().len();
                        mapped.push((
                            &span.text_item_id,
                            start..start + token.len(),
                            line_offset + position,
                        ));
                        searched = position + token.len();
                    }
                    line_offset += line.text.len() + 1;
                }
                let values: Vec<_> = formulas
                    .iter()
                    .filter_map(|formula| {
                        formula
                            .latex
                            .as_ref()
                            .map(|latex| (formula, format!("${latex}$")))
                    })
                    .collect();
                let mut replacements = Vec::new();
                let mut unanchored = Vec::new();
                for (formula, markdown) in &values {
                    let mut ranges = Vec::new();
                    for span in &formula.text_spans {
                        for (id, native, offset) in &mapped {
                            if **id != span.text_item_id {
                                continue;
                            }
                            let start = native.start.max(span.byte_range.start);
                            let end = native.end.min(span.byte_range.end);
                            if start < end {
                                ranges.push(
                                    offset + start - native.start
                                        ..offset + end - native.start,
                                );
                            }
                        }
                    }
                    if !ranges.is_empty() {
                        replacements.push((ranges, markdown.as_str()));
                    } else {
                        unanchored.push(markdown.as_str());
                    }
                }
                let mut markdown = crate::render::replace_formula_ranges(
                    &source,
                    replacements,
                    true,
                );
                // A scanned formula can exist without native spans; retain all existing cell prose.
                for formula in unanchored {
                    if !markdown.is_empty() {
                        markdown.push(' ');
                    }
                    markdown.push_str(formula);
                }
                cell.markdown = Some(markdown);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replays captured production rasters without model inference, recording timings and exact output fingerprints.
    #[test]
    #[ignore = "requires FORMULA_CROP_BENCH_PAGE, FORMULA_CROP_BENCH_RASTER and FORMULA_CROP_BENCH_OUTPUT"]
    fn benchmark_captured_formula_crops() {
        use docparse_layout::{
            AffineTransform, PageImageInput, PageRotation, PageTransform,
            PageTransformInput, PixelFormat,
        };
        use sha2::{Digest, Sha256};
        use std::hint::black_box;
        let page: PageResult = serde_json::from_slice(
            &std::fs::read(
                std::env::var("FORMULA_CROP_BENCH_PAGE").expect("page path"),
            )
            .expect("page JSON"),
        )
        .expect("page");
        let raster = image::open(
            std::env::var("FORMULA_CROP_BENCH_RASTER").expect("raster path"),
        )
        .expect("raster")
        .into_rgb8();
        let (width, height) = raster.dimensions();
        let rendered = RenderedPage {
            page_number: page.page_number,
            image: Arc::new(
                PageImage::try_from(
                    PageImageInput::builder()
                        .width(width)
                        .height(height)
                        .pixel_format(PixelFormat::Rgb8)
                        .data(Arc::from(raster.into_raw()))
                        .build(),
                )
                .expect("image"),
            ),
            transform: PageTransform::try_from(
                PageTransformInput::builder()
                    .page_to_viewport(AffineTransform::identity())
                    .viewport_width(page.width)
                    .viewport_height(page.height)
                    .render_width(width)
                    .render_height(height)
                    .model_width(width)
                    .model_height(height)
                    .rotation(PageRotation::Degrees0)
                    .build(),
            )
            .expect("transform"),
        };
        let formulas: Vec<_> = page
            .formulas
            .iter()
            .filter(|formula| formula.label == LayoutLabel::InlineFormula)
            .cloned()
            .map(|mut formula| {
                formula.crop_bbox = None;
                formula.refine_crop(&page);
                let expansion = formula.background_limits(&page);
                (formula, expansion)
            })
            .collect();
        assert!(!formulas.is_empty(), "capture must contain inline formulas");
        let cases: Vec<_> = formulas.iter().map(|(formula, expansion)| {
            let seed = formula.crop_bbox.unwrap_or(formula.bbox);
            let mut crop = FormulaCrop { bbox: seed, rendered: &rendered, expansion: *expansion };
            let image = PageImage::try_from(&mut crop).expect("crop");
            serde_json::json!({"id": formula.id, "seed": seed, "limits": expansion, "crop": crop.bbox, "width": image.width(), "height": image.height(), "sha256": Sha256::digest(image.data().as_ref()).iter().map(|byte| format!("{byte:02x}")).collect::<String>()})
        }).collect();
        let mut measurements = serde_json::Map::new();
        for stage in ["limits", "crop", "combined"] {
            let compute_limits = stage != "crop";
            let compute_crop = stage != "limits";
            let mut samples = Vec::new();
            for _ in 0..7 {
                let started = web_time::Instant::now();
                for _ in 0..1000 {
                    for (formula, cached) in &formulas {
                        let formula = black_box(formula);
                        let expansion = if compute_limits {
                            formula.background_limits(black_box(&page))
                        } else {
                            *cached
                        };
                        if compute_crop {
                            let mut crop = FormulaCrop {
                                bbox: formula.crop_bbox.unwrap_or(formula.bbox),
                                rendered: black_box(&rendered),
                                expansion,
                            };
                            black_box(
                                PageImage::try_from(&mut crop).expect("crop"),
                            );
                        } else {
                            black_box(expansion);
                        }
                    }
                }
                samples.push(
                    started.elapsed().as_secs_f64() * 1_000_000.0 / 1000.0,
                );
            }
            samples.sort_by(f64::total_cmp);
            measurements.insert(stage.into(), serde_json::json!({"median_us_per_page": samples.get(3).expect("median"), "samples_us_per_page": samples}));
        }
        let report = serde_json::json!({"inline_formulas": formulas.len(), "measurements": measurements, "cases": cases});
        std::fs::write(
            std::env::var("FORMULA_CROP_BENCH_OUTPUT").expect("output path"),
            serde_json::to_vec_pretty(&report).expect("report"),
        )
        .expect("save report");
        eprintln!(
            "{} inline formulas: {}",
            formulas.len(),
            report.get("measurements").expect("timings")
        );
    }

    /// Captures the exact raster passed to recognition without loading a model.
    struct CropCapture(std::sync::Mutex<Vec<Arc<PageImage>>>);

    impl FormulaEngine for CropCapture {
        /// Identifies the crop observation boundary in formula results.
        fn name(&self) -> &str {
            "crop-capture"
        }

        /// Records production crop pixels and supplies a minimal successful recognition result.
        fn recognize(
            &self,
            images: Vec<Arc<PageImage>>,
            _timings: Timings,
        ) -> docparse_common::WasmBoxedFuture<
            '_,
            Result<Vec<String>, FormulaError>,
        > {
            Box::pin(async move {
                let output = vec!["x".to_owned(); images.len()];
                *self.0.lock().expect("crops") = images;
                Ok(output)
            })
        }
    }

    /// Inline crops stop at the first background separators on all sides without including adjacent text.
    #[tokio::test]
    async fn inline_crops_follow_raster_background_separators() {
        use docparse_layout::{
            AffineTransform, GeometrySource, PageImageInput, PageRotation,
            PageTransform, PageTransformInput, PixelFormat,
        };
        for (background, ink) in [
            ([255, 255, 255], [0, 0, 0]),
            ([238, 242, 228], [30, 30, 30]),
            ([32, 48, 64], [220, 230, 240]),
        ] {
            for (scale, rotation) in [
                (1.0, PageRotation::Degrees0),
                (2.0, PageRotation::Degrees90),
            ] {
                let mut raster =
                    image::RgbImage::from_pixel(50, 50, image::Rgb(background));
                // The central ink crosses every detector edge; four unrelated regions sit beyond clear separators.
                for [left, top, right, bottom] in [
                    [17, 15, 33, 35],
                    [15, 10, 35, 13],
                    [15, 37, 35, 40],
                    [12, 18, 15, 32],
                    [35, 18, 38, 32],
                ] {
                    for y in top..bottom {
                        for x in left..right {
                            raster.put_pixel(x, y, image::Rgb(ink));
                        }
                    }
                }
                // A faint scanning artifact remains background; a dark one-pixel stroke would not.
                raster.put_pixel(
                    19,
                    14,
                    image::Rgb(
                        background
                            .map(|channel: u8| channel.saturating_sub(10)),
                    ),
                );
                let expected =
                    image::imageops::crop_imm(&raster, 16, 14, 18, 22)
                        .to_image();
                let rendered = RenderedPage {
                    page_number: 1,
                    image: Arc::new(
                        PageImage::try_from(
                            PageImageInput::builder()
                                .width(50)
                                .height(50)
                                .pixel_format(PixelFormat::Rgb8)
                                .data(Arc::from(raster.into_raw()))
                                .build(),
                        )
                        .expect("image"),
                    ),
                    transform: PageTransform::try_from(
                        PageTransformInput::builder()
                            .page_to_viewport(AffineTransform::identity())
                            .viewport_width(50.0 / scale)
                            .viewport_height(50.0 / scale)
                            .render_width(50)
                            .render_height(50)
                            .model_width(50)
                            .model_height(50)
                            .rotation(rotation)
                            .build(),
                    )
                    .expect("transform"),
                };
                for (label, neighbors) in [
                    (LayoutLabel::InlineFormula, false),
                    (LayoutLabel::DisplayFormula, false),
                    (LayoutLabel::InlineFormula, true),
                ] {
                    let original = Bbox::try_from([
                        20.0 / scale,
                        20.0 / scale,
                        30.0 / scale,
                        30.0 / scale,
                    ])
                    .expect("box");
                    let detection = LayoutDetection::builder()
                        .source_detection_index(0)
                        .raw_label(label.to_str().to_owned())
                        .class_id(label.idx().expect("label") as i64)
                        .label(label.clone())
                        .confidence(0.9)
                        .bbox(original)
                        .geometry_source(GeometrySource::DerivedFromBbox)
                        .model_order(0)
                        .metadata(Default::default())
                        .build();
                    // Model boxes can graze the previous row's descenders or the following row's ascenders.
                    let blocks = if neighbors {
                        let block_id = crate::BlockId::model(1, 99, 0);
                        let lines = [(0, 15.0, 21.0), (1, 29.0, 40.0)]
                            .into_iter()
                            .map(|(index, top, bottom)| {
                                let bbox = Bbox::try_from([
                                    17.0 / scale,
                                    top / scale,
                                    33.0 / scale,
                                    bottom / scale,
                                ])
                                .expect("neighbor");
                                crate::Line::builder()
                                    .id(crate::LineId::new(&block_id, index))
                                    .text("neighbor".into())
                                    .bbox(bbox)
                                    .direction(
                                        crate::WritingDirection::LeftToRight,
                                    )
                                    .text_items(vec![
                                        crate::TextItem::builder()
                                            .id(crate::TextItemId::native(
                                                1, index,
                                            ))
                                            .raw_text("neighbor".into())
                                            .bbox(bbox)
                                            .source(crate::TextSource::Native)
                                            .build(),
                                    ])
                                    .build()
                            })
                            .collect();
                        vec![
                            crate::Block::builder()
                                .id(block_id)
                                .label(LayoutLabel::Text)
                                .text("neighbor neighbor".into())
                                .label_source(crate::LabelSource::Model)
                                .bbox(
                                    Bbox::try_from([
                                        17.0 / scale,
                                        15.0 / scale,
                                        33.0 / scale,
                                        40.0 / scale,
                                    ])
                                    .expect("block"),
                                )
                                .final_order(0)
                                .lines(lines)
                                .build(),
                        ]
                    } else {
                        Vec::new()
                    };
                    let mut page = PageResult::builder()
                        .page_number(1)
                        .width(50.0 / scale)
                        .height(50.0 / scale)
                        .rotation(0)
                        .blocks(blocks)
                        .build();
                    let engine = CropCapture(std::sync::Mutex::new(Vec::new()));
                    page.recognize_formulas(
                        vec![detection],
                        &rendered,
                        Some(&engine),
                        &ValidatedConfig::try_from(
                            docparse_config::RawConfig::default(),
                        )
                        .expect("config"),
                        &Timings::default(),
                        &Default::default(),
                    )
                    .await;
                    let captured = engine.0.lock().expect("crops");
                    let captured = captured.first().expect("crop");
                    let formula = page.formulas.first().expect("formula");
                    assert_eq!(formula.bbox, original);
                    if neighbors {
                        assert_eq!(
                            (captured.width(), captured.height()),
                            (18, 10)
                        );
                        assert_eq!(
                            formula.crop_bbox,
                            Some(
                                Bbox::try_from([
                                    16.0 / scale,
                                    20.0 / scale,
                                    34.0 / scale,
                                    30.0 / scale
                                ])
                                .expect("guarded box")
                            )
                        );
                    } else if label == LayoutLabel::InlineFormula {
                        assert_eq!(
                            (captured.width(), captured.height()),
                            (18, 22)
                        );
                        assert_eq!(captured.data().as_ref(), expected.as_raw());
                        assert_eq!(
                            formula.crop_bbox,
                            Some(
                                Bbox::try_from([
                                    16.0 / scale,
                                    14.0 / scale,
                                    34.0 / scale,
                                    36.0 / scale
                                ])
                                .expect("expanded box")
                            )
                        );
                    } else {
                        assert_eq!(
                            (captured.width(), captured.height()),
                            (10, 10)
                        );
                        assert!(formula.crop_bbox.is_none());
                    }
                }
            }
        }
    }

    /// A clipped measured overbar belongs to its anchored formula, while nearby text remains outside.
    #[test]
    fn motion_descriptor_recovers_only_its_anchored_overbar() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/formula/motion-descriptor-overbar.json"
        ))
        .expect("fixture");
        let block: crate::Block = serde_json::from_value(
            fixture.get("block").expect("fixture block").clone(),
        )
        .expect("block");
        let original: FormulaResult = serde_json::from_value(
            fixture.get("formula").expect("fixture formula").clone(),
        )
        .expect("formula");
        let original_words: std::collections::BTreeMap<
            crate::TextItemId,
            Vec<crate::TableWord>,
        > = serde_json::from_value(
            fixture.get("words").expect("fixture words").clone(),
        )
        .expect("words");
        for case in [
            "overbar",
            "combining",
            "prose",
            "far-above",
            "sideways",
            "estimated",
            "outside-range",
            "other-line",
        ] {
            let expected = matches!(case, "overbar" | "combining");
            let mut page = PageResult::builder()
                .page_number(4)
                .width(612.0)
                .height(792.0)
                .rotation(0)
                .blocks(vec![block.clone()])
                .build();
            let mut formula = original.clone();
            let mut words = original_words.clone();
            let block = page.blocks.first_mut().expect("block");
            let line = block.lines.first_mut().expect("line");
            let accent = line.text_items.last_mut().expect("overbar");
            let accent_id = accent.id.clone();
            let word = words
                .get_mut(&accent_id)
                .expect("accent words")
                .first_mut()
                .expect("accent word");
            match case {
                "combining" => accent.raw_text = "\u{0305}".into(),
                "prose" => {
                    accent.raw_text = "a".into();
                    word.byte_range.end = 1;
                }
                "far-above" => {
                    word.bbox.top -= 20.0;
                    word.bbox.bottom -= 20.0;
                }
                "sideways" => {
                    word.bbox.left += 30.0;
                    word.bbox.right += 30.0;
                }
                "estimated" => {
                    accent.style.as_mut().expect("style").font_size_estimated =
                        true
                }
                "outside-range" => {
                    formula.text_item_range.as_mut().expect("range").end = 3
                }
                "other-line" => {
                    let accent = line.text_items.pop().expect("accent");
                    block.lines.push(
                        crate::Line::builder()
                            .id(crate::LineId::new(&block.id, 4))
                            .text(accent.raw_text.clone())
                            .bbox(accent.bbox)
                            .direction(crate::WritingDirection::LeftToRight)
                            .text_items(vec![accent])
                            .build(),
                    );
                }
                _ => {}
            }
            let before = serde_json::to_value(&page).expect("source");
            formula.bind_text_spans(&page, &words);
            formula.refine_crop(&page);
            assert_eq!(
                formula
                    .text_spans
                    .iter()
                    .any(|span| span.text_item_id == accent_id),
                expected,
                "{case}"
            );
            assert_eq!(formula.bbox, original.bbox);
            assert_eq!(
                before,
                serde_json::to_value(&page).expect("unchanged source")
            );
            if expected {
                assert!(
                    formula.crop_bbox.expect("completed crop").top <= 572.928
                );
                formula.latex = Some(r"\bar{\theta}_{f}".into());
                formula.markdown = Some(r"$\bar{\theta}_{f}$".into());
                page.formulas.push(formula);
                page.project_formulas("[formula]");
                let block = page.blocks.first().expect("block");
                assert_eq!(
                    block.markdown.as_deref(),
                    Some(r"on the motion descriptor $\bar{\theta}_{f}$")
                );
                assert!(block.text.contains('¯'));
            }
        }
    }

    /// Grazing punctuation from the real page-four/page-thirteen failures cannot expand another formula's crop.
    #[test]
    fn formula_crop_keeps_punctuation_on_its_source_line() {
        for (bounds, parent, punctuation, same_line) in [
            (
                [318.5, 616.5, 416.0, 629.0],
                [319.0, 618.0, 326.0, 627.0],
                [396.089, 628.667, 400.689, 637.625],
                false,
            ),
            (
                [226.5, 96.0, 293.0, 109.0],
                [282.151, 96.756, 290.022, 105.713],
                [243.592, 108.263, 247.458, 118.216],
                false,
            ),
            (
                [318.5, 616.5, 416.0, 629.0],
                [319.0, 618.0, 326.0, 627.0],
                [415.5, 618.0, 420.5, 627.0],
                true,
            ),
        ] {
            let block_id = crate::BlockId::model(1, 0, 0);
            let bbox = Bbox::try_from(bounds).expect("formula bounds");
            let items: Vec<_> = [("T", parent), (")", punctuation)]
                .into_iter()
                .enumerate()
                .map(|(index, (text, bounds))| {
                    crate::TextItem::builder()
                        .id(crate::TextItemId::native(1, index as u32))
                        .raw_text(text.into())
                        .bbox(Bbox::try_from(bounds).expect("glyph bounds"))
                        .source(crate::TextSource::Native)
                        .build()
                })
                .collect();
            let words = items
                .iter()
                .map(|item| {
                    (
                        item.id.clone(),
                        vec![
                            crate::TableWord::builder()
                                .byte_range(0..item.raw_text.len())
                                .bbox(item.bbox)
                                .build(),
                        ],
                    )
                })
                .collect();
            let lines: Vec<_> = items
                .chunks(if same_line { 2 } else { 1 })
                .enumerate()
                .map(|(index, items)| {
                    crate::Line::builder()
                        .id(crate::LineId::new(&block_id, index as u32))
                        .text(
                            items
                                .iter()
                                .map(|item| item.raw_text.as_str())
                                .collect(),
                        )
                        .bbox(bbox)
                        .direction(crate::WritingDirection::LeftToRight)
                        .text_items(items.to_vec())
                        .build()
                })
                .collect();
            let block = crate::Block::builder()
                .id(block_id.clone())
                .label(LayoutLabel::Text)
                .label_source(crate::LabelSource::Model)
                .text(crate::Block::derive_text(&LayoutLabel::Text, &lines))
                .bbox(bbox)
                .final_order(0)
                .lines(lines)
                .build();
            let page = PageResult::builder()
                .page_number(1)
                .width(612.0)
                .height(792.0)
                .rotation(0)
                .blocks(vec![block])
                .build();
            let mut formula = FormulaResult::builder()
                .id(ModelRegionId::detected(1, 0))
                .label(LayoutLabel::InlineFormula)
                .bbox(bbox)
                .block_id(Some(block_id.clone()))
                .line_id(Some(crate::LineId::new(&block_id, 0)))
                .text_item_range(Some(crate::TextItemRange::new(0, 1)))
                .build();
            formula.bind_text_spans(&page, &words);
            formula.refine_crop(&page);
            assert_eq!(
                formula
                    .text_spans
                    .iter()
                    .any(|span| span.text_item_id
                        == crate::TextItemId::native(1, 1)),
                same_line
            );
            assert!(formula.crop_bbox.unwrap_or(bbox).bottom <= bbox.bottom);
            if same_line {
                assert!(
                    formula
                        .crop_bbox
                        .expect("clipped delimiter")
                        .contains_bbox(
                            Bbox::try_from(punctuation)
                                .expect("delimiter bounds")
                        )
                );
            }
        }
    }

    /// Real softmax geometry recovers a missing final index, but ordinary neighboring text stays outside.
    #[test]
    fn softmax_crop_recovers_only_a_typographic_script() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/formula/softmax-subscript.json"
        ))
        .expect("fixture");
        let block: crate::Block = serde_json::from_value(
            fixture.get("block").expect("fixture block").clone(),
        )
        .expect("block");
        let original: FormulaResult = serde_json::from_value(
            fixture.get("formula").expect("fixture formula").clone(),
        )
        .expect("formula");
        for case in ["script", "body", "estimated"] {
            let script = case == "script";
            let mut page = PageResult::builder()
                .page_number(4)
                .width(612.0)
                .height(792.0)
                .rotation(0)
                .blocks(vec![block.clone()])
                .build();
            if case == "body" {
                let line = page
                    .blocks
                    .first_mut()
                    .expect("block")
                    .lines
                    .first_mut()
                    .expect("line");
                let baseline = line.baseline;
                let item = line.text_items.last_mut().expect("index");
                item.raw_text = "next".into();
                item.style.as_mut().expect("style").font_size = Some(9.9626);
                item.baseline = baseline;
            }
            if case == "estimated" {
                page.blocks
                    .first_mut()
                    .expect("block")
                    .lines
                    .first_mut()
                    .expect("line")
                    .text_items
                    .last_mut()
                    .expect("index")
                    .style
                    .as_mut()
                    .expect("style")
                    .font_size_estimated = true;
            }
            let before = serde_json::to_value(&page).expect("source JSON");
            let mut formula = original.clone();
            formula.refine_crop(&page);
            assert_eq!(formula.bbox, original.bbox);
            assert_eq!(
                formula
                    .text_spans
                    .iter()
                    .any(|span| span.text_item_id.as_str() == "p4:t374"),
                script
            );
            assert_eq!(
                before,
                serde_json::to_value(&page).expect("unchanged source")
            );
            if script {
                assert!(
                    formula.crop_bbox.expect("refined crop").right >= 281.49
                );
                assert_eq!(formula.text_item_range.expect("range").end, 12);
            }
        }
    }

    /// A subscript in a separate PDF text line is replaced once without consuming neighboring prose.
    #[test]
    fn inline_projection_includes_subscripts_from_other_source_lines() {
        let block_id = crate::BlockId::model(1, 0, 0);
        let bbox = Bbox::try_from([0.0, 0.0, 100.0, 20.0]).expect("bbox");
        let items: Vec<_> = [
            ("Before ", [0.0, 0.0, 30.0, 10.0]),
            ("T", [35.0, 0.0, 45.0, 10.0]),
            ("extract", [45.0, 8.0, 75.0, 14.0]),
            (" next", [80.0, 0.0, 100.0, 10.0]),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (text, bounds))| {
            crate::TextItem::builder()
                .id(crate::TextItemId::native(1, index as u32))
                .raw_text(text.into())
                .bbox(Bbox::try_from(bounds).expect("item bbox"))
                .source(crate::TextSource::Native)
                .build()
        })
        .collect();
        let lines = items
            .chunks(2)
            .enumerate()
            .map(|(index, items)| {
                crate::Line::builder()
                    .id(crate::LineId::new(&block_id, index as u32))
                    .text(
                        items
                            .iter()
                            .map(|item| item.raw_text.as_str())
                            .collect::<String>(),
                    )
                    .bbox(bbox)
                    .direction(crate::WritingDirection::LeftToRight)
                    .text_items(items.to_vec())
                    .build()
            })
            .collect();
        let block = crate::Block::builder()
            .id(block_id.clone())
            .label(LayoutLabel::Text)
            .label_source(crate::LabelSource::Model)
            .text("Before T\nextract next".into())
            .bbox(bbox)
            .final_order(0)
            .lines(lines)
            .build();
        let mut page = PageResult::builder()
            .page_number(1)
            .width(100.0)
            .height(100.0)
            .rotation(0)
            .blocks(vec![block])
            .build();
        let mut formula = FormulaResult::builder()
            .id(ModelRegionId::detected(1, 1))
            .label(LayoutLabel::InlineFormula)
            .bbox(
                Bbox::try_from([35.0, 0.0, 75.0, 16.0]).expect("formula bbox"),
            )
            .block_id(Some(block_id.clone()))
            .line_id(Some(crate::LineId::new(&block_id, 0)))
            .text_item_range(Some(crate::TextItemRange::new(1, 2)))
            .latex(Some("T_{extract}".into()))
            .markdown(Some("$T_{extract}$".into()))
            .build();
        formula.bind_text_spans(&page, &std::collections::BTreeMap::new());
        assert_eq!(formula.text_spans.len(), 2);
        page.formulas.push(formula);
        page.project_formulas("[formula]");
        let block = page.blocks.first().expect("block");
        assert_eq!(
            block.markdown.as_deref(),
            Some("Before $T_{extract}$\n next")
        );
        assert_eq!(block.text, "Before T\nextract next");
        let mut formula = page.formulas.remove(0);
        formula.text_spans.clear();
        formula.bbox =
            Bbox::try_from([45.0, 8.0, 75.0, 16.0]).expect("script-only bbox");
        formula.bind_text_spans(&page, &std::collections::BTreeMap::new());
        page.formulas.push(formula);
        page.project_formulas("[formula]");
        assert!(
            page.blocks.first().expect("block").markdown.is_none(),
            "an unanchored script must not be removed from prose"
        );
    }

    /// Inline projection preserves UTF-8 prose, punctuation and literal Markdown around exact formula spans.
    #[test]
    fn inline_projection_preserves_prose_and_source() {
        let bbox = Bbox::try_from([0.0, 0.0, 100.0, 20.0]).expect("bbox");
        let block_id = crate::BlockId::model(1, 0, 0);
        let item = crate::TextItem::builder()
            .id(crate::TextItemId::native(1, 0))
            .raw_text("学习 *rate* of 7, next".into())
            .bbox(bbox)
            .source(crate::TextSource::Native)
            .build();
        let line_id = crate::LineId::new(&block_id, 0);
        let formula = FormulaResult::builder()
            .id(ModelRegionId::detected(1, 1))
            .label(LayoutLabel::InlineFormula)
            .bbox(bbox)
            .block_id(Some(block_id.clone()))
            .line_id(Some(line_id.clone()))
            .text_item_range(Some(crate::TextItemRange::new(0, 1)))
            .text_spans(vec![crate::TableTextSpan {
                text_item_id: item.id.clone(),
                byte_range: 17..18,
                bbox,
            }])
            .latex(Some("7".into()))
            .markdown(Some("$7$".into()))
            .build();
        let line = crate::Line::builder()
            .id(line_id)
            .text(item.raw_text.clone())
            .bbox(bbox)
            .direction(crate::WritingDirection::LeftToRight)
            .text_items(vec![item])
            .build();
        let block = crate::Block::builder()
            .id(block_id)
            .label(LayoutLabel::Text)
            .label_source(crate::LabelSource::Model)
            .text(line.text.clone())
            .bbox(bbox)
            .final_order(0)
            .lines(vec![line])
            .build();
        let mut page = PageResult::builder()
            .page_number(1)
            .width(100.0)
            .height(100.0)
            .rotation(0)
            .blocks(vec![block])
            .formulas(vec![formula])
            .build();
        page.project_formulas("[formula]");
        let json = serde_json::to_value(&page).expect("page JSON");
        assert_eq!(
            json.pointer("/blocks/0/markdown")
                .and_then(serde_json::Value::as_str),
            Some(r"学习 \*rate\* of $7$, next")
        );
        assert_eq!(
            json.pointer("/blocks/0/text")
                .and_then(serde_json::Value::as_str),
            Some("学习 *rate* of 7, next")
        );
        assert_eq!(
            json.pointer("/blocks/0/lines/0/text_items/0/raw_text")
                .and_then(serde_json::Value::as_str),
            Some("学习 *rate* of 7, next")
        );
        // An ambiguous location must fall back to the separate formula view without deleting prose.
        let formula = page.formulas.first_mut().expect("formula");
        formula.text_spans.clear();
        formula.text_item_range = Some(crate::TextItemRange::new(0, 0));
        page.project_formulas("[formula]");
        assert!(page.blocks.first().expect("block").markdown.is_none());
    }

    /// A formula occupying the last word of a cell must preserve the preceding cell prose.
    #[test]
    fn table_formula_replaces_only_its_exact_source_bytes() {
        let bbox = Bbox::try_from([0.0, 0.0, 100.0, 20.0]).expect("bbox");
        let block_id = crate::BlockId::model(1, 0, 0);
        let item = crate::TextItem::builder()
            .id(crate::TextItemId::native(1, 0))
            .raw_text("learning *rate* of 7".into())
            .bbox(bbox)
            .source(crate::TextSource::Native)
            .build();
        let cell = crate::TableCell::builder()
            .row(0)
            .column(0)
            .bbox(Some(bbox))
            .text(item.raw_text.clone())
            .lines(vec![crate::TableCellLine {
                text: item.raw_text.clone(),
                bbox,
                spans: vec![crate::TableTextSpan {
                    text_item_id: item.id.clone(),
                    byte_range: 0..item.raw_text.len(),
                    bbox,
                }],
            }])
            .build();
        let formula = FormulaResult::builder()
            .id(ModelRegionId::detected(1, 1))
            .label(LayoutLabel::InlineFormula)
            .bbox(bbox)
            .block_id(Some(block_id.clone()))
            .table_cell(Some((0, 0)))
            .text_spans(vec![crate::TableTextSpan {
                text_item_id: item.id.clone(),
                byte_range: 19..20,
                bbox,
            }])
            .latex(Some("7".into()))
            .markdown(Some("$7$".into()))
            .build();
        let line = crate::Line::builder()
            .id(crate::LineId::new(&block_id, 0))
            .text(item.raw_text.clone())
            .bbox(bbox)
            .direction(crate::WritingDirection::LeftToRight)
            .text_items(vec![item])
            .build();
        let block = crate::Block::builder()
            .id(block_id)
            .label(LayoutLabel::Table)
            .label_source(crate::LabelSource::Model)
            .text(line.text.clone())
            .bbox(bbox)
            .final_order(0)
            .lines(vec![line])
            .table(Some(
                crate::Table::builder()
                    .row_count(1)
                    .column_count(1)
                    .cells(vec![cell])
                    .source(crate::TableStructureSource::TextAlignment)
                    .build(),
            ))
            .build();
        let mut page = PageResult::builder()
            .page_number(1)
            .width(100.0)
            .height(100.0)
            .rotation(0)
            .blocks(vec![block])
            .formulas(vec![formula])
            .build();
        page.project_formulas("[formula]");
        let cell = page
            .blocks
            .first()
            .expect("block")
            .table
            .as_ref()
            .expect("table")
            .cells
            .first()
            .expect("cell");
        assert_eq!(cell.markdown.as_deref(), Some(r"learning \*rate\* of $7$"));
        assert_eq!(cell.text, "learning *rate* of 7");
        let html = page
            .blocks
            .first()
            .expect("block")
            .table
            .as_ref()
            .expect("table")
            .to_markdown();
        assert!(
            html.contains("<td>learning *rate* of $7$</td>"),
            "HTML table fallback must not expose Markdown escape backslashes"
        );
    }
}
