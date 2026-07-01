//! Tensor operations shared by the native PP-DocLayoutV3 layers.
//!
//! Tensor shapes are checked before these low-level operations run, so direct
//! indexing keeps the model math close to the source graph.
#![allow(clippy::indexing_slicing)]

use anyhow::{Context, Result, bail};
use burn_tensor::{
    Int, Tensor, TensorData,
    activation::{
        relu as burn_relu, sigmoid as burn_sigmoid, silu as burn_silu, softmax,
    },
    backend::Backend,
    ops::GridSampleOptions,
};
use safetensors::{Dtype, SafeTensors};

pub(super) fn read_f32_tensor<B>(
    tensors: &SafeTensors<'_>,
    name: &str,
    shape: &[usize],
    device: &B::Device,
) -> Result<Tensor<B, 1>>
where
    B: Backend<FloatElem = f32>,
{
    Ok(Tensor::from_data(
        TensorData::new(read_f32_values(tensors, name, shape)?, shape.to_vec()),
        device,
    ))
}

pub(super) fn read_linear_weight<B>(
    tensors: &SafeTensors<'_>,
    name: &str,
    input_features: usize,
    output_features: usize,
    device: &B::Device,
) -> Result<Tensor<B, 2>>
where
    B: Backend<FloatElem = f32>,
{
    let values =
        read_f32_values(tensors, name, &[output_features, input_features])?;
    let mut transposed = vec![0.0; values.len()];
    for output in 0..output_features {
        for input in 0..input_features {
            transposed[input * output_features + output] =
                values[output * input_features + input];
        }
    }

    Ok(Tensor::from_data(
        TensorData::new(transposed, [input_features, output_features]),
        device,
    ))
}

pub(super) fn flatten_feature_maps<B: Backend<FloatElem = f32>>(
    features: Vec<Tensor<B, 4>>,
) -> Tensor<B, 3> {
    let flattened = features
        .into_iter()
        .map(|feature| {
            let [batch, channels, height, width] = feature.dims();
            feature.swap_dims(1, 3).swap_dims(1, 2).reshape([
                batch,
                height * width,
                channels,
            ])
        })
        .collect();

    Tensor::cat(flattened, 1)
}

pub(super) fn aifi_position_embedding<B: Backend<FloatElem = f32>>(
    height: usize,
    width: usize,
    device: &B::Device,
) -> Tensor<B, 3> {
    let position_dim = 64usize;
    let temperature = 10_000.0_f32;
    let mut values = Vec::with_capacity(height * width * 256);

    for y in 0..height {
        for x in 0..width {
            for index in 0..position_dim {
                let omega =
                    1.0 / temperature.powf(index as f32 / position_dim as f32);
                values.push((y as f32 * omega).sin());
            }
            for index in 0..position_dim {
                let omega =
                    1.0 / temperature.powf(index as f32 / position_dim as f32);
                values.push((y as f32 * omega).cos());
            }
            for index in 0..position_dim {
                let omega =
                    1.0 / temperature.powf(index as f32 / position_dim as f32);
                values.push((x as f32 * omega).sin());
            }
            for index in 0..position_dim {
                let omega =
                    1.0 / temperature.powf(index as f32 / position_dim as f32);
                values.push((x as f32 * omega).cos());
            }
        }
    }

    Tensor::from_data(TensorData::new(values, [1, height * width, 256]), device)
}

/// Selects top-scoring proposal indices with a CPU readback.
///
/// Burn/WGPU `argtopk` still blocks the following decoder graph on this stack,
/// so the small proposal sort stays on CPU until a fused backend path exists.
pub(super) fn topk_proposal_indices<B: Backend<FloatElem = f32>>(
    scores: Tensor<B, 3>,
    k: usize,
) -> Result<Vec<usize>> {
    let [batch, proposals, classes] = scores.dims();
    if batch != 1 {
        bail!("PP-DocLayoutV3 top-k proposal gather expects batch size 1");
    }
    let values = scores.into_data().to_vec::<f32>()?;

    topk_indices_from_values(values, proposals, classes, k)
}

/// Selects top-scoring proposal indices with async CPU readback for wasm.
pub(super) async fn topk_proposal_indices_async<B: Backend<FloatElem = f32>>(
    scores: Tensor<B, 3>,
    k: usize,
) -> Result<Vec<usize>> {
    let [batch, proposals, classes] = scores.dims();
    if batch != 1 {
        bail!("PP-DocLayoutV3 top-k proposal gather expects batch size 1");
    }
    let values = scores.into_data_async().await?.to_vec::<f32>()?;

    topk_indices_from_values(values, proposals, classes, k)
}

/// Ranks encoder proposals after tensor values have been copied to CPU.
fn topk_indices_from_values(
    values: Vec<f32>,
    proposals: usize,
    classes: usize,
    k: usize,
) -> Result<Vec<usize>> {
    let mut ranked = (0..proposals)
        .map(|proposal| {
            let start = proposal * classes;
            let score = values[start..start + classes]
                .iter()
                .copied()
                .fold(f32::NEG_INFINITY, f32::max);
            (proposal, score)
        })
        .collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.total_cmp(&left.1));

    Ok(ranked
        .into_iter()
        .take(k)
        .map(|(proposal, _score)| proposal)
        .collect())
}

/// Gathers sequence rows on the active backend after CPU proposal selection.
///
/// Keeping top-k on CPU avoids the pathological backend `argtopk` path while
/// still avoiding a full output-memory readback for the selected rows.
pub(super) fn gather_sequence<B: Backend<FloatElem = f32>>(
    tensor: Tensor<B, 3>,
    indices: &[usize],
    width: usize,
) -> Result<Tensor<B, 3>> {
    let [batch, sequence, actual_width] = tensor.dims();
    if batch != 1 || actual_width != width {
        bail!("PP-DocLayoutV3 sequence gather shape mismatch");
    }
    let device = tensor.device();
    let mut index_values = Vec::with_capacity(indices.len());
    for &index in indices {
        if index >= sequence {
            bail!(
                "PP-DocLayoutV3 proposal index {index} out of range {sequence}"
            );
        }
        index_values.push(i64::try_from(index)?);
    }
    let indices = Tensor::<B, 1, Int>::from_data(
        TensorData::new(index_values, [indices.len()]),
        &device,
    );

    Ok(tensor.select(1, indices))
}

/// Computes multi-scale deformable attention with backend `grid_sample_2d`.
///
/// This replaces the old CPU bilinear-sampling loop so WGPU inference can
/// keep decoder cross-attention on the device.
pub(super) fn deformable_attention_context<B: Backend<FloatElem = f32>>(
    value: Tensor<B, 4>,
    offsets: Tensor<B, 6>,
    weights: Tensor<B, 4>,
    reference_boxes: Tensor<B, 3>,
    spatial_shapes: &[(usize, usize)],
) -> Result<Tensor<B, 3>> {
    let [batch, sequence, heads, head_dim] = value.dims();
    let [
        offset_batch,
        queries,
        offset_heads,
        levels,
        points,
        offset_dims,
    ] = offsets.dims();
    let [weight_batch, weight_queries, weight_heads, weight_points] =
        weights.dims();
    let [reference_batch, reference_queries, reference_dims] =
        reference_boxes.dims();
    if batch != 1
        || offset_batch != 1
        || weight_batch != 1
        || reference_batch != 1
        || offset_heads != heads
        || weight_heads != heads
        || weight_queries != queries
        || reference_queries != queries
        || levels != spatial_shapes.len()
        || points != 4
        || offset_dims != 2
        || weight_points != levels * points
        || reference_dims != 4
    {
        bail!("PP-DocLayoutV3 deformable attention shape mismatch");
    }

    let reference_x = reference_boxes
        .clone()
        .narrow(2, 0, 1)
        .unsqueeze_dim::<4>(3);
    let reference_y = reference_boxes
        .clone()
        .narrow(2, 1, 1)
        .unsqueeze_dim::<4>(3);
    let reference_w = reference_boxes
        .clone()
        .narrow(2, 2, 1)
        .unsqueeze_dim::<4>(3);
    let reference_h = reference_boxes.narrow(2, 3, 1).unsqueeze_dim::<4>(3);
    let normalized_weights =
        softmax(weights, 3).reshape([1, queries, heads, levels, points]);
    let mut level_starts = Vec::with_capacity(levels);
    let mut start = 0usize;
    for &(height, width) in spatial_shapes {
        level_starts.push(start);
        start += height * width;
    }
    if start != sequence {
        bail!("PP-DocLayoutV3 deformable attention sequence mismatch");
    }

    let mut context: Option<Tensor<B, 4>> = None;
    for (level, &(height, width)) in spatial_shapes.iter().enumerate() {
        let level_value = value
            .clone()
            .narrow(1, level_starts[level], height * width)
            .reshape([height, width, heads, head_dim])
            .permute([2, 3, 0, 1]);
        let level_offsets =
            offsets.clone().narrow(3, level, 1).squeeze_dim::<5>(3);
        let offset_x =
            level_offsets.clone().narrow(4, 0, 1).squeeze_dim::<4>(4);
        let offset_y = level_offsets.narrow(4, 1, 1).squeeze_dim::<4>(4);
        let grid_x = (reference_x.clone()
            + offset_x / points as f64 * reference_w.clone() * 0.5)
            * 2.0
            - 1.0;
        let grid_y = (reference_y.clone()
            + offset_y / points as f64 * reference_h.clone() * 0.5)
            * 2.0
            - 1.0;
        let grid = Tensor::cat(
            vec![grid_x.permute([2, 1, 3, 0]), grid_y.permute([2, 1, 3, 0])],
            3,
        );
        let sampled =
            level_value.grid_sample_2d(grid, GridSampleOptions::default());
        let level_weights = normalized_weights
            .clone()
            .narrow(3, level, 1)
            .squeeze_dim::<4>(3)
            .permute([2, 0, 1, 3]);
        let level_context = (sampled * level_weights).sum_dim(3);
        context = Some(match context {
            Some(previous) => previous + level_context,
            None => level_context,
        });
    }

    Ok(context
        .context("PP-DocLayoutV3 deformable attention has no feature levels")?
        .squeeze_dim::<3>(3)
        .permute([2, 0, 1])
        .reshape([1, queries, heads * head_dim]))
}

pub(super) fn generate_encoder_anchors<B: Backend<FloatElem = f32>>(
    features: &[Tensor<B, 4>],
    device: &B::Device,
) -> Result<(Tensor<B, 3>, Tensor<B, 3>)> {
    let mut values = Vec::new();
    let mut mask = Vec::new();
    let grid_size = 0.05_f32;
    let eps = 1.0e-2_f32;
    for (level, feature) in features.iter().enumerate() {
        let [_batch, _channels, height, width] = feature.dims();
        let wh = grid_size * 2_f32.powi(i32::try_from(level)?);
        for y in 0..height {
            for x in 0..width {
                let center_x = (x as f32 + 0.5) / width as f32;
                let center_y = (y as f32 + 0.5) / height as f32;
                let valid = center_x > eps
                    && center_x < 1.0 - eps
                    && center_y > eps
                    && center_y < 1.0 - eps
                    && wh > eps
                    && wh < 1.0 - eps;
                let value = if valid { 1.0 } else { 0.0 };
                mask.push(value);
                values.push(if valid {
                    logit(center_x)
                } else {
                    f32::INFINITY
                });
                values.push(if valid {
                    logit(center_y)
                } else {
                    f32::INFINITY
                });
                values.push(if valid { logit(wh) } else { f32::INFINITY });
                values.push(if valid { logit(wh) } else { f32::INFINITY });
            }
        }
    }
    let count = values.len() / 4;

    Ok((
        Tensor::from_data(TensorData::new(values, [1, count, 4]), device),
        Tensor::from_data(TensorData::new(mask, [1, count, 1]), device),
    ))
}

pub(super) fn logit(value: f32) -> f32 {
    (value / (1.0 - value)).ln()
}

pub(super) fn relu<B: Backend<FloatElem = f32>, const D: usize>(
    x: Tensor<B, D>,
) -> Tensor<B, D> {
    burn_relu(x)
}

pub(super) fn gelu<B: Backend<FloatElem = f32>, const D: usize>(
    x: Tensor<B, D>,
) -> Tensor<B, D> {
    x.clone() * 0.5 * ((x / std::f64::consts::SQRT_2).erf() + 1.0)
}

pub(super) fn read_f32_values(
    tensors: &SafeTensors<'_>,
    name: &str,
    shape: &[usize],
) -> Result<Vec<f32>> {
    let tensor = tensors
        .tensor(name)
        .with_context(|| format!("missing PP-DocLayoutV3 tensor {name}"))?;
    if tensor.dtype() != Dtype::F32 {
        bail!("PP-DocLayoutV3 tensor {name} must be float32");
    }
    if tensor.shape() != shape {
        bail!(
            "PP-DocLayoutV3 tensor {name} shape {:?} does not match {:?}",
            tensor.shape(),
            shape
        );
    }

    Ok(tensor
        .data()
        .chunks_exact(4)
        .map(|bytes| {
            f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
        })
        .collect())
}

pub(super) fn silu<B: Backend<FloatElem = f32>, const D: usize>(
    x: Tensor<B, D>,
) -> Tensor<B, D> {
    burn_silu(x)
}

pub(super) fn sigmoid_tensor<B: Backend<FloatElem = f32>, const D: usize>(
    x: Tensor<B, D>,
) -> Tensor<B, D> {
    burn_sigmoid(x)
}

pub(super) fn inverse_sigmoid_tensor<
    B: Backend<FloatElem = f32>,
    const D: usize,
>(
    x: Tensor<B, D>,
) -> Tensor<B, D> {
    let x = x.clamp(1e-5, 1.0 - 1e-5);
    (x.clone() / (x * -1.0 + 1.0)).log()
}
