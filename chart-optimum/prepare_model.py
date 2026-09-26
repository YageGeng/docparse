"""Download OneChart, export its Optimum ONNX graphs, and verify them against PyTorch."""

import hashlib
import json
import shutil
from pathlib import Path
from typing import Any

import numpy as np
import onnx
import onnxruntime as ort
import torch
from huggingface_hub import snapshot_download
from optimum.exporters.onnx import export
from PIL import Image
from transformers import AutoModel, AutoTokenizer

from contract import (
    CACHED_DECODER_GRAPH,
    CUDA_PROVIDER_OPTIONS,
    DECODER_GRAPH,
    EMBEDDING_GRAPH,
    GRAPHS,
    NUMBER_GRAPH,
    OPSET,
    RUNTIME_ASSETS,
    VISION_GRAPH,
    past_key_values_names,
    present_key_values_names,
)
from graphs import graph_configs
from preprocessing import IMAGE_SIZE, build_prompt, preprocess, splice_image_features

# The revision whose weights, tokenizer, and published behaviour this deployment pins.
REVISION = "79212de2f520694e534e58240f228eff351d536c"
# Reject a substituted checkpoint before any graph is exported from it.
WEIGHTS_SHA256 = "15ab0e9971384db1f17414d6b1e4d9d55bfcdb80d810dfb161b316fb10b6220b"
MODEL_ROOT = Path(__file__).resolve().parent / "models"
SOURCE = MODEL_ROOT / "OneChart"
TARGET = MODEL_ROOT / "OneChart-optimum"
# Traced graphs run on CUDA kernels that accumulate in a different order from the CPU trace,
# so elementwise parity uses numpy's relative-plus-absolute rule rather than a fixed bound.
TOLERANCE = 2e-3
# Whole-tensor relative error allowed per graph. The measured figure with float32 accumulation
# is about 1e-5, so this bound leaves two orders of magnitude of headroom while still failing if
# TF32 is re-enabled, which pushed the same measurement to about 1.2e-3.
RELATIVE_LIMIT = 1e-3


def download() -> None:
    """Fetch only the released assets that the export and the runtime consume."""
    snapshot_download(
        "kppkkp/OneChart",
        revision=REVISION,
        local_dir=SOURCE,
        allow_patterns=["*.json", "*.txt", "*.py", "model.safetensors", "README.md"],
        ignore_patterns=["pytorch_model.bin"],
    )
    digest = hashlib.sha256((SOURCE / "model.safetensors").read_bytes()).hexdigest()
    if digest != WEIGHTS_SHA256:
        raise ValueError("The checkpoint does not match the pinned OneChart revision")


def load_source():
    """Load the release in float32 and stop the tokenizer from inserting a leading BOS.

    The checkpoint records `add_bos_token: true`, but it was published from
    transformers 4.32.1, where `GPT2Tokenizer` ignored that key. Later transformers
    prepend `</s>` because that is also OneChart's BOS, and the model then answers as
    if the conversation had already ended: the opening tokens fall back to unrelated
    languages and the auxiliary value head never fires. Only clearing the flag
    reproduces the published `chat()` behaviour, and the ONNX runtime must match it.
    """
    model = AutoModel.from_pretrained(SOURCE, trust_remote_code=True).eval()
    tokenizer = AutoTokenizer.from_pretrained(
        SOURCE, trust_remote_code=True, use_fast=False, padding_side="right"
    )
    tokenizer.add_bos_token = False
    return model, tokenizer


def export_graphs(model) -> None:
    """Write every graph through Optimum's exporter and validate the ONNX model itself.

    Optimum's post-export dynamic-axes fix re-runs the exported graph with synthetic inputs and
    rewrites any axis it finds outside the declared set. Its dummy inputs honour the requested
    framework, so the fix runs normally; `validate` then proves the surviving axes on real
    inputs, which is what the deployment actually depends on.
    """
    TARGET.mkdir(parents=True, exist_ok=True)
    for name, (module, config) in graph_configs(model).items():
        export(module, config, TARGET / name, opset=OPSET, device="cpu")
        onnx.checker.check_model(onnx.load(str(TARGET / name)))


def copy_runtime_assets() -> None:
    """Copy the released configuration and tokenizer files the runtime reads."""
    for name in RUNTIME_ASSETS:
        shutil.copy2(SOURCE / name, TARGET / name)


def cuda_session(name: str) -> ort.InferenceSession:
    """Open one graph on CUDA and refuse a silent fallback to the CPU provider."""
    session = ort.InferenceSession(
        str(TARGET / name),
        providers=[("CUDAExecutionProvider", CUDA_PROVIDER_OPTIONS)],
    )
    if session.get_providers()[0] != "CUDAExecutionProvider":
        raise RuntimeError(f"CUDAExecutionProvider failed to initialize for {name}")
    return session


def run(session: ort.InferenceSession, feeds: dict[str, np.ndarray]) -> dict[str, Any]:
    """Run one graph and key its outputs by the names the runtime will use."""
    names = [output.name for output in session.get_outputs()]
    return dict(zip(names, session.run(names, feeds)))


def compare(label: str, actual: np.ndarray, expected: np.ndarray) -> dict[str, float]:
    """Fail preparation when an exported graph stops matching its PyTorch source.

    Elementwise agreement follows `numpy.allclose`. On top of that, the whole-tensor relative
    error must stay under `RELATIVE_LIMIT`; that figure is what actually catches a numerical
    regression such as TF32 being re-enabled, which changes the result without moving any
    single element far enough to trip `allclose`.
    """
    actual = np.asarray(actual, dtype=np.float32)
    expected = np.asarray(expected, dtype=np.float32)
    if actual.shape != expected.shape:
        raise ValueError(
            f"{label}: exported shape {actual.shape} != PyTorch shape {expected.shape}"
        )
    difference = np.abs(actual - expected)
    scale = float(np.linalg.norm(expected.ravel()))
    # A per-element ratio would be dominated by elements that sit near zero, so the relative
    # figure is measured over the whole tensor instead.
    relative = float(np.linalg.norm(difference.ravel()) / scale) if scale else 0.0
    if not np.allclose(actual, expected, rtol=TOLERANCE, atol=TOLERANCE):
        raise ValueError(
            f"{label}: max absolute difference {difference.max()} exceeds the "
            f"{TOLERANCE} tolerance"
        )
    if relative > RELATIVE_LIMIT:
        raise ValueError(f"{label}: relative error {relative} exceeds {RELATIVE_LIMIT}")
    return {
        "max_abs": float(difference.max()) if difference.size else 0.0,
        "relative": relative,
    }


def synthetic_chart() -> Image.Image:
    """Build a deterministic chart-like image so validation needs no external fixtures."""
    axis = np.linspace(0.0, 255.0, IMAGE_SIZE, dtype=np.float32)
    grid = (axis[None, :] + axis[:, None]) % 256.0
    pixels = np.stack([grid, grid[::-1], 255.0 - grid], axis=-1)
    return Image.fromarray(pixels.astype(np.uint8), "RGB")


def worst(comparisons: list[dict[str, float]]) -> dict[str, float]:
    """Reduce several comparisons to the worst absolute and relative deviation."""
    return {
        "max_abs": max(item["max_abs"] for item in comparisons),
        "relative": max(item["relative"] for item in comparisons),
    }


def validate(model, tokenizer) -> dict[str, object]:
    """Compare every CUDA graph against the PyTorch modules it was traced from.

    The decoder graphs run on a batch of two and the cached decoder walks two steps, because a
    mis-declared dynamic axis only shows up once the axis actually changes. The vision tower
    stays at batch one: it is the graph that pins the target card, and two charts already need
    about 4.5 GiB of attention buffers, which is why `runtime.VISION_BATCH` is 1.
    """
    ort.preload_dlls()
    modules = {name: module for name, (module, _) in graph_configs(model).items()}
    chart = preprocess(synthetic_chart())
    batch = 2
    deviations: dict[str, dict[str, float]] = {}
    shapes: dict[str, list[int]] = {}

    vision = cuda_session(VISION_GRAPH)
    features = run(vision, {"pixel_values": chart})["image_features"]
    with torch.no_grad():
        expected = modules[VISION_GRAPH](torch.from_numpy(chart)).numpy()
    deviations["vision_encoder"] = compare("vision_encoder", features, expected)
    shapes["vision_encoder"] = list(features.shape)

    # The prompt must match the deployment prompt, BOS removal included.
    prompt_ids = tokenizer([build_prompt()]).input_ids[0]
    sequence_length = len(prompt_ids)
    input_ids = np.tile(np.asarray([prompt_ids], dtype=np.int64), (batch, 1))
    embedding = cuda_session(EMBEDDING_GRAPH)
    inputs_embeds = run(embedding, {"input_ids": input_ids})["inputs_embeds"]
    with torch.no_grad():
        expected = modules[EMBEDDING_GRAPH](torch.from_numpy(input_ids)).numpy()
    deviations["embedding"] = compare("embedding", inputs_embeds, expected)
    shapes["embedding"] = list(inputs_embeds.shape)

    # The vision graph is validated at batch one, so the batched decoder check reuses its
    # projection for both rows; `runtime.VISION_BATCH` encodes a batch exactly that way.
    spliced = splice_image_features(
        inputs_embeds,
        input_ids,
        np.concatenate([features, features]),
        tokenizer.convert_tokens_to_ids("<img>"),
        tokenizer.convert_tokens_to_ids("</img>"),
    )
    attention_mask = np.ones((batch, sequence_length), dtype=np.int64)

    num_layers = model.config.num_hidden_layers
    past_names = past_key_values_names(num_layers)
    present_names = present_key_values_names(num_layers)
    decoder = cuda_session(DECODER_GRAPH)
    outputs = run(decoder, {"inputs_embeds": spliced, "attention_mask": attention_mask})
    with torch.no_grad():
        reference = modules[DECODER_GRAPH](
            torch.from_numpy(spliced), torch.from_numpy(attention_mask)
        )
    deviations["decoder_logits"] = compare(
        "decoder logits", outputs["logits"], reference[0].numpy()
    )
    deviations["decoder_hidden"] = compare(
        "decoder hidden", outputs["last_hidden_state"], reference[1].numpy()
    )
    deviations["decoder_cache"] = worst(
        [
            compare(f"decoder {name}", outputs[name], reference[2 + index].numpy())
            for index, name in enumerate(present_names)
        ]
    )
    shapes["decoder_logits"] = list(outputs["logits"].shape)
    shapes["decoder_cache"] = list(outputs[present_names[0]].shape)

    # Extend the cache twice, which is exactly how generation proceeds.
    embeds = spliced[:, -1:, :]
    mask = attention_mask
    cache = {name: outputs[present] for name, present in zip(past_names, present_names)}
    torch_cache = tuple(torch.from_numpy(outputs[present]) for present in present_names)
    cached = cuda_session(CACHED_DECODER_GRAPH)
    for step in range(2):
        mask = np.concatenate([mask, np.ones((batch, 1), dtype=np.int64)], axis=1)
        cached_outputs = run(
            cached, {"inputs_embeds": embeds, "attention_mask": mask, **cache}
        )
        with torch.no_grad():
            reference_cached = modules[CACHED_DECODER_GRAPH](
                torch.from_numpy(embeds), torch.from_numpy(mask), torch_cache
            )
        deviations[f"cached_step{step}_logits"] = compare(
            f"cached step {step} logits",
            cached_outputs["logits"],
            reference_cached[0].numpy(),
        )
        deviations[f"cached_step{step}_hidden"] = compare(
            f"cached step {step} hidden",
            cached_outputs["last_hidden_state"],
            reference_cached[1].numpy(),
        )
        deviations[f"cached_step{step}_cache"] = worst(
            [
                compare(
                    f"cached step {step} {name}",
                    cached_outputs[name],
                    reference_cached[2 + index].numpy(),
                )
                for index, name in enumerate(present_names)
            ]
        )
        shapes[f"cached_step{step}_cache"] = list(
            cached_outputs[present_names[0]].shape
        )
        cache = {
            name: cached_outputs[present]
            for name, present in zip(past_names, present_names)
        }
        torch_cache = tuple(
            torch.from_numpy(cached_outputs[present]) for present in present_names
        )

    hidden = outputs["last_hidden_state"][:, 0, :]
    number_head = cuda_session(NUMBER_GRAPH)
    numbers = run(number_head, {"hidden_states": hidden})["numbers"]
    with torch.no_grad():
        expected = modules[NUMBER_GRAPH](torch.from_numpy(hidden)).numpy()
    deviations["number_head"] = compare("number_head", numbers, expected)
    shapes["number_head"] = list(numbers.shape)
    return {"deviations": deviations, "shapes": shapes}


def graph_report() -> dict[str, object]:
    """Record the exported graph files, their interfaces, and their digests."""
    report = {}
    for name in GRAPHS:
        path = TARGET / name
        model = onnx.load(str(path), load_external_data=False)
        report[name] = {
            "inputs": [value.name for value in model.graph.input],
            "outputs": [value.name for value in model.graph.output],
            "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
        }
    return report


def main() -> None:
    """Prepare the pinned deployment and refuse to continue on any parity failure."""
    if not torch.cuda.is_available():
        raise RuntimeError("Model preparation requires an NVIDIA CUDA GPU")
    download()
    model, tokenizer = load_source()
    export_graphs(model)
    copy_runtime_assets()
    validation = validate(model, tokenizer)
    report = {
        "revision": REVISION,
        "weights_sha256": WEIGHTS_SHA256,
        "opset": OPSET,
        "change": (
            "OneChart exported as five ONNX graphs; the prompt keeps the release's "
            "fixed 1024x1024 chart transform and drops the leading BOS that later "
            "transformers would insert; weights are unchanged"
        ),
        "graphs": graph_report(),
        # `deviations` covers batch 2 and two cached steps; `shapes` records what was exercised.
        "validation": {"tolerance": TOLERANCE, **validation},
    }
    (TARGET / "adaptation.json").write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(report), flush=True)


if __name__ == "__main__":
    main()
