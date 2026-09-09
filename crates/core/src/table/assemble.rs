use std::collections::BTreeMap;

use docparse_config::FusionConfig;
use docparse_layout::Bbox;
use typed_builder::TypedBuilder;

use super::grid::{RecoveredGrid, TableGrid};
use super::{Table, TableCellLine, TableEvidence, TableTextSpan};
use crate::line::{ConservativeLineAssembler, LineAssembler, LineFragment};
use crate::{Baseline, Block, TextItem, TextItemId};

/// One measured source slice, using the established physical-line baseline for script stability.
#[derive(TypedBuilder)]
pub(super) struct LocatedSpan<'a> {
    pub span: TableTextSpan,
    pub item: &'a TextItem,
    pub baseline: f64,
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
}

impl<'a> TableAssembler<'a> {
    /// Shares immutable page evidence with each independently detected table.
    pub(crate) const fn new(
        config: &'a FusionConfig,
        evidence: &'a TableEvidence,
    ) -> Self {
        Self { config, evidence }
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
                        .baseline(Some(Baseline {
                            start: docparse_layout::Point::new(
                                word.span.bbox.left,
                                word.baseline,
                            ),
                            end: docparse_layout::Point::new(
                                word.span.bbox.right,
                                word.baseline,
                            ),
                        }))
                        .rotation(word.item.rotation)
                        .source(word.item.source)
                        .style(word.item.style.clone())
                        .build(),
                );
            }
            let mut fragments = ConservativeLineAssembler
                .fragments(items, self.config)
                .map_err(|error| error.to_string())?;
            fragments.sort_by(|a, b| {
                a.reading_order_y()
                    .total_cmp(&b.reading_order_y())
                    .then_with(|| a.bbox.left.total_cmp(&b.bbox.left))
            });
            let mut lines: Vec<Vec<TextItem>> = Vec::new();
            let mut baselines: Vec<f64> = Vec::new();
            for fragment in fragments {
                let y = fragment.reading_order_y();
                let same_line = baselines.last().is_some_and(|previous| {
                    (y - previous).abs()
                        <= fragment.bbox.height().max(1.0) * 0.4
                });
                if same_line && let Some(line) = lines.last_mut() {
                    line.extend(fragment.items);
                } else {
                    baselines.push(y);
                    lines.push(fragment.items);
                }
            }
            for items in lines {
                let fragment =
                    LineFragment::from_items(items, block.bbox.width())
                        .map_err(|error| error.to_string())?;
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
