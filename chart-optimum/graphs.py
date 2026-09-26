"""Describe the exported OneChart ONNX graphs and their Optimum export configurations.

Optimum splits an autoregressive decoder into a first-step graph and a cached graph.
OneChart adds a SAM ViT-B vision tower, a shared token embedding table, and an
auxiliary numeric-value head on top of an OPT decoder, so each part becomes its own
graph and ``runtime.py`` recombines them.

The stock Optimum task classes cannot drive these graphs. ``ORTDecoder.forward``
binds exactly ``input_ids``, ``attention_mask``, ``position_ids``, and the key/value
cache, so the projected image features that replace the ``<imgpad>`` embeddings have
no route into a stock decoder session. Exporting the graphs here and running them
through ``onnxruntime`` sessions directly keeps the visual splice and the auxiliary
head intact without forking Optimum.
"""

from typing import Any

import numpy as np
import torch
from optimum.exporters.onnx import OnnxConfig
from optimum.utils import NormalizedConfigManager
from torch import nn

from contract import (
    CACHED_DECODER_GRAPH,
    DECODER_GRAPH,
    EMBEDDING_GRAPH,
    NUMBER_GRAPH,
    VISION_GRAPH,
    past_key_values_inputs,
    present_key_values_outputs,
)

# The upstream transform always resizes a chart to this square, so vision shapes are static.
IMAGE_SIZE = 1024


def dummy(
    framework: str, shape: tuple[int, ...], *, integer: bool = False, ones: bool = False
) -> Any:
    """Build one dummy graph input in the framework the caller asked for.

    Optimum requests `framework="np"` when it re-runs a freshly exported graph to fix dynamic
    axes, and hands the result straight to ONNX Runtime. Always returning a torch tensor makes
    that pass fail with "Input must be a list of dictionaries or a single numpy array".
    """
    if framework == "np":
        dtype = np.int64 if integer else np.float32
        return np.ones(shape, dtype=dtype) if ones else np.zeros(shape, dtype=dtype)
    dtype = torch.long if integer else torch.float32
    return torch.ones(shape, dtype=dtype) if ones else torch.zeros(shape, dtype=dtype)


class VisionGraph(nn.Module):
    """Fuse the SAM ViT-B tower and its linear projector into one image encoder."""

    def __init__(self, model) -> None:
        """Bind the released vision tower and its projector as one graph."""
        super().__init__()
        self.vision_tower = model.model.vision_tower
        self.mm_projector = model.model.mm_projector
        # Optimum's exporter writes `return_dict` on the graph's config object.
        self.config = model.config

    def forward(self, pixel_values: torch.Tensor) -> torch.Tensor:
        """Map one 1024x1024 chart to the 256 visual tokens spliced into the prompt."""
        features = self.vision_tower(pixel_values)
        return self.mm_projector(features.flatten(2).permute(0, 2, 1))


class EmbeddingGraph(nn.Module):
    """Expose the token embedding table that fills the prompt's non-visual positions."""

    def __init__(self, model) -> None:
        """Bind the released token embedding table."""
        super().__init__()
        self.embed_tokens = model.model.decoder.embed_tokens
        self.config = model.config

    def forward(self, input_ids: torch.Tensor) -> torch.Tensor:
        """Embed prompt tokens; the visual positions are replaced outside the graph."""
        return self.embed_tokens(input_ids)


class NumberGraph(nn.Module):
    """Expose the auxiliary head that scores the chart values OneChart predicts."""

    def __init__(self, model) -> None:
        """Bind the released auxiliary value head."""
        super().__init__()
        self.num_decoder = model.num_decoder
        self.config = model.config

    def forward(self, hidden_states: torch.Tensor) -> torch.Tensor:
        """Project the `<Number>` hidden state to 256 normalized magnitudes."""
        return self.num_decoder(hidden_states)


class DecoderGraph(nn.Module):
    """Run one decoder pass and return logits, hidden states, and cache tensors.

    A single module serves both exported graphs. The first-step graph omits the cache
    input, while the cached graph receives the flattened tensors named by
    :func:`past_key_values_inputs`.
    """

    def __init__(self, model) -> None:
        """Bind the released OPT decoder, language head, and layer count."""
        super().__init__()
        self.decoder = model.model.decoder
        self.lm_head = model.lm_head
        self.config = model.config
        self.num_layers = model.config.num_hidden_layers

    def forward(
        self,
        inputs_embeds: torch.Tensor,
        attention_mask: torch.Tensor,
        past_key_values: tuple[torch.Tensor, ...] | None = None,
    ) -> tuple[torch.Tensor, ...]:
        """Return logits, the last hidden state, and the flattened cache in ONNX order."""
        cache = None
        if past_key_values is not None:
            # OPT reads its cache as a legacy tuple of per-layer key/value pairs.
            cache = tuple(
                (past_key_values[index], past_key_values[index + 1])
                for index in range(0, len(past_key_values), 2)
            )
        outputs = self.decoder(
            input_ids=None,
            inputs_embeds=inputs_embeds,
            attention_mask=attention_mask,
            past_key_values=cache,
            use_cache=True,
            return_dict=True,
        )
        hidden_states = outputs.last_hidden_state
        # The reference always spends a float32 language head on the decoder output.
        logits = self.lm_head(hidden_states).float()
        present = tuple(tensor for layer in outputs.past_key_values for tensor in layer)
        return (logits, hidden_states, *present)


class OneChartOnnxConfig(OnnxConfig):
    """Carry an explicit OneChart input and output contract into Optimum's exporter."""

    # The decoder is an OPT stack, so Optimum's OPT view of the config describes it exactly.
    NORMALIZED_CONFIG_CLASS = NormalizedConfigManager.get_normalized_config_class("opt")
    # OneChart's config enables the cache, and the patcher rewrites `use_cache` from here.
    use_past = True

    def __init__(
        self,
        config: Any,
        inputs: dict[str, dict[int, str]],
        outputs: dict[str, dict[int, str]],
    ) -> None:
        """Store the explicit input and output contract for one graph."""
        super().__init__(config, task="feature-extraction")
        self.model_config = config
        self.graph_inputs = inputs
        self.graph_outputs = outputs

    @property
    def inputs(self) -> dict[str, dict[int, str]]:
        """Return ONNX input names mapped to the axes that may vary at run time."""
        return self.graph_inputs

    @property
    def outputs(self) -> dict[str, dict[int, str]]:
        """Return ONNX output names mapped to the axes that may vary at run time."""
        return self.graph_outputs


class VisionOnnxConfig(OneChartOnnxConfig):
    """Export the vision tower; the chart transform fixes every spatial dimension."""

    def __init__(self, config: Any) -> None:
        """Declare the fixed vision input and its projected output."""
        super().__init__(
            config,
            inputs={"pixel_values": {0: "batch_size"}},
            outputs={"image_features": {0: "batch_size", 1: "image_tokens"}},
        )

    def generate_dummy_inputs(self, framework: str = "pt", **kwargs: Any) -> dict:
        """Provide one black chart, the same tensor the reference transform would build."""
        return {"pixel_values": dummy(framework, (1, 3, IMAGE_SIZE, IMAGE_SIZE))}


class EmbeddingOnnxConfig(OneChartOnnxConfig):
    """Export the token embedding table shared by the prompt and the language head."""

    def __init__(self, config: Any) -> None:
        """Declare the token input and the shared embedding output."""
        super().__init__(
            config,
            inputs={"input_ids": {0: "batch_size", 1: "sequence_length"}},
            outputs={"inputs_embeds": {0: "batch_size", 1: "sequence_length"}},
        )

    def generate_dummy_inputs(self, framework: str = "pt", **kwargs: Any) -> dict:
        """Provide a short token sequence; the runtime splices the visual tokens."""
        return {"input_ids": dummy(framework, (1, 8), integer=True)}


class DecoderOnnxConfig(OneChartOnnxConfig):
    """Export the first decoding step, which creates the key/value cache."""

    def __init__(self, config: Any) -> None:
        """Declare the first decoder step, which creates the cache."""
        super().__init__(
            config,
            inputs={
                "inputs_embeds": {0: "batch_size", 1: "sequence_length"},
                "attention_mask": {0: "batch_size", 1: "total_sequence_length"},
            },
            outputs={
                "logits": {0: "batch_size", 1: "sequence_length"},
                "last_hidden_state": {0: "batch_size", 1: "sequence_length"},
                **present_key_values_outputs(config.num_hidden_layers),
            },
        )

    def generate_dummy_inputs(self, framework: str = "pt", **kwargs: Any) -> dict:
        """Provide one prompt whose length also sizes the freshly created cache."""
        sequence_length = 8
        hidden_size = self.model_config.hidden_size
        return {
            "inputs_embeds": dummy(framework, (1, sequence_length, hidden_size)),
            "attention_mask": dummy(
                framework, (1, sequence_length), integer=True, ones=True
            ),
        }


class CachedDecoderOnnxConfig(OneChartOnnxConfig):
    """Export later decoding steps, which read and extend the key/value cache."""

    def __init__(self, config: Any) -> None:
        """Declare a cached decoder step and its extended cache."""
        super().__init__(
            config,
            inputs={
                "inputs_embeds": {0: "batch_size", 1: "sequence_length"},
                "attention_mask": {0: "batch_size", 1: "total_sequence_length"},
                **past_key_values_inputs(config.num_hidden_layers),
            },
            outputs={
                "logits": {0: "batch_size", 1: "sequence_length"},
                "last_hidden_state": {0: "batch_size", 1: "sequence_length"},
                **present_key_values_outputs(config.num_hidden_layers),
            },
        )

    def generate_dummy_inputs(self, framework: str = "pt", **kwargs: Any) -> dict:
        """Provide one token and a populated cache so the traced graph stays dynamic."""
        past_length, sequence_length = 4, 1
        hidden_size = self.model_config.hidden_size
        num_layers = self.model_config.num_hidden_layers
        num_heads = self.model_config.num_attention_heads
        head_dim = hidden_size // num_heads
        cache = tuple(
            dummy(framework, (1, num_heads, past_length, head_dim))
            for _ in range(num_layers)
            for _ in (0, 1)
        )
        return {
            "inputs_embeds": dummy(framework, (1, sequence_length, hidden_size)),
            "attention_mask": dummy(
                framework, (1, past_length + sequence_length), integer=True, ones=True
            ),
            "past_key_values": cache,
        }


class NumberOnnxConfig(OneChartOnnxConfig):
    """Export the auxiliary value head applied to the `<Number>` hidden state."""

    def __init__(self, config: Any) -> None:
        """Declare the auxiliary head input and output."""
        super().__init__(
            config,
            inputs={"hidden_states": {0: "batch_size"}},
            outputs={"numbers": {0: "batch_size"}},
        )

    def generate_dummy_inputs(self, framework: str = "pt", **kwargs: Any) -> dict:
        """Provide one hidden state; the head is position independent."""
        return {"hidden_states": dummy(framework, (1, self.model_config.hidden_size))}


def graph_configs(model) -> dict[str, tuple[nn.Module, OneChartOnnxConfig]]:
    """Pair every exported graph with its module and Optimum export configuration."""
    config = model.config
    return {
        VISION_GRAPH: (VisionGraph(model), VisionOnnxConfig(config)),
        EMBEDDING_GRAPH: (EmbeddingGraph(model), EmbeddingOnnxConfig(config)),
        DECODER_GRAPH: (DecoderGraph(model), DecoderOnnxConfig(config)),
        CACHED_DECODER_GRAPH: (DecoderGraph(model), CachedDecoderOnnxConfig(config)),
        NUMBER_GRAPH: (NumberGraph(model), NumberOnnxConfig(config)),
    }
