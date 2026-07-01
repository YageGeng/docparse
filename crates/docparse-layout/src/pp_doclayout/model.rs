#![allow(
    clippy::indexing_slicing,
    clippy::large_enum_variant,
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity
)]
// Model graph tensors are shape-validated when loaded; direct indexing keeps
// the generated PP-DocLayout layer code readable and avoids helper noise.

#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
use anyhow::Context;
use anyhow::{Result, bail};
use burn_tensor::{
    Tensor, TensorData,
    activation::softmax,
    backend::Backend,
    module::{conv2d, interpolate, max_pool2d},
    ops::{InterpolateMode, InterpolateOptions, PadMode, PaddedConvOptions},
};
use image::DynamicImage;
use safetensors::SafeTensors;
use tracing::{Level, event};
use web_time::Instant;

#[cfg(not(target_arch = "wasm32"))]
use crate::pp_doclayout::config::read_pp_doclayout_config;
use crate::pp_doclayout::config::{
    PpDocLayoutConfig, PpDocLayoutPreprocessorConfig,
    read_pp_doclayout_config_from_bytes,
};
use crate::pp_doclayout::ops::*;
use crate::pp_doclayout::postprocess::{
    LayoutDetection, PpPostprocessOptions, postprocess_encoder_proposals,
    postprocess_encoder_proposals_async,
};
use crate::pp_doclayout::preprocess::preprocess_layout_image;
#[cfg(not(target_arch = "wasm32"))]
use crate::pp_doclayout::weights::load_pp_doclayout_files;
use crate::pp_doclayout::weights::validate_pp_doclayout_weights;

/// Synchronizes WGPU only for tracing profiles so phase timings include queued work.
fn sync_profile_event<B: Backend>(
    device: &B::Device,
    phase: &'static str,
    started: Instant,
) {
    if tracing::enabled!(Level::INFO)
        && let Err(error) = B::sync(device)
    {
        event!(
            Level::WARN,
            phase = phase,
            error = %error,
            "layout profile sync failed"
        );
    }
    event!(
        Level::INFO,
        phase = phase,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );
}

/// Synchronizes and emits an indexed profile event for repeated model blocks.
fn sync_profile_event_index<B: Backend>(
    device: &B::Device,
    phase: &'static str,
    index: usize,
    started: Instant,
) {
    if tracing::enabled!(Level::INFO)
        && let Err(error) = B::sync(device)
    {
        event!(
            Level::WARN,
            phase = phase,
            index = index,
            error = %error,
            "layout profile sync failed"
        );
    }
    event!(
        Level::INFO,
        phase = phase,
        index = index,
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );
}

#[derive(Debug)]
pub(crate) struct PpConv1x1BatchNorm<B: Backend> {
    weight: Tensor<B, 2>,
    bias: Tensor<B, 1>,
}

#[derive(Debug)]
pub(crate) struct PpEncoderInputProjection<B: Backend> {
    projections: Vec<PpConv1x1BatchNorm<B>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PpActivation {
    Identity,
    Relu,
    Silu,
}

#[derive(Debug)]
pub(crate) struct PpConvBatchNorm<B: Backend> {
    conv: PpConv2d<B>,
    activation: PpActivation,
}

/// Holds fused conv weights so layout inference avoids the high-level Burn NN crate.
#[derive(Debug)]
pub(crate) struct PpConv2d<B: Backend> {
    weight: Tensor<B, 4>,
    bias: Tensor<B, 1>,
    stride: [usize; 2],
    padding: [usize; 4],
    dilation: [usize; 2],
    groups: usize,
}

impl<B> PpConv2d<B>
where
    B: Backend<FloatElem = f32>,
{
    /// Applies convolution with explicit padding through the low-level tensor API.
    fn forward(&self, input: Tensor<B, 4>) -> Tensor<B, 4> {
        let [top, left, bottom, right] = self.padding;
        let options = PaddedConvOptions::asymmetric(
            self.stride,
            [top, left],
            [bottom, right],
            self.dilation,
            self.groups,
        );
        conv2d(input, self.weight.clone(), Some(self.bias.clone()), options)
    }
}

#[derive(Debug)]
pub(crate) struct PpHgnetStem<B: Backend> {
    stem1: PpConvBatchNorm<B>,
    stem2a: PpConvBatchNorm<B>,
    stem2b: PpConvBatchNorm<B>,
    stem3: PpConvBatchNorm<B>,
    stem4: PpConvBatchNorm<B>,
}

#[derive(Debug)]
pub(crate) struct PpHgnetBasicLayer<B: Backend> {
    layers: Vec<PpHgnetLayer<B>>,
    aggregation_squeeze: PpConvBatchNorm<B>,
    aggregation_excitation: PpConvBatchNorm<B>,
    residual: bool,
}

#[derive(Debug)]
pub(crate) enum PpHgnetLayer<B: Backend> {
    Conv(PpConvBatchNorm<B>),
    Light {
        pointwise: PpConvBatchNorm<B>,
        depthwise: PpConvBatchNorm<B>,
    },
}

#[derive(Debug)]
pub(crate) struct PpHgnetStage<B: Backend> {
    downsample: Option<PpConvBatchNorm<B>>,
    blocks: Vec<PpHgnetBasicLayer<B>>,
}

#[derive(Debug)]
pub(crate) struct PpHgnetBackbone<B: Backend> {
    stem: PpHgnetStem<B>,
    stage0: PpHgnetStage<B>,
    stage1: PpHgnetStage<B>,
    stage2: PpHgnetStage<B>,
    stage3: PpHgnetStage<B>,
}

#[derive(Debug)]
pub(crate) struct PpBackboneFeatureProjector<B: Backend> {
    backbone: PpHgnetBackbone<B>,
    projection: PpEncoderInputProjection<B>,
}

#[derive(Debug)]
pub(crate) struct PpHybridEncoderConvs<B: Backend> {
    aifi: PpAifiLayer<B>,
    lateral_convs: Vec<PpConvBatchNorm<B>>,
    downsample_convs: Vec<PpConvBatchNorm<B>>,
    fpn_blocks: Vec<PpCspRepLayer<B>>,
    pan_blocks: Vec<PpCspRepLayer<B>>,
}

#[derive(Debug)]
pub(crate) struct PpLayerNorm<B: Backend> {
    weight: Tensor<B, 1>,
    bias: Tensor<B, 1>,
    epsilon: f64,
}

#[derive(Debug)]
pub(crate) struct PpAifiLayer<B: Backend> {
    q_proj: PpLinear<B>,
    k_proj: PpLinear<B>,
    v_proj: PpLinear<B>,
    out_proj: PpLinear<B>,
    self_attn_layer_norm: PpLayerNorm<B>,
    fc1: PpLinear<B>,
    fc2: PpLinear<B>,
    final_layer_norm: PpLayerNorm<B>,
}

#[derive(Debug)]
pub(crate) struct PpRepVggBlock<B: Backend> {
    conv: PpConvBatchNorm<B>,
}

#[derive(Debug)]
pub(crate) struct PpCspRepLayer<B: Backend> {
    conv1: PpConvBatchNorm<B>,
    conv2: PpConvBatchNorm<B>,
    bottlenecks: Vec<PpRepVggBlock<B>>,
}

#[derive(Debug)]
pub(crate) struct PpLinear<B: Backend> {
    weight: Tensor<B, 2>,
    bias: Tensor<B, 1>,
}

#[derive(Debug)]
pub(crate) struct PpMlp<B: Backend> {
    layers: Vec<PpLinear<B>>,
}

#[derive(Debug)]
pub(crate) struct PpEncoderDetectionHead<B: Backend> {
    decoder_input_proj: PpEncoderInputProjection<B>,
    enc_output: PpLinear<B>,
    enc_output_norm: PpLayerNorm<B>,
    enc_score_head: PpLinear<B>,
    enc_bbox_head: PpMlp<B>,
}

#[derive(Debug)]
pub(crate) struct PpDecoderAttention<B: Backend> {
    q_proj: PpLinear<B>,
    k_proj: PpLinear<B>,
    v_proj: PpLinear<B>,
    out_proj: PpLinear<B>,
}

#[derive(Debug)]
pub(crate) struct PpDecoderCrossAttention<B: Backend> {
    sampling_offsets: PpLinear<B>,
    attention_weights: PpLinear<B>,
    value_proj: PpLinear<B>,
    output_proj: PpLinear<B>,
}

#[derive(Debug)]
pub(crate) struct PpDecoderLayer<B: Backend> {
    self_attn: PpDecoderAttention<B>,
    self_attn_layer_norm: PpLayerNorm<B>,
    encoder_attn: PpDecoderCrossAttention<B>,
    encoder_attn_layer_norm: PpLayerNorm<B>,
    fc1: PpLinear<B>,
    fc2: PpLinear<B>,
    final_layer_norm: PpLayerNorm<B>,
}

#[derive(Debug)]
pub(crate) struct PpDocLayoutDecoder<B: Backend> {
    query_pos_head: PpMlp<B>,
    layers: Vec<PpDecoderLayer<B>>,
    order_head: Vec<PpLinear<B>>,
    decoder_norm: PpLayerNorm<B>,
    global_pointer: PpLinear<B>,
}

#[derive(Debug)]
pub(crate) struct PpDocLayoutDetector<B: Backend> {
    backbone: PpBackboneFeatureProjector<B>,
    encoder: PpHybridEncoderConvs<B>,
    detection_head: PpEncoderDetectionHead<B>,
    decoder: PpDocLayoutDecoder<B>,
}

#[derive(Debug)]
pub(crate) struct PpDocLayoutRawOutput<B: Backend> {
    pub(crate) scores: Tensor<B, 3>,
    pub(crate) boxes: Tensor<B, 3>,
    pub(crate) order_features: Tensor<B, 3>,
}

#[derive(Debug)]
pub(crate) struct PpDocLayoutRuntime<B: Backend> {
    config: PpDocLayoutConfig,
    preprocessor: PpDocLayoutPreprocessorConfig,
    detector: PpDocLayoutDetector<B>,
}

impl<B> PpEncoderInputProjection<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn from_safetensors(
        tensors: &SafeTensors<'_>,
        input_channels: &[usize],
        output_channels: usize,
        device: &B::Device,
    ) -> Result<Self> {
        Self::from_safetensors_with_prefix(
            tensors,
            "model.encoder_input_proj",
            input_channels,
            output_channels,
            device,
        )
    }

    pub(crate) fn from_safetensors_with_prefix(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        input_channels: &[usize],
        output_channels: usize,
        device: &B::Device,
    ) -> Result<Self> {
        let mut projections = Vec::with_capacity(input_channels.len());
        for (index, channels) in input_channels.iter().copied().enumerate() {
            projections.push(PpConv1x1BatchNorm::from_safetensors(
                tensors,
                &format!("{prefix}.{index}.0"),
                &format!("{prefix}.{index}.1"),
                channels,
                output_channels,
                device,
            )?);
        }

        Ok(Self { projections })
    }

    pub(crate) fn forward(
        &self,
        features: Vec<Tensor<B, 4>>,
    ) -> Result<Vec<Tensor<B, 4>>> {
        if features.len() != self.projections.len() {
            bail!(
                "PP-DocLayoutV3 expected {} feature maps, got {}",
                self.projections.len(),
                features.len()
            );
        }

        Ok(features
            .into_iter()
            .zip(self.projections.iter())
            .map(|(feature, projection)| projection.forward(feature))
            .collect())
    }
}

impl<B> PpConvBatchNorm<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn from_safetensors(
        tensors: &SafeTensors<'_>,
        conv_prefix: &str,
        norm_prefix: &str,
        input_channels: usize,
        output_channels: usize,
        kernel_size: [usize; 2],
        stride: [usize; 2],
        padding: [usize; 4],
        activation: PpActivation,
        device: &B::Device,
    ) -> Result<Self> {
        Self::from_safetensors_grouped(
            tensors,
            conv_prefix,
            norm_prefix,
            input_channels,
            output_channels,
            kernel_size,
            stride,
            padding,
            1,
            activation,
            device,
        )
    }

    pub(crate) fn from_safetensors_grouped(
        tensors: &SafeTensors<'_>,
        conv_prefix: &str,
        norm_prefix: &str,
        input_channels: usize,
        output_channels: usize,
        kernel_size: [usize; 2],
        stride: [usize; 2],
        padding: [usize; 4],
        groups: usize,
        activation: PpActivation,
        device: &B::Device,
    ) -> Result<Self> {
        let (scale, bias) =
            fused_batch_norm_params(tensors, norm_prefix, output_channels)?;
        let mut weight = read_f32_values(
            tensors,
            &format!("{conv_prefix}.weight"),
            &[
                output_channels,
                input_channels / groups,
                kernel_size[0],
                kernel_size[1],
            ],
        )?;
        let output_stride =
            (input_channels / groups) * kernel_size[0] * kernel_size[1];
        for output in 0..output_channels {
            let scale = scale[output];
            let start = output * output_stride;
            let end = start + output_stride;
            for value in &mut weight[start..end] {
                *value *= scale;
            }
        }

        let conv = PpConv2d {
            weight: Tensor::from_data(
                TensorData::new(
                    weight,
                    [
                        output_channels,
                        input_channels / groups,
                        kernel_size[0],
                        kernel_size[1],
                    ],
                ),
                device,
            ),
            bias: Tensor::from_data(
                TensorData::new(bias, [output_channels]),
                device,
            ),
            stride,
            padding,
            dilation: [1, 1],
            groups,
        };

        Ok(Self { conv, activation })
    }

    pub(crate) fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        let normalized = self.conv.forward(x);

        match self.activation {
            PpActivation::Identity => normalized,
            PpActivation::Relu => relu(normalized),
            PpActivation::Silu => silu(normalized),
        }
    }
}

impl<B> PpHgnetStem<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn from_safetensors(
        tensors: &SafeTensors<'_>,
        device: &B::Device,
    ) -> Result<Self> {
        let prefix = "model.backbone.model.embedder";
        Ok(Self {
            stem1: load_hgnet_conv(
                tensors,
                prefix,
                "stem1",
                3,
                32,
                [3, 3],
                [2, 2],
                device,
            )?,
            stem2a: load_hgnet_conv(
                tensors,
                prefix,
                "stem2a",
                32,
                16,
                [2, 2],
                [1, 1],
                device,
            )?,
            stem2b: load_hgnet_conv(
                tensors,
                prefix,
                "stem2b",
                16,
                32,
                [2, 2],
                [1, 1],
                device,
            )?,
            stem3: load_hgnet_conv(
                tensors,
                prefix,
                "stem3",
                64,
                32,
                [3, 3],
                [2, 2],
                device,
            )?,
            stem4: load_hgnet_conv(
                tensors,
                prefix,
                "stem4",
                32,
                48,
                [1, 1],
                [1, 1],
                device,
            )?,
        })
    }

    pub(crate) fn forward(&self, pixel_values: Tensor<B, 4>) -> Tensor<B, 4> {
        let stem1 = self.stem1.forward(pixel_values);
        let embedding = pad_right_bottom(stem1);
        let stem2a = self.stem2a.forward(embedding.clone());
        let stem2 = self.stem2b.forward(pad_right_bottom(stem2a));
        let pooled =
            max_pool2d(embedding, [2, 2], [1, 1], [0, 0], [1, 1], true);
        let fused = Tensor::cat(vec![pooled, stem2], 1);

        let stem3 = self.stem3.forward(fused);
        self.stem4.forward(stem3)
    }
}

impl<B> PpHgnetBasicLayer<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        input_channels: usize,
        middle_channels: usize,
        output_channels: usize,
        layer_count: usize,
        kernel_size: [usize; 2],
        residual: bool,
        light_block: bool,
        device: &B::Device,
    ) -> Result<Self> {
        let mut layers = Vec::with_capacity(layer_count);
        for layer_index in 0..layer_count {
            let layer_input_channels = if layer_index == 0 {
                input_channels
            } else {
                middle_channels
            };
            let layer_prefix = format!("{prefix}.layers.{layer_index}");
            if light_block {
                layers.push(PpHgnetLayer::Light {
                    pointwise: load_conv_layer(
                        tensors,
                        &format!("{layer_prefix}.conv1"),
                        layer_input_channels,
                        middle_channels,
                        [1, 1],
                        [1, 1],
                        1,
                        PpActivation::Identity,
                        device,
                    )?,
                    depthwise: load_conv_layer(
                        tensors,
                        &format!("{layer_prefix}.conv2"),
                        middle_channels,
                        middle_channels,
                        kernel_size,
                        [1, 1],
                        middle_channels,
                        PpActivation::Relu,
                        device,
                    )?,
                });
            } else {
                layers.push(PpHgnetLayer::Conv(load_conv_layer(
                    tensors,
                    &layer_prefix,
                    layer_input_channels,
                    middle_channels,
                    kernel_size,
                    [1, 1],
                    1,
                    PpActivation::Relu,
                    device,
                )?));
            }
        }

        let aggregation_input_channels =
            input_channels + layer_count * middle_channels;
        Ok(Self {
            layers,
            aggregation_squeeze: load_conv_layer(
                tensors,
                &format!("{prefix}.aggregation.0"),
                aggregation_input_channels,
                output_channels / 2,
                [1, 1],
                [1, 1],
                1,
                PpActivation::Relu,
                device,
            )?,
            aggregation_excitation: load_conv_layer(
                tensors,
                &format!("{prefix}.aggregation.1"),
                output_channels / 2,
                output_channels,
                [1, 1],
                [1, 1],
                1,
                PpActivation::Relu,
                device,
            )?,
            residual,
        })
    }

    pub(crate) fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        let identity = x.clone();
        let mut outputs = vec![x.clone()];
        let mut hidden = x;
        for layer in &self.layers {
            hidden = layer.forward(hidden);
            outputs.push(hidden.clone());
        }

        let aggregated = self
            .aggregation_excitation
            .forward(self.aggregation_squeeze.forward(Tensor::cat(outputs, 1)));
        if self.residual {
            return aggregated + identity;
        }

        aggregated
    }
}

impl<B> PpHgnetLayer<B>
where
    B: Backend<FloatElem = f32>,
{
    fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        match self {
            Self::Conv(layer) => layer.forward(x),
            Self::Light {
                pointwise,
                depthwise,
            } => depthwise.forward(pointwise.forward(x)),
        }
    }
}

impl<B> PpHgnetStage<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn stage0_from_safetensors(
        tensors: &SafeTensors<'_>,
        device: &B::Device,
    ) -> Result<Self> {
        Self::stage_from_safetensors(
            tensors,
            0,
            48,
            48,
            128,
            1,
            6,
            false,
            false,
            [3, 3],
            device,
        )
    }

    pub(crate) fn stage1_from_safetensors(
        tensors: &SafeTensors<'_>,
        device: &B::Device,
    ) -> Result<Self> {
        Self::stage_from_safetensors(
            tensors,
            1,
            128,
            96,
            512,
            1,
            6,
            true,
            false,
            [3, 3],
            device,
        )
    }

    pub(crate) fn stage2_from_safetensors(
        tensors: &SafeTensors<'_>,
        device: &B::Device,
    ) -> Result<Self> {
        Self::stage_from_safetensors(
            tensors,
            2,
            512,
            192,
            1024,
            3,
            6,
            true,
            true,
            [5, 5],
            device,
        )
    }

    pub(crate) fn stage3_from_safetensors(
        tensors: &SafeTensors<'_>,
        device: &B::Device,
    ) -> Result<Self> {
        Self::stage_from_safetensors(
            tensors,
            3,
            1024,
            384,
            2048,
            1,
            6,
            true,
            true,
            [5, 5],
            device,
        )
    }

    pub(crate) fn stage_from_safetensors(
        tensors: &SafeTensors<'_>,
        stage_index: usize,
        input_channels: usize,
        middle_channels: usize,
        output_channels: usize,
        block_count: usize,
        layer_count: usize,
        downsample: bool,
        light_block: bool,
        kernel_size: [usize; 2],
        device: &B::Device,
    ) -> Result<Self> {
        let stage_prefix =
            format!("model.backbone.model.encoder.stages.{stage_index}");
        let downsample_layer = if downsample {
            Some(load_conv_layer(
                tensors,
                &format!("{stage_prefix}.downsample"),
                input_channels,
                input_channels,
                [3, 3],
                [2, 2],
                input_channels,
                PpActivation::Identity,
                device,
            )?)
        } else {
            None
        };

        let mut blocks = Vec::with_capacity(block_count);
        for block_index in 0..block_count {
            blocks.push(PpHgnetBasicLayer::from_safetensors(
                tensors,
                &format!("{stage_prefix}.blocks.{block_index}"),
                if block_index == 0 {
                    input_channels
                } else {
                    output_channels
                },
                middle_channels,
                output_channels,
                layer_count,
                kernel_size,
                block_index != 0,
                light_block,
                device,
            )?);
        }

        Ok(Self {
            downsample: downsample_layer,
            blocks,
        })
    }

    pub(crate) fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        let mut hidden = match &self.downsample {
            Some(downsample) => downsample.forward(x),
            None => x,
        };
        for block in &self.blocks {
            hidden = block.forward(hidden);
        }
        hidden
    }
}

impl<B> PpHgnetBackbone<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn from_safetensors(
        tensors: &SafeTensors<'_>,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            stem: PpHgnetStem::from_safetensors(tensors, device)?,
            stage0: PpHgnetStage::stage0_from_safetensors(tensors, device)?,
            stage1: PpHgnetStage::stage1_from_safetensors(tensors, device)?,
            stage2: PpHgnetStage::stage2_from_safetensors(tensors, device)?,
            stage3: PpHgnetStage::stage3_from_safetensors(tensors, device)?,
        })
    }

    pub(crate) fn forward(
        &self,
        pixel_values: Tensor<B, 4>,
    ) -> Vec<Tensor<B, 4>> {
        let started = Instant::now();
        let stem = self.stem.forward(pixel_values);
        sync_profile_event::<B>(
            &stem.device(),
            "layout.backbone.stem",
            started,
        );
        let started = Instant::now();
        let stage0 = self.stage0.forward(stem);
        sync_profile_event::<B>(
            &stage0.device(),
            "layout.backbone.stage0",
            started,
        );
        let started = Instant::now();
        let stage1 = self.stage1.forward(stage0);
        sync_profile_event::<B>(
            &stage1.device(),
            "layout.backbone.stage1",
            started,
        );
        let started = Instant::now();
        let stage2 = self.stage2.forward(stage1.clone());
        sync_profile_event::<B>(
            &stage2.device(),
            "layout.backbone.stage2",
            started,
        );
        let started = Instant::now();
        let stage3 = self.stage3.forward(stage2.clone());
        sync_profile_event::<B>(
            &stage3.device(),
            "layout.backbone.stage3",
            started,
        );

        vec![stage1, stage2, stage3]
    }
}

impl<B> PpBackboneFeatureProjector<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn from_safetensors(
        tensors: &SafeTensors<'_>,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            backbone: PpHgnetBackbone::from_safetensors(tensors, device)?,
            projection: PpEncoderInputProjection::from_safetensors(
                tensors,
                &[512, 1024, 2048],
                256,
                device,
            )?,
        })
    }

    pub(crate) fn forward(
        &self,
        pixel_values: Tensor<B, 4>,
    ) -> Result<Vec<Tensor<B, 4>>> {
        let features = self.backbone.forward(pixel_values);
        let started = Instant::now();
        let projected = self.projection.forward(features)?;
        sync_profile_event::<B>(
            &projected[0].device(),
            "layout.backbone.projection",
            started,
        );
        Ok(projected)
    }
}

impl<B> PpHybridEncoderConvs<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn from_safetensors(
        tensors: &SafeTensors<'_>,
        device: &B::Device,
    ) -> Result<Self> {
        let mut lateral_convs = Vec::with_capacity(2);
        let mut downsample_convs = Vec::with_capacity(2);
        let mut fpn_blocks = Vec::with_capacity(2);
        let mut pan_blocks = Vec::with_capacity(2);
        for index in 0..2 {
            lateral_convs.push(load_conv_norm_layer(
                tensors,
                &format!("model.encoder.lateral_convs.{index}"),
                256,
                256,
                [1, 1],
                [1, 1],
                PpActivation::Silu,
                device,
            )?);
            downsample_convs.push(load_conv_norm_layer(
                tensors,
                &format!("model.encoder.downsample_convs.{index}"),
                256,
                256,
                [3, 3],
                [2, 2],
                PpActivation::Silu,
                device,
            )?);
            fpn_blocks.push(PpCspRepLayer::from_safetensors(
                tensors,
                &format!("model.encoder.fpn_blocks.{index}"),
                device,
            )?);
            pan_blocks.push(PpCspRepLayer::from_safetensors(
                tensors,
                &format!("model.encoder.pan_blocks.{index}"),
                device,
            )?);
        }

        Ok(Self {
            aifi: PpAifiLayer::from_safetensors(
                tensors,
                "model.encoder.encoder.0.layers.0",
                device,
            )?,
            lateral_convs,
            downsample_convs,
            fpn_blocks,
            pan_blocks,
        })
    }

    pub(crate) fn forward(
        &self,
        mut features: Vec<Tensor<B, 4>>,
    ) -> Vec<Tensor<B, 4>> {
        let started = Instant::now();
        features[2] = self.aifi.forward(features[2].clone());
        sync_profile_event::<B>(
            &features[2].device(),
            "layout.encoder.aifi",
            started,
        );
        let mut fpn_features =
            vec![features.pop().expect("top feature must exist")];
        for index in 0..2 {
            let started = Instant::now();
            let backbone_feature =
                features.pop().expect("backbone feature must exist");
            let top = self.lateral_convs[index].forward(
                fpn_features.pop().expect("top FPN feature must exist"),
            );
            let [_batch, _channels, height, width] = backbone_feature.dims();
            let fused = Tensor::cat(
                vec![
                    upsample_nearest_to(top.clone(), height, width),
                    backbone_feature,
                ],
                1,
            );
            fpn_features.push(top);
            fpn_features.push(self.fpn_blocks[index].forward(fused));
            sync_profile_event_index::<B>(
                &fpn_features
                    .last()
                    .expect("FPN feature must exist")
                    .device(),
                "layout.encoder.fpn",
                index,
                started,
            );
        }
        fpn_features.reverse();

        let mut pan_features = vec![fpn_features.remove(0)];
        for index in 0..2 {
            let started = Instant::now();
            let downsampled = self.downsample_convs[index].forward(
                pan_features.last().expect("PAN feature must exist").clone(),
            );
            let fused =
                Tensor::cat(vec![downsampled, fpn_features.remove(0)], 1);
            pan_features.push(self.pan_blocks[index].forward(fused));
            sync_profile_event_index::<B>(
                &pan_features
                    .last()
                    .expect("PAN feature must exist")
                    .device(),
                "layout.encoder.pan",
                index,
                started,
            );
        }

        pan_features
    }
}

impl<B> PpRepVggBlock<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            conv: load_fused_rep_vgg_block(tensors, prefix, 256, device)?,
        })
    }

    fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        self.conv.forward(x)
    }
}

impl<B> PpCspRepLayer<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        device: &B::Device,
    ) -> Result<Self> {
        let mut bottlenecks = Vec::with_capacity(3);
        for index in 0..3 {
            bottlenecks.push(PpRepVggBlock::from_safetensors(
                tensors,
                &format!("{prefix}.bottlenecks.{index}"),
                device,
            )?);
        }

        Ok(Self {
            conv1: load_conv_norm_layer(
                tensors,
                &format!("{prefix}.conv1"),
                512,
                256,
                [1, 1],
                [1, 1],
                PpActivation::Silu,
                device,
            )?,
            conv2: load_conv_norm_layer(
                tensors,
                &format!("{prefix}.conv2"),
                512,
                256,
                [1, 1],
                [1, 1],
                PpActivation::Silu,
                device,
            )?,
            bottlenecks,
        })
    }

    fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        let mut left = self.conv1.forward(x.clone());
        for bottleneck in &self.bottlenecks {
            left = bottleneck.forward(left);
        }
        left + self.conv2.forward(x)
    }
}

impl<B> PpLinear<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        input_features: usize,
        output_features: usize,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            weight: read_linear_weight(
                tensors,
                &format!("{prefix}.weight"),
                input_features,
                output_features,
                device,
            )?,
            bias: read_f32_tensor(
                tensors,
                &format!("{prefix}.bias"),
                &[output_features],
                device,
            )?,
        })
    }

    fn forward(&self, x: Tensor<B, 3>) -> Tensor<B, 3> {
        let [batch, sequence, input_features] = x.dims();
        let flattened = x.reshape([batch * sequence, input_features]);
        let projected = flattened.matmul(self.weight.clone())
            + self.bias.clone().unsqueeze();
        let [_rows, output_features] = projected.dims();

        projected.reshape([batch, sequence, output_features])
    }
}

impl<B> PpMlp<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        dims: &[(usize, usize)],
        device: &B::Device,
    ) -> Result<Self> {
        let mut layers = Vec::with_capacity(dims.len());
        for (index, (input_features, output_features)) in
            dims.iter().copied().enumerate()
        {
            layers.push(PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.layers.{index}"),
                input_features,
                output_features,
                device,
            )?);
        }
        Ok(Self { layers })
    }

    fn forward(&self, mut x: Tensor<B, 3>) -> Tensor<B, 3> {
        for (index, layer) in self.layers.iter().enumerate() {
            x = layer.forward(x);
            if index + 1 < self.layers.len() {
                x = relu(x);
            }
        }
        x
    }
}

impl<B> PpLayerNorm<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        size: usize,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            weight: read_f32_tensor(
                tensors,
                &format!("{prefix}.weight"),
                &[size],
                device,
            )?,
            bias: read_f32_tensor(
                tensors,
                &format!("{prefix}.bias"),
                &[size],
                device,
            )?,
            epsilon: 1.0e-5,
        })
    }

    fn forward(&self, x: Tensor<B, 3>) -> Tensor<B, 3> {
        let [_batch, _sequence, hidden] = x.dims();
        let mean = x.clone().mean_dim(2);
        let centered = x - mean;
        let variance = centered.clone().powf_scalar(2.0).mean_dim(2);
        centered
            * (variance + self.epsilon).sqrt().recip()
            * self.weight.clone().reshape([1, 1, hidden])
            + self.bias.clone().reshape([1, 1, hidden])
    }
}

impl<B> PpAifiLayer<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            q_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.self_attn.q_proj"),
                256,
                256,
                device,
            )?,
            k_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.self_attn.k_proj"),
                256,
                256,
                device,
            )?,
            v_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.self_attn.v_proj"),
                256,
                256,
                device,
            )?,
            out_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.self_attn.out_proj"),
                256,
                256,
                device,
            )?,
            self_attn_layer_norm: PpLayerNorm::from_safetensors(
                tensors,
                &format!("{prefix}.self_attn_layer_norm"),
                256,
                device,
            )?,
            fc1: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.fc1"),
                256,
                1024,
                device,
            )?,
            fc2: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.fc2"),
                1024,
                256,
                device,
            )?,
            final_layer_norm: PpLayerNorm::from_safetensors(
                tensors,
                &format!("{prefix}.final_layer_norm"),
                256,
                device,
            )?,
        })
    }

    fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        let [batch, channels, height, width] = x.dims();
        let hidden = x.swap_dims(1, 3).swap_dims(1, 2).reshape([
            batch,
            height * width,
            channels,
        ]);
        // RT-DETR's AIFI 2D-sincos positional embedding is a runtime-computed
        // buffer (not a stored weight); compute it natively here. Verified
        // identical (max abs diff ~6e-8) to PaddlePaddle's baked `eager_tmp_0`.
        let position =
            aifi_position_embedding::<B>(height, width, &hidden.device());
        let residual = hidden.clone();
        let attended = self.self_attention(hidden, position);
        let hidden = self.self_attn_layer_norm.forward(residual + attended);
        let residual = hidden.clone();
        let mlp = self.fc2.forward(gelu(self.fc1.forward(hidden)));
        self.final_layer_norm
            .forward(residual + mlp)
            .reshape([batch, height, width, channels])
            .swap_dims(1, 2)
            .swap_dims(1, 3)
    }

    fn self_attention(
        &self,
        hidden: Tensor<B, 3>,
        position: Tensor<B, 3>,
    ) -> Tensor<B, 3> {
        let [batch, sequence, _hidden] = hidden.dims();
        let heads = 8;
        let head_dim = 32;
        let query_key_input = hidden.clone() + position;
        let q = self
            .q_proj
            .forward(query_key_input.clone())
            .reshape([batch, sequence, heads, head_dim])
            .swap_dims(1, 2);
        let k = self
            .k_proj
            .forward(query_key_input)
            .reshape([batch, sequence, heads, head_dim])
            .swap_dims(1, 2);
        let v = self
            .v_proj
            .forward(hidden)
            .reshape([batch, sequence, heads, head_dim])
            .swap_dims(1, 2);
        let scores = q.matmul(k.swap_dims(2, 3)) / (head_dim as f64).sqrt();
        let context = softmax(scores, 3)
            .matmul(v)
            .swap_dims(1, 2)
            .reshape([batch, sequence, 256]);

        self.out_proj.forward(context)
    }
}

impl<B> PpEncoderDetectionHead<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        output_classes: usize,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            decoder_input_proj:
                PpEncoderInputProjection::from_safetensors_with_prefix(
                    tensors,
                    "model.decoder_input_proj",
                    &[256, 256, 256],
                    256,
                    device,
                )?,
            enc_output: PpLinear::from_safetensors(
                tensors,
                "model.enc_output.0",
                256,
                256,
                device,
            )?,
            enc_output_norm: PpLayerNorm::from_safetensors(
                tensors,
                "model.enc_output.1",
                256,
                device,
            )?,
            enc_score_head: PpLinear::from_safetensors(
                tensors,
                "model.enc_score_head",
                256,
                output_classes,
                device,
            )?,
            enc_bbox_head: PpMlp::from_safetensors(
                tensors,
                "model.enc_bbox_head",
                &[(256, 256), (256, 256), (256, 4)],
                device,
            )?,
        })
    }

    fn forward(
        &self,
        features: Vec<Tensor<B, 4>>,
    ) -> Result<(
        Tensor<B, 3>,
        Tensor<B, 3>,
        Tensor<B, 3>,
        Tensor<B, 3>,
        Vec<(usize, usize)>,
    )> {
        let started = Instant::now();
        let (anchors, valid_mask) =
            generate_encoder_anchors(&features, &features[0].device())?;
        event!(
            Level::INFO,
            phase = "layout.detection_head.anchors",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let started = Instant::now();
        let projected = self.decoder_input_proj.forward(features)?;
        event!(
            Level::INFO,
            phase = "layout.detection_head.input_proj",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let spatial_shapes = projected
            .iter()
            .map(|feature| {
                let [_batch, _channels, height, width] = feature.dims();
                (height, width)
            })
            .collect();
        let started = Instant::now();
        let source_flatten = flatten_feature_maps(projected);
        let memory = source_flatten.clone() * valid_mask.clone();
        event!(
            Level::INFO,
            phase = "layout.detection_head.flatten_mask",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let started = Instant::now();
        let encoded = self
            .enc_output_norm
            .forward(self.enc_output.forward(memory.clone()));
        event!(
            Level::INFO,
            phase = "layout.detection_head.enc_output",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let started = Instant::now();
        let encoder_scores = self.enc_score_head.forward(encoded.clone());
        event!(
            Level::INFO,
            phase = "layout.detection_head.score_head",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let started = Instant::now();
        let encoder_boxes =
            self.enc_bbox_head.forward(encoded.clone()) + anchors;
        event!(
            Level::INFO,
            phase = "layout.detection_head.bbox_head",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        Ok((
            encoder_scores,
            encoder_boxes,
            encoded,
            source_flatten,
            spatial_shapes,
        ))
    }
}

impl<B> PpDecoderAttention<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            q_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.q_proj"),
                256,
                256,
                device,
            )?,
            k_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.k_proj"),
                256,
                256,
                device,
            )?,
            v_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.v_proj"),
                256,
                256,
                device,
            )?,
            out_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.out_proj"),
                256,
                256,
                device,
            )?,
        })
    }

    fn forward(
        &self,
        hidden: Tensor<B, 3>,
        query_pos: Tensor<B, 3>,
    ) -> Tensor<B, 3> {
        let [batch, sequence, _hidden] = hidden.dims();
        let heads = 8;
        let head_dim = 32;
        let query = hidden.clone() + query_pos.clone();
        let key = hidden.clone() + query_pos;
        let q = self
            .q_proj
            .forward(query)
            .reshape([batch, sequence, heads, head_dim])
            .swap_dims(1, 2);
        let k = self
            .k_proj
            .forward(key)
            .reshape([batch, sequence, heads, head_dim])
            .swap_dims(1, 2);
        let v = self
            .v_proj
            .forward(hidden)
            .reshape([batch, sequence, heads, head_dim])
            .swap_dims(1, 2);
        let scores = q.matmul(k.swap_dims(2, 3)) / (head_dim as f64).sqrt();
        let context = softmax(scores, 3)
            .matmul(v)
            .swap_dims(1, 2)
            .reshape([batch, sequence, 256]);

        self.out_proj.forward(context)
    }
}

impl<B> PpDecoderCrossAttention<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            sampling_offsets: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.sampling_offsets"),
                256,
                192,
                device,
            )?,
            attention_weights: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.attention_weights"),
                256,
                96,
                device,
            )?,
            value_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.value_proj"),
                256,
                256,
                device,
            )?,
            output_proj: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.output_proj"),
                256,
                256,
                device,
            )?,
        })
    }

    fn forward(
        &self,
        hidden: Tensor<B, 3>,
        query_pos: Tensor<B, 3>,
        memory: Tensor<B, 3>,
        reference_boxes: Tensor<B, 3>,
        spatial_shapes: &[(usize, usize)],
    ) -> Tensor<B, 3> {
        let [batch, queries, _hidden] = hidden.dims();
        let [_memory_batch, sequence, _memory_hidden] = memory.dims();
        let heads = 8;
        let head_dim = 32;
        let levels = spatial_shapes.len();
        let device = hidden.device();
        let value = self
            .value_proj
            .forward(memory)
            .reshape([batch, sequence, heads, head_dim]);
        let query = hidden + query_pos;
        let offsets = self
            .sampling_offsets
            .forward(query.clone())
            .reshape([batch, queries, heads, levels, 4, 2]);
        let weights = self.attention_weights.forward(query).reshape([
            batch,
            queries,
            heads,
            levels * 4,
        ]);
        let context = deformable_attention_context(
            value,
            offsets,
            weights,
            reference_boxes,
            spatial_shapes,
        )
        .unwrap_or_else(|_| {
            Tensor::zeros([batch, queries, heads * head_dim], &device)
        });

        self.output_proj.forward(context)
    }
}

impl<B> PpDecoderLayer<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        prefix: &str,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            self_attn: PpDecoderAttention::from_safetensors(
                tensors,
                &format!("{prefix}.self_attn"),
                device,
            )?,
            self_attn_layer_norm: PpLayerNorm::from_safetensors(
                tensors,
                &format!("{prefix}.self_attn_layer_norm"),
                256,
                device,
            )?,
            encoder_attn: PpDecoderCrossAttention::from_safetensors(
                tensors,
                &format!("{prefix}.encoder_attn"),
                device,
            )?,
            encoder_attn_layer_norm: PpLayerNorm::from_safetensors(
                tensors,
                &format!("{prefix}.encoder_attn_layer_norm"),
                256,
                device,
            )?,
            fc1: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.fc1"),
                256,
                1024,
                device,
            )?,
            fc2: PpLinear::from_safetensors(
                tensors,
                &format!("{prefix}.fc2"),
                1024,
                256,
                device,
            )?,
            final_layer_norm: PpLayerNorm::from_safetensors(
                tensors,
                &format!("{prefix}.final_layer_norm"),
                256,
                device,
            )?,
        })
    }

    fn forward(
        &self,
        hidden: Tensor<B, 3>,
        query_pos: Tensor<B, 3>,
        memory: Tensor<B, 3>,
        reference_boxes: Tensor<B, 3>,
        spatial_shapes: &[(usize, usize)],
        layer_index: usize,
    ) -> Tensor<B, 3> {
        let residual = hidden.clone();
        let started = Instant::now();
        let hidden = self.self_attn_layer_norm.forward(
            residual + self.self_attn.forward(hidden, query_pos.clone()),
        );
        event!(
            Level::INFO,
            phase = "layout.decoder.layer.self_attn",
            layer = layer_index,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let residual = hidden.clone();
        let started = Instant::now();
        let hidden = self.encoder_attn_layer_norm.forward(
            residual
                + self.encoder_attn.forward(
                    hidden,
                    query_pos,
                    memory,
                    reference_boxes,
                    spatial_shapes,
                ),
        );
        event!(
            Level::INFO,
            phase = "layout.decoder.layer.cross_attn",
            layer = layer_index,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let residual = hidden.clone();
        let started = Instant::now();
        let mlp = self.fc2.forward(relu(self.fc1.forward(hidden)));
        let output = self.final_layer_norm.forward(residual + mlp);
        event!(
            Level::INFO,
            phase = "layout.decoder.layer.mlp",
            layer = layer_index,
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        output
    }
}

impl<B> PpDocLayoutDecoder<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        device: &B::Device,
    ) -> Result<Self> {
        let mut layers = Vec::with_capacity(6);
        for index in 0..6 {
            layers.push(PpDecoderLayer::from_safetensors(
                tensors,
                &format!("model.decoder.layers.{index}"),
                device,
            )?);
        }
        let mut order_head = Vec::with_capacity(6);
        for index in 0..6 {
            order_head.push(PpLinear::from_safetensors(
                tensors,
                &format!("model.decoder_order_head.{index}"),
                256,
                256,
                device,
            )?);
        }

        Ok(Self {
            query_pos_head: PpMlp::from_safetensors(
                tensors,
                "model.decoder.query_pos_head",
                &[(4, 512), (512, 256)],
                device,
            )?,
            layers,
            order_head,
            decoder_norm: PpLayerNorm::from_safetensors(
                tensors,
                "model.decoder_norm",
                256,
                device,
            )?,
            global_pointer: PpLinear::from_safetensors(
                tensors,
                "model.decoder_global_pointer.dense",
                256,
                128,
                device,
            )?,
        })
    }

    fn forward(
        &self,
        output_memory: Tensor<B, 3>,
        encoder_hidden_states: Tensor<B, 3>,
        encoder_scores: Tensor<B, 3>,
        proposal_boxes: Tensor<B, 3>,
        bbox_head: &PpMlp<B>,
        score_head: &PpLinear<B>,
        spatial_shapes: &[(usize, usize)],
        num_queries: usize,
    ) -> Result<(Tensor<B, 3>, Tensor<B, 3>, Tensor<B, 3>)> {
        let [batch, sequence, hidden] = output_memory.dims();
        if batch != 1 {
            bail!("PP-DocLayoutV3 decoder currently expects batch size 1");
        }
        if sequence < num_queries {
            bail!(
                "PP-DocLayoutV3 decoder needs at least {num_queries} encoder proposals, got {sequence}"
            );
        }

        let started = Instant::now();
        let indices = topk_proposal_indices(encoder_scores, num_queries)?;
        event!(
            Level::INFO,
            phase = "layout.decoder.topk",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );

        self.forward_selected(
            output_memory,
            encoder_hidden_states,
            proposal_boxes,
            bbox_head,
            score_head,
            spatial_shapes,
            &indices,
            hidden,
        )
    }

    /// Runs decoder with async proposal selection for browser WebGPU.
    async fn forward_async(
        &self,
        output_memory: Tensor<B, 3>,
        encoder_hidden_states: Tensor<B, 3>,
        encoder_scores: Tensor<B, 3>,
        proposal_boxes: Tensor<B, 3>,
        bbox_head: &PpMlp<B>,
        score_head: &PpLinear<B>,
        spatial_shapes: &[(usize, usize)],
        num_queries: usize,
    ) -> Result<(Tensor<B, 3>, Tensor<B, 3>, Tensor<B, 3>)> {
        let [batch, sequence, hidden] = output_memory.dims();
        if batch != 1 {
            bail!("PP-DocLayoutV3 decoder currently expects batch size 1");
        }
        if sequence < num_queries {
            bail!(
                "PP-DocLayoutV3 decoder needs at least {num_queries} encoder proposals, got {sequence}"
            );
        }

        let started = Instant::now();
        let indices =
            topk_proposal_indices_async(encoder_scores, num_queries).await?;
        event!(
            Level::INFO,
            phase = "layout.decoder.topk",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );

        self.forward_selected(
            output_memory,
            encoder_hidden_states,
            proposal_boxes,
            bbox_head,
            score_head,
            spatial_shapes,
            &indices,
            hidden,
        )
    }

    /// Runs decoder layers after top-k proposal indices are known.
    fn forward_selected(
        &self,
        output_memory: Tensor<B, 3>,
        encoder_hidden_states: Tensor<B, 3>,
        proposal_boxes: Tensor<B, 3>,
        bbox_head: &PpMlp<B>,
        score_head: &PpLinear<B>,
        spatial_shapes: &[(usize, usize)],
        indices: &[usize],
        hidden: usize,
    ) -> Result<(Tensor<B, 3>, Tensor<B, 3>, Tensor<B, 3>)> {
        let started = Instant::now();
        let query = gather_sequence(output_memory, indices, hidden)?;
        let mut reference =
            sigmoid_tensor(gather_sequence(proposal_boxes, indices, 4)?);
        event!(
            Level::INFO,
            phase = "layout.decoder.gather",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let mut decoded = query;
        for (layer_index, layer) in self.layers.iter().enumerate() {
            let started = Instant::now();
            let query_pos = self.query_pos_head.forward(reference.clone());
            event!(
                Level::INFO,
                phase = "layout.decoder.query_pos",
                layer = layer_index,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
            );

            let started = Instant::now();
            decoded = layer.forward(
                decoded,
                query_pos,
                encoder_hidden_states.clone(),
                reference.clone(),
                spatial_shapes,
                layer_index,
            );
            event!(
                Level::INFO,
                phase = "layout.decoder.layer.total",
                layer = layer_index,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
            );

            let started = Instant::now();
            reference = sigmoid_tensor(
                bbox_head.forward(decoded.clone())
                    + inverse_sigmoid_tensor(reference),
            );
            event!(
                Level::INFO,
                phase = "layout.decoder.bbox_update",
                layer = layer_index,
                elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
            );
        }
        let started = Instant::now();
        let normalized = self.decoder_norm.forward(decoded);
        let class_scores = score_head.forward(normalized.clone());
        let order_features = self.global_pointer.forward(
            self.order_head[self.order_head.len() - 1].forward(normalized),
        );
        event!(
            Level::INFO,
            phase = "layout.decoder.output_heads",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );

        Ok((reference, class_scores, order_features))
    }
}

impl<B> PpDocLayoutDetector<B>
where
    B: Backend<FloatElem = f32>,
{
    fn from_safetensors(
        tensors: &SafeTensors<'_>,
        output_classes: usize,
        device: &B::Device,
    ) -> Result<Self> {
        Ok(Self {
            backbone: PpBackboneFeatureProjector::from_safetensors(
                tensors, device,
            )?,
            encoder: PpHybridEncoderConvs::from_safetensors(tensors, device)?,
            detection_head: PpEncoderDetectionHead::from_safetensors(
                tensors,
                output_classes,
                device,
            )?,
            decoder: PpDocLayoutDecoder::from_safetensors(tensors, device)?,
        })
    }

    fn forward(
        &self,
        pixel_values: Tensor<B, 4>,
    ) -> Result<PpDocLayoutRawOutput<B>> {
        let started = Instant::now();
        let projected = self.backbone.forward(pixel_values)?;
        sync_profile_event::<B>(
            &projected[0].device(),
            "layout.forward.backbone",
            started,
        );

        let started = Instant::now();
        let pan_features = self.encoder.forward(projected);
        sync_profile_event::<B>(
            &pan_features[0].device(),
            "layout.forward.encoder",
            started,
        );

        let started = Instant::now();
        let (
            encoder_scores,
            encoder_boxes,
            output_memory,
            encoder_hidden_states,
            spatial_shapes,
        ) = self.detection_head.forward(pan_features)?;
        sync_profile_event::<B>(
            &output_memory.device(),
            "layout.forward.detection_head",
            started,
        );

        let num_queries = 300.min(encoder_boxes.dims()[1]);
        let started = Instant::now();
        let (reference_boxes, decoder_scores, order_features) =
            self.decoder.forward(
                output_memory,
                encoder_hidden_states,
                encoder_scores,
                encoder_boxes,
                &self.detection_head.enc_bbox_head,
                &self.detection_head.enc_score_head,
                &spatial_shapes,
                num_queries,
            )?;
        sync_profile_event::<B>(
            &decoder_scores.device(),
            "layout.forward.decoder",
            started,
        );
        Ok(PpDocLayoutRawOutput {
            scores: decoder_scores,
            boxes: reference_boxes,
            order_features,
        })
    }

    /// Runs model forward with async readbacks required by browser WebGPU.
    async fn forward_async(
        &self,
        pixel_values: Tensor<B, 4>,
    ) -> Result<PpDocLayoutRawOutput<B>> {
        let started = Instant::now();
        let projected = self.backbone.forward(pixel_values)?;
        sync_profile_event::<B>(
            &projected[0].device(),
            "layout.forward.backbone",
            started,
        );

        let started = Instant::now();
        let pan_features = self.encoder.forward(projected);
        sync_profile_event::<B>(
            &pan_features[0].device(),
            "layout.forward.encoder",
            started,
        );

        let started = Instant::now();
        let (
            encoder_scores,
            encoder_boxes,
            output_memory,
            encoder_hidden_states,
            spatial_shapes,
        ) = self.detection_head.forward(pan_features)?;
        sync_profile_event::<B>(
            &output_memory.device(),
            "layout.forward.detection_head",
            started,
        );

        let num_queries = 300.min(encoder_boxes.dims()[1]);
        let started = Instant::now();
        let (reference_boxes, decoder_scores, order_features) = self
            .decoder
            .forward_async(
                output_memory,
                encoder_hidden_states,
                encoder_scores,
                encoder_boxes,
                &self.detection_head.enc_bbox_head,
                &self.detection_head.enc_score_head,
                &spatial_shapes,
                num_queries,
            )
            .await?;
        sync_profile_event::<B>(
            &decoder_scores.device(),
            "layout.forward.decoder",
            started,
        );
        Ok(PpDocLayoutRawOutput {
            scores: decoder_scores,
            boxes: reference_boxes,
            order_features,
        })
    }
}

fn load_hgnet_conv<B: Backend<FloatElem = f32>>(
    tensors: &SafeTensors<'_>,
    prefix: &str,
    name: &str,
    input_channels: usize,
    output_channels: usize,
    kernel_size: [usize; 2],
    stride: [usize; 2],
    device: &B::Device,
) -> Result<PpConvBatchNorm<B>> {
    load_conv_layer(
        tensors,
        &format!("{prefix}.{name}"),
        input_channels,
        output_channels,
        kernel_size,
        stride,
        1,
        PpActivation::Relu,
        device,
    )
}

/// Computes inference BatchNorm as per-channel conv scale and bias.
///
/// This folds old runtime BatchNorm work into weights at load time so forward
/// executes fewer kernels while preserving the same inference formula.
fn fused_batch_norm_params(
    tensors: &SafeTensors<'_>,
    norm_prefix: &str,
    channels: usize,
) -> Result<(Vec<f32>, Vec<f32>)> {
    let norm_weight = read_f32_values(
        tensors,
        &format!("{norm_prefix}.weight"),
        &[channels],
    )?;
    let norm_bias =
        read_f32_values(tensors, &format!("{norm_prefix}.bias"), &[channels])?;
    let running_mean = read_f32_values(
        tensors,
        &format!("{norm_prefix}.running_mean"),
        &[channels],
    )?;
    let running_var = read_f32_values(
        tensors,
        &format!("{norm_prefix}.running_var"),
        &[channels],
    )?;
    let epsilon = 1.0e-5_f32;
    let mut scale = Vec::with_capacity(channels);
    let mut bias = Vec::with_capacity(channels);
    for channel in 0..channels {
        let channel_scale =
            norm_weight[channel] / (running_var[channel] + epsilon).sqrt();
        scale.push(channel_scale);
        bias.push(norm_bias[channel] - running_mean[channel] * channel_scale);
    }

    Ok((scale, bias))
}

fn load_conv_layer<B: Backend<FloatElem = f32>>(
    tensors: &SafeTensors<'_>,
    prefix: &str,
    input_channels: usize,
    output_channels: usize,
    kernel_size: [usize; 2],
    stride: [usize; 2],
    groups: usize,
    activation: PpActivation,
    device: &B::Device,
) -> Result<PpConvBatchNorm<B>> {
    PpConvBatchNorm::from_safetensors_grouped(
        tensors,
        &format!("{prefix}.convolution"),
        &format!("{prefix}.normalization"),
        input_channels,
        output_channels,
        kernel_size,
        stride,
        [
            (kernel_size[0] - 1) / 2,
            (kernel_size[1] - 1) / 2,
            (kernel_size[0] - 1) / 2,
            (kernel_size[1] - 1) / 2,
        ],
        groups,
        activation,
        device,
    )
}

fn load_conv_norm_layer<B: Backend<FloatElem = f32>>(
    tensors: &SafeTensors<'_>,
    prefix: &str,
    input_channels: usize,
    output_channels: usize,
    kernel_size: [usize; 2],
    stride: [usize; 2],
    activation: PpActivation,
    device: &B::Device,
) -> Result<PpConvBatchNorm<B>> {
    PpConvBatchNorm::from_safetensors(
        tensors,
        &format!("{prefix}.conv"),
        &format!("{prefix}.norm"),
        input_channels,
        output_channels,
        kernel_size,
        stride,
        [
            (kernel_size[0] - 1) / 2,
            (kernel_size[1] - 1) / 2,
            (kernel_size[0] - 1) / 2,
            (kernel_size[1] - 1) / 2,
        ],
        activation,
        device,
    )
}

/// Loads a RepVGG inference block as one fused 3x3 convolution.
///
/// The original graph computes `silu(conv3x3_bn(x) + conv1x1_bn(x))`.
/// During inference both branches are linear, so the 1x1 branch is folded into
/// the center of the 3x3 kernel and the two biases are added.
fn load_fused_rep_vgg_block<B: Backend<FloatElem = f32>>(
    tensors: &SafeTensors<'_>,
    prefix: &str,
    channels: usize,
    device: &B::Device,
) -> Result<PpConvBatchNorm<B>> {
    let (conv1_scale, conv1_bias) = fused_batch_norm_params(
        tensors,
        &format!("{prefix}.conv1.norm"),
        channels,
    )?;
    let mut weight = read_f32_values(
        tensors,
        &format!("{prefix}.conv1.conv.weight"),
        &[channels, channels, 3, 3],
    )?;
    let kernel_stride = channels * 3 * 3;
    for output in 0..channels {
        let scale = conv1_scale[output];
        let start = output * kernel_stride;
        let end = start + kernel_stride;
        for value in &mut weight[start..end] {
            *value *= scale;
        }
    }

    let (conv2_scale, conv2_bias) = fused_batch_norm_params(
        tensors,
        &format!("{prefix}.conv2.norm"),
        channels,
    )?;
    let conv2_weight = read_f32_values(
        tensors,
        &format!("{prefix}.conv2.conv.weight"),
        &[channels, channels, 1, 1],
    )?;
    for output in 0..channels {
        for input in 0..channels {
            let source = output * channels + input;
            let target = output * channels * 9 + input * 9 + 4;
            weight[target] += conv2_weight[source] * conv2_scale[output];
        }
    }
    let bias = conv1_bias
        .into_iter()
        .zip(conv2_bias)
        .map(|(left, right)| left + right)
        .collect::<Vec<_>>();

    Ok(PpConvBatchNorm {
        conv: PpConv2d {
            weight: Tensor::from_data(
                TensorData::new(weight, [channels, channels, 3, 3]),
                device,
            ),
            bias: Tensor::from_data(TensorData::new(bias, [channels]), device),
            stride: [1, 1],
            padding: [1, 1, 1, 1],
            dilation: [1, 1],
            groups: 1,
        },
        activation: PpActivation::Silu,
    })
}

fn upsample_nearest_to<B: Backend<FloatElem = f32>>(
    x: Tensor<B, 4>,
    height: usize,
    width: usize,
) -> Tensor<B, 4> {
    interpolate(
        x,
        [height, width],
        InterpolateOptions::new(InterpolateMode::Nearest),
    )
}

fn pad_right_bottom<B: Backend<FloatElem = f32>>(
    x: Tensor<B, 4>,
) -> Tensor<B, 4> {
    x.pad([(0, 0), (0, 0), (0, 1), (0, 1)], PadMode::Constant(0.0))
}

impl<B> PpConv1x1BatchNorm<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn from_safetensors(
        tensors: &SafeTensors<'_>,
        conv_prefix: &str,
        norm_prefix: &str,
        input_channels: usize,
        output_channels: usize,
        device: &B::Device,
    ) -> Result<Self> {
        let (scale, bias) =
            fused_batch_norm_params(tensors, norm_prefix, output_channels)?;
        let values = read_f32_values(
            tensors,
            &format!("{conv_prefix}.weight"),
            &[output_channels, input_channels, 1, 1],
        )?;
        let mut transposed = vec![0.0; values.len()];
        for output in 0..output_channels {
            for input in 0..input_channels {
                transposed[input * output_channels + output] =
                    values[output * input_channels + input] * scale[output];
            }
        }

        Ok(Self {
            weight: Tensor::from_data(
                TensorData::new(transposed, [input_channels, output_channels]),
                device,
            ),
            bias: Tensor::from_data(
                TensorData::new(bias, [output_channels]),
                device,
            ),
        })
    }

    pub(crate) fn forward(&self, x: Tensor<B, 4>) -> Tensor<B, 4> {
        let [batch, channels, height, width] = x.dims();
        let flattened = x
            .swap_dims(1, 3)
            .swap_dims(1, 2)
            .reshape([batch * height * width, channels]);
        let projected = flattened.matmul(self.weight.clone())
            + self.bias.clone().unsqueeze();
        let [_rows, output_channels] = projected.dims();

        projected
            .reshape([batch, height, width, output_channels])
            .swap_dims(1, 2)
            .swap_dims(1, 3)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn load_pp_doclayout_runtime<B>(
    device: &B::Device,
    cache_dir: Option<PathBuf>,
) -> Result<PpDocLayoutRuntime<B>>
where
    B: Backend<FloatElem = f32>,
{
    let total = Instant::now();
    let started = Instant::now();
    let files = load_pp_doclayout_files(cache_dir).await?;
    event!(
        Level::INFO,
        phase = "layout.load.files",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );

    let started = Instant::now();
    let (config, preprocessor) = read_pp_doclayout_config(&files)?;
    event!(
        Level::INFO,
        phase = "layout.load.config",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );

    let started = Instant::now();
    let bytes = std::fs::read(&files.weights_path).with_context(|| {
        format!("failed to read {}", files.weights_path.display())
    })?;
    event!(
        Level::INFO,
        phase = "layout.load.read_weights",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
        bytes = bytes.len()
    );

    let runtime = build_pp_doclayout_runtime(
        device,
        config,
        preprocessor,
        &bytes,
        total,
    )?;
    Ok(runtime)
}

/// Loads PP-DocLayoutV3 runtime from browser/native in-memory model files.
pub(crate) fn load_pp_doclayout_runtime_from_bytes<B>(
    device: &B::Device,
    config_bytes: &[u8],
    preprocessor_bytes: &[u8],
    weights_bytes: &[u8],
) -> Result<PpDocLayoutRuntime<B>>
where
    B: Backend<FloatElem = f32>,
{
    let total = Instant::now();
    let started = Instant::now();
    let (config, preprocessor) =
        read_pp_doclayout_config_from_bytes(config_bytes, preprocessor_bytes)?;
    event!(
        Level::INFO,
        phase = "layout.load.config",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );

    build_pp_doclayout_runtime(
        device,
        config,
        preprocessor,
        weights_bytes,
        total,
    )
}

/// Builds the runtime after config bytes and weight bytes are available.
fn build_pp_doclayout_runtime<B>(
    device: &B::Device,
    config: PpDocLayoutConfig,
    preprocessor: PpDocLayoutPreprocessorConfig,
    weights_bytes: &[u8],
    total: Instant,
) -> Result<PpDocLayoutRuntime<B>>
where
    B: Backend<FloatElem = f32>,
{
    let started = Instant::now();
    let tensors = SafeTensors::deserialize(weights_bytes)?;
    validate_pp_doclayout_weights(&tensors)?;
    event!(
        Level::INFO,
        phase = "layout.load.deserialize_weights",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );

    let started = Instant::now();
    let detector = PpDocLayoutDetector::from_safetensors(
        &tensors,
        config.id2label.len(),
        device,
    )?;
    event!(
        Level::INFO,
        phase = "layout.load.build_detector",
        elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
    );
    event!(
        Level::INFO,
        phase = "layout.load.total",
        elapsed_ms = total.elapsed().as_secs_f64() * 1000.0
    );

    Ok(PpDocLayoutRuntime {
        config,
        preprocessor,
        detector,
    })
}

impl<B> PpDocLayoutRuntime<B>
where
    B: Backend<FloatElem = f32>,
{
    pub(crate) fn detect_image(
        &self,
        image: &DynamicImage,
        device: &B::Device,
    ) -> Result<Vec<LayoutDetection>> {
        let total = Instant::now();
        let started = Instant::now();
        let input = preprocess_layout_image(image, &self.preprocessor)?;
        event!(
            Level::INFO,
            phase = "layout.detect.preprocess",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let channels = input.channels;
        let height = input.height;
        let width = input.width;
        let started = Instant::now();
        let tensor = Tensor::from_data(
            TensorData::new(input.values.clone(), [1, channels, height, width]),
            device,
        );
        event!(
            Level::INFO,
            phase = "layout.detect.tensor_from_data",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );

        let started = Instant::now();
        let raw = self.detector.forward(tensor)?;
        event!(
            Level::INFO,
            phase = "layout.detect.forward",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );

        let started = Instant::now();
        let detections = postprocess_encoder_proposals(
            raw,
            &self.config.id2label,
            input.original_width,
            input.original_height,
            PpPostprocessOptions::default(),
        )?;
        event!(
            Level::INFO,
            phase = "layout.detect.postprocess",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            detections = detections.len()
        );
        event!(
            Level::INFO,
            phase = "layout.detect.total",
            elapsed_ms = total.elapsed().as_secs_f64() * 1000.0
        );
        Ok(detections)
    }

    /// Detects layout blocks without synchronous tensor readback.
    pub(crate) async fn detect_image_async(
        &self,
        image: &DynamicImage,
        device: &B::Device,
    ) -> Result<Vec<LayoutDetection>> {
        let total = Instant::now();
        let started = Instant::now();
        let input = preprocess_layout_image(image, &self.preprocessor)?;
        event!(
            Level::INFO,
            phase = "layout.detect.preprocess",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );
        let channels = input.channels;
        let height = input.height;
        let width = input.width;
        let started = Instant::now();
        let tensor = Tensor::from_data(
            TensorData::new(input.values.clone(), [1, channels, height, width]),
            device,
        );
        event!(
            Level::INFO,
            phase = "layout.detect.tensor_from_data",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );

        let started = Instant::now();
        let raw = self.detector.forward_async(tensor).await?;
        event!(
            Level::INFO,
            phase = "layout.detect.forward",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0
        );

        let started = Instant::now();
        let detections = postprocess_encoder_proposals_async(
            raw,
            &self.config.id2label,
            input.original_width,
            input.original_height,
            PpPostprocessOptions::default(),
        )
        .await?;
        event!(
            Level::INFO,
            phase = "layout.detect.postprocess",
            elapsed_ms = started.elapsed().as_secs_f64() * 1000.0,
            detections = detections.len()
        );
        event!(
            Level::INFO,
            phase = "layout.detect.total",
            elapsed_ms = total.elapsed().as_secs_f64() * 1000.0
        );
        Ok(detections)
    }
}
