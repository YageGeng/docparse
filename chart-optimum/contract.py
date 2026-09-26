"""Name the OneChart ONNX graphs, their cache tensors, and their runtime assets.

Both the exporter and the serving runtime import this module, so it must stay free
of torch, Optimum, and transformers imports. The runtime then loads only
``onnxruntime``, ``numpy``, ``pillow``, and the released tokenizer files.
"""

# One graph per stage: the release has no published ONNX export, so preparation writes these.
VISION_GRAPH = "vision_encoder.onnx"
EMBEDDING_GRAPH = "embedding.onnx"
DECODER_GRAPH = "decoder_model.onnx"
CACHED_DECODER_GRAPH = "decoder_with_past_model.onnx"
NUMBER_GRAPH = "number_head.onnx"
GRAPHS = (
    VISION_GRAPH,
    EMBEDDING_GRAPH,
    DECODER_GRAPH,
    CACHED_DECODER_GRAPH,
    NUMBER_GRAPH,
)
# Files the runtime reads; the remote modelling code is deliberately not part of the deployment.
RUNTIME_ASSETS = (
    "config.json",
    "generation_config.json",
    "tokenizer_config.json",
    "special_tokens_map.json",
    "added_tokens.json",
    "vocab.json",
    "merges.txt",
)
# Pinned so the validated graphs and the deployment graphs are the same operator set.
OPSET = 18
# ONNX Runtime enables TF32 tensor cores by default on Ampere and later GPUs. TF32 keeps only ten
# mantissa bits, which costs about 1e-3 relative accuracy against the PyTorch reference and is
# enough to change greedy token choices, so preparation and serving pin float32 accumulation.
# Only the tensor-core setting is overridden: the CUDA arena must keep its default power-of-two
# extension, because every decode step grows the key/value cache and exact-size chunks fragment
# the arena until decoding fails part way through a chart. Power-of-two chunks stay reusable.
CUDA_PROVIDER_OPTIONS = {"use_tf32": "0"}
# The release compares at most this many of the auxiliary head's magnitudes against its values.
NUMBER_LIMIT = 100


def past_key_values_inputs(num_layers: int) -> dict[str, dict[int, str]]:
    """Name the flattened cache inputs Optimum expects for a cached decoder step."""
    return {
        f"past_key_values.{index}.{kind}": {0: "batch_size", 2: "past_sequence_length"}
        for index in range(num_layers)
        for kind in ("key", "value")
    }


def present_key_values_outputs(num_layers: int) -> dict[str, dict[int, str]]:
    """Name the extended cache outputs so the next step can consume them directly."""
    return {
        f"present.{index}.{kind}": {0: "batch_size", 2: "total_sequence_length"}
        for index in range(num_layers)
        for kind in ("key", "value")
    }


def past_key_values_names(num_layers: int) -> list[str]:
    """List the session input names that carry the cache into a cached decoder step."""
    return list(past_key_values_inputs(num_layers))


def present_key_values_names(num_layers: int) -> list[str]:
    """List the session output names that carry the cache out of a decoder step."""
    return list(present_key_values_outputs(num_layers))
