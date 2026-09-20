"""Load Texo through Optimum with the custom HGNetV2 configuration registered."""
from pathlib import Path
import logging

import numpy as np
import onnxruntime as ort
import torch
from transformers import AutoConfig, AutoTokenizer, PretrainedConfig
from optimum.onnxruntime import ORTModelForVision2Seq
from optimum.utils import NormalizedConfigManager, NormalizedTextConfig

from preprocessing import preprocess

LOGGER = logging.getLogger(__name__)
MODEL_DIR = Path(__file__).resolve().parent / "models/FormulaNet-optimum"


class HGNetV2Config(PretrainedConfig):
    """Describe the custom encoder without instantiating its PyTorch training model."""
    model_type = "my_hgnetv2"


AutoConfig.register("my_hgnetv2", HGNetV2Config)
NormalizedConfigManager._conf["my-hgnetv2"] = NormalizedTextConfig


def load_model():
    """Require CUDA and load the published split ONNX graphs with KV-cache reuse."""
    if not torch.cuda.is_available():
        raise RuntimeError("CUDA is required for this service")
    # Torch supplies matching CUDA 12/cuDNN libraries to ONNX Runtime.
    ort.preload_dlls()
    options = ort.SessionOptions()
    options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    LOGGER.info("Loading Texo Optimum encoder and cached decoder on CUDA")
    model = ORTModelForVision2Seq.from_pretrained(
        MODEL_DIR, provider="CUDAExecutionProvider", session_options=options,
        use_cache=True, use_merged=False, use_io_binding=True, local_files_only=True,
    )
    for part in [model.encoder, model.decoder, model.decoder_with_past]:
        if part.session.get_providers()[0] != "CUDAExecutionProvider":
            raise RuntimeError(f"Unexpected execution provider: {part.session.get_providers()}")
    tokenizer = AutoTokenizer.from_pretrained(MODEL_DIR, local_files_only=True)
    LOGGER.info("Loaded Texo with CUDA, KV cache, and Optimum-managed I/O Binding")
    return model, tokenizer


def generate(model, tokenizer, images, max_length=1024):
    """Run one real image batch with Optimum generate and reject unfinished LaTeX."""
    pixels = np.concatenate([preprocess(image) for image in images])
    with torch.inference_mode():
        ids = model.generate(
            pixel_values=torch.from_numpy(pixels).to("cuda"),
            do_sample=False, num_beams=1, use_cache=True,
            max_length=max_length, forced_eos_token_id=None,
        ).cpu().tolist()
    results = []
    for row in ids:
        if tokenizer.eos_token_id not in row:
            results.append({"error": "generation reached its limit without EOS"})
            continue
        end = row.index(tokenizer.eos_token_id)
        latex = tokenizer.decode(row[:end + 1], skip_special_tokens=True, clean_up_tokenization_spaces=False).strip()
        if not latex:
            results.append({"error": "model returned empty LaTeX"})
            continue
        results.append({"text": latex, "output_tokens": end})
    return results
