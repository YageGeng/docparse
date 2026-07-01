//! PP-DocLayoutV3 model file download and safetensors validation.

use anyhow::{Context, Result, bail};
#[cfg(not(target_arch = "wasm32"))]
use hf_hub::{Repo, RepoType, api::tokio::ApiBuilder};
use safetensors::{Dtype, SafeTensors};
#[cfg(not(target_arch = "wasm32"))]
use std::path::PathBuf;

#[cfg(not(target_arch = "wasm32"))]
const PP_DOCLAYOUT_V3_REPO_ID: &str = "PaddlePaddle/PP-DocLayoutV3_safetensors";
#[cfg(not(target_arch = "wasm32"))]
const PP_DOCLAYOUT_CONFIG: &str = "config.json";
#[cfg(not(target_arch = "wasm32"))]
const PP_DOCLAYOUT_PREPROCESSOR: &str = "preprocessor_config.json";
#[cfg(not(target_arch = "wasm32"))]
const PP_DOCLAYOUT_WEIGHTS: &str = "model.safetensors";
#[cfg(not(target_arch = "wasm32"))]
const PP_DOCLAYOUT_WEIGHTS_ENV: &str = "PP_DOCLAYOUT_WEIGHTS";

/// Local paths to every file needed to run PP-DocLayoutV3.
#[derive(Debug, Clone)]
#[cfg(not(target_arch = "wasm32"))]
pub(crate) struct PpDocLayoutFiles {
    pub(crate) config_path: PathBuf,
    pub(crate) preprocessor_path: PathBuf,
    pub(crate) weights_path: PathBuf,
}

/// Downloads or resolves PP-DocLayoutV3 files from the Hugging Face cache.
///
/// This is native-only because browser inference loads user-selected files
/// directly into memory.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn load_pp_doclayout_files(
    cache_dir: Option<PathBuf>,
) -> Result<PpDocLayoutFiles> {
    let mut builder = ApiBuilder::new();
    if let Some(cache_dir) = cache_dir {
        builder = builder.with_cache_dir(cache_dir);
    }
    let api = builder.build()?;
    let repo = api.repo(Repo::new(
        PP_DOCLAYOUT_V3_REPO_ID.to_string(),
        RepoType::Model,
    ));

    let weights_path = match std::env::var_os(PP_DOCLAYOUT_WEIGHTS_ENV) {
        Some(path) => PathBuf::from(path),
        None => repo.get(PP_DOCLAYOUT_WEIGHTS).await?,
    };

    Ok(PpDocLayoutFiles {
        config_path: repo.get(PP_DOCLAYOUT_CONFIG).await?,
        preprocessor_path: repo.get(PP_DOCLAYOUT_PREPROCESSOR).await?,
        weights_path,
    })
}

/// Checks required tensors exist and are stored as `f32`.
pub(crate) fn validate_pp_doclayout_weights(
    tensors: &SafeTensors<'_>,
) -> Result<()> {
    let required = [
        "model.backbone.model.embedder.stem1.convolution.weight",
        "model.encoder_input_proj.0.0.weight",
        "model.encoder.lateral_convs.0.conv.weight",
        "model.enc_bbox_head.layers.0.weight",
        "model.decoder.query_pos_head.layers.0.weight",
        "model.decoder.layers.0.self_attn.q_proj.weight",
        "model.decoder.layers.0.encoder_attn.sampling_offsets.weight",
        "model.decoder_norm.weight",
        "model.decoder_order_head.0.weight",
        "model.decoder_global_pointer.dense.weight",
        "model.denoising_class_embed.weight",
    ];

    for name in required {
        validate_f32_tensor(tensors, name)?;
    }

    Ok(())
}

/// Verifies one required tensor exists and is stored as `f32`.
fn validate_f32_tensor(tensors: &SafeTensors<'_>, name: &str) -> Result<()> {
    let tensor = tensors
        .tensor(name)
        .with_context(|| format!("missing PP layout tensor {name}"))?;
    if tensor.dtype() != Dtype::F32 {
        bail!("PP layout tensor {name} must be float32");
    }

    Ok(())
}
