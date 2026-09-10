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
            include_str!("../../tests/fixtures/table/survey-page-35.json"),
            include_str!("../../tests/fixtures/table/survey-page-47-0.json"),
            include_str!("../../tests/fixtures/table/survey-page-57-0.json"),
            include_str!("../../tests/fixtures/table/survey-page-68-0.json"),
            include_str!("../../tests/fixtures/table/survey-page-84-0.json"),
            include_str!("../../tests/fixtures/table/survey-page-84-1.json"),
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
                } else if page == 33 {
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
                } else if page == 35 {
                    assert_eq!(
                        row_text(0),
                        [
                            "Models",
                            "DatasetMixtures",
                            "InstructionNumbers",
                            "LexicalDiversity",
                            "Chat",
                            "QA"
                        ]
                    );
                    assert_eq!(row_text(1), ["AlpacaFarm", "MMLU", "BBH3k"]);
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
                            (1, 2, 1),
                            (2, 2, 1),
                            (3, 2, 1),
                            (4, 1, 1),
                            (5, 1, 2)
                        ]
                    );
                    // These values are transcribed from Table 10, independently of reconstruction.
                    let expected = [
                        ["80,000", "48.48", "23.77", "38.58", "32.79"],
                        ["63,184", "77.31", "81.30", "38.11", "27.71"],
                        ["82,439", "25.92", "/∗", "37.52", "29.81"],
                        ["145,623", "48.22", "71.36", "41.26", "28.36"],
                        ["225,623", "48.28", "70.00", "43.69", "29.69"],
                        ["82,439", "25.92", "/∗", "37.52", "29.81"],
                        ["70,000", "70.43", "76.96", "39.73", "33.25"],
                        ["70,000", "75.59", "81.55", "38.01", "30.03"],
                        ["70,000", "73.48", "79.15", "32.55", "31.25"],
                        ["220,000", "57.78", "51.13", "33.81", "26.63"],
                        ["80,000", "48.48", "22.12", "34.12", "34.05"],
                        ["63,184", "77.31", "77.13", "47.49", "33.82"],
                        ["82,439", "25.92", "/∗", "36.73", "25.43"],
                        ["145,623", "48.22", "72.85", "41.16", "29.49"],
                        ["225,623", "48.28", "69.49", "43.50", "31.16"],
                        ["82,439", "25.92", "/∗", "36.73", "25.43"],
                        ["70,000", "70.43", "77.94", "46.89", "35.75"],
                        ["70,000", "75.59", "78.92", "44.97", "36.40"],
                        ["70,000", "73.48", "80.45", "43.15", "34.59"],
                        ["220,000", "57.78", "58.12", "38.07", "27.28"],
                    ];
                    for (index, expected) in expected.iter().enumerate() {
                        assert_eq!(
                            table
                                .cells
                                .iter()
                                .filter(|cell| cell.row == index + 2
                                    && cell.column >= 2)
                                .map(|cell| cell
                                    .text
                                    .chars()
                                    .filter(|c| !c.is_whitespace())
                                    .collect::<String>())
                                .collect::<Vec<_>>(),
                            *expected,
                            "data row {index}"
                        );
                    }
                } else if page == 47 {
                    assert_eq!(
                        row_text(0),
                        ["Ingredient", "CollectedPrompts", "Prin."]
                    );
                    assert_eq!(
                        table
                            .cells
                            .iter()
                            .filter(|cell| cell.column == 0 && cell.row > 0)
                            .map(|cell| (
                                cell.text.as_str(),
                                cell.row,
                                cell.row_span
                            ))
                            .collect::<Vec<_>>(),
                        [
                            ("Task Description", 1, 4),
                            ("Input Data", 5, 2),
                            ("Contextual Information", 7, 4),
                            ("Demonstration", 11, 9),
                            ("Other Designs", 20, 8)
                        ]
                    );
                    let prefixes: Vec<_> =
                        [('T', 4), ('I', 2), ('C', 4), ('D', 9), ('O', 8)]
                            .into_iter()
                            .flat_map(|(prefix, count)| {
                                (1..=count)
                                    .map(move |i| format!("{prefix}{i}."))
                            })
                            .collect();
                    for (row, prefix) in prefixes.iter().enumerate() {
                        let cell = table
                            .cells
                            .iter()
                            .find(|cell| {
                                cell.row == row + 1 && cell.column == 1
                            })
                            .expect("prompt cell");
                        assert!(
                            cell.text.starts_with(prefix),
                            "row {row}: {}",
                            cell.text
                        );
                        assert!(
                            prefixes
                                .iter()
                                .filter(|p| *p != prefix)
                                .all(|p| !cell.text.contains(p)),
                            "prompts must stay in separate cells"
                        );
                    }
                } else if page == 57 {
                    assert_eq!(
                        row_text(0),
                        ["Level", "Ability", "Task", "Dataset"]
                    );
                    assert_eq!(
                        table
                            .cells
                            .iter()
                            .filter(|cell| cell.column == 0 && cell.row > 0)
                            .map(|cell| (
                                cell.text.as_str(),
                                cell.row,
                                cell.row_span
                            ))
                            .collect::<Vec<_>>(),
                        [("Basic", 1, 9), ("Advanced", 10, 11)]
                    );
                    assert_eq!(
                        table
                            .cells
                            .iter()
                            .filter(|cell| cell.column == 1 && cell.row > 0)
                            .map(|cell| (
                                cell.text
                                    .split_whitespace()
                                    .collect::<String>(),
                                cell.row,
                                cell.row_span
                            ))
                            .collect::<Vec<_>>(),
                        [
                            ("LanguageGeneration", 1, 3),
                            ("KnowledgeUtilization", 4, 3),
                            ("ComplexReasoning", 7, 3),
                            ("HumanAlignment", 10, 3),
                            ("InteractionwithExternalEnvironment", 13, 3),
                            ("ToolManipulation", 16, 5)
                        ]
                        .map(|(text, row, span)| (
                            text.to_owned(),
                            row,
                            span
                        ))
                    );
                    assert_eq!(
                        table
                            .cells
                            .iter()
                            .filter(|cell| cell.column == 2 && cell.row > 0)
                            .map(|cell| cell
                                .text
                                .split_whitespace()
                                .collect::<String>())
                            .collect::<Vec<_>>(),
                        [
                            "LanguageModeling",
                            "ConditionalTextGeneration",
                            "CodeSynthesis",
                            "Closed-BookQA",
                            "Open-BookQA",
                            "KnowledgeCompletion",
                            "KnowledgeReasoning",
                            "SymbolicReasoning",
                            "MathematicalReasoning",
                            "Honestness",
                            "Helpfulness",
                            "Harmlessness",
                            "Household",
                            "WebsiteEnvironment",
                            "OpenWorld",
                            "SearchEngine",
                            "CodeExecutor",
                            "Calculator",
                            "ModelInterface",
                            "DataInterface"
                        ]
                    );
                } else if page == 68 {
                    assert_eq!(
                        row_text(0),
                        [
                            "Tasks",
                            "Datasets",
                            "Instructions",
                            "ChatGPT",
                            "Supervised"
                        ]
                    );
                    assert!(table.cells.iter().any(|cell| cell.row == 0
                        && cell.column == 0
                        && cell.column_span == 2));
                    assert_eq!(
                        table
                            .cells
                            .iter()
                            .filter(|cell| cell.column == 0 && cell.row > 0)
                            .map(|cell| (
                                cell.text.as_str(),
                                cell.row,
                                cell.row_span
                            ))
                            .collect::<Vec<_>>(),
                        [
                            ("LG", 1, 4),
                            ("KU", 5, 6),
                            ("CR", 11, 4),
                            ("SDG", 15, 2),
                            ("IR", 17, 2)
                        ]
                    );
                    assert_eq!(
                        table
                            .cells
                            .iter()
                            .filter(|cell| cell.column == 4 && cell.row > 0)
                            .map(|cell| cell.text.as_str())
                            .collect::<Vec<_>>(),
                        [
                            "20.66", "21.12", "21.71", "23.01", "85.19",
                            "85.86", "81.20", "82.20", "29.25", "31.21",
                            "53.20", "66.75", "78.47", "79.30", "79.88",
                            "70.10", "48.80", "17.20"
                        ]
                    );
                    assert!(table.cells.iter().any(|cell| cell.column == 3
                        && cell.row == 14
                        && cell.text.contains("money_left")));
                } else if page == 84 {
                    assert_eq!(
                        row_text(0),
                        [
                            "Equations",
                            "Computation",
                            "Datatransfer",
                            "Arithmeticintensity"
                        ]
                    );
                    let expected: &[&str] = if table.row_count == 10 {
                        &[
                            "6BTH2",
                            "6BTH",
                            "4BT2ND+4BT2N",
                            "2BTH2",
                            "5BTH",
                            "4BTHH′",
                            "2BTH′",
                            "2BTHH′",
                            "5BTH",
                        ]
                    } else {
                        &[
                            "6BH2",
                            "6BH",
                            "-",
                            "4BTND+4BTN",
                            "2BH2",
                            "5BH",
                            "4BHH′",
                            "2BH′",
                            "2BHH′",
                            "5BH",
                        ]
                    };
                    assert_eq!(
                        table
                            .cells
                            .iter()
                            .filter(|cell| cell.column == 1 && cell.row > 0)
                            .map(|cell| cell
                                .text
                                .split_whitespace()
                                .collect::<String>())
                            .collect::<Vec<_>>(),
                        expected
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
        for strategy in 0..4 {
            let candidate = match strategy {
                0 => grid.tagged(&self.evidence.tagged_tables),
                1 => grid.ruled(&self.evidence.rules),
                2 => grid.aligned(&self.evidence.rules),
                _ => grid.sparse(&self.evidence.rules),
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
