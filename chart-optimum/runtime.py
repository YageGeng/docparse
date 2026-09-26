"""Run the exported OneChart graphs on CUDA and extract chart data via ONNX Runtime."""

import json
import logging
import re
from pathlib import Path
from typing import Any

import numpy as np
import onnxruntime as ort
from numpy.typing import NDArray
from PIL import Image

from contract import (
    CACHED_DECODER_GRAPH,
    CUDA_PROVIDER_OPTIONS,
    DECODER_GRAPH,
    EMBEDDING_GRAPH,
    GRAPHS,
    NUMBER_GRAPH,
    NUMBER_LIMIT,
    VISION_GRAPH,
    past_key_values_names,
    present_key_values_names,
)
from preprocessing import build_prompt, preprocess, splice_image_features

LOGGER = logging.getLogger(__name__)
MODEL_DIR = Path(__file__).resolve().parent / "models/OneChart-optimum"
# The released demo decodes up to 1024 new tokens, which keeps every position inside OPT's
# positional table (`max_position_embeddings` plus the embedding's two-row offset). The Hugging
# Face port's 4096 budget overruns that table and aborts with a CUDA indexing fault.
MAX_NEW_TOKENS = 1024
# The release labels a chart reliable when the L1 distance to its parsed values is below this.
RELIABLE_DISTANCE = 0.1
# SAM ViT-B attends over all 4096 patches in its four global blocks, so one 1024x1024 chart
# peaks at about 2.1 GiB of float32 score, softmax, and relative-position buffers. Encoding one
# chart per call bounds that peak; `batch_size` instead pays for decoder cache, so the two
# settings bound different resources and only this one is tied to the vision tower.
VISION_BATCH = 1
# Response values stay JSON-serializable for the HTTP layer.
Response = dict[str, str | int | float | dict | list | None]
# A series key followed by the brace that opens its rows.
GROUP = re.compile(r'"([^"]*)"\s*:\s*\{')
# A row label followed by a numeric value; the value's opening quote may be missing. Thousands
# separators stay inside the match so a grouped value is not truncated to its first group, and
# the exponent and bare-fraction forms keep scientific and sub-unit readings intact.
ROW = re.compile(
    r'"([^"]*)"\s*:\s*"?'
    r"(-?(?:\d{1,3}(?:,\d{3})+(?:\.\d+)?|\d+(?:\.\d+)?|\.\d+)(?:[eE][+-]?\d+)?)"
)
# Keys of the release's schema: a number on one of these is a field, never a chart row.
SCHEMA_KEYS = frozenset({"title", "source", "x_title", "y_title", "values"})


class OneChartModel:
    """Own the five CUDA sessions, the fixed prompt, and one owner's decoding state."""

    def __init__(
        self, sessions: dict[str, ort.InferenceSession], config: dict, tokenizer
    ):
        """Keep every graph and derive the cache layout and token ids the graphs need."""
        self.sessions = sessions
        self.tokenizer = tokenizer
        self.num_layers = int(config["num_hidden_layers"])
        self.past_names = past_key_values_names(self.num_layers)
        self.present_names = present_key_values_names(self.num_layers)
        self.eos_token_id = int(config["eos_token_id"])
        self.number_token_id = int(config["number_token"])
        self.image_start_id = tokenizer.convert_tokens_to_ids("<img>")
        self.image_end_id = tokenizer.convert_tokens_to_ids("</img>")
        self.prompt_ids = np.asarray([tokenizer([build_prompt()]).input_ids[0]])
        # Decoding only scores the auxiliary head on later steps, which stays correct only
        # because the fixed prompt never carries the auxiliary token itself.
        if (self.prompt_ids == self.number_token_id).any():
            raise ValueError(
                "The fixed chart prompt must not contain the `<Number>` token"
            )
        # The positional table is finite, so a long chart can never decode past it.
        self.new_token_limit = (
            int(config["max_position_embeddings"]) - self.prompt_ids.shape[1] - 1
        )

    def extract(
        self, pixels: NDArray[np.float32], max_new_tokens: int
    ) -> list[Response]:
        """Greedily decode one batch of charts and run the release's reliability check."""
        batch = pixels.shape[0]
        budget = max(1, min(max_new_tokens, self.new_token_limit))
        input_ids = np.repeat(self.prompt_ids, batch, axis=0)
        embeds = self._run(EMBEDDING_GRAPH, {"input_ids": input_ids})[0]
        features = self._encode_images(pixels)
        embeds = splice_image_features(
            embeds, input_ids, features, self.image_start_id, self.image_end_id
        )
        attention = np.ones(input_ids.shape, dtype=np.int64)

        logits, hidden, cache, producer = self._first_step(embeds, attention)
        tokens = np.argmax(logits[:, -1, :], axis=-1).astype(np.int64)
        finished = tokens == self.eos_token_id
        # Only non-EOS tokens are kept, so `output_tokens` counts the same thing in every step.
        rows: list[list[int]] = [[] for _ in range(batch)]
        predicted: list[list[float]] = [[] for _ in range(batch)]
        for row, token in enumerate(tokens):
            if not finished[row]:
                rows[row].append(int(token))
        # This step consumed the fixed prompt, which carries no `<Number>` token, so the
        # auxiliary head has nothing to score yet. `__init__` enforces that assumption.

        for _ in range(budget - 1):
            if finished.all():
                break
            # Finished rows keep decoding the pad token so every row shares one batch step.
            step = np.where(finished, self.pad_token_id, tokens).astype(np.int64)
            embeds = self._run(EMBEDDING_GRAPH, {"input_ids": step[:, None]})[0]
            attention = np.concatenate(
                [attention, np.ones((batch, 1), dtype=np.int64)], axis=1
            )
            # The previous binding and its cache tensors must outlive this call, because the
            # cached graph reads device buffers that the producing binding still owns.
            logits, hidden, cache, producer = self._cached_step(
                embeds, attention, cache
            )
            tokens = np.argmax(logits[:, -1, :], axis=-1).astype(np.int64)
            # Score the rows that just consumed `<Number>`, using this step's hidden states.
            self._collect_numbers(step, hidden, predicted)
            for row, token in enumerate(tokens):
                # `finished` still holds the previous step's state, so the terminating EOS has
                # to be excluded here as well to keep `output_tokens` counting the same thing.
                if not finished[row] and int(token) != self.eos_token_id:
                    rows[row].append(int(token))
            finished = finished | (tokens == self.eos_token_id)
        del producer, cache

        results: list[Response] = []
        for row, tokens_row in enumerate(rows):
            text = clean_output(self.tokenizer, tokens_row)
            distance, reliable = reliability(text, predicted[row])
            results.append(
                {
                    "text": text,
                    "data": parse_chart(text),
                    "table": recover_table(text),
                    "magnitudes": predicted[row],
                    "output_tokens": len(tokens_row),
                    "reliable_distance": distance,
                    "reliable": reliable,
                }
            )
        return results

    @property
    def pad_token_id(self) -> int:
        """Return the release's padding id, used to keep finished rows in the batch."""
        return int(self.tokenizer.pad_token_id)

    @property
    def providers(self) -> list[str]:
        """Report the providers of the decoder session that owns the decoding loop."""
        return list(self.sessions[DECODER_GRAPH].get_providers())

    @property
    def use_io_binding(self) -> bool:
        """Report whether decoding keeps its key/value cache on the device.

        Both decoder steps bind their cache with an `IOBinding`, so this mirrors the release's
        health field rather than hardcoding it at the call site.
        """
        return True

    def _collect_numbers(
        self,
        tokens: NDArray[np.int64],
        hidden: NDArray[np.float32],
        predicted: list[list[float]],
    ) -> None:
        """Score the rows whose consumed token is the auxiliary `<Number>` token.

        The release reads the head off the hidden state of the `<Number>` token itself, so the
        caller passes the tokens this step consumed rather than the tokens it produced, and the
        matching hidden states are the ones at that step's single position.
        """
        rows = np.flatnonzero(tokens == self.number_token_id)
        if not len(rows):
            return
        scores = self._run(NUMBER_GRAPH, {"hidden_states": hidden[:, 0, :]})[0]
        for row in rows:
            predicted[int(row)] = scores[int(row)][:NUMBER_LIMIT].tolist()

    def _encode_images(self, pixels: NDArray[np.float32]) -> NDArray[np.float32]:
        """Project charts into visual tokens, chunked so the vision tower's peak stays bounded."""
        chunks = [
            self._run(
                VISION_GRAPH, {"pixel_values": pixels[start : start + VISION_BATCH]}
            )[0]
            for start in range(0, pixels.shape[0], VISION_BATCH)
        ]
        return np.concatenate(chunks, axis=0)

    def _run(self, name: str, feeds: dict[str, NDArray]) -> list[Any]:
        """Run a graph whose outputs are small enough to return on the host."""
        return list(self.sessions[name].run(None, feeds))

    def _first_step(self, embeds, attention):
        """Run the cache-building graph and keep its cache tensors on the device."""
        session = self.sessions[DECODER_GRAPH]
        binding = session.io_binding()
        binding.bind_cpu_input("inputs_embeds", embeds)
        binding.bind_cpu_input("attention_mask", attention)
        # Only the cache benefits from staying on the device; logits and hidden states are small.
        binding.bind_output("logits", "cpu")
        binding.bind_output("last_hidden_state", "cpu")
        for name in self.present_names:
            binding.bind_output(name, "cuda")
        session.run_with_iobinding(binding)
        outputs = binding.get_outputs()
        cache = dict(zip(self.present_names, outputs[2:]))
        return outputs[0].numpy(), outputs[1].numpy(), cache, binding

    def _cached_step(self, embeds, attention, cache):
        """Extend the device-resident cache by one token for the whole batch."""
        session = self.sessions[CACHED_DECODER_GRAPH]
        binding = session.io_binding()
        binding.bind_cpu_input("inputs_embeds", embeds)
        binding.bind_cpu_input("attention_mask", attention)
        for past, present in zip(self.past_names, self.present_names):
            binding.bind_ortvalue_input(past, cache[present])
        binding.bind_output("logits", "cpu")
        binding.bind_output("last_hidden_state", "cpu")
        for name in self.present_names:
            binding.bind_output(name, "cuda")
        session.run_with_iobinding(binding)
        outputs = binding.get_outputs()
        cache = dict(zip(self.present_names, outputs[2:]))
        return outputs[0].numpy(), outputs[1].numpy(), cache, binding


def load_model() -> OneChartModel:
    """Load every exported graph on CUDA together with the released tokenizer."""
    from transformers import AutoTokenizer

    config = json.loads((MODEL_DIR / "config.json").read_text())
    # Torch supplies the matching CUDA and cuDNN libraries that ONNX Runtime needs.
    ort.preload_dlls()
    options = ort.SessionOptions()
    options.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
    LOGGER.info("Loading OneChart ONNX graphs on CUDA")
    sessions = {}
    for name in GRAPHS:
        session = ort.InferenceSession(
            str(MODEL_DIR / name),
            options,
            providers=[("CUDAExecutionProvider", CUDA_PROVIDER_OPTIONS)],
        )
        if session.get_providers()[0] != "CUDAExecutionProvider":
            raise RuntimeError(
                f"Unexpected execution provider: {session.get_providers()}"
            )
        sessions[name] = session
    tokenizer = AutoTokenizer.from_pretrained(
        MODEL_DIR, use_fast=False, local_files_only=True
    )
    # The release predates generic `add_bos_token` support, so transformers now prepend
    # `</s>` and the model answers as though its turn had already ended.
    tokenizer.add_bos_token = False
    LOGGER.info("Loaded OneChart with %d ONNX graphs on CUDA", len(sessions))
    return OneChartModel(sessions, config, tokenizer)


def generate(
    model: OneChartModel,
    images: list[Image.Image],
    max_new_tokens: int = MAX_NEW_TOKENS,
) -> list[Response]:
    """Batch valid charts with ONNX Runtime while preserving per-image failures and order."""
    # One unusable upload must not discard unrelated charts sharing the same CUDA batch.
    results: list[Response] = [{} for _ in images]
    pixels, indices = [], []
    for index, image in enumerate(images):
        try:
            pixels.append(preprocess(image))
            indices.append(index)
        except (ValueError, OSError) as error:
            results[index] = {"error": str(error)}
    if not pixels:
        return results
    for index, extracted in zip(
        indices, model.extract(np.concatenate(pixels), max_new_tokens)
    ):
        results[index] = extracted
    return results


def clean_output(tokenizer, tokens: list[int]) -> str:
    """Turn generated ids into the release's cleaned chart dictionary text."""
    text = tokenizer.decode(tokens, skip_special_tokens=True)
    # `<Number>` is an added token rather than a special token, so it survives decoding.
    text = text.replace("<Number>", "").strip()
    # The release drops only a trailing `</s>`. Its demo also drops a trailing period, but the
    # artifact deployed here does not, and diverging would report fields the release rejects.
    text = text.removesuffix("</s>")
    return text.strip()


def values_body(text: str) -> str:
    """Return the text holding the chart's rows, tolerating a missing or unclosed `values`.

    Normally this is inside the `values` object. The release frequently omits that wrapper and
    writes its series straight after `title`, so a missing `values` key falls back to the whole
    document rather than reporting an empty table. Where `values` is present, brace depth bounds
    the scan, which keeps a later numeric schema field such as `source` from being read as a
    data point. The scan is quote-aware so a brace inside a string does not skew the depth.
    """
    marker = text.find('"values"')
    if marker < 0:
        return text
    brace = text.find("{", marker)
    if brace < 0:
        return text
    depth = 0
    quoted = False
    for index in range(brace, len(text)):
        character = text[index]
        if character == '"':
            quoted = not quoted
        elif not quoted:
            if character == "{":
                depth += 1
            elif character == "}":
                depth -= 1
                if depth == 0:
                    return text[brace + 1 : index]
    # No closing brace survived; keep everything that follows rather than losing the reading.
    return text[brace + 1 :]


def recover_table(text: str) -> dict[str, list[dict[str, str]]]:
    """Recover the chart's series rows from the release's output, tolerating malformed JSON.

    The released model reliably writes the `title`/`source`/`x_title`/`y_title`/`values`
    schema, but on annotation-heavy charts it drops a quote or a brace and the text stops being
    valid JSON, so :func:`parse_chart` returns nothing. This scan walks the same text, keeps
    every `"label": value` pair it can still recognise, and attributes each pair to the series
    opened most recently before it. Series names are followed by `{` rather than a number and
    the schema's own fields are skipped, so neither becomes a row. Grouped values lose their
    separators, matching how the release's reliability check reads a magnitude.
    """
    body = values_body(text)
    groups = [(match.start(), match.group(1)) for match in GROUP.finditer(body)]
    table: dict[str, list[dict[str, str]]] = {}
    for match in ROW.finditer(body):
        label = match.group(1)
        if label in SCHEMA_KEYS:
            continue
        series = ""
        for start, candidate in groups:
            if start >= match.start():
                break
            series = candidate
        table.setdefault(series, []).append(
            {"label": label, "value": match.group(2).replace(",", "")}
        )
    return table


def collect_numbers(value) -> list[float]:
    """Collect numeric magnitudes from parsed chart values, following the release's rules."""
    # A malformed chart can still parse while `values` holds a scalar or a list; the release
    # would raise there, but the service must reject the reading instead of failing a batch.
    if not isinstance(value, dict):
        return []
    collected: list[float] = []
    for entry in value.values():
        if isinstance(entry, dict):
            collected.extend(collect_numbers(entry))
        elif isinstance(entry, list):
            return []
        elif isinstance(entry, (int, float)):
            # The release tests `float`/`int` only, so it counts booleans as 0 and 1; mirroring
            # that keeps the reliability verdict identical to the published implementation.
            collected.append(float(entry))
        else:
            # Drop footnote markers such as "(1)" before stripping every non-numeric character.
            cleaned = re.sub(r"[^\d.-]", "", re.sub(r"\(\d+\)|\[\d+\]", "", str(entry)))
            if cleaned not in ("", "-", "*", "none", "None"):
                collected.append(float(cleaned))
    return collected


def reliability(text: str, predicted: list[float]) -> tuple[float | None, bool]:
    """Compare the release's auxiliary magnitudes with the values it parsed from its own output."""
    try:
        values = collect_numbers(json.loads(text)["values"])
        if not values or len(predicted) < len(values):
            return None, False
        normalized = np.asarray([round(item, 4) for item in normalize(values)])
        distance = float(
            np.abs(np.asarray(predicted[: len(values)]) - normalized).mean()
        )
    except (KeyError, TypeError, ValueError):
        return None, False
    return distance, distance < RELIABLE_DISTANCE


def normalize(values: list[float]) -> list[float]:
    """Map chart values onto 0..1, exactly as the release's reliability check does."""
    if len(values) < 2:
        return values
    array = np.asarray(values, dtype=np.float64)
    return list((array - array.min()) / (array.max() - array.min() + 1e-9))


def parse_chart(text: str) -> dict | None:
    """Return the extracted chart dictionary, or None when the model did not close valid JSON."""
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return None
    return parsed if isinstance(parsed, dict) else None
