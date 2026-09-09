use docparse_layout::{Bbox, LayoutLabel};

use super::SemanticAssembler;
use crate::fusion::assign::BlockSeed;
use crate::label_policy::LabelPolicy;
use crate::line::{ConservativeLineAssembler, LineAssembler};
use crate::{
    Block, BlockId, Evidence, LabelSource, SemanticError, SourceRegionEvidence,
};

/// Owns candidate blocks while merge groups are computed without rebuilding text.
struct LayoutGroup {
    blocks: Vec<Block>,
    bbox: Bbox,
}

impl From<Block> for LayoutGroup {
    /// Retains original candidate geometry and moves the text owner into its initial group.
    fn from(block: Block) -> Self {
        Self {
            bbox: block.bbox,
            blocks: vec![block],
        }
    }
}

impl LayoutGroup {
    /// Merges only full containment in either direction, including identical boxes.
    fn joins(&self, other: &Self) -> bool {
        // Exact inclusive bounds allow shared edges without treating a protruding
        // fragment as contained. Partial overlap alone never establishes one owner.
        self.bbox.contains_bbox(other.bbox)
            || other.bbox.contains_bbox(self.bbox)
    }
}

impl SemanticAssembler {
    /// Emits original reference geometry without invoking text or paragraph assembly.
    pub(crate) fn reference_block(&self, seed: BlockSeed) -> Block {
        let source = SourceRegionEvidence::builder()
            .label(LayoutLabel::Reference)
            .model_region_id(Some(seed.region_id.clone()))
            .bbox(seed.bbox)
            .polygon(seed.polygon.clone())
            .geometry_source(seed.geometry_source)
            .confidence(Some(seed.confidence))
            .model_order(Some(seed.model_order))
            .build();
        Block::builder()
            .id(BlockId::model(
                self.page_number,
                seed.source_detection_index,
                0,
            ))
            .label(LayoutLabel::Reference)
            .text(String::new())
            .raw_label(Some(seed.raw_label))
            .label_source(LabelSource::Model)
            .confidence(Some(seed.confidence))
            .bbox(seed.bbox)
            .polygon(seed.polygon)
            .source_region(Some(source))
            .model_region_id(Some(seed.region_id))
            .model_order(Some(seed.model_order))
            .final_order(0)
            .lines(Vec::new())
            .build()
    }

    /// Consolidates contained candidates, moving each text fact exactly once.
    pub(crate) fn normalize_blocks(
        &self,
        mut blocks: Vec<Block>,
    ) -> Result<Vec<Block>, SemanticError> {
        let original_count = blocks.len();
        blocks.sort_by(|left, right| left.id.cmp(&right.id));
        let mut groups: Vec<_> =
            blocks.into_iter().map(LayoutGroup::from).collect();
        loop {
            let pair = groups.iter().enumerate().find_map(|(left, group)| {
                groups
                    .iter()
                    .enumerate()
                    .skip(left + 1)
                    .find(|(_, other)| group.joins(other))
                    .map(|(right, _)| (left, right))
            });
            let Some((left, right)) = pair else {
                break;
            };
            let mut other = groups.remove(right);
            if let Some(group) = groups.get_mut(left) {
                group.bbox = Bbox::try_from([
                    group.bbox.left.min(other.bbox.left),
                    group.bbox.top.min(other.bbox.top),
                    group.bbox.right.max(other.bbox.right),
                    group.bbox.bottom.max(other.bbox.bottom),
                ])?;
                group.blocks.append(&mut other.blocks);
            }
            // Recheck containment against the current union after each merge.
            // Partial intersection alone cannot pull another neighbor into the group.
        }
        let mut result = Vec::with_capacity(groups.len());
        for group in groups {
            let mut blocks = group.blocks;
            if blocks.len() == 1 {
                result.extend(blocks);
                continue;
            }
            // Prefer an actual model owner to an empty duplicate or residual fragment.
            // Stable identity breaks score ties independently of extraction/input order.
            blocks.sort_by(|left, right| {
                let priority = |block: &Block| {
                    (
                        matches!(
                            LabelPolicy::from(&block.label),
                            LabelPolicy::Atomic | LabelPolicy::Structured
                        ),
                        !block.lines.is_empty(),
                        block.model_region_id.is_some(),
                    )
                };
                priority(right)
                    .cmp(&priority(left))
                    .then_with(|| {
                        right
                            .confidence
                            .unwrap_or(-1.0)
                            .total_cmp(&left.confidence.unwrap_or(-1.0))
                    })
                    .then_with(|| left.id.cmp(&right.id))
            });
            let count = blocks.len();
            let mut sources = Vec::new();
            for source in blocks.iter().flat_map(Block::source_regions) {
                if !sources.contains(source) {
                    sources.push(source.clone());
                }
            }
            sources.sort_by(|left, right| {
                left.model_region_id
                    .cmp(&right.model_region_id)
                    .then_with(|| {
                        left.fallback_region_id.cmp(&right.fallback_region_id)
                    })
                    .then_with(|| left.bbox.top.total_cmp(&right.bbox.top))
                    .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
            });
            let mut blocks = blocks.into_iter();
            let Some(mut block) = blocks.next() else {
                continue;
            };
            let mut items: Vec<_> = std::mem::take(&mut block.lines)
                .into_iter()
                .flat_map(|line| line.text_items)
                .collect();
            for other in blocks {
                items.extend(
                    other.lines.into_iter().flat_map(|line| line.text_items),
                );
                block.evidence.extend(other.evidence);
            }
            let fragments =
                ConservativeLineAssembler.fragments(items, &self.config)?;
            block.lines = self.lines(&block.id, fragments);
            block.bbox = group.bbox;
            block.polygon = Self::content_polygon(&block.lines);
            block.text = Block::derive_text(&block.label, &block.lines);
            block.model_order =
                sources.iter().filter_map(|source| source.model_order).min();
            block.source_regions = sources;
            block.evidence.push(
                Evidence::builder()
                    .kind("content_layout_merge".to_owned())
                    .details(std::collections::BTreeMap::from([(
                        "blocks".to_owned(),
                        count.to_string(),
                    )]))
                    .build(),
            );
            result.push(block);
        }
        if original_count != result.len() {
            tracing::debug!(
                "normalized page {} content layouts from {} to {} blocks",
                self.page_number,
                original_count,
                result.len()
            );
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use docparse_layout::{Bbox, LayoutLabel};

    use super::{LayoutGroup, SemanticAssembler};
    use crate::{
        Block, BlockId, LabelSource, Line, LineId, RegionPath, TextItem,
        TextItemId, TextSource, TextStyle, WritingDirection,
    };

    /// Supplies text-bearing residual layouts for geometry and ownership regressions.
    fn group(index: u32, bounds: [f64; 4]) -> LayoutGroup {
        let bbox = Bbox::try_from(bounds).expect("test bounds");
        let id = BlockId::fallback(1, &RegionPath::root(), index);
        let item = TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text("text".to_owned())
            .bbox(bbox)
            .source(TextSource::Native)
            .style(Some(TextStyle::builder().font_size(Some(10.0)).build()))
            .build();
        let line = Line::builder()
            .id(LineId::new(&id, 0))
            .text("text".to_owned())
            .bbox(bbox)
            .direction(WritingDirection::LeftToRight)
            .text_items(vec![item])
            .build();
        Block::builder()
            .id(id)
            .label(LayoutLabel::Text)
            .text("text".to_owned())
            .label_source(LabelSource::Fallback)
            .bbox(bbox)
            .final_order(0)
            .lines(vec![line])
            .build()
            .into()
    }

    /// Even almost coincident boxes remain separate unless one fully contains the other.
    #[test]
    fn high_iou_partial_overlaps_remain_separate() {
        let base = group(0, [0.0, 0.0, 10.0, 10.0]);
        for (bounds, expected) in [
            ([0.0, 0.0, 10.0, 10.0], true),
            ([0.01, 0.0, 10.01, 10.0], false),
            ([2.0, 0.0, 12.0, 10.0], false),
            ([2.5, 0.0, 12.5, 10.0], false),
            ([3.0, 0.0, 13.0, 10.0], false),
            ([10.0, 0.0, 20.0, 10.0], false),
        ] {
            assert_eq!(
                base.joins(&group(1, bounds)),
                expected,
                "containment decision for {bounds:?}"
            );
        }
    }

    /// Full containment is symmetric and includes shared edges, regardless of the area ratio.
    #[test]
    fn complete_containment_merges_in_both_directions() {
        let outer = group(0, [0.0, 0.0, 10.0, 10.0]);
        for bounds in [
            [1.0, 1.0, 2.0, 2.0],
            [0.0, 0.0, 1.0, 1.0],
            [9.0, 9.0, 10.0, 10.0],
            [0.0, 4.0, 10.0, 5.0],
        ] {
            let inner = group(1, bounds);
            assert!(outer.joins(&inner), "outer must absorb {bounds:?}");
            assert!(
                inner.joins(&outer),
                "input order must not affect containment"
            );
        }
    }

    /// A slight protrusion on any edge is partial overlap, not complete containment.
    #[test]
    fn partial_containment_does_not_merge() {
        let outer = group(0, [0.0, 0.0, 10.0, 10.0]);
        for bounds in [
            [-0.001, 1.0, 2.0, 2.0],
            [1.0, -0.001, 2.0, 2.0],
            [9.0, 1.0, 10.001, 2.0],
            [1.0, 9.0, 2.0, 10.001],
        ] {
            let protruding = group(1, bounds);
            assert!(
                !outer.joins(&protruding),
                "partial overlap for {bounds:?}"
            );
            assert!(
                !protruding.joins(&outer),
                "input order must not change the rule"
            );
        }
    }

    /// Absorbing a contained layout retains its text exactly once without expanding the outer box.
    #[test]
    fn normalization_preserves_text_from_contained_layouts() {
        let assembler = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("page"),
            docparse_config::FusionConfig::default(),
        );
        let outer_bounds = [0.0, 0.0, 10.0, 10.0];
        // Test both stable-ID orders, not just a permutation that sorting would erase.
        for bounds in [
            [outer_bounds, [1.0, 1.0, 2.0, 2.0]],
            [[1.0, 1.0, 2.0, 2.0], outer_bounds],
        ] {
            let blocks = [group(0, bounds[0]), group(1, bounds[1])]
                .into_iter()
                .flat_map(|group| group.blocks)
                .collect();
            let result =
                assembler.normalize_blocks(blocks).expect("normalization");
            assert_eq!(
                result.len(),
                1,
                "contained layouts must have one owner"
            );
            let block = result.first().expect("merged block");
            assert_eq!(
                block.bbox,
                Bbox::try_from(outer_bounds).expect("outer box")
            );
            let mut item_ids: Vec<_> = block
                .lines
                .iter()
                .flat_map(|line| &line.text_items)
                .map(|item| item.id.clone())
                .collect();
            item_ids.sort();
            assert_eq!(
                item_ids,
                vec![TextItemId::native(1, 0), TextItemId::native(1, 1)]
            );
        }
    }

    /// Nearby text with no intersection must remain separate even when it shares a visual row.
    #[test]
    fn proximity_does_not_merge() {
        assert!(
            !group(0, [0.0, 0.0, 10.0, 10.0])
                .joins(&group(1, [10.25, 0.0, 20.25, 10.0]))
        );
    }

    /// An overlapping chain preserves all three owners and their original bounds.
    #[test]
    fn partial_overlap_chain_preserves_layouts_and_text() {
        let assembler = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("page"),
            docparse_config::FusionConfig::default(),
        );
        let blocks = [
            group(0, [0.0, 0.0, 10.0, 10.0]),
            group(1, [2.0, 0.0, 12.0, 10.0]),
            group(2, [4.0, 0.0, 14.0, 10.0]),
        ]
        .into_iter()
        .flat_map(|group| group.blocks)
        .collect();
        let result = assembler
            .normalize_blocks(blocks)
            .expect("containment normalization");
        assert_eq!(result.len(), 3, "partial overlaps must not consolidate");
        for (block, bounds) in result.iter().zip([
            [0.0, 0.0, 10.0, 10.0],
            [2.0, 0.0, 12.0, 10.0],
            [4.0, 0.0, 14.0, 10.0],
        ]) {
            assert_eq!(
                block.bbox,
                Bbox::try_from(bounds).expect("original bounds")
            );
            assert_eq!(
                block.lines.iter().flat_map(|line| &line.text_items).count(),
                1
            );
        }
        assert_eq!(
            result
                .iter()
                .flat_map(|block| &block.lines)
                .flat_map(|line| &line.text_items)
                .count(),
            3
        );
    }
}
