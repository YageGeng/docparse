//! PP-DocLayoutV3 output decoding.
//!
//! Model output dimensions are validated before decoding; direct indexing keeps
//! the CPU proposal and reading-order code compact.
#![allow(clippy::indexing_slicing, clippy::too_many_arguments)]

use anyhow::{Result, anyhow, bail};
use burn_tensor::{Transaction, backend::Backend};

use crate::ml::sigmoid_f32;
use crate::pp_doclayout::model::PpDocLayoutRawOutput;

/// One PP-DocLayoutV3 detection in xyxy pixel coordinates.
#[derive(Debug, Clone)]
pub(crate) struct LayoutDetection {
    pub(crate) label: String,
    pub(crate) score: f32,
    pub(crate) bbox: [f32; 4],
    pub(crate) order: i64,
}

/// Thresholds and limits used when decoding raw proposals.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PpPostprocessOptions {
    score_threshold: f32,
    nms_threshold: Option<f32>,
    max_detections: usize,
}

impl Default for PpPostprocessOptions {
    /// Builds PaddleX-compatible default postprocess options.
    fn default() -> Self {
        Self {
            score_threshold: 0.5,
            nms_threshold: None,
            max_detections: 300,
        }
    }
}

/// Converts raw model tensors into sorted layout detections.
pub(crate) fn postprocess_encoder_proposals<B: Backend<FloatElem = f32>>(
    output: PpDocLayoutRawOutput<B>,
    labels: &std::collections::BTreeMap<String, String>,
    original_width: u32,
    original_height: u32,
    options: PpPostprocessOptions,
) -> Result<Vec<LayoutDetection>> {
    let shapes = validate_output_shapes(&output)?;
    let [scores_data, boxes_data, order_data] = Transaction::default()
        .register(output.scores)
        .register(output.boxes)
        .register(output.order_features)
        .execute()
        .try_into()
        .map_err(|_error| {
            anyhow!("PP-DocLayoutV3 tensor readback count mismatch")
        })?;

    decode_encoder_proposals(
        scores_data.to_vec::<f32>()?,
        boxes_data.to_vec::<f32>()?,
        order_data.to_vec::<f32>()?,
        shapes,
        labels,
        original_width,
        original_height,
        options,
    )
}

/// Converts raw model tensors into sorted detections without blocking wasm.
pub(crate) async fn postprocess_encoder_proposals_async<
    B: Backend<FloatElem = f32>,
>(
    output: PpDocLayoutRawOutput<B>,
    labels: &std::collections::BTreeMap<String, String>,
    original_width: u32,
    original_height: u32,
    options: PpPostprocessOptions,
) -> Result<Vec<LayoutDetection>> {
    let shapes = validate_output_shapes(&output)?;
    let [scores_data, boxes_data, order_data] = Transaction::default()
        .register(output.scores)
        .register(output.boxes)
        .register(output.order_features)
        .execute_async()
        .await?
        .try_into()
        .map_err(|_error| {
            anyhow!("PP-DocLayoutV3 tensor readback count mismatch")
        })?;

    decode_encoder_proposals(
        scores_data.to_vec::<f32>()?,
        boxes_data.to_vec::<f32>()?,
        order_data.to_vec::<f32>()?,
        shapes,
        labels,
        original_width,
        original_height,
        options,
    )
}

/// Checks model output shapes before any CPU decoding.
fn validate_output_shapes<B: Backend<FloatElem = f32>>(
    output: &PpDocLayoutRawOutput<B>,
) -> Result<OutputShapes> {
    let [batch, proposals, classes] = output.scores.dims();
    if batch != 1 {
        bail!("PP-DocLayoutV3 postprocess expects batch size 1");
    }
    let [box_batch, box_proposals, box_dims] = output.boxes.dims();
    if box_batch != batch || box_proposals != proposals || box_dims != 4 {
        bail!("PP-DocLayoutV3 score and box shapes do not match");
    }
    if classes == 0 {
        bail!("PP-DocLayoutV3 postprocess expects class scores");
    }
    let order_dims = output.order_features.dims();

    Ok(OutputShapes {
        proposals,
        classes,
        order_dims,
    })
}

/// Output dimensions needed by CPU post-processing.
#[derive(Debug, Clone, Copy)]
struct OutputShapes {
    proposals: usize,
    classes: usize,
    order_dims: [usize; 3],
}

/// Decodes raw CPU vectors into final layout detections.
fn decode_encoder_proposals(
    scores: Vec<f32>,
    boxes: Vec<f32>,
    order_values: Vec<f32>,
    shapes: OutputShapes,
    labels: &std::collections::BTreeMap<String, String>,
    original_width: u32,
    original_height: u32,
    options: PpPostprocessOptions,
) -> Result<Vec<LayoutDetection>> {
    if scores.len() != shapes.proposals * shapes.classes
        || boxes.len() != shapes.proposals * 4
    {
        bail!("PP-DocLayoutV3 output data length mismatch");
    }
    let orders =
        reading_order_ranks_from_values(shapes.order_dims, order_values)?;
    let mut candidates = scores
        .iter()
        .copied()
        .enumerate()
        .map(|(index, logit)| {
            (
                index / shapes.classes,
                index % shapes.classes,
                sigmoid_f32(logit),
            )
        })
        .filter(|(_, _, score)| *score >= options.score_threshold)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.2.total_cmp(&left.2));

    let mut detections = Vec::new();
    for (proposal, label_id, score) in candidates {
        if detections.len() >= options.max_detections {
            break;
        }
        let box_start = proposal * 4;
        let center_x = boxes[box_start] * original_width as f32;
        let center_y = boxes[box_start + 1] * original_height as f32;
        let width = boxes[box_start + 2] * original_width as f32;
        let height = boxes[box_start + 3] * original_height as f32;
        let bbox = [
            (center_x - width * 0.5).clamp(0.0, original_width as f32),
            (center_y - height * 0.5).clamp(0.0, original_height as f32),
            (center_x + width * 0.5).clamp(0.0, original_width as f32),
            (center_y + height * 0.5).clamp(0.0, original_height as f32),
        ];
        let label = labels
            .get(&label_id.to_string())
            .cloned()
            .unwrap_or_else(|| label_id.to_string());
        detections.push(LayoutDetection {
            label,
            score,
            bbox,
            order: i64::try_from(orders[proposal])?,
        });
    }

    detections.sort_by(|left, right| right.score.total_cmp(&left.score));
    let mut kept: Vec<LayoutDetection> = Vec::new();
    for detection in detections {
        if kept.len() >= options.max_detections {
            break;
        }
        if options.nms_threshold.is_some_and(|nms_threshold| {
            kept.iter().any(|existing| {
                bbox_iou(existing.bbox, detection.bbox) > nms_threshold
            })
        }) {
            continue;
        }
        kept.push(detection);
    }

    Ok(sort_detections_by_order(kept))
}

/// Sorts detections by model reading order with geometric tie-breakers.
pub(crate) fn sort_detections_by_order(
    mut detections: Vec<LayoutDetection>,
) -> Vec<LayoutDetection> {
    detections.sort_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.bbox[1].total_cmp(&right.bbox[1]))
            .then_with(|| left.bbox[0].total_cmp(&right.bbox[0]))
    });
    detections
}

/// Infers reading-order ranks after global pointer features are on CPU.
fn reading_order_ranks_from_values(
    dims: [usize; 3],
    values: Vec<f32>,
) -> Result<Vec<usize>> {
    let [batch, proposals, features] = dims;
    if batch != 1 || features % 2 != 0 {
        bail!("PP-DocLayoutV3 order feature shape mismatch");
    }
    let half = features / 2;
    let mut edges = vec![vec![f32::NEG_INFINITY; proposals]; proposals];
    let mut incoming = vec![0.0; proposals];
    let mut outgoing = vec![0.0; proposals];

    for from in 0..proposals {
        let from_start = from * features;
        for to in 0..proposals {
            if from == to {
                continue;
            }
            let to_start = to * features + half;
            let score = (0..half)
                .map(|index| {
                    values[from_start + index] * values[to_start + index]
                })
                .sum::<f32>();
            edges[from][to] = score;
            outgoing[from] += score;
            incoming[to] += score;
        }
    }

    let mut visited = vec![false; proposals];
    let mut ranks = vec![proposals; proposals];
    let mut current = (0..proposals)
        .min_by(|left, right| {
            (incoming[*left] - outgoing[*left])
                .total_cmp(&(incoming[*right] - outgoing[*right]))
        })
        .unwrap_or(0);

    for rank in 0..proposals {
        ranks[current] = rank;
        visited[current] = true;
        let next = edges[current]
            .iter()
            .copied()
            .enumerate()
            .filter(|(index, _score)| !visited[*index])
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .map(|(index, _score)| index)
            .or_else(|| (0..proposals).find(|index| !visited[*index]));
        let Some(next) = next else {
            break;
        };
        current = next;
    }

    Ok(ranks)
}

/// Computes intersection-over-union for xyxy boxes.
fn bbox_iou(left: [f32; 4], right: [f32; 4]) -> f32 {
    let x1 = left[0].max(right[0]);
    let y1 = left[1].max(right[1]);
    let x2 = left[2].min(right[2]);
    let y2 = left[3].min(right[3]);
    let intersection = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let left_area = (left[2] - left[0]).max(0.0) * (left[3] - left[1]).max(0.0);
    let right_area =
        (right[2] - right[0]).max(0.0) * (right[3] - right[1]).max(0.0);
    let union = left_area + right_area - intersection;

    if union <= 0.0 {
        return 0.0;
    }

    intersection / union
}
