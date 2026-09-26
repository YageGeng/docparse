"""Load Texo through Optimum with the custom HGNetV2 configuration registered."""

import logging
from pathlib import Path

import numpy as np
from PIL import Image

from preprocessing import preprocess

LOGGER = logging.getLogger(__name__)
MODEL_DIR = Path(__file__).resolve().parent / "models/FormulaNet-optimum"


def load_model():
    """Require CUDA and load the published split ONNX graphs with KV-cache reuse."""
    # Keep model-loading dependencies and registration out of module import and CPU-only tests.
    import onnxruntime as ort
    import torch
    from optimum.onnxruntime import ORTModelForVision2Seq
    from optimum.utils import NormalizedConfigManager, NormalizedTextConfig
    from transformers import AutoConfig, AutoTokenizer, PretrainedConfig

    class HGNetV2Config(PretrainedConfig):
        """Describe the custom encoder without instantiating its PyTorch training model."""

        model_type = "my_hgnetv2"

    AutoConfig.register("my_hgnetv2", HGNetV2Config)
    NormalizedConfigManager._conf["my-hgnetv2"] = NormalizedTextConfig
    if not torch.cuda.is_available():
        raise RuntimeError("CUDA is required for this service")
    # Torch supplies matching CUDA 12/cuDNN libraries to ONNX Runtime.
    ort.preload_dlls()
    options = ort.SessionOptions()
    options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    LOGGER.info("Loading Texo Optimum encoder and cached decoder on CUDA")
    model = ORTModelForVision2Seq.from_pretrained(
        MODEL_DIR,
        provider="CUDAExecutionProvider",
        session_options=options,
        use_cache=True,
        use_merged=False,
        use_io_binding=True,
        local_files_only=True,
    )
    for part in [model.encoder, model.decoder, model.decoder_with_past]:
        if part.session.get_providers()[0] != "CUDAExecutionProvider":
            raise RuntimeError(
                f"Unexpected execution provider: {part.session.get_providers()}"
            )
    tokenizer = AutoTokenizer.from_pretrained(MODEL_DIR, local_files_only=True)
    LOGGER.info("Loaded Texo with CUDA, KV cache, and Optimum-managed I/O Binding")
    return model, tokenizer


def generate(
    model, tokenizer, images: list[Image.Image], max_length: int = 1024
) -> list[dict[str, str | int]]:
    """Batch valid crops with Optimum while preserving per-image failures and order."""
    # Invalid crop geometry must not discard unrelated callers sharing this CUDA batch.
    # Every slot is filled below by either preprocessing failure or its matching decoded row.
    results: list[dict[str, str | int]] = [{} for _ in images]
    inputs, indices = [], []
    for index, image in enumerate(images):
        try:
            inputs.append(preprocess(image))
            indices.append(index)
        except ValueError as error:
            results[index] = {"error": str(error)}
    if not inputs:
        return results
    # CUDA is needed only after at least one crop passes preprocessing.
    import torch

    pixels = np.concatenate(inputs)
    with torch.inference_mode():
        ids = (
            model.generate(
                pixel_values=torch.from_numpy(pixels).to("cuda"),
                do_sample=False,
                num_beams=1,
                use_cache=True,
                max_length=max_length,
                forced_eos_token_id=None,
            )
            .cpu()
            .tolist()
        )
    # Preserve the service's count-mismatch failure instead of leaving unfilled result slots.
    if len(ids) != len(indices):
        raise RuntimeError("model result count mismatch")
    for index, row in zip(indices, ids):
        if tokenizer.eos_token_id not in row:
            results[index] = {"error": "generation reached its limit without EOS"}
            continue
        end = row.index(tokenizer.eos_token_id)
        latex = tokenizer.decode(
            row[: end + 1], skip_special_tokens=True, clean_up_tokenization_spaces=False
        ).strip()
        if not latex:
            results[index] = {"error": "model returned empty LaTeX"}
            continue
        results[index] = {"text": latex, "output_tokens": end}
    return results
