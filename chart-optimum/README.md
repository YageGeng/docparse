# OneChart with Optimum and ONNX CUDA

Standalone chart structural extraction service. It converts
[`kppkkp/OneChart`](https://huggingface.co/kppkkp/OneChart) into ONNX with Optimum's
exporter and serves it from ONNX Runtime on CUDA.

OneChart is an OPT-125M decoder with a SAM ViT-B vision tower, a linear projector, and an
auxiliary head that predicts the magnitudes of the chart values it just wrote. `runtime.py`
reproduces the released `chat()` flow: resize the chart, project it into the 256 `<imgpad>`
positions of a fixed prompt, greedily decode a chart dictionary, and compare the auxiliary
head's magnitudes with the values parsed back out of that dictionary.

## Setup

Use Linux x86_64, an NVIDIA CUDA GPU with a compatible driver, and `uv`. Python 3.12 and the
tested dependencies are pinned in `pyproject.toml` and `uv.lock`. PyTorch comes from the CUDA
12.8 wheel index. This deployment does not provide a CPU or macOS inference fallback.

```sh
cd chart-optimum
uv sync --locked
uv run --locked python prepare_model.py
./start.sh
```

The startup script works from any directory and defaults to `0.0.0.0:6009`. Override `HOST`,
`PORT`, or `CUDA_VISIBLE_DEVICES` when needed. Keep one Uvicorn worker: `SessionManager`
creates the independent model owners inside that process. Edit `config.toml` to control
concurrency and memory use:

```toml
session_size = 1
queue_size = 128
batch_size = 1
```

`session_size` independent owners each load the full graph set on their own thread and consume
one shared bounded queue. `queue_size` counts admitted pending charts. `batch_size` is the
largest group of ready charts one owner decodes together, and every value must be a positive
integer with `batch_size` at most 32. Unknown keys fail startup.

The shipped default is one owner and one chart per batch. Two settings bound different
resources. `session_size` duplicates the whole graph set, about 1.45 GiB per owner, so it is
what the device's total memory limits. `batch_size` pays only for the decoder: about 72 MiB of
key/value cache per chart at a 1024-token budget, plus the first step's logits. Owners project
charts one at a time regardless of `batch_size`, because the vision tower is the expensive part
— one chart peaks at about 2.1 GiB of float32 attention buffers — and running two through it at
once exhausts an 8 GiB card. The shipped `batch_size` of 1 is therefore the measured choice for
an 8 GiB device: a single chart leaves roughly 1.7 GiB free, and a three-chart request was
observed to exhaust the arena. Every capacity is also capped in `configuration.py`, so a typo
fails at startup rather than after loading several graph sets.

```sh
CUDA_VISIBLE_DEVICES=0 PORT=6009 ./start.sh
# Select another configuration file with an absolute path.
CHART_CONFIG=/path/to/config.toml ./start.sh
```

Each owner drains ready uploads up to its batch limit, grouping equal generation budgets;
sparse uploads get a 3 ms collection window while full ready batches start immediately.
Canceled queued requests are skipped and one failed batch does not stop other owners. HTTP
disconnects cancel admission waits and the corresponding queued reply. The service becomes
ready only after every model has loaded, and shutdown releases pending callers and waits for
running inference to finish. Model files and virtual environments are ignored by Git.

## Model preparation

`prepare_model.py` pins revision `79212de2f520694e534e58240f228eff351d536c` and rejects a
checkpoint whose `model.safetensors` does not hash to
`15ab0e9971384db1f17414d6b1e4d9d55bfcdb80d810dfb161b316fb10b6220b`. Original assets stay in
`models/OneChart`; the deployment copy goes to `models/OneChart-optimum`. Run preparation
before starting the service, not while it is loading models.

The release publishes no ONNX export, so preparation writes five graphs through
`optimum.exporters.onnx.export` at opset 18 in float32:

| Graph | Inputs | Outputs |
| --- | --- | --- |
| `vision_encoder.onnx` | `pixel_values` `[batch, 3, 1024, 1024]` | `image_features` `[batch, 256, 768]` |
| `embedding.onnx` | `input_ids` `[batch, sequence]` | `inputs_embeds` `[batch, sequence, 768]` |
| `decoder_model.onnx` | `inputs_embeds`, `attention_mask` | `logits`, `last_hidden_state`, `present.N.key/value` |
| `decoder_with_past_model.onnx` | `inputs_embeds`, `attention_mask`, `past_key_values.N.key/value` | `logits`, `last_hidden_state`, `present.N.key/value` |
| `number_head.onnx` | `hidden_states` `[batch, 768]` | `numbers` `[batch, 256]` |

Optimum's task classes cannot drive these graphs. `ORTDecoder.forward` binds exactly
`input_ids`, `attention_mask`, `position_ids`, and the key/value cache, so the projected image
features that replace the `<imgpad>` embeddings have no route into a stock decoder session.
Exporting the split decoder with Optimum and running it directly keeps the visual splice and the
auxiliary head intact without forking Optimum. The projection is fused into the vision graph and
the splice happens between the embedding and decoder graphs, which also avoids an in-graph
gather over a dynamic placeholder run.

The decoder is left unmerged, so the cache-building and cache-extending steps are separate
graphs. Optimum's post-export dynamic-axes fix runs normally, and preparation then proves the
surviving axes on real inputs: the decoder graphs run on a batch of two and the cached decoder
walks two steps, so its `past` axis grows 308, 309, 310 and a frozen dimension cannot pass. The
vision graph stays at batch one for the memory reason above. `adaptation.json` records both the
deviations and the shapes that were exercised. Values below are rounded up:

| Graph | Largest absolute deviation | Whole-tensor relative error |
| --- | --- | --- |
| Vision encoder | 1.4e-3 | 1.1e-5 |
| Token embedding | 0 | 0 |
| First decoder step (logits / hidden / cache) | 3.5e-4 / 1.5e-4 / 1.0e-4 | 2.5e-6 |
| Cached decoder steps, both (logits / hidden / cache) | 3.2e-5 / 3.2e-5 / 9.6e-6 | 2.0e-6 |
| Auxiliary value head | 6.0e-8 | 1.6e-7 |

Preparation fails if any graph exceeds 2e-3 elementwise or 1e-3 of whole-tensor relative error.
The relative bound is the one that matters: re-enabling TF32 moves the same measurement to
about 1.2e-3 while leaving every individual element within `allclose`.

`adaptation.json` records the revision, the checkpoint digest, every graph's inputs, outputs,
and digest, and these deviations.

### Release compatibility notes

Three properties of the published release needed explicit handling.

**Leading BOS.** The checkpoint records `add_bos_token: true`, but it was published from
transformers 4.32.1, where `GPT2Tokenizer` ignored that key. Later transformers act on it, and
because `</s>` is also OneChart's BOS they prepend it, so the model answers as though its turn
had already ended: the opening tokens fall back to unrelated languages and the auxiliary head
never fires. Preparation and serving clear the flag, which restores the published behaviour.

**Positional table.** OPT's positional embedding holds `max_position_embeddings` rows plus a
two-row offset, so it can address 4096 positions, and the deployed prompt is 308 tokens. The
model card's `chat()` asks for up to 4096 new tokens, which overruns the table and aborts with a
CUDA indexing fault. Preparation pins the released demo's 1024-token budget and the runtime
additionally clamps every request to the table's remaining room.

**Tensor-core precision.** ONNX Runtime enables TF32 tensor cores by default on Ampere and later
GPUs. TF32 keeps ten mantissa bits, which raised the vision tower's deviation from PyTorch to
1.2e-3 relative — enough to change greedy token choices — so preparation and serving pin
`use_tf32=0` and full float32 accumulation.

**Chart interpolation.** The GitHub repository's demo resizes with OpenCV's `INTER_LINEAR`, while
the Hugging Face release — the artifact deployed here — uses its own bicubic
`OneChartImageEvalProcessor`. Those differ materially. Downscaling a 1643-pixel-wide chart with
`INTER_LINEAR` applies no antialiasing, and on the reference charts it scales the reading by ten,
so its values fall outside the plotted axis entirely. The deployment therefore keeps the
release's bicubic transform, which recovers every value it reports from inside the axis range.

One setting is deliberately left alone: the CUDA arena keeps its default power-of-two chunk
extension. Every decode step grows the key/value cache, and asking for exact-size chunks instead
fragments the arena until decoding fails part way through a chart.

## HTTP API

- `GET /v1/health` (also `/health`): readiness, providers, and queue occupancy.
- `GET /v1/models`: served model identity.
- `POST /v1/predictions/upload`: multipart PNG/image input and JSON chart output.
- `/docs`: interactive API documentation.

```sh
curl --fail http://127.0.0.1:6009/v1/predictions/upload \
  -F 'image=@chart.png' -F 'max_new_tokens=1024'
```

`max_new_tokens` limits the generated tokens (2–1024). Responses contain:

- `text`: the model's cleaned chart dictionary, exactly as generated.
- `data`: that dictionary when the model closed valid JSON, otherwise `null`.
- `table`: the extracted chart data as `{series: [{label, value}, ...]}`, recovered from `text`
  by a tolerant scan. The release does not always close valid JSON on annotation-heavy charts,
  and a strict parse would then return nothing, so this scan keeps every `"label": value` pair it
  can still recognise, within the `values` object only, and attributes each to the series opened
  before it. It reports `{}` when the model never reached a `values` section. Labels are verbatim
  but a grouped value loses its separators (`"1,234"` becomes `1234`), matching how the release's
  own reliability check reads a magnitude.
- `output_tokens`: how many tokens the model generated for that chart, excluding the
  terminating `</s>` and including the leading `<Number>` marker that `text` drops.
- `magnitudes`: the auxiliary head's reading of the `<Number>` token, which is what the release's
  reliability check compares against the parsed values. It is reported because it is the one
  signal that proves the head was applied to the right decoding step.
- `reliable_distance` and `reliable`: the release's own verdict, which compares those magnitudes
  with the values parsed from the dictionary and accepts them below an L1 distance of 0.1. The
  distance stays `null` whenever the text is not valid JSON, because there is nothing to compare.

An undecodable image or one above 16 megapixels returns 400, a body above 16 MiB returns 413, a
malformed form field returns 422, an unavailable session or failed inference returns 503, a
request exceeding 120 seconds returns 504, and a client disconnect abandons the request and
returns the non-standard 499. The handler downscales each chart to the deployment's 1024x1024
square before admitting it, so a full queue holds one small image per pending request rather than
the upload the caller sent. CORS and authentication are not configured by this service; browser
deployments can provide them at a proxy.

This directory is a standalone deployment. `docparse.toml` has no chart engine entry, so
nothing in the Rust workspace consumes it yet.

## Verification

From this directory, install the locked dependencies and check all Python scripts and tests.
Pyright uses the project's Python 3.12 `.venv` via `pyproject.toml`:

```sh
uv sync --locked
uvx pyright --project .
uvx ruff check .
uvx ruff format --check .
```

The CPU-only regression checks cover the released image transform and prompt layout, the visual
splice, TOML validation, concurrent owners, queue saturation, cancellation, teardown, and HTTP
disconnect handling:

```sh
uv run --locked python -m unittest discover -s tests -p 'test_*.py'
```

`tests/parity.py` is the acceptance test for the conversion. It captures the released pipeline's
text in float32, releases the PyTorch model, and requires the ONNX owner to reproduce that text
exactly for every real chart. It also requires the auxiliary head's magnitudes to match PyTorch,
and fails if a chart that emitted a `values` section yields no recoverable table rows:

```sh
uv run --locked python tests/parity.py --images /path/to/charts
```

Add `--write-reference tests/reference.json` to repin the cases that the HTTP check replays.

With the service running, the smoke check sends the real charts, compares every response's `text`,
`table`, and `magnitudes` against the pinned cases, repeats them concurrently to exercise queueing
and cancellation, and verifies invalid-image rejection and recovery:

```sh
uv run --locked python tests/smoke.py --url http://127.0.0.1:6009
```

`tests/controls.py` reports how the served model does on clean matplotlib charts with known
values, and is the check that separates a hard input from a weak model:

```sh
uv run --locked python tests/controls.py --url http://127.0.0.1:6009
```

### Local performance and extraction quality

On an RTX 4060 Laptop GPU (8 GiB) with one owner, one chart per batch, and a 1024-token budget,
a chart takes 0.7-3.8 seconds depending on how much it generates: 811 tokens for the dual-axis
line, 396 for the histogram, and 82 for a five-slice pie.

The conversion is faithful; the extraction quality is the released weights', and it is poor
enough that this output should not be consumed as data.

The three charts prepared for this deployment:

- **Histogram.** 15 rows for 15 bars, but the labels are corrupted (`15-14`, `24-24`, `30-33`
  instead of `13-14` to `27-28`), only 4 values land within 1% of a bar and 7 within 10%, and the
  61775 peak is split across two rows reading 61717 and 61789.
- **Dual-axis line.** 18 rows for the first series, all inside the plotted 575-705 W band, but
  only 19% of its 96 points, with labels drifting to `16:`, `22:`, `42`. Its second series is
  named `Ambient Temple: (%deg(%` and carries left-axis magnitudes instead of the 22.1-22.9 C
  ambient readings, so the second axis is lost entirely.
- **Three-series line.** Nothing: the model degenerates into one repeated token for all 1024
  tokens and never opens `values`.

Those inputs are demanding, which is why `tests/controls.py` replays three plain matplotlib
charts whose values are known exactly. They behave no better:

| Control | Labels correct | Values present | Valid JSON | Emitted `values` |
| --- | --- | --- | --- | --- |
| Six bars with the values printed on them | 1/6 | 3/6 | no | no |
| Twelve-point line | 0/12 | 2/12 | no | no |
| Five-slice pie | 0/5 | 4/5 | yes | no |

On the bar chart the model wrote no `values` key at all and mangled the title into `"title": {`;
on the line chart it produced values between 5 and 15 where the data runs 10 to 35; on the pie it
read four of five percentages but invented every slice name (`Girls`, `USA`). The difficulty of
the first three charts is therefore not an adequate explanation for the results.

Because nothing closes the release's schema, `data` is `null` and `table` carries the reading,
and `reliable_distance` is `null` as well: the release's reliability check needs parsed values and
cannot fire on any chart tested here. The float32 PyTorch pipeline reproduces every one of these
outputs token for token, so a `tests/parity.py` failure means the conversion regressed, not that
the model improved. A consumer that needs accurate chart values has to add its own extraction and
validation stage rather than trusting this output.

One caveat this deployment cannot settle: the weights are a community upload on Hugging Face. Its
digest is pinned and the upstream repository points at it, but there is no official reference
output here to compare against, so a corrupted upload would look exactly like a weak model.
Checking a few ChartSE benchmark images would separate those two cases.
