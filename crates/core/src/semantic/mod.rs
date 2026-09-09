mod formula;
mod normalize;
mod paragraph;

use std::collections::BTreeMap;

use docparse_config::FusionConfig;
use docparse_layout::{Bbox, GeometrySource, LayoutLabel, Polygon};
use typed_builder::TypedBuilder;

use crate::fusion::assign::BlockSeed;
use crate::fusion::fallback::RegionTree;
use crate::label_policy::LabelPolicy;
use crate::line::{ConservativeLineAssembler, LineAssembler, LineFragment};
use crate::{
    Block, BlockId, Evidence, FallbackRegionId, LabelSource, Line, LineId,
    PageWarning, SemanticError, SourceRegionEvidence,
};

pub(crate) use formula::FormulaMatcher;
pub(crate) use paragraph::{ParagraphDecision, ParagraphSplitter};

/// Blocks and non-fatal warnings produced while preserving one source region.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct SemanticOutput {
    pub(crate) blocks: Vec<Block>,
    #[builder(default)]
    pub(crate) warnings: Vec<PageWarning>,
}

/// Converts uniquely owned local fragments into canonical nested result blocks.
#[derive(Debug, Clone)]
pub(crate) struct SemanticAssembler {
    page_number: u32,
    page_bbox: Bbox,
    config: FusionConfig,
}

impl SemanticAssembler {
    /// Builds detached watermarks without assigning, splitting, or ordering them with body content.
    pub(crate) fn watermark_blocks(
        &self,
        items: Vec<crate::TextItem>,
        annotations: Vec<Bbox>,
        decisions: &BTreeMap<crate::TextItemId, Evidence>,
    ) -> Result<Vec<Block>, SemanticError> {
        let mut fragments =
            ConservativeLineAssembler.fragments(items, &self.config)?;
        fragments.sort_by_key(|fragment| {
            fragment
                .items
                .iter()
                .map(|item| item.extraction_order)
                .min()
        });
        let mut blocks = Vec::new();
        for fragment in fragments {
            let ordinal = fragment
                .items
                .iter()
                .map(|item| item.extraction_order)
                .min()
                .unwrap_or(0);
            let explicit = fragment.items.iter().all(|item| {
                item.watermark == Some(crate::WatermarkSource::PdfMarkedContent)
            });
            let mut evidence: Vec<_> = fragment
                .items
                .iter()
                .filter_map(|item| decisions.get(&item.id).cloned())
                .collect();
            evidence.dedup();
            if evidence.is_empty() {
                evidence.push(
                    Evidence::builder()
                        .kind(
                            if explicit {
                                "pdf_watermark_mark"
                            } else {
                                "watermark_text_pattern"
                            }
                            .to_owned(),
                        )
                        .build(),
                );
            }
            let id = BlockId::watermark(self.page_number, ordinal, false);
            let lines = self.lines(&id, vec![fragment]);
            let polygon = Self::content_polygon(&lines)
                .and_then(|polygon| polygon.clipped(self.page_bbox));
            // Keep the original loose text bounds as the conservative index box. The
            // copied PDFium contour is the precise paint/hit-test footprint inside it.
            let bbox = Self::content_bbox(&lines)?.unwrap_or(self.page_bbox);
            blocks.push(
                Block::builder()
                    .id(id)
                    .label(LayoutLabel::Watermark)
                    .label_source(if explicit {
                        LabelSource::Pdf
                    } else {
                        LabelSource::Heuristic
                    })
                    .text(Block::derive_text(&LayoutLabel::Watermark, &lines))
                    .bbox(bbox)
                    .polygon(polygon)
                    .final_order(0)
                    .evidence(evidence)
                    .lines(lines)
                    .build(),
            );
        }
        for (ordinal, bbox) in annotations.into_iter().enumerate() {
            // Annotation /Contents is a comment, not its appearance text. Preserve the region
            // without inventing native text or claiming overlapping body text belongs to it.
            blocks.push(
                Block::builder()
                    .id(BlockId::watermark(
                        self.page_number,
                        ordinal as u32,
                        true,
                    ))
                    .label(LayoutLabel::Watermark)
                    .label_source(LabelSource::Pdf)
                    .text(String::new())
                    .bbox(bbox)
                    .final_order(0)
                    .evidence(vec![
                        Evidence::builder()
                            .kind("pdf_watermark_annotation".to_owned())
                            .build(),
                    ])
                    .lines(Vec::new())
                    .build(),
            );
        }
        Ok(blocks)
    }

    /// Encloses complete copied text footprints; missing geometry keeps the conservative bbox.
    fn content_polygon(lines: &[Line]) -> Option<Polygon> {
        let items: Vec<_> =
            lines.iter().flat_map(|line| &line.text_items).collect();
        if items.is_empty() || items.iter().any(|item| item.polygon.is_none()) {
            return None;
        }
        Polygon::enclosing(
            items
                .into_iter()
                .filter_map(|item| item.polygon.as_ref())
                .flat_map(|polygon| polygon.points().iter().copied()),
        )
        .ok()
    }

    /// Creates a page-local semantic assembler from validated fusion settings.
    pub(crate) const fn new(
        page_number: u32,
        page_bbox: Bbox,
        config: FusionConfig,
    ) -> Self {
        Self {
            page_number,
            page_bbox,
            config,
        }
    }

    /// Assembles one model candidate before page-wide content normalization.
    pub(crate) fn model_blocks(
        &self,
        mut seed: BlockSeed,
    ) -> Result<SemanticOutput, SemanticError> {
        let policy = LabelPolicy::from(&seed.label);
        let fragments =
            self.reassemble_fragments(std::mem::take(&mut seed.fragments))?;
        // Keep model-owned text together here. Page-wide normalization resolves candidate
        // overlaps after residual text has also been assembled.
        let blocks = vec![self.model_block(&seed, fragments)?];
        let warnings = if blocks.iter().all(|block| block.lines.is_empty())
            && matches!(policy, LabelPolicy::FlowText | LabelPolicy::Title)
        {
            vec![PageWarning {
                code: "EmptyModelRegion".to_owned(),
                stage: "semantic".to_owned(),
                message: format!(
                    "model region {} contains no extracted text",
                    seed.region_id.as_str()
                ),
            }]
        } else {
            Vec::new()
        };
        Ok(SemanticOutput::builder()
            .blocks(blocks)
            .warnings(warnings)
            .build())
    }

    /// Converts residual XY-cut leaves into fallback text blocks without losing fragments.
    pub(crate) fn fallback_blocks(
        &self,
        fragments: Vec<LineFragment>,
        tree: &RegionTree,
    ) -> Result<SemanticOutput, SemanticError> {
        let mut slots: Vec<_> = fragments.into_iter().map(Some).collect();
        let mut leaves = Vec::new();
        tree.visit_leaves(&mut |path, indices| {
            leaves.push((path.clone(), indices.to_vec()));
        });
        let mut blocks = Vec::new();
        for (path, indices) in leaves {
            let mut leaf_fragments = Vec::with_capacity(indices.len());
            for index in indices {
                let fragment = slots
                    .get_mut(index)
                    .and_then(Option::take)
                    .ok_or(SemanticError::MissingFallbackFragment { index })?;
                leaf_fragments.push(fragment);
            }
            for (split_ordinal, group) in self
                .fallback_paragraph_groups(
                    self.reassemble_fragments(leaf_fragments)?,
                )
                .into_iter()
                .enumerate()
            {
                blocks.push(self.fallback_block(
                    &path,
                    group,
                    u32::try_from(split_ordinal).unwrap_or(u32::MAX),
                )?);
            }
        }
        Ok(SemanticOutput::builder().blocks(blocks).build())
    }

    /// Applies paragraph splitting only to fallback regions after stable geometry sorting.
    fn fallback_paragraph_groups(
        &self,
        mut fragments: Vec<LineFragment>,
    ) -> Vec<Vec<LineFragment>> {
        fragments.sort_by(|left, right| {
            left.reading_order_y()
                .total_cmp(&right.reading_order_y())
                .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
                .then_with(|| {
                    left.items
                        .first()
                        .map(|item| item.id.as_str())
                        .cmp(&right.items.first().map(|item| item.id.as_str()))
                })
        });
        if fragments.is_empty() {
            return Vec::new();
        }
        let splitter = ParagraphSplitter::new(self.config.clone());
        let mut groups: Vec<Vec<LineFragment>> = Vec::new();
        for fragment in fragments {
            let should_split = groups
                .last()
                .and_then(|group| group.last())
                .is_some_and(|previous| {
                    matches!(
                        splitter.between(previous, &fragment),
                        ParagraphDecision::Split { .. }
                    )
                });
            if should_split || groups.is_empty() {
                groups.push(vec![fragment]);
            } else if let Some(group) = groups.last_mut() {
                group.push(fragment);
            }
        }
        groups
    }

    /// Reassembles conservative assignment facts only after their owner is known.
    fn reassemble_fragments(
        &self,
        fragments: Vec<LineFragment>,
    ) -> Result<Vec<LineFragment>, SemanticError> {
        let items = fragments
            .into_iter()
            .flat_map(|fragment| fragment.items)
            .collect();
        ConservativeLineAssembler
            .fragments(items, &self.config)
            .map_err(SemanticError::from)
    }

    /// Builds one model candidate while retaining complete source evidence.
    fn model_block(
        &self,
        seed: &BlockSeed,
        fragments: Vec<LineFragment>,
    ) -> Result<Block, SemanticError> {
        let block_id =
            BlockId::model(self.page_number, seed.source_detection_index, 0);
        let lines = self.lines(&block_id, fragments);
        let polygon = Self::content_polygon(&lines);
        let text = Block::derive_text(&seed.label, &lines);
        let bbox = Self::content_bbox(&lines)?.unwrap_or(seed.bbox);
        let evidence = seed
            .assignment_evidence
            .iter()
            .map(|assignment| {
                let mut details = BTreeMap::new();
                details.insert(
                    "coverage".to_owned(),
                    assignment.selected.coverage.to_string(),
                );
                details.insert(
                    "alternatives".to_owned(),
                    assignment.alternative_count.to_string(),
                );
                Evidence::builder()
                    .kind("primary_assignment".to_owned())
                    .score(Some(assignment.selected.assignment_score))
                    .details(details)
                    .build()
            })
            .collect();
        let source_region = SourceRegionEvidence::builder()
            .label(seed.label.clone())
            .model_region_id(Some(seed.region_id.clone()))
            .bbox(seed.bbox)
            .polygon(seed.polygon.clone())
            .geometry_source(seed.geometry_source)
            .confidence(Some(seed.confidence))
            .model_order(Some(seed.model_order))
            .build();
        Ok(Block::builder()
            .id(block_id)
            .label(seed.label.clone())
            .text(text)
            .raw_label(Some(seed.raw_label.clone()))
            .label_source(LabelSource::Model)
            .confidence(Some(seed.confidence))
            .bbox(bbox)
            .polygon(polygon)
            .source_region(Some(source_region))
            .model_region_id(Some(seed.region_id.clone()))
            .model_order(Some(seed.model_order))
            .final_order(0)
            .evidence(evidence)
            .semantic_hints(BTreeMap::new())
            .lines(lines)
            .build())
    }

    /// Builds one fallback child block from a stable XY-cut path.
    fn fallback_block(
        &self,
        path: &crate::RegionPath,
        fragments: Vec<LineFragment>,
        split_ordinal: u32,
    ) -> Result<Block, SemanticError> {
        let block_id = BlockId::fallback(self.page_number, path, split_ordinal);
        let lines = self.lines(&block_id, fragments);
        let polygon = Self::content_polygon(&lines);
        let text = Block::derive_text(&LayoutLabel::Text, &lines);
        let bbox = Self::content_bbox(&lines)?.unwrap_or(self.page_bbox);
        let fallback_id = FallbackRegionId::from_path(self.page_number, path);
        let source_region = SourceRegionEvidence::builder()
            .label(LayoutLabel::Text)
            .fallback_region_id(Some(fallback_id))
            .bbox(bbox)
            .geometry_source(GeometrySource::DerivedFromBbox)
            .build();
        Ok(Block::builder()
            .id(block_id)
            .label(LayoutLabel::Text)
            .text(text)
            .raw_label(None)
            .label_source(LabelSource::Fallback)
            .confidence(None)
            .bbox(bbox)
            .polygon(polygon)
            .source_region(Some(source_region))
            .model_region_id(None)
            .model_order(None)
            .final_order(0)
            .evidence(Vec::new())
            .semantic_hints(BTreeMap::new())
            .lines(lines)
            .build())
    }

    /// Converts owned fragments into final lines with stable local ordinals.
    fn lines(
        &self,
        block_id: &BlockId,
        fragments: Vec<LineFragment>,
    ) -> Vec<Line> {
        fragments
            .into_iter()
            .enumerate()
            .map(|(ordinal, fragment)| {
                let mut items = fragment.items;
                for (item_ordinal, item) in items.iter_mut().enumerate() {
                    item.final_order =
                        u32::try_from(item_ordinal).unwrap_or(u32::MAX);
                }
                let text = Line::derive_text(&items);
                Line::builder()
                    .id(LineId::new(
                        block_id,
                        u32::try_from(ordinal).unwrap_or(u32::MAX),
                    ))
                    .text(text)
                    .bbox(fragment.bbox)
                    .baseline(Some(fragment.baseline))
                    .rotation(fragment.rotation)
                    .direction(fragment.direction)
                    .model_region_coverage(None)
                    .inline_spans(Vec::new())
                    .text_items(items)
                    .build()
            })
            .collect()
    }

    /// Returns the exact union of non-empty final line geometry.
    fn content_bbox(lines: &[Line]) -> Result<Option<Bbox>, SemanticError> {
        let Some(first) = lines.first() else {
            return Ok(None);
        };
        lines
            .iter()
            .skip(1)
            .try_fold(first.bbox, |bbox, line| {
                Bbox::try_from([
                    bbox.left.min(line.bbox.left),
                    bbox.top.min(line.bbox.top),
                    bbox.right.max(line.bbox.right),
                    bbox.bottom.max(line.bbox.bottom),
                ])
            })
            .map(Some)
            .map_err(SemanticError::from)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use docparse_config::FusionConfig;
    use docparse_layout::{
        Bbox, GeometrySource, LayoutDetection, LayoutLabel, Point,
    };

    use super::SemanticAssembler;
    use crate::fusion::assign::{BlockSeed, ModelSeedInput};
    use crate::line::metrics::LineMetrics;
    use crate::line::{LineAnchor, LineFragment};
    use crate::{
        Baseline, BlockId, RepairAction, TextItem, TextItemId, TextSource,
        WritingDirection,
    };

    /// Builds one line fragment used to verify model-region paragraph splitting.
    fn fragment(index: u32, top: f64) -> LineFragment {
        positioned_fragment(
            index,
            &format!("line-{index}"),
            [10.0, top, 90.0, top + 10.0],
        )
    }

    /// Builds one positioned fragment for owner-local line regrouping tests.
    fn positioned_fragment(
        index: u32,
        text: &str,
        bounds: [f64; 4],
    ) -> LineFragment {
        let bbox = Bbox::try_from(bounds).expect("test bbox must be valid");
        let baseline = Baseline {
            start: Point::new(bbox.left, bbox.bottom),
            end: Point::new(bbox.right, bbox.bottom),
        };
        let item = TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text(text.to_owned())
            .bbox(bbox)
            .source(TextSource::Native)
            .build();
        LineFragment::builder()
            .items(vec![item])
            .bbox(bbox)
            .baseline(baseline)
            .direction(WritingDirection::LeftToRight)
            .metrics(
                LineMetrics::builder()
                    .font_size(10.0)
                    .font_size_estimated(true)
                    .bold_ratio(0.0)
                    .italic_ratio(0.0)
                    .bbox(bbox)
                    .baseline(baseline)
                    .anchor(LineAnchor::Left)
                    .indent(10.0)
                    .build(),
            )
            .rotation(0.0)
            .build()
    }

    /// Builds one fragment whose final source fact is an isolated encoded hyphen.
    fn encoded_hyphenated_fragment(
        index: u32,
        text: &str,
        bounds: [f64; 4],
        hyphen_bounds: [f64; 4],
    ) -> LineFragment {
        let mut fragment = positioned_fragment(index, text, bounds);
        fragment.items.push(
            TextItem::builder()
                .id(TextItemId::native(1, index.saturating_add(1)))
                .raw_text("-".to_owned())
                .bbox(
                    Bbox::try_from(hyphen_bounds)
                        .expect("test hyphen bbox must be valid"),
                )
                .source(TextSource::Native)
                .repair_actions(vec![RepairAction::EncodedHyphen])
                .build(),
        );
        fragment
    }

    /// Builds one text seed spanning all test fragments.
    fn text_seed() -> BlockSeed {
        let page = Bbox::try_from([0.0, 0.0, 100.0, 100.0])
            .expect("test page must be valid");
        let detection = LayoutDetection::builder()
            .source_detection_index(7)
            .raw_label("text".to_owned())
            .class_id(22)
            .label(LayoutLabel::Text)
            .confidence(0.9)
            .bbox(Bbox::try_from([5.0, 5.0, 95.0, 90.0]).expect("valid region"))
            .polygon(None)
            .geometry_source(GeometrySource::DerivedFromBbox)
            .model_order(2)
            .metadata(BTreeMap::new())
            .build();
        let mut seed =
            BlockSeed::try_from(ModelSeedInput::new(1, detection, page))
                .expect("test detection must become a seed");
        seed.fragments =
            vec![fragment(0, 10.0), fragment(1, 22.0), fragment(2, 60.0)];
        seed
    }

    /// Verifies a model region remains the authoritative final Block boundary.
    #[test]
    fn flow_region_remains_one_model_block() {
        let output = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("valid page"),
            FusionConfig::default(),
        )
        .model_blocks(text_seed())
        .expect("semantic assembly must succeed");

        assert_eq!(output.blocks.len(), 1);
        let block = output.blocks.first().expect("model block must exist");
        assert_eq!(block.id.as_str(), "p1:b:m7:s0");
        assert_eq!(block.label, LayoutLabel::Text);
        assert_eq!(
            block.model_region_id.as_ref().map(|id| id.as_str()),
            Some("p1:m7")
        );
        assert_eq!(block.lines.len(), 3);
    }

    /// Verifies atomic owner facts are regrouped into visual lines inside the model boundary.
    #[test]
    fn model_block_reassembles_owned_subpoint_fragments() {
        let mut seed = text_seed();
        seed.bbox = Bbox::try_from([69.5, 124.0, 262.5, 147.0])
            .expect("author region must be valid");
        seed.fragments = vec![
            positioned_fragment(
                0,
                "Shubham Ugare",
                [72.0, 125.390, 152.458, 134.347],
            ),
            positioned_fragment(1, "USA", [241.958, 136.893, 260.814, 145.053]),
            positioned_fragment(
                2,
                "University of Illinois Urbana-Champaign, ",
                [72.0, 136.992, 238.667, 145.142],
            ),
        ];

        let output = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 612.0, 792.0]).expect("valid page"),
            FusionConfig::default(),
        )
        .model_blocks(seed)
        .expect("semantic assembly must succeed");
        let block = output.blocks.first().expect("model block must exist");

        assert_eq!(output.blocks.len(), 1);
        assert_eq!(block.lines.len(), 2);
        assert_eq!(
            block
                .lines
                .get(1)
                .expect("affiliation line must exist")
                .text_items
                .iter()
                .map(|item| item.raw_text.as_str())
                .collect::<Vec<_>>(),
            vec!["University of Illinois Urbana-Champaign, ", "USA"]
        );
        assert_eq!(
            block.text,
            "Shubham Ugare University of Illinois Urbana-Champaign, USA"
        );
    }

    /// Verifies an encoded line-end hyphen remains visible in the canonical summary.
    #[test]
    fn flow_block_preserves_encoded_hyphen_without_separator() {
        let mut seed = text_seed();
        seed.fragments = vec![
            encoded_hyphenated_fragment(
                0,
                "pro",
                [10.0, 10.0, 35.0, 20.0],
                [35.0, 10.0, 38.0, 20.0],
            ),
            positioned_fragment(2, "grams", [10.0, 22.0, 40.0, 32.0]),
        ];

        let output = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("valid page"),
            FusionConfig::default(),
        )
        .model_blocks(seed)
        .expect("semantic assembly must succeed");
        let block = output.blocks.first().expect("model block must exist");

        assert_eq!(
            block
                .lines
                .first()
                .expect("the first physical line must exist")
                .text,
            "pro-"
        );
        assert_eq!(
            block
                .lines
                .get(1)
                .expect("the second physical line must exist")
                .text,
            "grams"
        );
        assert_eq!(block.text, "pro-grams");
    }

    /// Verifies a lowercase lexical compound cannot lose its encoded hyphen.
    #[test]
    fn flow_block_preserves_lowercase_lexical_hyphen() {
        let cases = [
            ("question", "answer", "question-answer"),
            ("Llama-2-7B", "chat", "Llama-2-7B-chat"),
            ("state-of", "the-art", "state-of-the-art"),
        ];
        for (prefix, suffix, expected) in cases {
            let mut seed = text_seed();
            seed.fragments = vec![
                encoded_hyphenated_fragment(
                    0,
                    prefix,
                    [10.0, 10.0, 65.0, 20.0],
                    [65.0, 10.0, 68.0, 20.0],
                ),
                positioned_fragment(2, suffix, [10.0, 22.0, 60.0, 32.0]),
            ];

            let output = SemanticAssembler::new(
                1,
                Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("valid page"),
                FusionConfig::default(),
            )
            .model_blocks(seed)
            .expect("semantic assembly must succeed");
            let block = output.blocks.first().expect("model block must exist");

            assert_eq!(block.text, expected);
        }
    }

    /// Verifies non-prose labels never apply prose hyphen projection.
    #[test]
    fn image_block_does_not_apply_prose_hyphen_projection() {
        let mut seed = text_seed();
        seed.label = LayoutLabel::Image;
        seed.raw_label = "image".to_owned();
        seed.fragments = vec![
            encoded_hyphenated_fragment(
                0,
                "high",
                [10.0, 10.0, 40.0, 20.0],
                [40.0, 10.0, 43.0, 20.0],
            ),
            positioned_fragment(2, "quality", [10.0, 22.0, 50.0, 32.0]),
        ];

        let output = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("valid page"),
            FusionConfig::default(),
        )
        .model_blocks(seed)
        .expect("semantic assembly must succeed");
        let block = output.blocks.first().expect("model block must exist");

        assert_eq!(block.text, "high- quality");
    }

    /// Verifies a vertically overlapping formula fragment cannot consume a prose hyphen.
    #[test]
    fn flow_block_does_not_join_encoded_hyphen_to_formula_fragment() {
        let mut seed = text_seed();
        seed.fragments = vec![
            encoded_hyphenated_fragment(
                0,
                "pro",
                [10.0, 10.0, 35.0, 20.0],
                [35.0, 10.0, 38.0, 20.0],
            ),
            positioned_fragment(2, "k", [20.0, 14.0, 25.0, 19.0]),
            positioned_fragment(3, "grams", [10.0, 22.0, 40.0, 32.0]),
        ];

        let output = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("valid page"),
            FusionConfig::default(),
        )
        .model_blocks(seed)
        .expect("semantic assembly must succeed");
        let block = output.blocks.first().expect("model block must exist");

        assert_eq!(block.text, "pro- k grams");
    }

    /// Verifies a capitalized continuation retains a lexical hyphen without a space.
    #[test]
    fn flow_block_preserves_hyphen_before_capitalized_continuation() {
        let mut seed = text_seed();
        seed.fragments = vec![
            encoded_hyphenated_fragment(
                0,
                "Fine",
                [10.0, 10.0, 40.0, 20.0],
                [40.0, 10.0, 43.0, 20.0],
            ),
            positioned_fragment(2, "Tuning", [10.0, 22.0, 45.0, 32.0]),
        ];

        let output = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("valid page"),
            FusionConfig::default(),
        )
        .model_blocks(seed)
        .expect("semantic assembly must succeed");
        let block = output.blocks.first().expect("model block must exist");

        assert_eq!(block.text, "Fine-Tuning");
    }

    /// Verifies algorithm summaries retain physical source-line boundaries.
    #[test]
    fn algorithm_block_summary_preserves_newlines() {
        let mut seed = text_seed();
        seed.label = LayoutLabel::Algorithm;
        seed.raw_label = "algorithm".to_owned();
        seed.fragments = vec![
            positioned_fragment(0, "let value = 1;", [10.0, 10.0, 70.0, 20.0]),
            positioned_fragment(1, "  return value;", [10.0, 22.0, 70.0, 32.0]),
        ];

        let output = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("valid page"),
            FusionConfig::default(),
        )
        .model_blocks(seed)
        .expect("semantic assembly must succeed");
        let block = output.blocks.first().expect("model block must exist");

        assert_eq!(block.text, "let value = 1;\n  return value;");
    }

    /// Verifies an empty flow region remains observable and carries a warning.
    #[test]
    fn empty_flow_region_is_preserved() {
        let mut seed = text_seed();
        seed.fragments.clear();
        let output = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0]).expect("valid page"),
            FusionConfig::default(),
        )
        .model_blocks(seed)
        .expect("semantic assembly must succeed");

        assert_eq!(output.blocks.len(), 1);
        assert!(
            output
                .blocks
                .first()
                .expect("empty block must remain")
                .lines
                .is_empty()
        );
        assert_eq!(
            output
                .warnings
                .first()
                .expect("empty model warning must exist")
                .code,
            "EmptyModelRegion"
        );
    }

    /// Verifies final lines concatenate source facts without geometric spacing inference.
    #[test]
    fn final_line_does_not_invent_space_between_text_items() {
        let mut first = fragment(0, 10.0);
        let first_item =
            first.items.first_mut().expect("first text item must exist");
        first_item.raw_text = "hello".to_owned();
        first_item.bbox = Bbox::try_from([10.0, 10.0, 35.0, 20.0])
            .expect("first word bbox must be valid");
        first.items.push(
            TextItem::builder()
                .id(TextItemId::native(1, 1))
                .raw_text("world".to_owned())
                .bbox(
                    Bbox::try_from([38.0, 10.0, 63.0, 20.0])
                        .expect("second word bbox must be valid"),
                )
                .source(TextSource::Native)
                .build(),
        );
        first.bbox = Bbox::try_from([10.0, 10.0, 63.0, 20.0])
            .expect("line bbox must be valid");
        let assembler = SemanticAssembler::new(
            1,
            Bbox::try_from([0.0, 0.0, 100.0, 100.0])
                .expect("page bbox must be valid"),
            FusionConfig::default(),
        );

        let lines = assembler.lines(&BlockId::model(1, 0, 0), vec![first]);
        let line = lines.first().expect("one final line must exist");
        let first_item = line
            .text_items
            .first()
            .expect("first final item must exist");
        let second_item = line
            .text_items
            .get(1)
            .expect("second final item must exist");

        assert_eq!(line.text, "helloworld");
        assert_eq!(first_item.raw_text, "hello");
        assert_eq!(second_item.raw_text, "world");
        assert!(second_item.repair_actions.is_empty());
    }
}
