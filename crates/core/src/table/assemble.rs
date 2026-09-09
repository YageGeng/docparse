use std::collections::BTreeMap;

use docparse_config::FusionConfig;
use docparse_layout::Bbox;
use typed_builder::TypedBuilder;

use super::grid::{RecoveredGrid, TableGrid};
use super::{Table, TableCellLine, TableEvidence, TableTextSpan};
use crate::line::{ConservativeLineAssembler, LineFragment};
use crate::{Baseline, Block, TextItem, TextItemId};

#[cfg(test)]
mod tests {
    use super::*;

    /// A measured native word's byte range, ink bounds, and optional baseline.
    type WordFact = (usize, usize, [f64; 4], Option<f64>);

    /// The survey's real native words must recover all data columns and multi-line/grouped headers.
    #[test]
    fn survey_tables_recover_source_columns_and_headers() {
        for source in [
            include_str!("../../tests/fixtures/table/survey-page-29.json"),
            include_str!("../../tests/fixtures/table/survey-page-33.json"),
        ] {
            let fixture: serde_json::Value =
                serde_json::from_str(source).expect("native table");
            let page: u32 = serde_json::from_value(
                fixture.get("page").expect("page").clone(),
            )
            .expect("page");
            let bounds: [f64; 4] = serde_json::from_value(
                fixture.get("bbox").expect("bbox").clone(),
            )
            .expect("bounds");
            let facts: Vec<(u32, String, [f64; 4], f64, f64, f64)> =
                serde_json::from_value(
                    fixture.get("items").expect("items").clone(),
                )
                .expect("items");
            let words: Vec<(u32, Vec<WordFact>)> = serde_json::from_value(
                fixture.get("words").expect("words").clone(),
            )
            .expect("words");
            let rules: Vec<(String, f64, f64, f64)> = serde_json::from_value(
                fixture.get("rules").expect("rules").clone(),
            )
            .expect("rules");
            let id = crate::BlockId::model(page, 0, 0);
            let lines = facts
                .into_iter()
                .map(|(index, text, b, size, y, row_y)| {
                    let item = TextItem::builder()
                        .id(TextItemId::native(page, index))
                        .raw_text(text.clone())
                        .bbox(Bbox::try_from(b).expect("item"))
                        .baseline(Some(Baseline {
                            start: docparse_layout::Point::new(b[0], y),
                            end: docparse_layout::Point::new(b[2], y),
                        }))
                        .style(Some(
                            crate::TextStyle::builder()
                                .font_size(Some(size))
                                .build(),
                        ))
                        .source(crate::TextSource::Native)
                        .build();
                    crate::Line::builder()
                        .id(crate::LineId::new(&id, index))
                        .text(text)
                        .bbox(item.bbox)
                        .baseline(Some(Baseline {
                            start: docparse_layout::Point::new(b[0], row_y),
                            end: docparse_layout::Point::new(b[2], row_y),
                        }))
                        .direction(crate::WritingDirection::LeftToRight)
                        .text_items(vec![item])
                        .build()
                })
                .collect();
            let mut evidence = TableEvidence {
                rules: rules
                    .into_iter()
                    .map(|(kind, at, from, to)| {
                        if kind == "h" {
                            super::super::TableRule::Horizontal {
                                y: at,
                                left: from,
                                right: to,
                            }
                        } else {
                            super::super::TableRule::Vertical {
                                x: at,
                                top: from,
                                bottom: to,
                            }
                        }
                    })
                    .collect(),
                ..TableEvidence::default()
            };
            for (index, words) in words {
                evidence.words.insert(
                    TextItemId::native(page, index),
                    words
                        .into_iter()
                        .map(|(start, end, b, y)| {
                            super::super::TableWord::builder()
                                .byte_range(start..end)
                                .bbox(Bbox::try_from(b).expect("word"))
                                .baseline(y.map(|y| Baseline {
                                    start: docparse_layout::Point::new(b[0], y),
                                    end: docparse_layout::Point::new(b[2], y),
                                }))
                                .build()
                        })
                        .collect(),
                );
            }
            let block = Block::builder()
                .id(id)
                .label(docparse_layout::LayoutLabel::Table)
                .text(String::new())
                .label_source(crate::LabelSource::Model)
                .bbox(Bbox::try_from(bounds).expect("table bounds"))
                .final_order(0)
                .lines(lines)
                .build();
            let config = FusionConfig::default();
            for reverse in [false, true] {
                let mut block = block.clone();
                let mut evidence = evidence.clone();
                if reverse {
                    block.lines.reverse();
                    evidence.rules.reverse();
                    for words in evidence.words.values_mut() {
                        words.reverse();
                    }
                }
                let original = block.lines.clone();
                let reconstructed =
                    TableAssembler::new(&config, &evidence, &[])
                        .reconstruct(&mut block);
                assert!(
                    reconstructed.is_ok(),
                    "page {page}: {reconstructed:?}"
                );
                assert_eq!(
                    block.lines, original,
                    "canonical source facts stay unchanged"
                );
                let table = block.table.expect("structured table");
                assert_eq!(
                    table.column_count,
                    serde_json::from_value::<usize>(
                        fixture.get("columns").expect("columns").clone()
                    )
                    .expect("columns"),
                    "page {page}"
                );
                assert_eq!(
                    table.row_count,
                    serde_json::from_value::<usize>(
                        fixture.get("rows").expect("rows").clone()
                    )
                    .expect("rows"),
                    "page {page}"
                );
                // Compare independently transcribed source cells, not only grid dimensions.
                let row_text = |row| {
                    table
                        .cells
                        .iter()
                        .filter(|cell| cell.row == row)
                        .map(|cell| {
                            cell.text
                                .chars()
                                .filter(|c| !c.is_whitespace())
                                .collect::<String>()
                        })
                        .collect::<Vec<_>>()
                };
                if page == 29 {
                    assert_eq!(
                        row_text(0),
                        [
                            "Model",
                            "BatchSize(#tokens)",
                            "LearningRate",
                            "Warmup",
                            "DecayMethod",
                            "Optimizer",
                            "PrecisionType",
                            "WeightDecay",
                            "GradClip",
                            "Dropout"
                        ]
                    );
                    assert_eq!(
                        row_text(1),
                        [
                            "GPT3(175B)",
                            "32K→3.2M",
                            "6×10−5",
                            "yes",
                            "cosinedecayto10%",
                            "Adam",
                            "FP16",
                            "0.1",
                            "1.0",
                            "-"
                        ]
                    );
                } else {
                    assert_eq!(
                        row_text(0),
                        [
                            "Models",
                            "A800FullTuning",
                            "A800LoRATuning",
                            "A800Inference(16-bit)",
                            "3090Inference(16-bit)",
                            "3090Inference(8-bit)"
                        ]
                    );
                    assert_eq!(
                        table
                            .cells
                            .iter()
                            .filter(|cell| cell.row == 0)
                            .map(|cell| (
                                cell.column,
                                cell.row_span,
                                cell.column_span
                            ))
                            .collect::<Vec<_>>(),
                        [
                            (0, 2, 1),
                            (1, 1, 3),
                            (4, 1, 3),
                            (7, 1, 2),
                            (9, 1, 2),
                            (11, 1, 2)
                        ]
                    );
                    assert_eq!(
                        row_text(2),
                        [
                            "LLaMA(7B)",
                            "2",
                            "8",
                            "3.0h",
                            "1",
                            "80",
                            "3.5h",
                            "1",
                            "36.6",
                            "1",
                            "24.3",
                            "1",
                            "7.5"
                        ]
                    );
                    assert_eq!(
                        row_text(5),
                        [
                            "LLaMA(65B)",
                            "16",
                            "2",
                            "11.2h",
                            "1",
                            "4",
                            "60.6h",
                            "2",
                            "8.8",
                            "8",
                            "2.0",
                            "4",
                            "1.5"
                        ]
                    );
                }
            }
        }
    }
}

/// One source slice with separate logical-row and measured text baselines.
#[derive(TypedBuilder)]
pub(super) struct LocatedSpan<'a> {
    pub span: TableTextSpan,
    pub item: &'a TextItem,
    pub baseline: f64,
    #[builder(default)]
    pub measured_baseline: Option<Baseline>,
    #[builder(default)]
    pub mcid: Option<i32>,
}

impl LocatedSpan<'_> {
    /// Uses source font metrics instead of wide or rotated item bounds as a scale estimate.
    pub(super) fn font_size(&self) -> f64 {
        self.item
            .style
            .as_ref()
            .and_then(|style| style.font_size)
            .filter(|size| size.is_finite() && *size > 0.0)
            .unwrap_or(self.span.bbox.height())
            .max(1.0)
    }

    /// Interprets available font evidence without changing the raw extracted style flags.
    pub(super) fn is_bold(&self) -> bool {
        self.item.style.as_ref().is_some_and(|style| {
            style.bold
                || style.weight.is_some_and(|weight| weight >= 600)
                || style.flags.is_some_and(|flags| flags & (1 << 18) != 0)
        })
    }

    /// Reads only the verified slice of the original immutable string.
    pub(super) fn text(&self) -> &str {
        self.item
            .raw_text
            .get(self.span.byte_range.clone())
            .unwrap_or("")
    }
}

/// Reconstructs cell-local text without moving or duplicating canonical text ownership.
pub(crate) struct TableAssembler<'a> {
    config: &'a FusionConfig,
    evidence: &'a TableEvidence,
    formulas: &'a [crate::line::FormulaRegion],
}

impl<'a> TableAssembler<'a> {
    /// Shares immutable page evidence with each independently detected table.
    pub(crate) const fn new(
        config: &'a FusionConfig,
        evidence: &'a TableEvidence,
        formulas: &'a [crate::line::FormulaRegion],
    ) -> Self {
        Self {
            config,
            evidence,
            formulas,
        }
    }

    /// Tries explicit topology before geometry, publishing only a complete validated table.
    pub(crate) fn reconstruct(&self, block: &mut Block) -> Result<(), String> {
        let mut spans = Vec::new();
        for line in &block.lines {
            for item in &line.text_items {
                if item.raw_text.trim().is_empty() {
                    continue;
                }
                let baseline = line
                    .baseline
                    .map_or(line.bbox.bottom, |baseline| baseline.start.y);
                if let Some(words) = self
                    .evidence
                    .words
                    .get(&item.id)
                    .filter(|words| !words.is_empty())
                {
                    for word in words {
                        Bbox::try_from([
                            word.bbox.left,
                            word.bbox.top,
                            word.bbox.right,
                            word.bbox.bottom,
                        ])
                        .map_err(|error| {
                            format!("invalid measured word geometry: {error}")
                        })?;
                        let text = item
                            .raw_text
                            .get(word.byte_range.clone())
                            .ok_or("invalid measured word range")?;
                        if text.trim().is_empty() {
                            continue;
                        }
                        spans.push(
                            LocatedSpan::builder()
                                .span(TableTextSpan {
                                    text_item_id: item.id.clone(),
                                    byte_range: word.byte_range.clone(),
                                    bbox: word.bbox,
                                })
                                .item(item)
                                .baseline(baseline)
                                .measured_baseline(
                                    word.baseline.or(item.baseline),
                                )
                                .mcid(word.mcid)
                                .build(),
                        );
                    }
                } else {
                    // Caller-supplied/OCR runs without measured words remain indivisible.
                    spans.push(
                        LocatedSpan::builder()
                            .span(TableTextSpan {
                                text_item_id: item.id.clone(),
                                byte_range: 0..item.raw_text.len(),
                                bbox: item.bbox,
                            })
                            .item(item)
                            .baseline(baseline)
                            .measured_baseline(item.baseline)
                            .mcid(
                                item.provenance
                                    .as_ref()
                                    .and_then(|provenance| provenance.mcid),
                            )
                            .build(),
                    );
                }
            }
        }
        if spans.is_empty() {
            return Err("no native or OCR text is available inside the table"
                .to_owned());
        }
        let grid = TableGrid::new(block.bbox, &spans);
        let mut failures = Vec::new();
        for strategy in 0..3 {
            let candidate = match strategy {
                0 => grid.tagged(&self.evidence.tagged_tables),
                1 => grid.ruled(&self.evidence.rules),
                _ => grid.aligned(&self.evidence.rules),
            };
            if let Some(candidate) = candidate {
                match self.populate(candidate, &spans, block) {
                    Ok(table) => {
                        tracing::debug!(
                            "reconstructed table {} with {} rows and {} columns from {:?}",
                            block.id.as_str(),
                            table.row_count,
                            table.column_count,
                            table.source
                        );
                        block.text = table.to_text();
                        block.table = Some(table);
                        return Ok(());
                    }
                    Err(reason) => failures.push(reason),
                }
            }
        }
        Err(failures.pop().unwrap_or_else(|| "no consistent row/column structure accounts for the source text".to_owned()))
    }

    /// Reuses script-aware line assembly inside each cell, then retains only source references.
    fn populate(
        &self,
        mut grid: RecoveredGrid,
        spans: &[LocatedSpan<'_>],
        block: &Block,
    ) -> Result<Table, String> {
        let sources: BTreeMap<_, _> = block
            .lines
            .iter()
            .flat_map(|line| &line.text_items)
            .map(|item| (item.id.clone(), item))
            .collect();
        let mut members = vec![Vec::new(); grid.table.cells.len()];
        for (index, &cell) in grid.assignment.iter().enumerate() {
            members
                .get_mut(cell)
                .ok_or("invalid cell assignment")?
                .push(index);
        }
        for (cell, indices) in grid.table.cells.iter_mut().zip(members) {
            let mut items = Vec::new();
            let mut references = BTreeMap::new();
            for index in indices {
                let word = spans.get(index).ok_or("missing table word")?;
                let id = TextItemId::native(
                    1,
                    u32::try_from(index)
                        .map_err(|_error| "too many table words")?,
                );
                references.insert(id.clone(), word.span.clone());
                // Temporary words do not copy PDF provenance/character-code arrays or become canonical owners.
                items.push(
                    TextItem::builder()
                        .id(id)
                        .raw_text(word.text().to_owned())
                        .bbox(word.span.bbox)
                        // The logical row anchors grid inference, but flattening a word's
                        // measured baseline here destroys scripts and multiline source runs.
                        .baseline(word.measured_baseline.or(Some(Baseline {
                            start: docparse_layout::Point::new(
                                word.span.bbox.left,
                                word.baseline,
                            ),
                            end: docparse_layout::Point::new(
                                word.span.bbox.right,
                                word.baseline,
                            ),
                        })))
                        .rotation(word.item.rotation)
                        .source(word.item.source)
                        .style(word.item.style.clone())
                        .build(),
                );
            }
            let mut fragments = ConservativeLineAssembler
                // Cell ownership remains authoritative; formula scopes only order its own words.
                .fragments_with_formulas(
                    items,
                    self.config,
                    &self.evidence.rules,
                    self.formulas,
                )
                .map_err(|error| error.to_string())?;
            fragments.sort_by(|a, b| {
                a.reading_order_y()
                    .total_cmp(&b.reading_order_y())
                    .then_with(|| a.bbox.left.total_cmp(&b.bbox.left))
            });
            let mut lines: Vec<Vec<LineFragment>> = Vec::new();
            let mut baselines: Vec<f64> = Vec::new();
            for fragment in fragments {
                let y = fragment.reading_order_y();
                let same_line = baselines.last().is_some_and(|previous| {
                    (y - previous).abs()
                        <= fragment.bbox.height().max(1.0) * 0.4
                });
                if same_line && let Some(line) = lines.last_mut() {
                    line.push(fragment);
                } else {
                    baselines.push(y);
                    lines.push(vec![fragment]);
                }
            }
            for mut row in lines {
                // Sort complete fragments, preserving each already ordered fraction
                // and its scripts instead of flattening everything back into x order.
                let direction = crate::line::bidi::detect_direction(
                    row.iter().flat_map(|fragment| &fragment.items),
                    row.first().map_or(0.0, |fragment| fragment.rotation),
                );
                row.sort_by(|a, b| {
                    if direction == crate::WritingDirection::RightToLeft {
                        b.bbox.right.total_cmp(&a.bbox.right)
                    } else {
                        a.bbox.left.total_cmp(&b.bbox.left)
                    }
                });
                let mut row = row.into_iter();
                let Some(mut fragment) = row.next() else {
                    continue;
                };
                for next in row {
                    fragment.bbox = Bbox::try_from([
                        fragment.bbox.left.min(next.bbox.left),
                        fragment.bbox.top.min(next.bbox.top),
                        fragment.bbox.right.max(next.bbox.right),
                        fragment.bbox.bottom.max(next.bbox.bottom),
                    ])
                    .map_err(|error| error.to_string())?;
                    fragment.items.extend(next.items);
                }
                let references = fragment
                    .items
                    .iter()
                    .map(|item| {
                        references
                            .get(&item.id)
                            .cloned()
                            .ok_or("lost table source reference")
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let mut line = TableCellLine {
                    text: String::new(),
                    bbox: fragment.bbox,
                    spans: references,
                };
                line.text = line
                    .derive_text(&sources)
                    .ok_or("invalid table source text")?;
                cell.lines.push(line);
            }
            cell.text = cell
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
        }
        grid.table.cells.sort_by_key(|cell| (cell.row, cell.column));
        grid.table.validate(block)?;
        Ok(grid.table)
    }
}

impl TableCellLine {
    /// Concatenates source slices without treating glyph ink gaps as word spaces.
    pub(crate) fn derive_text(
        &self,
        sources: &BTreeMap<TextItemId, &TextItem>,
    ) -> Option<String> {
        let mut text = String::new();
        let mut previous: Option<&TableTextSpan> = None;
        for span in &self.spans {
            let item = sources.get(&span.text_item_id)?;
            let raw = item.raw_text.get(span.byte_range.clone())?;
            // External word evidence may exclude source whitespace from its ranges.
            // Preserve that whitespace, but never infer a space around decimal points,
            // operators, or style-split fragments merely from their tight glyph boxes.
            if let Some(previous) = previous
                && previous.text_item_id == span.text_item_id
                && previous.byte_range.end < span.byte_range.start
            {
                let gap = item
                    .raw_text
                    .get(previous.byte_range.end..span.byte_range.start)?;
                if gap.chars().all(char::is_whitespace) {
                    text.push_str(gap);
                }
            }
            text.push_str(raw);
            previous = Some(span);
        }
        Some(text.trim().to_owned())
    }
}
