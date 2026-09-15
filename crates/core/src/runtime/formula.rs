//! Formula enrichment retains native ownership and one result for every original model region.
use super::RenderedPage;
use crate::{FormulaResult, ModelRegionId, PageResult, PageWarning};
use docparse_config::FormulaConfig;
use docparse_formula::{FormulaEngine, FormulaError};
use docparse_layout::{
    Bbox, LayoutDetection, LayoutLabel, PageImage, PageImageInput, Point,
    timing::Timings,
};
use std::sync::Arc;

/// A viewport formula box paired with the already rendered page raster.
struct FormulaCrop<'a> {
    bbox: Bbox,
    rendered: &'a RenderedPage,
}

impl TryFrom<FormulaCrop<'_>> for PageImage {
    type Error = FormulaError;

    /// Uses the same viewport-to-pixel transform as layout, including rotation and crop-box offsets.
    #[allow(clippy::cast_sign_loss)] // Coordinates are clamped to the validated raster before conversion.
    fn try_from(crop: FormulaCrop<'_>) -> Result<Self, Self::Error> {
        let image = &crop.rendered.image;
        let transform = &crop.rendered.transform;
        if transform.render_size() != (image.width(), image.height()) {
            return Err(FormulaError::Invalid(
                "formula image/transform mismatch".into(),
            ));
        }
        let start = transform
            .viewport_to_rendered(Point::new(crop.bbox.left, crop.bbox.top));
        let end = transform.viewport_to_rendered(Point::new(
            crop.bbox.right,
            crop.bbox.bottom,
        ));
        let left = start.x.floor().clamp(0.0, image.width() as f64) as u32;
        let top = start.y.floor().clamp(0.0, image.height() as f64) as u32;
        let right = end.x.ceil().clamp(0.0, image.width() as f64) as u32;
        let bottom = end.y.ceil().clamp(0.0, image.height() as f64) as u32;
        if left >= right || top >= bottom {
            return Err(FormulaError::Invalid(
                "formula crop is outside the page".into(),
            ));
        }
        let row_bytes = (right - left) as usize * 3;
        let mut pixels =
            Vec::with_capacity(row_bytes * (bottom - top) as usize);
        for y in top..bottom {
            let offset =
                (y as usize * image.width() as usize + left as usize) * 3;
            pixels.extend_from_slice(
                image.data().get(offset..offset + row_bytes).ok_or_else(
                    || {
                        FormulaError::Invalid(
                            "formula crop exceeds raster".into(),
                        )
                    },
                )?,
            );
        }
        PageImage::try_from(
            PageImageInput::builder()
                .width(right - left)
                .height(bottom - top)
                .pixel_format(image.pixel_format())
                .data(Arc::from(pixels))
                .build(),
        )
        .map_err(|error| FormulaError::Invalid(error.to_string()))
    }
}

impl FormulaResult {
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
            for line in block.lines.iter().filter(|line| {
                self.line_id.as_ref().is_none_or(|id| id == &line.id)
            }) {
                for item in &line.text_items {
                    if let Some(measured) = words.get(&item.id) {
                        for word in measured {
                            let Some(text) =
                                item.raw_text.get(word.byte_range.clone())
                            else {
                                continue;
                            };
                            let overlap =
                                self.bbox.intersection_area(word.bbox);
                            let punctuation = !text.trim().is_empty()
                                && text
                                    .trim()
                                    .chars()
                                    .all(|ch| ch.is_ascii_punctuation());
                            if overlap <= 0.0
                                || (overlap
                                    / word.bbox.area().max(f64::EPSILON)
                                    < 0.5
                                    && !punctuation)
                            {
                                continue;
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
    /// Recognizes actual batches and records crop, inference and empty-output failures for each affected region.
    pub(crate) async fn recognize_formulas(
        &mut self,
        detections: Vec<LayoutDetection>,
        rendered: &RenderedPage,
        engine: Option<&dyn FormulaEngine>,
        config: &FormulaConfig,
        timings: &Timings,
        words: &std::collections::BTreeMap<
            crate::TextItemId,
            Vec<crate::TableWord>,
        >,
    ) {
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
                formula
            })
            .collect();
        let batch_size = config.batch_size;
        tracing::info!(
            "recognizing {} formula regions on page {} with batch size {}",
            formulas.len(),
            self.page_number,
            batch_size
        );
        for chunk in formulas.chunks_mut(batch_size) {
            let mut images = Vec::new();
            let mut positions = Vec::new();
            for (index, formula) in chunk.iter_mut().enumerate() {
                match PageImage::try_from(FormulaCrop {
                    bbox: formula.bbox,
                    rendered,
                }) {
                    Ok(image) => {
                        images.push(Arc::new(image));
                        positions.push(index);
                    }
                    Err(error) => {
                        tracing::warn!(
                            "formula {} crop failed: {}",
                            formula.id.as_str(),
                            error
                        );
                        formula.error = Some(error.to_string());
                    }
                }
            }
            if images.is_empty() {
                continue;
            }
            let count = images.len();
            let result = if let Some(engine) = engine {
                match crate::wasm_compat::timeout(
                    std::time::Duration::from_millis(config.timeout_ms),
                    engine.recognize(images, timings.clone()),
                )
                .await
                {
                    Ok(result) => result,
                    Err(_) => Err(FormulaError::Invalid(format!(
                        "formula batch exceeded {} ms",
                        config.timeout_ms
                    ))),
                }
            } else {
                Err(FormulaError::Invalid(
                    "formula recognizer unavailable".into(),
                ))
            };
            let result = result.and_then(|output| {
                if output.len() == count {
                    Ok(output)
                } else {
                    Err(FormulaError::Invalid(format!(
                        "batch returned {} formulas for {count} crops",
                        output.len()
                    )))
                }
            });
            match result {
                Ok(output) => {
                    for (index, latex) in positions.into_iter().zip(output) {
                        if let Some(formula) = chunk.get_mut(index) {
                            let latex = latex.trim();
                            let latex = latex
                                .strip_prefix("$$")
                                .and_then(|s| s.strip_suffix("$$"))
                                .or_else(|| {
                                    latex
                                        .strip_prefix('$')
                                        .and_then(|s| s.strip_suffix('$'))
                                })
                                .unwrap_or(latex)
                                .trim();
                            if latex.is_empty() {
                                formula.error = Some(
                                    "formula model returned empty LaTeX".into(),
                                );
                                tracing::warn!(
                                    "formula {} returned empty LaTeX",
                                    formula.id.as_str()
                                );
                            } else {
                                formula.latex = Some(latex.to_owned());
                                formula.markdown = Some(
                                    if formula.label
                                        == LayoutLabel::InlineFormula
                                    {
                                        format!("${latex}$")
                                    } else {
                                        format!("$$\n{latex}\n$$")
                                    },
                                );
                                formula.retain_unrecognized_punctuation(self);
                            }
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        "formula batch failed on page {} for {} regions: {}",
                        self.page_number,
                        count,
                        error
                    );
                    for index in positions {
                        if let Some(formula) = chunk.get_mut(index) {
                            formula.error = Some(error.to_string());
                        }
                    }
                }
            }
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
        self.project_table_formulas();
        self.warnings.sort_by(|a, b| {
            (&a.stage, &a.code, &a.message)
                .cmp(&(&b.stage, &b.code, &b.message))
        });
    }

    /// Enriches cell presentation while keeping canonical source spans and text unchanged.
    fn project_table_formulas(&mut self) {
        for block in &mut self.blocks {
            let Some(table) = &mut block.table else {
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
        page.project_table_formulas();
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
