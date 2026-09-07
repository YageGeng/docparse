use std::cmp::Ordering;
use std::collections::BTreeSet;

use docparse_config::FusionConfig;
use docparse_layout::{
    Bbox, GeometrySource, LayoutDetection, LayoutLabel, Point, Polygon,
};
use typed_builder::TypedBuilder;

use crate::line::LineFragment;
use crate::{LineError, ModelRegionId, TextItem};

/// Reasons that an untrusted model detection cannot become a fusion seed.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SeedError {
    /// The public page number must follow the one-based schema contract.
    #[error("page number must be greater than zero")]
    InvalidPageNumber,
    /// A model row references an unsupported class index.
    #[error("class ID {0} is outside the fixed model schema")]
    InvalidClass(i64),
    /// Model confidence must already be a finite probability.
    #[error("confidence must be finite and within [0, 1]")]
    InvalidConfidence,
    /// Model geometry must be finite, non-degenerate, and on the page.
    #[error("detection geometry lies outside the page")]
    InvalidGeometry,
    /// A rectangle could not be converted to the canonical polygon API.
    #[error(transparent)]
    Geometry(#[from] docparse_layout::GeometryError),
}

/// Context required to validate one detection into one stable model seed.
#[derive(Debug, Clone)]
pub(crate) struct ModelSeedInput {
    page_number: u32,
    detection: LayoutDetection,
    page_bbox: Bbox,
}

impl ModelSeedInput {
    /// Binds a source detection to its one-based page and canonical page box.
    pub(crate) const fn new(
        page_number: u32,
        detection: LayoutDetection,
        page_bbox: Bbox,
    ) -> Self {
        Self {
            page_number,
            detection,
            page_bbox,
        }
    }
}

/// Exact assignment score components retained for diagnostics and tie-breaking.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct AssignmentScore {
    pub(crate) coverage: f64,
    pub(crate) center_inside: f64,
    pub(crate) baseline_intersection: f64,
    pub(crate) model_confidence: f64,
    pub(crate) specificity: f64,
    pub(crate) assignment_score: f64,
    pub(crate) region_area: f64,
}

/// Stable evidence explaining one selected primary owner and its alternatives.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct AssignEvidence {
    pub(crate) selected: AssignmentScore,
    pub(crate) alternative_count: usize,
}

/// One model-backed region seed that owns zero or more conservative fragments.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct BlockSeed {
    pub(crate) page_number: u32,
    pub(crate) region_id: ModelRegionId,
    pub(crate) source_detection_index: u32,
    pub(crate) raw_label: String,
    pub(crate) class_id: i64,
    pub(crate) label: LayoutLabel,
    pub(crate) confidence: f64,
    pub(crate) bbox: Bbox,
    #[builder(default)]
    pub(crate) polygon: Option<Polygon>,
    pub(crate) geometry_source: GeometrySource,
    pub(crate) model_order: i64,
    geometry: Polygon,
    #[builder(default)]
    pub(crate) fragments: Vec<LineFragment>,
    #[builder(default)]
    pub(crate) assignment_evidence: Vec<AssignEvidence>,
}

impl TryFrom<ModelSeedInput> for BlockSeed {
    type Error = SeedError;

    /// Validates a source model row while preserving its original stable index.
    fn try_from(input: ModelSeedInput) -> Result<Self, Self::Error> {
        let ModelSeedInput {
            page_number,
            detection,
            page_bbox,
        } = input;
        if page_number == 0 {
            return Err(SeedError::InvalidPageNumber);
        }
        LayoutLabel::try_from(detection.class_id)
            .map_err(|_source| SeedError::InvalidClass(detection.class_id))?;
        if !detection.confidence.is_finite()
            || !(0.0..=1.0).contains(&detection.confidence)
        {
            return Err(SeedError::InvalidConfidence);
        }
        if !valid_bbox(detection.bbox)
            || !valid_bbox(page_bbox)
            || !page_contains(page_bbox, detection.bbox)
        {
            return Err(SeedError::InvalidGeometry);
        }

        // Prefer factual model polygons but use the canonical polygon APIs for bbox-only rows too.
        let geometry = match detection.polygon.as_ref() {
            Some(polygon) => {
                let polygon_bbox = polygon.bbox();
                if !polygon.area().is_finite()
                    || polygon.area() <= 0.0
                    || !page_contains(page_bbox, polygon_bbox)
                {
                    return Err(SeedError::InvalidGeometry);
                }
                polygon.clone()
            }
            None => rectangle_polygon(detection.bbox)?,
        };
        Ok(Self::builder()
            .page_number(page_number)
            .region_id(ModelRegionId::detected(
                page_number,
                detection.source_detection_index,
            ))
            .source_detection_index(detection.source_detection_index)
            .raw_label(detection.raw_label)
            .class_id(detection.class_id)
            .label(detection.label)
            .confidence(detection.confidence)
            .bbox(detection.bbox)
            .polygon(detection.polygon)
            .geometry_source(detection.geometry_source)
            .model_order(detection.model_order)
            .geometry(geometry)
            .build())
    }
}

impl BlockSeed {
    /// Scores one conservative fragment when it satisfies an inclusive eligibility branch.
    pub(crate) fn score(
        &self,
        fragment: &LineFragment,
        page_bbox: Bbox,
        config: &FusionConfig,
    ) -> Option<AssignmentScore> {
        if !valid_bbox(fragment.bbox) || !valid_bbox(page_bbox) {
            return None;
        }
        let line_area = fragment.bbox.area();
        let page_area = page_bbox.area();
        let region_area = self.geometry.area();
        if !line_area.is_finite()
            || line_area <= 0.0
            || !page_area.is_finite()
            || page_area <= 0.0
            || !region_area.is_finite()
            || region_area <= 0.0
        {
            return None;
        }
        let coverage = (self.geometry.intersection_area(fragment.bbox)
            / line_area)
            .clamp(0.0, 1.0);
        let center_is_inside =
            self.geometry.contains_point(fragment.bbox.center());
        let center_inside = if center_is_inside { 1.0 } else { 0.0 };
        if coverage < config.minimum_line_coverage
            && !(center_is_inside
                && coverage >= config.center_minimum_line_coverage)
        {
            return None;
        }
        let baseline_length =
            segment_length(fragment.baseline.start, fragment.baseline.end);
        let baseline_intersection = if baseline_length > 0.0 {
            (self.geometry.baseline_inside_length(
                fragment.baseline.start,
                fragment.baseline.end,
            ) / baseline_length)
                .clamp(0.0, 1.0)
        } else {
            0.0
        };
        let specificity = 1.0 - (region_area / page_area).clamp(0.0, 1.0);
        let assignment_score = coverage * config.assignment_coverage_weight
            + center_inside * config.assignment_center_weight
            + baseline_intersection * config.assignment_baseline_weight
            + self.confidence * config.assignment_confidence_weight
            + specificity * config.assignment_specificity_weight;
        [
            coverage,
            center_inside,
            baseline_intersection,
            specificity,
            assignment_score,
        ]
        .iter()
        .all(|value| value.is_finite())
        .then(|| {
            AssignmentScore::builder()
                .coverage(coverage)
                .center_inside(center_inside)
                .baseline_intersection(baseline_intersection)
                .model_confidence(self.confidence)
                .specificity(specificity)
                .assignment_score(assignment_score)
                .region_area(region_area)
                .build()
        })
    }
}

/// Final ownership partition from ordinary regions plus non-owning formula rows.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct AssignmentResult {
    pub(crate) model_seeds: Vec<BlockSeed>,
    pub(crate) residual: Vec<LineFragment>,
    pub(crate) inline_formulas: Vec<LayoutDetection>,
    #[builder(default)]
    pub(crate) diagnostics: Vec<String>,
}

/// Deterministic model assignment boundary for one canonical page.
#[derive(Debug, Clone)]
pub(crate) struct AssignmentEngine {
    page_number: u32,
    page_bbox: Bbox,
    config: FusionConfig,
}

impl AssignmentEngine {
    /// Creates an assignment engine from already validated page-local settings.
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

    /// Partitions every text fact into exactly one model seed or the residual set.
    pub(crate) fn assign(
        &self,
        items: Vec<TextItem>,
        detections: Vec<LayoutDetection>,
    ) -> Result<AssignmentResult, LineError> {
        let mut model_seeds = Vec::new();
        let mut inline_formulas = Vec::new();
        let mut diagnostics = BTreeSet::new();
        for detection in detections {
            let source_index = detection.source_detection_index;
            if detection.label == LayoutLabel::InlineFormula {
                match BlockSeed::try_from(ModelSeedInput::new(
                    self.page_number,
                    detection.clone(),
                    self.page_bbox,
                )) {
                    Ok(_) => inline_formulas.push(detection),
                    Err(error) => {
                        diagnostics.insert(format!(
                            "ignored inline formula detection {source_index}: {error}"
                        ));
                    }
                }
                continue;
            }
            match BlockSeed::try_from(ModelSeedInput::new(
                self.page_number,
                detection,
                self.page_bbox,
            )) {
                Ok(seed) => model_seeds.push(seed),
                Err(error) => {
                    diagnostics.insert(format!(
                        "ignored layout detection {source_index}: {error}"
                    ));
                }
            }
        }
        model_seeds.sort_by_key(|seed| seed.source_detection_index);
        inline_formulas
            .sort_by_key(|detection| detection.source_detection_index);

        // Ownership is decided at the smallest retained text-fact boundary. A visual
        // line can straddle an incomplete or inaccurate model box, so assigning the
        // preassembled union would incorrectly pull outside items into that region.
        let mut residual = Vec::with_capacity(items.len());
        for item in items {
            let fragment =
                LineFragment::from_items(vec![item], self.page_bbox.width())?;
            let mut owner = None;
            let mut candidate_count = 0_usize;
            // Select the stable maximum in one pass so each fragment avoids allocating and
            // retaining a complete candidate vector solely to count alternatives.
            for (seed_index, seed) in model_seeds.iter().enumerate() {
                let Some(score) =
                    seed.score(&fragment, self.page_bbox, &self.config)
                else {
                    continue;
                };
                candidate_count = candidate_count.saturating_add(1);
                let replace =
                    owner.as_ref().is_none_or(|(owner_index, owner_score)| {
                        compare_candidates(
                            seed_index,
                            &score,
                            *owner_index,
                            owner_score,
                            &model_seeds,
                        )
                        // `Iterator::max_by` selected the last exactly-equal candidate.
                        .is_ge()
                    });
                if replace {
                    owner = Some((seed_index, score));
                }
            }
            let Some((owner_index, owner_score)) = owner else {
                residual.push(fragment);
                continue;
            };
            let evidence = AssignEvidence::builder()
                .selected(owner_score)
                .alternative_count(candidate_count.saturating_sub(1))
                .build();
            // Moving the fragment only after selecting one owner enforces unique ownership.
            if let Some(owner) = model_seeds.get_mut(owner_index) {
                owner.fragments.push(fragment);
                owner.assignment_evidence.push(evidence);
            } else {
                // Candidate indices originate from this vector, but preserve text if invariants drift.
                residual.push(fragment);
            }
        }
        for seed in &mut model_seeds {
            seed.fragments.sort_by(fragment_order);
        }

        Ok(AssignmentResult::builder()
            .model_seeds(model_seeds)
            .residual(residual)
            .inline_formulas(inline_formulas)
            .diagnostics(diagnostics.into_iter().collect())
            .build())
    }
}

/// Compares candidate tuples according to the schema-stable assignment policy.
fn compare_candidates(
    left_index: usize,
    left: &AssignmentScore,
    right_index: usize,
    right: &AssignmentScore,
    seeds: &[BlockSeed],
) -> Ordering {
    let (Some(left_seed), Some(right_seed)) =
        (seeds.get(left_index), seeds.get(right_index))
    else {
        return right_index.cmp(&left_index);
    };
    left.assignment_score
        .total_cmp(&right.assignment_score)
        .then_with(|| left.coverage.total_cmp(&right.coverage))
        .then_with(|| left_seed.confidence.total_cmp(&right_seed.confidence))
        .then_with(|| right.region_area.total_cmp(&left.region_area))
        .then_with(|| right_seed.model_order.cmp(&left_seed.model_order))
        .then_with(|| {
            right_seed
                .source_detection_index
                .cmp(&left_seed.source_detection_index)
        })
}

/// Sorts fragments by canonical geometry and their first stable item identity.
fn fragment_order(left: &LineFragment, right: &LineFragment) -> Ordering {
    left.bbox
        .top
        .total_cmp(&right.bbox.top)
        .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
        .then_with(|| {
            left.items
                .first()
                .map(|item| item.id.as_str())
                .cmp(&right.items.first().map(|item| item.id.as_str()))
        })
}

/// Converts one valid axis-aligned box into a polygon for unified scoring.
fn rectangle_polygon(
    bbox: Bbox,
) -> Result<Polygon, docparse_layout::GeometryError> {
    Polygon::try_from(vec![
        Point::new(bbox.left, bbox.top),
        Point::new(bbox.right, bbox.top),
        Point::new(bbox.right, bbox.bottom),
        Point::new(bbox.left, bbox.bottom),
    ])
}

/// Returns whether a public box still satisfies finite positive-area invariants.
fn valid_bbox(bbox: Bbox) -> bool {
    [bbox.left, bbox.top, bbox.right, bbox.bottom]
        .iter()
        .all(|value| value.is_finite())
        && bbox.right > bbox.left
        && bbox.bottom > bbox.top
}

/// Returns whether one finite inner box stays within its canonical page bounds.
fn page_contains(page: Bbox, inner: Bbox) -> bool {
    inner.left >= page.left
        && inner.top >= page.top
        && inner.right <= page.right
        && inner.bottom <= page.bottom
}

/// Measures one finite line segment without introducing a geometry fallback.
fn segment_length(start: Point, end: Point) -> f64 {
    let delta_x = end.x - start.x;
    let delta_y = end.y - start.y;
    let length = (delta_x * delta_x + delta_y * delta_y).sqrt();
    if length.is_finite() { length } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use docparse_config::FusionConfig;
    use docparse_layout::{
        Bbox, GeometrySource, LayoutDetection, LayoutLabel, Point, Polygon,
    };

    use super::{AssignmentEngine, BlockSeed, ModelSeedInput};
    use crate::line::metrics::LineMetrics;
    use crate::line::{LineAnchor, LineFragment};
    use crate::{Baseline, TextItem, TextItemId, TextSource, WritingDirection};

    /// Builds one validated box used throughout assignment tests.
    fn bbox(value: [f64; 4]) -> Bbox {
        Bbox::try_from(value).expect("test bbox must be valid")
    }

    /// Builds one rectangular polygon to exercise the canonical polygon APIs.
    fn polygon(value: [f64; 4]) -> Polygon {
        let box_value = bbox(value);
        Polygon::try_from(vec![
            Point::new(box_value.left, box_value.top),
            Point::new(box_value.right, box_value.top),
            Point::new(box_value.right, box_value.bottom),
            Point::new(box_value.left, box_value.bottom),
        ])
        .expect("test polygon must be valid")
    }

    /// Builds one deterministic layout detection with caller-selected ranking fields.
    fn detection(
        source_detection_index: u32,
        bounds: [f64; 4],
        confidence: f64,
        model_order: i64,
    ) -> LayoutDetection {
        LayoutDetection::builder()
            .source_detection_index(source_detection_index)
            .raw_label("text".to_owned())
            .class_id(22)
            .label(LayoutLabel::Text)
            .confidence(confidence)
            .bbox(bbox(bounds))
            .polygon(Some(polygon(bounds)))
            .geometry_source(GeometrySource::ModelPolygon)
            .model_order(model_order)
            .metadata(BTreeMap::new())
            .build()
    }

    /// Builds one line fragment owning exactly one stable native text item.
    fn fragment(index: u32, bounds: [f64; 4]) -> LineFragment {
        let bounds = bbox(bounds);
        let baseline = Baseline {
            start: Point::new(bounds.left, bounds.bottom),
            end: Point::new(bounds.right, bounds.bottom),
        };
        let item = TextItem::builder()
            .id(TextItemId::native(1, index))
            .raw_text(format!("item-{index}"))
            .bbox(bounds)
            .source(TextSource::Native)
            .build();
        let metrics = LineMetrics::builder()
            .font_size(bounds.height())
            .bold_ratio(0.0)
            .italic_ratio(0.0)
            .bbox(bounds)
            .baseline(baseline)
            .anchor(LineAnchor::Left)
            .indent(bounds.left)
            .build();
        LineFragment::builder()
            .items(vec![item])
            .bbox(bounds)
            .baseline(baseline)
            .direction(WritingDirection::LeftToRight)
            .metrics(metrics)
            .rotation(0.0)
            .build()
    }

    /// Verifies both eligibility branches include exact configured thresholds.
    #[test]
    fn eligibility_uses_inclusive_thresholds() {
        let page = bbox([0.0, 0.0, 100.0, 100.0]);
        let line = fragment(0, [0.0, 0.0, 100.0, 10.0]);
        let regular = BlockSeed::try_from(ModelSeedInput::new(
            1,
            detection(0, [0.0, 0.0, 30.0, 10.0], 0.5, 0),
            page,
        ))
        .expect("valid detection must become a seed");
        let centered = BlockSeed::try_from(ModelSeedInput::new(
            1,
            detection(1, [45.0, 0.0, 55.0, 10.0], 0.5, 1),
            page,
        ))
        .expect("valid detection must become a seed");

        let config = FusionConfig::default();
        assert!(regular.score(&line, page, &config).is_some());
        assert!(centered.score(&line, page, &config).is_some());
    }

    /// Verifies the weighted score uses exact polygon-derived components.
    #[test]
    fn score_matches_documented_formula() {
        let page = bbox([0.0, 0.0, 100.0, 100.0]);
        let line = fragment(0, [0.0, 0.0, 100.0, 10.0]);
        let seed = BlockSeed::try_from(ModelSeedInput::new(
            1,
            detection(0, [0.0, 0.0, 50.0, 10.0], 0.8, 0),
            page,
        ))
        .expect("valid detection must become a seed");

        let score = seed
            .score(&line, page, &FusionConfig::default())
            .expect("half-covered line must be eligible");

        assert!((score.coverage - 0.5).abs() <= f64::EPSILON);
        assert!((score.center_inside - 1.0).abs() <= f64::EPSILON);
        assert!((score.baseline_intersection - 0.5).abs() <= f64::EPSILON);
        assert!((score.specificity - 0.95).abs() <= f64::EPSILON);
        let expected =
            0.5 * 0.55 + 1.0 * 0.20 + 0.5 * 0.10 + 0.8 * 0.10 + 0.95 * 0.05;
        assert!((score.assignment_score - expected).abs() <= f64::EPSILON);
    }

    /// Verifies invalid geometry, confidence, class, and page bounds are rejected.
    #[test]
    fn invalid_detections_do_not_create_seeds() {
        let page = bbox([0.0, 0.0, 100.0, 100.0]);
        let mut invalid_class = detection(0, [0.0, 0.0, 10.0, 10.0], 0.5, 0);
        invalid_class.class_id = 25;
        let mut invalid_confidence =
            detection(1, [0.0, 0.0, 10.0, 10.0], 0.5, 0);
        invalid_confidence.confidence = f64::NAN;
        let outside = detection(2, [110.0, 0.0, 120.0, 10.0], 0.5, 0);

        let _invalid_class =
            BlockSeed::try_from(ModelSeedInput::new(1, invalid_class, page))
                .expect_err("invalid class must be rejected");
        let _invalid_confidence = BlockSeed::try_from(ModelSeedInput::new(
            1,
            invalid_confidence,
            page,
        ))
        .expect_err("invalid confidence must be rejected");
        let _outside =
            BlockSeed::try_from(ModelSeedInput::new(1, outside, page))
                .expect_err("outside geometry must be rejected");
    }

    /// Verifies ranking falls through all stable tie-break fields.
    #[test]
    fn assignment_tie_break_prefers_smaller_model_order() {
        let page = bbox([0.0, 0.0, 100.0, 100.0]);
        let line = fragment(0, [10.0, 10.0, 90.0, 20.0]);
        let later = detection(9, [0.0, 0.0, 100.0, 30.0], 0.8, 9);
        let earlier = detection(8, [0.0, 0.0, 100.0, 30.0], 0.8, 2);

        let result = AssignmentEngine::new(1, page, FusionConfig::default())
            .assign(line.items, vec![later, earlier])
            .expect("assignment must succeed");

        assert_eq!(result.model_seeds.len(), 2);
        let owner = result
            .model_seeds
            .iter()
            .find(|seed| !seed.fragments.is_empty())
            .expect("one seed must own the line");
        assert_eq!(owner.source_detection_index, 8);
    }

    /// Verifies assignment evidence retains only the number of non-owning candidates.
    #[test]
    fn assignment_records_alternative_count() {
        let page = bbox([0.0, 0.0, 100.0, 100.0]);
        let line = fragment(0, [10.0, 10.0, 90.0, 20.0]);
        let detections = vec![
            detection(0, [0.0, 0.0, 100.0, 30.0], 0.9, 0),
            detection(1, [0.0, 0.0, 100.0, 30.0], 0.8, 1),
            detection(2, [0.0, 0.0, 100.0, 30.0], 0.7, 2),
        ];

        let result = AssignmentEngine::new(1, page, FusionConfig::default())
            .assign(line.items, detections)
            .expect("assignment must succeed");
        let owner = result
            .model_seeds
            .iter()
            .find(|seed| !seed.assignment_evidence.is_empty())
            .expect("one seed must retain assignment evidence");

        assert_eq!(
            owner
                .assignment_evidence
                .first()
                .expect("one assignment evidence entry must exist")
                .alternative_count,
            2
        );
    }

    /// Verifies inline formulas never compete for primary ownership.
    #[test]
    fn inline_formula_is_non_owning() {
        let page = bbox([0.0, 0.0, 100.0, 100.0]);
        let line = fragment(0, [10.0, 10.0, 90.0, 20.0]);
        let mut formula = detection(0, [10.0, 10.0, 90.0, 20.0], 0.99, 0);
        formula.raw_label = "inline_formula".to_owned();
        formula.class_id = 15;
        formula.label = LayoutLabel::InlineFormula;

        let result = AssignmentEngine::new(1, page, FusionConfig::default())
            .assign(line.items, vec![formula])
            .expect("assignment must succeed");

        assert_eq!(result.inline_formulas.len(), 1);
        assert_eq!(result.residual.len(), 1);
        assert!(result.model_seeds.is_empty());
    }

    /// Verifies every input text identity remains in exactly one owner or residual list.
    #[test]
    fn assignment_preserves_text_identity_multiset() {
        let page = bbox([0.0, 0.0, 100.0, 100.0]);
        let fragments = vec![
            fragment(0, [5.0, 5.0, 40.0, 15.0]),
            fragment(1, [60.0, 5.0, 95.0, 15.0]),
            fragment(2, [5.0, 50.0, 40.0, 60.0]),
        ];
        let items = fragments
            .into_iter()
            .flat_map(|fragment| fragment.items)
            .collect();
        let result = AssignmentEngine::new(1, page, FusionConfig::default())
            .assign(items, vec![detection(0, [0.0, 0.0, 50.0, 20.0], 0.9, 0)])
            .expect("assignment must succeed");
        let mut ids: Vec<_> = result
            .model_seeds
            .iter()
            .flat_map(|seed| seed.fragments.iter())
            .chain(result.residual.iter())
            .flat_map(|fragment| fragment.items.iter())
            .map(|item| item.id.as_str())
            .collect();
        ids.sort_unstable();

        assert_eq!(ids, vec!["p1:t0", "p1:t1", "p1:t2"]);
    }

    /// Verifies a partially covered line cannot pull an outside text fact into a model region.
    #[test]
    fn assignment_keeps_outside_items_in_residual() {
        let page = bbox([0.0, 0.0, 100.0, 100.0]);
        let mut mixed = fragment(0, [10.0, 10.0, 40.0, 20.0]);
        let outside = fragment(1, [60.0, 10.0, 90.0, 20.0]);
        mixed.items.extend(outside.items);
        let result = AssignmentEngine::new(1, page, FusionConfig::default())
            .assign(
                mixed.items,
                vec![detection(0, [5.0, 5.0, 50.0, 30.0], 0.9, 0)],
            )
            .expect("assignment must succeed");
        let model_ids = result
            .model_seeds
            .first()
            .expect("one model seed must exist")
            .fragments
            .iter()
            .flat_map(|fragment| &fragment.items)
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();
        let residual_ids = result
            .residual
            .iter()
            .flat_map(|fragment| &fragment.items)
            .map(|item| item.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(model_ids, vec!["p1:t0"]);
        assert_eq!(residual_ids, vec!["p1:t1"]);
    }
}
