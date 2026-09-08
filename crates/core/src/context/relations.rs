use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Block, ContextError, DocumentContext, DocumentRelation, DocumentRelations,
    Evidence, NodeRef, PageResult, RelationKind,
};
use docparse_layout::LayoutLabel;

/// Read-only document linker that emits sidecar relations without mutating pages.
pub struct DocumentLinker;

impl DocumentLinker {
    /// Creates a stateless document linker.
    pub const fn new() -> Self {
        Self
    }

    /// Returns deterministic sidecar relations for immutable page results.
    pub fn link(
        &self,
        context: &DocumentContext,
        pages: &[PageResult],
    ) -> Result<DocumentRelations, ContextError> {
        let repeated: BTreeSet<_> = context
            .repeated_header_fingerprints
            .iter()
            .chain(&context.repeated_footer_fingerprints)
            .cloned()
            .collect();
        let mut ordered_pages: Vec<_> = pages.iter().collect();
        ordered_pages.sort_by_key(|page| page.page_number);
        let mut occurrences = BTreeMap::<String, Vec<NodeRef>>::new();
        for page in &ordered_pages {
            for block in &page.blocks {
                if block.label == LayoutLabel::Watermark {
                    continue;
                }
                let fingerprint = block
                    .lines
                    .iter()
                    .map(|line| line.text.trim().to_lowercase())
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !repeated.contains(&fingerprint) {
                    continue;
                }
                occurrences
                    .entry(fingerprint)
                    .or_default()
                    .push(Self::node_ref(page.page_number, block));
            }
        }

        let mut relations = Vec::new();
        for nodes in occurrences.values_mut() {
            nodes.sort_by(|left, right| {
                (left.page_number, left.block_id.as_str())
                    .cmp(&(right.page_number, right.block_id.as_str()))
            });
            for pair in nodes.windows(2) {
                let [source, target] = pair else {
                    continue;
                };
                relations.push(
                    DocumentRelation::builder()
                        .kind(RelationKind::RepeatedChrome)
                        .source(source.clone())
                        .target(target.clone())
                        .build(),
                );
            }
        }
        for pair in ordered_pages.windows(2) {
            let [source_page, target_page] = pair else {
                continue;
            };
            Self::add_paragraph_continuation(
                source_page,
                target_page,
                &mut relations,
            );
            Self::add_table_continuation(
                source_page,
                target_page,
                &mut relations,
            );
        }
        Self::add_heading_hierarchy(&ordered_pages, &mut relations);
        relations.sort_by(|left, right| {
            (
                left.kind,
                left.source.page_number,
                left.source.block_id.as_str(),
                left.target.page_number,
                left.target.block_id.as_str(),
            )
                .cmp(&(
                    right.kind,
                    right.source.page_number,
                    right.source.block_id.as_str(),
                    right.target.page_number,
                    right.target.block_id.as_str(),
                ))
        });
        Ok(DocumentRelations { relations })
    }

    /// Builds a stable non-owning reference to one block and its first line.
    fn node_ref(page_number: u32, block: &Block) -> NodeRef {
        NodeRef::builder()
            .page_number(page_number)
            .block_id(block.id.clone())
            .line_id(block.lines.first().map(|line| line.id.clone()))
            .build()
    }

    /// Adds a conservative relation when prose appears to continue across a page break.
    fn add_paragraph_continuation(
        source_page: &PageResult,
        target_page: &PageResult,
        relations: &mut Vec<DocumentRelation>,
    ) {
        let source = source_page
            .blocks
            .iter()
            .rev()
            .find(|block| Self::is_flow_text(block));
        let target = target_page
            .blocks
            .iter()
            .find(|block| Self::is_flow_text(block));
        let (Some(source), Some(target)) = (source, target) else {
            return;
        };
        // Block text is validated against its ordered lines, so relation decisions can borrow it
        // directly instead of rebuilding and allocating the same summary.
        let source_text = source.text.as_str();
        if source_text.is_empty()
            || source_text
                .chars()
                .last()
                .is_some_and(|character| ".?!。！？；;：:".contains(character))
        {
            return;
        }
        relations.push(
            DocumentRelation::builder()
                .kind(RelationKind::ParagraphContinuation)
                .source(Self::node_ref(source_page.page_number, source))
                .target(Self::node_ref(target_page.page_number, target))
                .score(Some(0.7))
                .evidence(vec![
                    Evidence::builder()
                        .kind("open_sentence_at_page_break".to_owned())
                        .build(),
                ])
                .build(),
        );
    }

    /// Adds a candidate relation for table blocks touching consecutive pages.
    fn add_table_continuation(
        source_page: &PageResult,
        target_page: &PageResult,
        relations: &mut Vec<DocumentRelation>,
    ) {
        let source = source_page
            .blocks
            .iter()
            .rev()
            .find(|block| block.label == LayoutLabel::Table);
        let target = target_page
            .blocks
            .iter()
            .find(|block| block.label == LayoutLabel::Table);
        if let (Some(source), Some(target)) = (source, target) {
            relations.push(
                DocumentRelation::builder()
                    .kind(RelationKind::TableContinuationCandidate)
                    .source(Self::node_ref(source_page.page_number, source))
                    .target(Self::node_ref(target_page.page_number, target))
                    .score(Some(0.75))
                    .evidence(vec![
                        Evidence::builder()
                            .kind("adjacent_page_tables".to_owned())
                            .build(),
                    ])
                    .build(),
            );
        }
    }

    /// Connects each title to the next title at the same or a smaller visual level.
    fn add_heading_hierarchy(
        pages: &[&PageResult],
        relations: &mut Vec<DocumentRelation>,
    ) {
        let headings: Vec<_> = pages
            .iter()
            .flat_map(|page| {
                page.blocks
                    .iter()
                    .filter(|block| Self::is_heading(block))
                    .map(|block| {
                        (page.page_number, block, Self::font_size(block))
                    })
            })
            .collect();
        for pair in headings.windows(2) {
            let [
                (source_page, source, source_size),
                (target_page, target, target_size),
            ] = pair
            else {
                continue;
            };
            if source_size + f64::EPSILON < *target_size {
                continue;
            }
            relations.push(
                DocumentRelation::builder()
                    .kind(RelationKind::HeadingHierarchy)
                    .source(Self::node_ref(*source_page, source))
                    .target(Self::node_ref(*target_page, target))
                    .score(Some(0.8))
                    .evidence(vec![
                        Evidence::builder()
                            .kind("non_increasing_heading_size".to_owned())
                            .build(),
                    ])
                    .build(),
            );
        }
    }

    /// Returns whether one model/fallback block uses a prose-like label.
    fn is_flow_text(block: &Block) -> bool {
        matches!(
            block.label,
            LayoutLabel::Abstract
                | LayoutLabel::AsideText
                | LayoutLabel::Content
                | LayoutLabel::Footnote
                | LayoutLabel::ReferenceContent
                | LayoutLabel::Text
                | LayoutLabel::VerticalText
                | LayoutLabel::VisionFootnote
        ) && !block.lines.is_empty()
    }

    /// Returns whether one block is a model-recognized title class.
    fn is_heading(block: &Block) -> bool {
        matches!(
            block.label,
            LayoutLabel::DocTitle
                | LayoutLabel::FigureTitle
                | LayoutLabel::ParagraphTitle
        ) && !block.lines.is_empty()
    }

    /// Returns character-weighted heading font size or its bbox-height fallback.
    fn font_size(block: &Block) -> f64 {
        let mut weighted_sum = 0.0;
        let mut weight = 0_usize;
        for item in block.lines.iter().flat_map(|line| &line.text_items) {
            if let Some(size) =
                item.style.as_ref().and_then(|style| style.font_size)
            {
                let count = item.raw_text.chars().count().max(1);
                weighted_sum += size * count as f64;
                weight += count;
            }
        }
        if weight > 0 {
            weighted_sum / weight as f64
        } else {
            block.bbox.height()
        }
    }
}

impl Default for DocumentLinker {
    /// Creates the stateless default linker.
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        Block, BlockId, DocumentContext, DocumentLinker, LabelSource, Line,
        LineId, PageResult, RelationKind, WritingDirection,
    };
    use docparse_layout::{Bbox, LayoutLabel};

    /// Builds one page containing a repeated header block.
    fn page(page_number: u32) -> PageResult {
        let block_id =
            BlockId::fallback(page_number, &crate::RegionPath::root(), 0);
        let line = Line::builder()
            .id(LineId::new(&block_id, 0))
            .text("Running Header".to_owned())
            .bbox(
                Bbox::try_from([10.0, 10.0, 100.0, 20.0]).expect("valid bbox"),
            )
            .direction(WritingDirection::LeftToRight)
            .text_items(Vec::new())
            .build();
        let block = Block::builder()
            .id(block_id)
            .label(LayoutLabel::Header)
            .text("Running Header".to_owned())
            .label_source(LabelSource::Fallback)
            .bbox(
                Bbox::try_from([10.0, 10.0, 100.0, 20.0]).expect("valid bbox"),
            )
            .final_order(0)
            .lines(vec![line])
            .build();
        PageResult::builder()
            .page_number(page_number)
            .width(612.0)
            .height(792.0)
            .rotation(0)
            .blocks(vec![block])
            .build()
    }

    /// Verifies the linker type can be constructed without mutable page access.
    #[test]
    fn linker_has_a_read_only_boundary() {
        let _linker = DocumentLinker::new();
    }

    /// Verifies repeated chrome relations are stable and never mutate page aggregates.
    #[test]
    fn repeated_headers_produce_non_owning_relations() {
        let context = DocumentContext::builder()
            .page_count(3)
            .repeated_header_fingerprints(vec!["running header".to_owned()])
            .build();
        let pages = vec![page(1), page(2), page(3)];
        let before = serde_json::to_vec(&pages).expect("pages must serialize");

        let relations = DocumentLinker::new()
            .link(&context, &pages)
            .expect("linking must succeed");
        let after = serde_json::to_vec(&pages).expect("pages must serialize");

        assert_eq!(before, after);
        assert_eq!(relations.relations.len(), 2);
        assert!(
            relations
                .relations
                .iter()
                .all(|relation| relation.kind == RelationKind::RepeatedChrome)
        );
        let page_pairs: Vec<_> = relations
            .relations
            .iter()
            .map(|relation| {
                (relation.source.page_number, relation.target.page_number)
            })
            .collect();
        assert_eq!(page_pairs, vec![(1, 2), (2, 3)]);
    }
}
