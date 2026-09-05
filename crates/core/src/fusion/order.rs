use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap};

use docparse_layout::{Bbox, LayoutLabel};
use typed_builder::TypedBuilder;

use crate::{Block, BlockId, LabelSource, OrderError};

/// Provenance category for one page-local ordering constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum EdgeSource {
    Model,
    FallbackInsertion,
    BandHorizontal,
    CaptionRelation,
    XyCut,
    IntraRegion,
    StrongVertical,
}

impl EdgeSource {
    /// Returns the fixed removal priority used after edge weight ties.
    pub(crate) const fn rank(self) -> u8 {
        match self {
            Self::Model => 0,
            Self::FallbackInsertion => 1,
            Self::BandHorizontal => 2,
            Self::CaptionRelation => 3,
            Self::XyCut => 4,
            Self::IntraRegion => 5,
            Self::StrongVertical => 6,
        }
    }

    /// Returns whether deleting this edge would violate a structural invariant.
    const fn is_strong(self) -> bool {
        matches!(self, Self::StrongVertical | Self::IntraRegion)
    }
}

/// Schema-stable preservation weights for all supported ordering evidence.
pub(crate) struct OrderPolicy;

impl OrderPolicy {
    /// Returns the fixed preservation weight for one source and optional confidence.
    pub(crate) fn weight(source: EdgeSource, confidence: Option<f64>) -> f64 {
        match source {
            EdgeSource::StrongVertical | EdgeSource::IntraRegion => 1.0,
            EdgeSource::Model => confidence.unwrap_or(0.0).clamp(0.0, 1.0),
            EdgeSource::XyCut => 0.85,
            EdgeSource::CaptionRelation => 0.80,
            EdgeSource::BandHorizontal => 0.65,
            EdgeSource::FallbackInsertion => 0.60,
        }
    }
}

/// Stable graph node metadata used by deterministic topological tie-breaking.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct OrderNode {
    pub(crate) block_id: BlockId,
    pub(crate) bbox: Bbox,
    #[builder(default)]
    pub(crate) xy_path: Option<String>,
    pub(crate) source_priority: u8,
}

impl OrderNode {
    /// Compares nodes by the schema-stable ready-queue policy.
    fn stable_cmp(&self, right: &Self) -> Ordering {
        self.xy_path
            .as_deref()
            .unwrap_or("")
            .cmp(right.xy_path.as_deref().unwrap_or(""))
            .then_with(|| {
                quantized_band(self.bbox.top)
                    .cmp(&quantized_band(right.bbox.top))
            })
            .then_with(|| self.bbox.top.total_cmp(&right.bbox.top))
            .then_with(|| self.bbox.left.total_cmp(&right.bbox.left))
            .then_with(|| self.source_priority.cmp(&right.source_priority))
            .then_with(|| self.block_id.cmp(&right.block_id))
    }
}

/// One directed ordering constraint with complete removal evidence.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct OrderEdge {
    pub(crate) from: BlockId,
    pub(crate) to: BlockId,
    pub(crate) source: EdgeSource,
    pub(crate) reason: String,
    #[builder(default)]
    pub(crate) source_confidence: Option<f64>,
    pub(crate) preservation_weight: f64,
    pub(crate) stable_key: String,
}

/// Stable diagnostic emitted whenever a weak cycle edge is removed.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct RemovedOrderEdge {
    pub(crate) from: BlockId,
    pub(crate) to: BlockId,
    pub(crate) source: EdgeSource,
    pub(crate) preservation_weight: f64,
    pub(crate) reason: String,
}

impl From<OrderEdge> for RemovedOrderEdge {
    /// Retains user-relevant edge evidence while dropping internal graph keys.
    fn from(edge: OrderEdge) -> Self {
        Self::builder()
            .from(edge.from)
            .to(edge.to)
            .source(edge.source)
            .preservation_weight(edge.preservation_weight)
            .reason(edge.reason)
            .build()
    }
}

/// Stable node order and all weak constraints removed to obtain it.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct OrderResolution {
    pub(crate) ordered_ids: Vec<BlockId>,
    #[builder(default)]
    pub(crate) removed_edges: Vec<RemovedOrderEdge>,
}

/// Page-local ordering graph keyed only by canonical block identities.
#[derive(Debug, Clone, Default)]
pub(crate) struct OrderGraph {
    nodes: BTreeMap<BlockId, OrderNode>,
    edges: Vec<OrderEdge>,
}

/// Integer-indexed edge storage built once and reused through cycle removal and sorting.
struct IndexedEdges {
    endpoints: Vec<(usize, usize)>,
    forward: Vec<Vec<usize>>,
    reverse: Vec<Vec<usize>>,
}

/// Borrowed ready node with reversed ordering for Rust's max-oriented binary heap.
#[derive(Debug, Clone, Copy)]
struct ReadyNode<'a> {
    index: usize,
    node: &'a OrderNode,
}

impl PartialEq for ReadyNode<'_> {
    /// Treats one stable integer node index as heap identity.
    fn eq(&self, other: &Self) -> bool {
        self.index == other.index
    }
}

impl Eq for ReadyNode<'_> {}

impl PartialOrd for ReadyNode<'_> {
    /// Delegates partial ordering to the total deterministic heap ordering.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ReadyNode<'_> {
    /// Reverses the stable policy so the smallest ready node is popped first.
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .node
            .stable_cmp(self.node)
            .then_with(|| other.index.cmp(&self.index))
    }
}

impl OrderGraph {
    /// Registers or deterministically replaces one node with the same stable identity.
    pub(crate) fn insert_node(&mut self, node: OrderNode) {
        self.nodes.insert(node.block_id.clone(), node);
    }

    /// Registers one ordering edge; duplicates are normalized before resolution.
    pub(crate) fn insert_edge(&mut self, edge: OrderEdge) {
        self.edges.push(edge);
    }

    /// Builds all first-version ordering constraints from final unsorted blocks.
    pub(crate) fn from_blocks(blocks: &[Block]) -> Self {
        let mut graph = Self::default();
        for block in blocks {
            let xy_path = block
                .source_region
                .as_ref()
                .and_then(|source| source.fallback_region_id.as_ref())
                .and_then(|id| {
                    id.as_str()
                        .split_once(":f")
                        .map(|(_, path)| path.to_owned())
                });
            let source_priority = match block.label_source {
                LabelSource::Model => 0,
                LabelSource::Fallback => 1,
                LabelSource::Heuristic => 2,
            };
            graph.insert_node(
                OrderNode::builder()
                    .block_id(block.id.clone())
                    .bbox(block.bbox)
                    .xy_path(xy_path)
                    .source_priority(source_priority)
                    .build(),
            );
        }
        graph.add_model_edges(blocks);
        graph.add_rotated_marginal_edges(blocks);
        graph.add_xy_cut_edges(blocks);
        graph.add_geometry_edges(blocks);
        graph.add_caption_edges(blocks);
        graph.add_fallback_insertion_edges(blocks);
        graph
    }

    /// Resolves cycles by deleting the weakest removable SCC edge, then sorts stably.
    pub(crate) fn resolve(mut self) -> Result<OrderResolution, OrderError> {
        self.normalize_edges()?;
        let indexed = self.index_edges()?;
        let mut active_edges = vec![true; self.edges.len()];
        let mut removed_edges = Vec::new();
        loop {
            let components = Self::cyclic_components(&indexed, &active_edges);
            if components.is_empty() {
                break;
            }
            let mut memberships = vec![None; self.nodes.len()];
            for (component_index, component) in components.iter().enumerate() {
                for node in component {
                    if let Some(membership) = memberships.get_mut(*node) {
                        *membership = Some(component_index);
                    }
                }
            }
            // Scan active edges once and retain one weakest removable candidate per SCC.
            let mut selected = vec![None; components.len()];
            for (edge_index, edge) in self.edges.iter().enumerate() {
                if !active_edges.get(edge_index).copied().unwrap_or(false)
                    || edge.source.is_strong()
                {
                    continue;
                }
                let Some(&(from, to)) = indexed.endpoints.get(edge_index)
                else {
                    return Err(OrderError::InternalOrderConflict);
                };
                let Some(component_index) =
                    memberships.get(from).copied().flatten()
                else {
                    continue;
                };
                if memberships.get(to).copied().flatten()
                    != Some(component_index)
                {
                    continue;
                }
                let replace = selected
                    .get(component_index)
                    .copied()
                    .flatten()
                    .and_then(|selected_index| self.edges.get(selected_index))
                    .is_none_or(|current| {
                        Self::compare_removal_candidates(edge, current).is_lt()
                    });
                if replace
                    && let Some(selection) = selected.get_mut(component_index)
                {
                    *selection = Some(edge_index);
                }
            }
            // Independent SCCs each lose one edge in stable component order, matching prior policy.
            for edge_index in selected {
                let edge_index =
                    edge_index.ok_or(OrderError::InternalOrderConflict)?;
                let active = active_edges
                    .get_mut(edge_index)
                    .ok_or(OrderError::InternalOrderConflict)?;
                *active = false;
                let edge = self
                    .edges
                    .get(edge_index)
                    .cloned()
                    .ok_or(OrderError::InternalOrderConflict)?;
                removed_edges.push(RemovedOrderEdge::from(edge));
            }
        }
        let ordered_ids = self.topological_order(&indexed, &active_edges)?;
        Ok(OrderResolution::builder()
            .ordered_ids(ordered_ids)
            .removed_edges(removed_edges)
            .build())
    }

    /// Validates endpoints and removes exact duplicate constraints deterministically.
    fn normalize_edges(&mut self) -> Result<(), OrderError> {
        for edge in &self.edges {
            for endpoint in [&edge.from, &edge.to] {
                if !self.nodes.contains_key(endpoint) {
                    return Err(OrderError::UnknownBlock {
                        block_id: endpoint.as_str().to_owned(),
                    });
                }
            }
        }
        self.edges
            .sort_by(|left, right| left.stable_key.cmp(&right.stable_key));
        self.edges
            .dedup_by(|left, right| left.stable_key == right.stable_key);
        Ok(())
    }

    /// Converts stable block identities into compact reusable integer adjacency.
    fn index_edges(&self) -> Result<IndexedEdges, OrderError> {
        let node_indices: BTreeMap<_, _> = self
            .nodes
            .keys()
            .enumerate()
            .map(|(index, block_id)| (block_id, index))
            .collect();
        let mut endpoints = Vec::with_capacity(self.edges.len());
        let mut forward = vec![Vec::new(); self.nodes.len()];
        let mut reverse = vec![Vec::new(); self.nodes.len()];
        for (edge_index, edge) in self.edges.iter().enumerate() {
            let from = node_indices
                .get(&edge.from)
                .copied()
                .ok_or(OrderError::InternalOrderConflict)?;
            let to = node_indices
                .get(&edge.to)
                .copied()
                .ok_or(OrderError::InternalOrderConflict)?;
            endpoints.push((from, to));
            forward
                .get_mut(from)
                .ok_or(OrderError::InternalOrderConflict)?
                .push(edge_index);
            reverse
                .get_mut(to)
                .ok_or(OrderError::InternalOrderConflict)?
                .push(edge_index);
        }
        Ok(IndexedEdges {
            endpoints,
            forward,
            reverse,
        })
    }

    /// Returns cyclic integer SCCs with stable component order over active edges.
    fn cyclic_components(
        indexed: &IndexedEdges,
        active: &[bool],
    ) -> Vec<Vec<usize>> {
        let mut visited = vec![false; indexed.forward.len()];
        let mut finished = Vec::with_capacity(indexed.forward.len());
        for node in 0..indexed.forward.len() {
            Self::finish_depth_first(
                node,
                indexed,
                active,
                &mut visited,
                &mut finished,
            );
        }
        visited.fill(false);
        let mut components = Vec::new();
        while let Some(node) = finished.pop() {
            if visited.get(node).copied().unwrap_or(true) {
                continue;
            }
            let mut component = Vec::new();
            Self::collect_depth_first(
                node,
                indexed,
                active,
                &mut visited,
                &mut component,
            );
            component.sort_unstable();
            let self_loop = component.len() == 1
                && indexed.forward.get(node).is_some_and(|edges| {
                    edges.iter().any(|edge_index| {
                        active.get(*edge_index).copied().unwrap_or(false)
                            && indexed.endpoints.get(*edge_index).is_some_and(
                                |&(from, to)| from == node && to == node,
                            )
                    })
                });
            if component.len() > 1 || self_loop {
                components.push(component);
            }
        }
        components.sort_by_key(|component| component.first().copied());
        components
    }

    /// Appends integer nodes in depth-first finish order for the first Kosaraju pass.
    fn finish_depth_first(
        node: usize,
        indexed: &IndexedEdges,
        active: &[bool],
        visited: &mut [bool],
        finished: &mut Vec<usize>,
    ) {
        let Some(seen) = visited.get_mut(node) else {
            return;
        };
        if *seen {
            return;
        }
        *seen = true;
        if let Some(edges) = indexed.forward.get(node) {
            for edge_index in edges {
                if !active.get(*edge_index).copied().unwrap_or(false) {
                    continue;
                }
                if let Some(&(_, target)) = indexed.endpoints.get(*edge_index) {
                    Self::finish_depth_first(
                        target, indexed, active, visited, finished,
                    );
                }
            }
        }
        finished.push(node);
    }

    /// Collects one integer component during the reversed Kosaraju pass.
    fn collect_depth_first(
        node: usize,
        indexed: &IndexedEdges,
        active: &[bool],
        visited: &mut [bool],
        component: &mut Vec<usize>,
    ) {
        let Some(seen) = visited.get_mut(node) else {
            return;
        };
        if *seen {
            return;
        }
        *seen = true;
        component.push(node);
        if let Some(edges) = indexed.reverse.get(node) {
            for edge_index in edges {
                if !active.get(*edge_index).copied().unwrap_or(false) {
                    continue;
                }
                if let Some(&(source, _)) = indexed.endpoints.get(*edge_index) {
                    Self::collect_depth_first(
                        source, indexed, active, visited, component,
                    );
                }
            }
        }
    }

    /// Performs deterministic Kahn sorting with indexed outgoing edges and a ready heap.
    fn topological_order(
        &self,
        indexed: &IndexedEdges,
        active: &[bool],
    ) -> Result<Vec<BlockId>, OrderError> {
        let nodes: Vec<_> = self.nodes.values().collect();
        let mut indegrees = vec![0_usize; nodes.len()];
        for (edge_index, &(_, to)) in indexed.endpoints.iter().enumerate() {
            if active.get(edge_index).copied().unwrap_or(false)
                && let Some(indegree) = indegrees.get_mut(to)
            {
                *indegree = indegree.saturating_add(1);
            }
        }
        let mut ready = BinaryHeap::new();
        for (index, node) in nodes.iter().enumerate() {
            if indegrees.get(index).copied() == Some(0) {
                ready.push(ReadyNode { index, node });
            }
        }
        let mut ordered = Vec::with_capacity(nodes.len());
        while let Some(next) = ready.pop() {
            ordered.push(next.node.block_id.clone());
            let Some(edges) = indexed.forward.get(next.index) else {
                return Err(OrderError::InternalOrderConflict);
            };
            for edge_index in edges {
                if !active.get(*edge_index).copied().unwrap_or(false) {
                    continue;
                }
                let Some(&(_, target)) = indexed.endpoints.get(*edge_index)
                else {
                    return Err(OrderError::InternalOrderConflict);
                };
                let Some(indegree) = indegrees.get_mut(target) else {
                    return Err(OrderError::InternalOrderConflict);
                };
                if *indegree > 0 {
                    *indegree -= 1;
                    if *indegree == 0 {
                        let node = nodes
                            .get(target)
                            .copied()
                            .ok_or(OrderError::InternalOrderConflict)?;
                        ready.push(ReadyNode {
                            index: target,
                            node,
                        });
                    }
                }
            }
        }
        if ordered.len() != nodes.len() {
            return Err(OrderError::InternalOrderConflict);
        }
        Ok(ordered)
    }

    /// Compares two removable edges by the existing stable deletion policy.
    fn compare_removal_candidates(
        left: &OrderEdge,
        right: &OrderEdge,
    ) -> Ordering {
        left.preservation_weight
            .total_cmp(&right.preservation_weight)
            .then_with(|| left.source.rank().cmp(&right.source.rank()))
            .then_with(|| left.stable_key.cmp(&right.stable_key))
    }

    /// Adds adjacent model-region and intra-region constraints only.
    fn add_model_edges(&mut self, blocks: &[Block]) {
        let mut groups: BTreeMap<String, Vec<&Block>> = BTreeMap::new();
        for block in blocks {
            if let Some(region_id) = &block.model_region_id {
                groups
                    .entry(region_id.as_str().to_owned())
                    .or_default()
                    .push(block);
            }
        }
        let mut regions = Vec::new();
        for (region_id, mut children) in groups {
            children.sort_by(|left, right| {
                left.bbox
                    .top
                    .total_cmp(&right.bbox.top)
                    .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
                    .then_with(|| left.id.cmp(&right.id))
            });
            for pair in children.windows(2) {
                if let [left, right] = pair {
                    self.push_edge(
                        &left.id,
                        &right.id,
                        EdgeSource::IntraRegion,
                        "children of one model region",
                        Some(
                            left.confidence
                                .unwrap_or(0.0)
                                .min(right.confidence.unwrap_or(0.0)),
                        ),
                    );
                }
            }
            let is_marginal = children
                .iter()
                .all(|block| Self::is_rotated_marginal(block));
            if !is_marginal
                && let (Some(first), Some(last)) =
                    (children.first(), children.last())
            {
                regions.push((
                    first.model_order.unwrap_or(i64::MAX),
                    model_region_index(&region_id),
                    first.id.clone(),
                    last.id.clone(),
                    children
                        .iter()
                        .filter_map(|block| block.confidence)
                        .fold(1.0_f64, f64::min),
                ));
            }
        }
        regions.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.cmp(&right.1))
                .then_with(|| left.2.cmp(&right.2))
        });
        for pair in regions.windows(2) {
            if let [left, right] = pair {
                self.push_edge(
                    &left.3,
                    &right.2,
                    EdgeSource::Model,
                    "adjacent model region order",
                    Some(left.4.min(right.4)),
                );
            }
        }
    }

    /// Places narrow rotated margin content after the connected main page flow.
    fn add_rotated_marginal_edges(&mut self, blocks: &[Block]) {
        let mut marginals: Vec<_> = blocks
            .iter()
            .filter(|block| Self::is_rotated_marginal(block))
            .collect();
        if marginals.is_empty() {
            return;
        }
        marginals.sort_by(|left, right| {
            left.bbox
                .top
                .total_cmp(&right.bbox.top)
                .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
                .then_with(|| left.id.cmp(&right.id))
        });
        let last_main = blocks
            .iter()
            .filter(|block| !Self::is_rotated_marginal(block))
            .max_by(|left, right| {
                left.bbox
                    .bottom
                    .total_cmp(&right.bbox.bottom)
                    .then_with(|| left.bbox.right.total_cmp(&right.bbox.right))
                    .then_with(|| right.id.cmp(&left.id))
            });
        if let (Some(last_main), Some(first_margin)) =
            (last_main, marginals.first())
        {
            self.push_edge(
                &last_main.id,
                &first_margin.id,
                EdgeSource::StrongVertical,
                "main flow before rotated marginal content",
                None,
            );
        }
        for pair in marginals.windows(2) {
            if let [left, right] = pair {
                self.push_edge(
                    &left.id,
                    &right.id,
                    EdgeSource::IntraRegion,
                    "rotated marginal content order",
                    None,
                );
            }
        }
    }

    /// Detects narrow vertical or aside regions that should not lead main model order.
    fn is_rotated_marginal(block: &Block) -> bool {
        let vertical_line = block.lines.iter().any(|line| {
            line.direction == crate::WritingDirection::Vertical
                || (line.rotation.abs() - 90.0).abs() <= 2.0
                || (line.rotation.abs() - 270.0).abs() <= 2.0
        });
        (vertical_line || block.label == LayoutLabel::AsideText)
            && block.bbox.height() >= block.bbox.width() * 3.0
    }

    /// Adds stable pre-order constraints between adjacent fallback blocks.
    fn add_xy_cut_edges(&mut self, blocks: &[Block]) {
        let mut fallback: Vec<_> = blocks
            .iter()
            .filter_map(|block| {
                block
                    .source_region
                    .as_ref()
                    .and_then(|source| source.fallback_region_id.as_ref())
                    .map(|id| (id.as_str().to_owned(), block))
            })
            .collect();
        fallback.sort_by(|left, right| {
            left.0
                .cmp(&right.0)
                .then_with(|| left.1.id.cmp(&right.1.id))
        });
        for pair in fallback.windows(2) {
            if let [left, right] = pair {
                self.push_edge(
                    &left.1.id,
                    &right.1.id,
                    EdgeSource::XyCut,
                    "adjacent residual XY-cut leaves",
                    None,
                );
            }
        }
    }

    /// Adds only nearest unambiguous neighbors to keep geometry edges linear in nodes.
    fn add_geometry_edges(&mut self, blocks: &[Block]) {
        for source in blocks {
            let below = blocks
                .iter()
                .filter(|candidate| {
                    candidate.id != source.id
                        && source.bbox.bottom <= candidate.bbox.top
                        && overlap(
                            source.bbox.left,
                            source.bbox.right,
                            candidate.bbox.left,
                            candidate.bbox.right,
                        ) > 0.0
                })
                .min_by(|left, right| {
                    left.bbox
                        .top
                        .total_cmp(&right.bbox.top)
                        .then_with(|| {
                            left.bbox.left.total_cmp(&right.bbox.left)
                        })
                        .then_with(|| left.id.cmp(&right.id))
                });
            if let Some(below) = below {
                self.push_edge(
                    &source.id,
                    &below.id,
                    EdgeSource::StrongVertical,
                    "nearest non-overlapping vertical geometry",
                    None,
                );
            }

            let right = blocks
                .iter()
                .filter(|candidate| {
                    if candidate.id == source.id
                        || source.bbox.right > candidate.bbox.left
                    {
                        return false;
                    }
                    let vertical_overlap = overlap(
                        source.bbox.top,
                        source.bbox.bottom,
                        candidate.bbox.top,
                        candidate.bbox.bottom,
                    );
                    vertical_overlap
                        / source
                            .bbox
                            .height()
                            .min(candidate.bbox.height())
                            .max(f64::EPSILON)
                        >= 0.5
                })
                .min_by(|left, right| {
                    (left.bbox.left - source.bbox.right)
                        .total_cmp(&(right.bbox.left - source.bbox.right))
                        .then_with(|| left.bbox.top.total_cmp(&right.bbox.top))
                        .then_with(|| left.id.cmp(&right.id))
                });
            if let Some(right) = right {
                self.push_edge(
                    &source.id,
                    &right.id,
                    EdgeSource::BandHorizontal,
                    "nearest same-band left-to-right geometry",
                    None,
                );
            }
        }
    }

    /// Adds a local caption constraint to the nearest visual object when available.
    fn add_caption_edges(&mut self, blocks: &[Block]) {
        let objects: Vec<_> = blocks
            .iter()
            .filter(|block| {
                matches!(
                    block.label,
                    LayoutLabel::Chart
                        | LayoutLabel::Image
                        | LayoutLabel::Table
                )
            })
            .collect();
        for caption in blocks
            .iter()
            .filter(|block| block.label == LayoutLabel::FigureTitle)
        {
            let nearest = objects.iter().min_by(|left, right| {
                vertical_distance(caption.bbox, left.bbox)
                    .total_cmp(&vertical_distance(caption.bbox, right.bbox))
                    .then_with(|| left.id.cmp(&right.id))
            });
            if let Some(object) = nearest {
                if caption.bbox.top >= object.bbox.bottom {
                    self.push_edge(
                        &object.id,
                        &caption.id,
                        EdgeSource::CaptionRelation,
                        "visual object before lower caption",
                        caption.confidence,
                    );
                } else {
                    self.push_edge(
                        &caption.id,
                        &object.id,
                        EdgeSource::CaptionRelation,
                        "upper caption before visual object",
                        caption.confidence,
                    );
                }
            }
        }
    }

    /// Adds weak geometry insertion edges around residual fallback blocks.
    fn add_fallback_insertion_edges(&mut self, blocks: &[Block]) {
        let mut sorted: Vec<_> = blocks.iter().collect();
        sorted.sort_by(|left, right| {
            left.bbox
                .top
                .total_cmp(&right.bbox.top)
                .then_with(|| left.bbox.left.total_cmp(&right.bbox.left))
                .then_with(|| left.id.cmp(&right.id))
        });
        for pair in sorted.windows(2) {
            if let [left, right] = pair
                && (left.label_source == LabelSource::Fallback
                    || right.label_source == LabelSource::Fallback)
            {
                self.push_edge(
                    &left.id,
                    &right.id,
                    EdgeSource::FallbackInsertion,
                    "fallback block geometry insertion",
                    None,
                );
            }
        }
    }

    /// Creates one edge with a canonical key and policy-derived weight.
    fn push_edge(
        &mut self,
        from: &BlockId,
        to: &BlockId,
        source: EdgeSource,
        reason: &str,
        confidence: Option<f64>,
    ) {
        if from == to {
            return;
        }
        let stable_key =
            format!("{}>{}:{}", from.as_str(), to.as_str(), source.rank());
        self.insert_edge(
            OrderEdge::builder()
                .from(from.clone())
                .to(to.clone())
                .source(source)
                .reason(reason.to_owned())
                .source_confidence(confidence)
                .preservation_weight(OrderPolicy::weight(source, confidence))
                .stable_key(stable_key)
                .build(),
        );
    }
}

/// Ordered blocks plus diagnostics from any removed weak constraints.
#[derive(Debug, Clone, PartialEq, TypedBuilder)]
pub(crate) struct OrderedBlocks {
    pub(crate) blocks: Vec<Block>,
    #[builder(default)]
    pub(crate) removed_edges: Vec<RemovedOrderEdge>,
}

/// Resolves order constraints and moves each block into its one final position.
pub(crate) fn order_blocks(
    blocks: Vec<Block>,
) -> Result<OrderedBlocks, OrderError> {
    let graph = OrderGraph::from_blocks(&blocks);
    let resolution = graph.resolve()?;
    let mut owned = BTreeMap::new();
    for block in blocks {
        let id = block.id.clone();
        if owned.insert(id.clone(), block).is_some() {
            return Err(OrderError::DuplicateBlock {
                block_id: id.as_str().to_owned(),
            });
        }
    }
    let mut ordered = Vec::with_capacity(owned.len());
    for (ordinal, id) in resolution.ordered_ids.into_iter().enumerate() {
        let mut block =
            owned.remove(&id).ok_or_else(|| OrderError::MissingBlock {
                block_id: id.as_str().to_owned(),
            })?;
        block.final_order = u32::try_from(ordinal).unwrap_or(u32::MAX);
        ordered.push(block);
    }
    Ok(OrderedBlocks::builder()
        .blocks(ordered)
        .removed_edges(resolution.removed_edges)
        .build())
}

/// Quantizes one finite top coordinate into the fixed half-point band.
fn quantized_band(top: f64) -> i64 {
    let band = (top / 0.5).floor();
    if band <= i64::MIN as f64 {
        i64::MIN
    } else if band >= i64::MAX as f64 {
        i64::MAX
    } else {
        band as i64
    }
}

/// Returns positive one-dimensional overlap between two ordered intervals.
fn overlap(
    left_start: f64,
    left_end: f64,
    right_start: f64,
    right_end: f64,
) -> f64 {
    (left_end.min(right_end) - left_start.max(right_start)).max(0.0)
}

/// Returns the shortest vertical gap between two boxes, or zero on overlap.
fn vertical_distance(left: Bbox, right: Bbox) -> f64 {
    if left.bottom < right.top {
        right.top - left.bottom
    } else if right.bottom < left.top {
        left.top - right.bottom
    } else {
        0.0
    }
}

/// Parses the stable source row index from one canonical model region ID.
fn model_region_index(region_id: &str) -> u32 {
    region_id
        .split_once(":m")
        .and_then(|(_, index)| index.parse().ok())
        .unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use docparse_layout::{Bbox, LayoutLabel};

    use super::{EdgeSource, OrderEdge, OrderGraph, OrderNode, OrderPolicy};
    use crate::{Block, BlockId, LabelSource, ModelRegionId, RegionPath};

    /// Builds one stable order node at the requested vertical position.
    fn node(index: u32, top: f64) -> OrderNode {
        OrderNode::builder()
            .block_id(BlockId::model(1, index, 0))
            .bbox(
                Bbox::try_from([10.0, top, 90.0, top + 10.0])
                    .expect("test bbox must be valid"),
            )
            .source_priority(0)
            .build()
    }

    /// Builds one explicit graph edge for cycle and ordering tests.
    fn edge(
        from: u32,
        to: u32,
        source: EdgeSource,
        confidence: f64,
    ) -> OrderEdge {
        let from = BlockId::model(1, from, 0);
        let to = BlockId::model(1, to, 0);
        OrderEdge::builder()
            .from(from.clone())
            .to(to.clone())
            .source(source)
            .reason("test".to_owned())
            .source_confidence(Some(confidence))
            .preservation_weight(OrderPolicy::weight(source, Some(confidence)))
            .stable_key(format!(
                "{}>{}:{}",
                from.as_str(),
                to.as_str(),
                source.rank()
            ))
            .build()
    }

    /// Builds one vertically stacked fallback block for edge-density regression tests.
    fn block(index: u32) -> Block {
        let top = f64::from(index) * 12.0;
        Block::builder()
            .id(BlockId::fallback(
                1,
                &RegionPath::root().horizontal_child(index),
                0,
            ))
            .label(LayoutLabel::Text)
            .text(String::new())
            .label_source(LabelSource::Fallback)
            .bbox(
                Bbox::try_from([10.0, top, 90.0, top + 10.0])
                    .expect("test bbox must be valid"),
            )
            .final_order(index)
            .lines(Vec::new())
            .build()
    }

    /// Verifies stable topology honors explicit single-column constraints.
    #[test]
    fn simple_graph_resolves_in_edge_order() {
        let mut graph = OrderGraph::default();
        graph.insert_node(node(2, 30.0));
        graph.insert_node(node(0, 10.0));
        graph.insert_node(node(1, 20.0));
        graph.insert_edge(edge(0, 1, EdgeSource::StrongVertical, 1.0));
        graph.insert_edge(edge(1, 2, EdgeSource::StrongVertical, 1.0));

        let resolution = graph.resolve().expect("acyclic graph must resolve");

        let ids: Vec<_> =
            resolution.ordered_ids.iter().map(BlockId::as_str).collect();
        assert_eq!(ids, vec!["p1:b:m0:s0", "p1:b:m1:s0", "p1:b:m2:s0"]);
        assert!(resolution.removed_edges.is_empty());
    }

    /// Verifies a low-confidence model edge is removed before geometric evidence.
    #[test]
    fn cycle_removes_low_weight_model_edge() {
        let mut graph = OrderGraph::default();
        graph.insert_node(node(0, 10.0));
        graph.insert_node(node(1, 20.0));
        graph.insert_node(node(2, 30.0));
        graph.insert_edge(edge(0, 1, EdgeSource::StrongVertical, 1.0));
        graph.insert_edge(edge(1, 2, EdgeSource::XyCut, 1.0));
        graph.insert_edge(edge(2, 0, EdgeSource::Model, 0.2));

        let resolution = graph.resolve().expect("removable cycle must resolve");

        assert_eq!(resolution.removed_edges.len(), 1);
        let removed = resolution
            .removed_edges
            .first()
            .expect("one removed edge must exist");
        assert_eq!(removed.source, EdgeSource::Model);
        assert_eq!(removed.from.as_str(), "p1:b:m2:s0");
    }

    /// Verifies equally weighted removable edges use source priority then stable key.
    #[test]
    fn equal_weight_cycle_uses_stable_source_tie_break() {
        let mut graph = OrderGraph::default();
        graph.insert_node(node(0, 10.0));
        graph.insert_node(node(1, 20.0));
        graph.insert_node(node(2, 30.0));
        graph.insert_edge(edge(0, 1, EdgeSource::CaptionRelation, 1.0));
        graph.insert_edge(edge(1, 2, EdgeSource::CaptionRelation, 1.0));
        graph.insert_edge(edge(2, 0, EdgeSource::CaptionRelation, 1.0));

        let resolution = graph.resolve().expect("removable cycle must resolve");

        assert_eq!(resolution.removed_edges.len(), 1);
        assert_eq!(
            resolution
                .removed_edges
                .first()
                .expect("one removed edge must exist")
                .from
                .as_str(),
            "p1:b:m0:s0"
        );
    }

    /// Verifies overlapping cycles retain iterative weakest-edge removal semantics.
    #[test]
    fn overlapping_cycles_remove_one_stable_edge_per_iteration() {
        let mut graph = OrderGraph::default();
        for index in 0..3 {
            graph.insert_node(node(index, 10.0 + f64::from(index) * 10.0));
        }
        graph.insert_edge(edge(0, 1, EdgeSource::CaptionRelation, 1.0));
        graph.insert_edge(edge(1, 0, EdgeSource::Model, 0.2));
        graph.insert_edge(edge(1, 2, EdgeSource::CaptionRelation, 1.0));
        graph.insert_edge(edge(2, 1, EdgeSource::Model, 0.3));

        let resolution =
            graph.resolve().expect("overlapping cycles must resolve");
        let removed: Vec<_> = resolution
            .removed_edges
            .iter()
            .map(|edge| (edge.from.as_str(), edge.to.as_str()))
            .collect();

        assert_eq!(
            removed,
            vec![("p1:b:m1:s0", "p1:b:m0:s0"), ("p1:b:m2:s0", "p1:b:m1:s0")]
        );
        assert_eq!(
            resolution
                .ordered_ids
                .iter()
                .map(BlockId::as_str)
                .collect::<Vec<_>>(),
            vec!["p1:b:m0:s0", "p1:b:m1:s0", "p1:b:m2:s0"]
        );
    }

    /// Verifies independent SCCs remove candidates in stable component order.
    #[test]
    fn independent_cycles_resolve_in_stable_component_order() {
        let mut graph = OrderGraph::default();
        for index in 0..4 {
            graph.insert_node(node(index, 10.0 + f64::from(index) * 10.0));
        }
        graph.insert_edge(edge(0, 1, EdgeSource::CaptionRelation, 1.0));
        graph.insert_edge(edge(1, 0, EdgeSource::Model, 0.2));
        graph.insert_edge(edge(2, 3, EdgeSource::CaptionRelation, 1.0));
        graph.insert_edge(edge(3, 2, EdgeSource::BandHorizontal, 1.0));

        let resolution =
            graph.resolve().expect("independent cycles must resolve");
        let removed: Vec<_> = resolution
            .removed_edges
            .iter()
            .map(|edge| (edge.from.as_str(), edge.to.as_str()))
            .collect();

        assert_eq!(
            removed,
            vec![("p1:b:m1:s0", "p1:b:m0:s0"), ("p1:b:m3:s0", "p1:b:m2:s0")]
        );
    }

    /// Verifies contradictory strong-only constraints return an internal conflict.
    #[test]
    fn strong_only_cycle_is_an_error() {
        let mut graph = OrderGraph::default();
        graph.insert_node(node(0, 10.0));
        graph.insert_node(node(1, 20.0));
        graph.insert_edge(edge(0, 1, EdgeSource::StrongVertical, 1.0));
        graph.insert_edge(edge(1, 0, EdgeSource::IntraRegion, 1.0));

        let _error = graph.resolve().expect_err("strong-only cycle must fail");
    }

    /// Verifies zero-indegree ties fall back to geometry and stable identity.
    #[test]
    fn unconstrained_nodes_use_stable_geometry_order() {
        let mut graph = OrderGraph::default();
        graph.insert_node(node(2, 30.0));
        graph.insert_node(node(1, 10.0));
        graph.insert_node(node(0, 10.0));

        let resolution =
            graph.resolve().expect("empty-edge graph must resolve");
        let ids: Vec<_> =
            resolution.ordered_ids.iter().map(BlockId::as_str).collect();

        assert_eq!(ids, vec!["p1:b:m0:s0", "p1:b:m1:s0", "p1:b:m2:s0"]);
    }

    /// Verifies geometry construction stays linear instead of connecting every pair.
    #[test]
    fn geometry_edges_are_linear_in_block_count() {
        let blocks: Vec<_> = (0..200).map(block).collect();

        let graph = OrderGraph::from_blocks(&blocks);

        assert!(graph.edges.len() < blocks.len() * 4);
        assert_eq!(
            graph
                .resolve()
                .expect("stacked blocks must resolve")
                .ordered_ids
                .len(),
            blocks.len()
        );
    }

    /// Verifies a tall aside margin cannot precede the main document title.
    #[test]
    fn rotated_margin_follows_main_flow() {
        let mut margin = block(0);
        margin.id = BlockId::model(1, 0, 0);
        margin.label = LayoutLabel::AsideText;
        margin.label_source = LabelSource::Model;
        margin.model_region_id = Some(ModelRegionId::detected(1, 0));
        margin.model_order = Some(0);
        margin.bbox = Bbox::try_from([5.0, 100.0, 20.0, 700.0])
            .expect("margin bbox must be valid");
        let mut title = block(1);
        title.id = BlockId::model(1, 1, 0);
        title.label = LayoutLabel::DocTitle;
        title.label_source = LabelSource::Model;
        title.model_region_id = Some(ModelRegionId::detected(1, 1));
        title.model_order = Some(1);
        title.bbox = Bbox::try_from([80.0, 40.0, 520.0, 80.0])
            .expect("title bbox must be valid");

        let ordered = super::order_blocks(vec![margin, title])
            .expect("margin ordering must resolve");

        assert_eq!(
            ordered.blocks.first().expect("title must be first").label,
            LayoutLabel::DocTitle
        );
        assert_eq!(
            ordered.blocks.last().expect("margin must be last").label,
            LayoutLabel::AsideText
        );
    }
}
