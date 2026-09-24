use std::collections::BTreeMap;

use docparse_layout::{Bbox, LayoutLabel};

use super::SemanticAssembler;
use crate::fusion::assign::BlockSeed;
use crate::label_policy::LabelPolicy;
use crate::line::ConservativeLineAssembler;
use crate::{
    Block, BlockId, Evidence, LabelSource, SemanticError, SourceRegionEvidence,
};

/// Owns candidate blocks while merge groups are computed without rebuilding text.
struct LayoutGroup {
    blocks: Vec<Block>,
    bbox: Bbox,
    aligned_rows: bool,
}

impl From<Block> for LayoutGroup {
    /// Retains original candidate geometry and moves the text owner into its initial group.
    fn from(block: Block) -> Self {
        Self {
            bbox: block.bbox,
            blocks: vec![block],
            aligned_rows: false,
        }
    }
}

impl LayoutGroup {
    /// Joins complete list owners, original containment, and algorithm gutters.
    fn joins(&self, other: &Self) -> bool {
        // One row cannot transfer ownership of an entire preassembled list.
        if self.aligned_rows || other.aligned_rows {
            return (self.aligned_rows && other.bbox.contains_bbox(self.bbox))
                || (other.aligned_rows && self.bbox.contains_bbox(other.bbox));
        }
        // A merged gutter expands the group's rectangle across blank space; only
        // original source boxes can prove containment of another content block.
        if self.blocks.iter().any(|left| {
            other.blocks.iter().any(|right| {
                left.bbox.contains_bbox(right.bbox)
                    || right.bbox.contains_bbox(left.bbox)
            })
        }) {
            return true;
        }
        let is_line_numbers = |algorithm: &Block, candidate: &Block| {
            if algorithm.label != LayoutLabel::Algorithm
                || !matches!(
                    candidate.label,
                    LayoutLabel::Text | LayoutLabel::AsideText
                )
                || candidate.bbox.width() > algorithm.bbox.width() * 0.1
            {
                return false;
            }
            let empty_aside = candidate.label == LayoutLabel::AsideText
                && candidate.lines.is_empty()
                && candidate.text.is_empty();
            // An empty aside can be a duplicate detection straddling the code's left edge.
            let beside = candidate.bbox.right <= algorithm.bbox.left
                && algorithm.bbox.left - candidate.bbox.right <= 15.0;
            let duplicate = empty_aside
                && candidate.bbox.left <= algorithm.bbox.left
                && algorithm.bbox.left - candidate.bbox.left <= 15.0
                && candidate.bbox.right - algorithm.bbox.left
                    >= candidate.bbox.width() * 0.5;
            if !beside && !duplicate {
                return false;
            }
            let overlap = (algorithm.bbox.bottom.min(candidate.bbox.bottom)
                - algorithm.bbox.top.max(candidate.bbox.top))
            .max(0.0);
            if overlap < candidate.bbox.height() * 0.8 {
                return false;
            }
            if duplicate {
                return true;
            }
            // A short first number may be cut separately from the long vertical strip.
            let (digits, visible) = candidate
                .text
                .chars()
                .filter(|character| !character.is_whitespace())
                .fold((0, 0), |(digits, visible), character| {
                    (
                        digits + usize::from(character.is_ascii_digit()),
                        visible + 1,
                    )
                });
            visible > 0 && digits * 5 >= visible * 4
        };
        self.blocks.iter().any(|algorithm| {
            other
                .blocks
                .iter()
                .any(|candidate| is_line_numbers(algorithm, candidate))
        }) || other.blocks.iter().any(|algorithm| {
            self.blocks
                .iter()
                .any(|candidate| is_line_numbers(algorithm, candidate))
        })
    }
}

impl SemanticAssembler<'_> {
    /// Groups repeated narrow symbols and aligned descriptions into one spatial list.
    fn aligned_row_groups(
        &self,
        blocks: Vec<Block>,
    ) -> Result<Vec<LayoutGroup>, SemanticError> {
        // ponytail: page-local row pairing is quadratic; index row bands if dense pages make it costly.
        // A narrow symbol and nearby definition must share a physical row; repeated
        // column starts, rather than one aligned pair, establish a structured list.
        let pairs: Vec<_> = blocks
            .iter()
            .filter_map(|left| {
                if left.label != LayoutLabel::Text
                    || left.lines.len() != 1
                    || left.bbox.width() > left.bbox.height() * 4.0
                {
                    return None;
                }
                blocks
                    .iter()
                    .filter(|right| {
                        let overlap = (left.bbox.bottom.min(right.bbox.bottom)
                            - left.bbox.top.max(right.bbox.top))
                        .max(0.0);
                        right.label == LayoutLabel::Text
                            && right.lines.len() == 1
                            && right.bbox.left >= left.bbox.right
                            && right.bbox.left - left.bbox.left
                                <= left.bbox.height() * 12.0
                            && overlap
                                >= left.bbox.height().min(right.bbox.height())
                                    * 0.5
                    })
                    .min_by(|a, b| a.bbox.left.total_cmp(&b.bbox.left))
                    .map(|right| (left, right))
            })
            .collect();
        // Every pair can be a center: the densest aligned run wins when the first row drifts.
        let mut candidates = Vec::new();
        for &(left, right) in &pairs {
            let left_x = left.bbox.left;
            let right_x = right.bbox.left;
            let mut aligned: Vec<_> = pairs
                .iter()
                .copied()
                .filter(|&(left, right)| {
                    (left.bbox.left - left_x).abs() <= 2.0
                        && (right.bbox.left - right_x).abs() <= 2.0
                })
                .collect();
            aligned.sort_by(|a, b| a.0.bbox.top.total_cmp(&b.0.bbox.top));
            // Six row heights permit a short wrapped entry without bridging distant lists.
            for run in aligned.chunk_by(|previous, next| {
                next.0.bbox.top - previous.0.bbox.top
                    <= previous.0.bbox.height() * 6.0
            }) {
                if run.len() < 5 {
                    continue;
                }
                let top = run
                    .iter()
                    .map(|(left, right)| left.bbox.top.min(right.bbox.top))
                    .fold(f64::INFINITY, f64::min);
                let bottom = run
                    .iter()
                    .map(|(left, right)| {
                        left.bbox.bottom.max(right.bbox.bottom)
                    })
                    .fold(f64::NEG_INFINITY, f64::max);
                let row_height = run
                    .iter()
                    .map(|(left, _)| left.bbox.height())
                    .fold(0.0_f64, f64::max);
                // Candidate: paired rows, column starts, vertical span, tallest symbol row.
                candidates.push((
                    run.len(),
                    (left_x, right_x),
                    (top, bottom),
                    row_height,
                ));
            }
        }
        candidates.sort_by_key(|candidate| std::cmp::Reverse(candidate.0));
        let mut membership = BTreeMap::<BlockId, usize>::new();
        let mut list_count = 0;
        for (count, (left_x, right_x), (top, bottom), row_height) in candidates
        {
            // Keep short wrapped or fused rows between confirmed pairs in the same list.
            let members: Vec<_> = blocks
                .iter()
                .filter(|block| {
                    !membership.contains_key(&block.id)
                        && block.label == LayoutLabel::Text
                        && ((block.bbox.left - left_x).abs() <= 2.0
                            || (block.bbox.left - right_x).abs() <= 2.0)
                        && block.bbox.center().y >= top
                        && block.bbox.center().y <= bottom
                        && block.bbox.height() <= row_height * 4.0
                })
                .collect();
            if members.len() < count * 2 {
                continue;
            }
            for block in members {
                membership.insert(block.id.clone(), list_count);
            }
            list_count += 1;
        }
        let mut bundles = BTreeMap::<usize, Vec<Block>>::new();
        let mut groups = Vec::new();
        for block in blocks {
            if let Some(list_index) = membership.get(&block.id) {
                bundles.entry(*list_index).or_default().push(block);
            } else {
                groups.push(LayoutGroup::from(block));
            }
        }
        for bundle in bundles.into_values() {
            let mut rows = bundle.into_iter();
            let Some(first) = rows.next() else {
                continue;
            };
            let mut group = LayoutGroup::from(first);
            group.aligned_rows = true;
            for block in rows {
                group.bbox = Bbox::try_from([
                    group.bbox.left.min(block.bbox.left),
                    group.bbox.top.min(block.bbox.top),
                    group.bbox.right.max(block.bbox.right),
                    group.bbox.bottom.max(block.bbox.bottom),
                ])?;
                group.blocks.push(block);
            }
            tracing::debug!(
                "grouped {} aligned row fragments on page {}",
                group.blocks.len(),
                self.page_number
            );
            groups.push(group);
        }
        groups.sort_by(|left, right| {
            left.blocks
                .first()
                .map(|block| &block.id)
                .cmp(&right.blocks.first().map(|block| &block.id))
        });
        Ok(groups)
    }

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

    /// Consolidates contained blocks and aligned lists, moving each text fact exactly once.
    pub(crate) fn normalize_blocks(
        &self,
        mut blocks: Vec<Block>,
    ) -> Result<Vec<Block>, SemanticError> {
        let original_count = blocks.len();
        blocks.sort_by(|left, right| left.id.cmp(&right.id));
        let mut groups = self.aligned_row_groups(blocks)?;
        let mut left = 0;
        while left < groups.len() {
            let mut right = left + 1;
            while right < groups.len() {
                if !groups
                    .get(left)
                    .expect("left index is below group count")
                    .joins(
                        groups
                            .get(right)
                            .expect("right index is below group count"),
                    )
                {
                    right += 1;
                    continue;
                }
                let mut other = groups.remove(right);
                let group = groups
                    .get_mut(left)
                    .expect("removing a later group retains the left index");
                // Sidecar gutters expand the owner beyond its original model box.
                // Recheck skipped candidates whenever the group's bounds grow.
                if !group.bbox.contains_bbox(other.bbox) {
                    group.bbox = Bbox::try_from([
                        group.bbox.left.min(other.bbox.left),
                        group.bbox.top.min(other.bbox.top),
                        group.bbox.right.max(other.bbox.right),
                        group.bbox.bottom.max(other.bbox.bottom),
                    ])?;
                    right = left + 1;
                }
                group.aligned_rows |= other.aligned_rows;
                group.blocks.append(&mut other.blocks);
            }
            left += 1;
        }
        let mut result = Vec::with_capacity(groups.len());
        for group in groups {
            let mut blocks = group.blocks;
            if blocks.len() == 1 {
                result.extend(blocks);
                continue;
            }
            let aligned_text = group.aligned_rows
                && blocks.iter().all(|block| block.label == LayoutLabel::Text);
            // Prefer an actual model owner to an empty duplicate or residual fragment.
            // Stable identity breaks score ties independently of extraction/input order.
            blocks.sort_by(|left, right| {
                let priority = |block: &Block| {
                    (
                        matches!(
                            LabelPolicy::from(&block.label),
                            LabelPolicy::Algorithm
                                | LabelPolicy::Atomic
                                | LabelPolicy::Structured
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
            // Empty aside detections contribute provenance, not content geometry.
            let mut content_bbox: Option<Bbox> = None;
            for original in &blocks {
                if original.label == LayoutLabel::AsideText
                    && original.lines.is_empty()
                    && original.text.is_empty()
                {
                    continue;
                }
                content_bbox = Some(match content_bbox {
                    Some(bounds) => Bbox::try_from([
                        bounds.left.min(original.bbox.left),
                        bounds.top.min(original.bbox.top),
                        bounds.right.max(original.bbox.right),
                        bounds.bottom.max(original.bbox.bottom),
                    ])?,
                    None => original.bbox,
                });
            }
            let final_bbox = content_bbox.unwrap_or(group.bbox);
            let sparse_envelope = !blocks.iter().any(|original| {
                original.bbox.contains_bbox(final_bbox)
                    && final_bbox.contains_bbox(original.bbox)
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
            if aligned_text {
                // Content preserves paired columns and physical rows without invoking table recovery.
                block.label = LayoutLabel::Content;
                block.label_source = LabelSource::Heuristic;
                block.raw_label = None;
            }
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
            // Rebuilding a merged owner must retain the original formula scopes.
            let fragments = ConservativeLineAssembler.fragments_with_formulas(
                items,
                &self.config,
                self.rules,
                self.formulas,
            )?;
            block.lines = self.lines(&block.id, fragments);
            block.bbox = final_bbox;
            if sparse_envelope {
                // This survives evidence-hidden JSON so the validator can distinguish
                // a sparse union from an unmerged block containing another layout.
                block.semantic_hints.insert(
                    Block::SPARSE_LAYOUT_HINT.to_owned(),
                    "true".to_owned(),
                );
            }
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
        Block, BlockId, DocumentContext, DocumentResult, JsonRenderer,
        LabelSource, Line, LineId, ModelRegionId, PageResult, RegionPath,
        ResultValidator, SchemaVersion, TextItem, TextItemId, TextSource,
        TextStyle, WritingDirection,
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

    /// Builds one fallback row with source text that can be reassembled after grouping.
    fn text_group(index: u32, bounds: [f64; 4], text: &str) -> LayoutGroup {
        let mut result = group(index, bounds);
        let block = result.blocks.first_mut().expect("group has one block");
        block.text = text.to_owned();
        let line = block.lines.first_mut().expect("block has one line");
        line.text = text.to_owned();
        line.text_items
            .first_mut()
            .expect("line has one item")
            .raw_text = text.to_owned();
        result
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

    /// Line-number strips beside code join their algorithm, including a separately cut first number.
    #[test]
    fn numeric_gutters_join_their_algorithm_without_claiming_page_numbers() {
        let assembler = SemanticAssembler::new(
            37,
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("page"),
            docparse_config::FusionConfig::default(),
        );
        // Preserve source text through the real normalization and line-reassembly path.
        let block = |index, bounds, label, value: &str| {
            let mut result = text_group(index, bounds, value);
            result.blocks.first_mut().expect("one block").label = label;
            result
        };
        let mut algorithm = block(
            0,
            [77.0, 87.0, 485.0, 731.0],
            LayoutLabel::Algorithm,
            "try_stmt",
        );
        let algorithm_block =
            algorithm.blocks.first_mut().expect("algorithm block");
        algorithm_block.id = BlockId::model(37, 0, 0);
        algorithm_block.label_source = LabelSource::Model;
        algorithm_block.model_region_id = Some(ModelRegionId::detected(37, 0));
        algorithm_block.confidence = Some(0.8);
        let mut aside = block(
            2,
            [57.0, 104.0, 67.0, 571.0],
            LayoutLabel::AsideText,
            "64 65 66 67 68",
        );
        let aside_block = aside.blocks.first_mut().expect("aside block");
        aside_block.id = BlockId::model(37, 1, 0);
        aside_block.label_source = LabelSource::Model;
        aside_block.model_region_id = Some(ModelRegionId::detected(37, 1));
        aside_block.confidence = Some(0.9);
        let blocks = [
            algorithm,
            block(1, [60.0, 88.0, 67.0, 93.0], LayoutLabel::Text, "63"),
            aside,
            block(
                3,
                [57.0, 583.0, 67.0, 730.0],
                LayoutLabel::Text,
                "123 124 125",
            ),
            block(4, [301.0, 753.0, 311.0, 762.0], LayoutLabel::Number, "37"),
            block(5, [30.0, 120.0, 40.0, 200.0], LayoutLabel::Text, "1 2 3"),
            block(6, [68.0, 220.0, 75.0, 300.0], LayoutLabel::Text, "A B C"),
            block(7, [10.0, 320.0, 67.0, 400.0], LayoutLabel::Text, "1 2 3"),
            block(8, [490.0, 420.0, 500.0, 500.0], LayoutLabel::Text, "1 2 3"),
        ]
        .into_iter()
        .flat_map(|group| group.blocks)
        .collect();

        let result = assembler.normalize_blocks(blocks).expect("normalize");
        assert_eq!(result.len(), 6, "only adjacent numeric gutters join");
        let algorithm = result
            .iter()
            .find(|block| block.label == LayoutLabel::Algorithm)
            .expect("algorithm retains its label");
        assert!((algorithm.bbox.left - 57.0).abs() < 1e-6);
        assert_eq!(
            algorithm
                .lines
                .iter()
                .flat_map(|line| &line.text_items)
                .count(),
            4
        );
        assert!(algorithm.text.contains("63"));
        assert!(algorithm.text.contains("64"));
        assert!(algorithm.text.contains("123"));
        assert!(
            !result
                .iter()
                .any(|block| block.label == LayoutLabel::AsideText)
        );
        assert!(
            result
                .iter()
                .any(|block| block.label == LayoutLabel::Number)
        );
    }

    /// A sparse merged envelope may contain unrelated text without making that page invalid.
    #[test]
    fn sparse_algorithm_envelope_keeps_unrelated_text_valid() {
        let assembler = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("page"),
            docparse_config::FusionConfig::default(),
        );
        let mut algorithm = text_group(0, [77.0, 10.0, 400.0, 90.0], "code");
        algorithm.blocks.first_mut().expect("algorithm block").label =
            LayoutLabel::Algorithm;
        let blocks = [
            algorithm,
            text_group(1, [60.0, 12.0, 67.0, 80.0], "1 2 3 4 5"),
            text_group(2, [68.0, 30.0, 75.0, 40.0], "aside"),
        ]
        .into_iter()
        .flat_map(|group| group.blocks)
        .collect();
        let mut blocks = assembler.normalize_blocks(blocks).expect("normalize");
        assert_eq!(blocks.len(), 2);
        for (order, block) in blocks.iter_mut().enumerate() {
            block.final_order = order as u32;
        }
        let page = PageResult::builder()
            .page_number(1)
            .width(612.0)
            .height(792.0)
            .rotation(0)
            .blocks(blocks)
            .build();
        ResultValidator::validate_page(&page)
            .expect("sparse merge remains valid");
        let document = DocumentResult::builder()
            .schema_version(SchemaVersion::V2_0)
            .context(DocumentContext::builder().page_count(1).build())
            .pages(vec![page])
            .build();
        let output = docparse_config::OutputConfig::builder()
            .formula_placeholder("[formula]".to_owned())
            .include_evidence(false)
            .include_diagnostics(false)
            .build();
        let browser = serde_json::to_value(JsonRenderer::view_with_config(
            &document, &output,
        ))
        .expect("browser JSON");
        let decoded: DocumentResult = serde_json::from_value(browser)
            .expect("schema-compatible browser JSON");
        ResultValidator::validate(&decoded)
            .expect("browser JSON remains valid");
    }

    /// An empty duplicate aside stripe beside a numbered algorithm has no standalone layout.
    #[test]
    fn empty_aside_stripe_is_absorbed_by_algorithm() {
        let assembler = SemanticAssembler::new(
            46,
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("page"),
            docparse_config::FusionConfig::default(),
        );
        let mut algorithm = text_group(
            0,
            [56.8, 87.0, 480.7, 731.0],
            "153 // a comma-separated list",
        );
        let algorithm_block =
            algorithm.blocks.first_mut().expect("algorithm block");
        algorithm_block.id = BlockId::model(46, 0, 0);
        algorithm_block.label = LayoutLabel::Algorithm;
        algorithm_block.label_source = LabelSource::Model;
        algorithm_block.model_region_id = Some(ModelRegionId::detected(46, 0));
        let mut aside = group(1, [55.0, 93.0, 69.5, 727.0]);
        let aside_block = aside.blocks.first_mut().expect("aside block");
        aside_block.id = BlockId::model(46, 2, 0);
        aside_block.label = LayoutLabel::AsideText;
        aside_block.label_source = LabelSource::Model;
        aside_block.model_region_id = Some(ModelRegionId::detected(46, 2));
        aside_block.lines.clear();
        aside_block.text.clear();

        let result = assembler
            .normalize_blocks(
                [algorithm, aside]
                    .into_iter()
                    .flat_map(|group| group.blocks)
                    .collect(),
            )
            .expect("normalize");
        assert_eq!(result.len(), 1);
        let algorithm = result.first().expect("merged algorithm");
        assert_eq!(algorithm.label, LayoutLabel::Algorithm);
        assert!((algorithm.bbox.left - 56.8).abs() < 1e-6);
        assert_eq!(
            algorithm
                .lines
                .iter()
                .flat_map(|line| &line.text_items)
                .count(),
            1
        );
    }

    /// Repeated aligned symbol-definition rows become one spatial layout without claiming headings or prose columns.
    #[test]
    fn aligned_symbol_rows_form_one_layout() {
        let assembler = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("page"),
            docparse_config::FusionConfig::default(),
        );
        let mut title =
            text_group(1, [72.0, 80.0, 200.0, 90.0], "List of Symbols");
        title.blocks.first_mut().expect("title block").label =
            LayoutLabel::ParagraphTitle;
        let mut footer = text_group(2, [301.0, 750.0, 311.0, 760.0], "29");
        footer.blocks.first_mut().expect("footer block").label =
            LayoutLabel::Number;
        let mut table =
            text_group(6, [120.0, 99.0, 300.0, 110.0], "separate table");
        table.blocks.first_mut().expect("table block").label =
            LayoutLabel::Table;
        let mut groups = vec![
            title,
            footer,
            table,
            text_group(3, [50.0, 300.0, 260.0, 310.0], "Left column prose"),
            text_group(4, [330.0, 300.0, 540.0, 310.0], "Right column prose"),
            text_group(5, [78.0, 136.0, 300.0, 168.0], "Mα dmatch pmatch"),
        ];
        for (row, (top, symbol, description)) in [
            (100.0, "G", "Formal Grammar"),
            (112.0, "L", "Language of a grammar"),
            (124.0, "P", "Parser"),
            (172.0, "T", "Tokenizer in an LLM"),
            (184.0, "V", "Vocabulary of an LLM"),
            (196.0, "Q", "States in a DFA"),
        ]
        .into_iter()
        .enumerate()
        {
            groups.push(text_group(
                10 + row as u32 * 2,
                [78.0, top, 88.0, top + 9.0],
                symbol,
            ));
            groups.push(text_group(
                11 + row as u32 * 2,
                [121.2, top, 290.0, top + 9.0],
                description,
            ));
        }
        let blocks =
            groups.into_iter().flat_map(|group| group.blocks).collect();

        let result = assembler.normalize_blocks(blocks).expect("normalize");
        assert_eq!(
            result.len(),
            6,
            "rows form one layout without swallowing a model table"
        );
        let content = result
            .iter()
            .find(|block| block.label == LayoutLabel::Content)
            .expect("paired rows retain spatial structure");
        assert!((content.bbox.left - 78.0).abs() < 1e-6);
        assert!((content.bbox.right - 300.0).abs() < 1e-6);
        assert_eq!(
            content
                .lines
                .iter()
                .flat_map(|line| &line.text_items)
                .count(),
            13
        );
        assert!(content.text.lines().next().is_some_and(|line| {
            line.contains('G') && line.contains("Formal Grammar")
        }));
        assert!(content.text.contains("Mα dmatch pmatch"));
        assert!(result.iter().any(|block| block.label == LayoutLabel::Table));
        assert!(
            result
                .iter()
                .any(|block| block.label == LayoutLabel::Number)
        );
        assert!(
            result
                .iter()
                .any(|block| block.label == LayoutLabel::ParagraphTitle)
        );
    }

    /// Small drift around a shared column center must not depend on the first row's x position.
    #[test]
    fn aligned_rows_tolerate_first_anchor_drift() {
        let assembler = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("page"),
            docparse_config::FusionConfig::default(),
        );
        let mut blocks = Vec::new();
        for (row, (left, top)) in [
            (78.0, 100.0),
            (78.0, 112.0),
            (79.5, 124.0),
            (79.5, 136.0),
            (81.0, 148.0),
            (81.0, 160.0),
        ]
        .into_iter()
        .enumerate()
        {
            blocks.extend(
                text_group(
                    (row * 2) as u32,
                    [left, top, left + 10.0, top + 9.0],
                    "symbol",
                )
                .blocks,
            );
            blocks.extend(
                text_group(
                    (row * 2 + 1) as u32,
                    [121.2, top, 290.0, top + 9.0],
                    "definition",
                )
                .blocks,
            );
        }
        let result = assembler.normalize_blocks(blocks).expect("normalize");
        assert_eq!(result.len(), 1);
        assert_eq!(
            result.first().expect("aligned list").label,
            LayoutLabel::Content
        );
    }

    /// Expanding a group revisits earlier candidates without changing partial-overlap ownership.
    #[test]
    fn expansion_rechecks_skipped_boxes_without_merging_partial_neighbors() {
        let assembler = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("page"),
            docparse_config::FusionConfig::default(),
        );
        // The later enclosing box must absorb the skipped sibling, but not the protruding neighbor.
        let blocks = [
            group(0, [2.0, 2.0, 3.0, 3.0]),
            group(1, [6.0, 6.0, 7.0, 7.0]),
            group(2, [0.0, 0.0, 10.0, 10.0]),
            group(3, [9.0, 0.0, 12.0, 10.0]),
        ]
        .into_iter()
        .flat_map(|group| group.blocks)
        .collect();
        let result = assembler.normalize_blocks(blocks).expect("normalize");
        assert_eq!(result.len(), 2);
        let counts: Vec<_> = result
            .iter()
            .map(|block| {
                block.lines.iter().flat_map(|line| &line.text_items).count()
            })
            .collect();
        assert_eq!(counts, [3, 1]);
    }

    /// Measures a duplicate-heavy tail after unrelated blocks without a machine-specific timing assertion.
    #[test]
    #[ignore = "manual normalization performance measurement"]
    fn duplicate_tail_benchmark() {
        for count in [256_u32, 512, 1024] {
            let assembler = SemanticAssembler::new(
                1,
                Bbox::try_from([0.0, 0.0, 10000.0, 100.0]).expect("page"),
                docparse_config::FusionConfig::default(),
            );
            let blocks = (0..count)
                .flat_map(|index| {
                    let x = if index < count / 2 {
                        f64::from(index) * 3.0
                    } else {
                        5000.0
                    };
                    group(index + 10000, [x, 0.0, x + 1.0, 1.0]).blocks
                })
                .collect();
            let started = std::time::Instant::now();
            let result = assembler.normalize_blocks(blocks).expect("normalize");
            eprintln!("normalize {count} blocks: {:?}", started.elapsed());
            assert_eq!(result.len(), count as usize / 2 + 1);
            assert_eq!(
                result
                    .iter()
                    .flat_map(|block| &block.lines)
                    .flat_map(|line| &line.text_items)
                    .count(),
                count as usize
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
