# Formula tokenizer optimization and Plus-S default

## Changes

- Cache the tokenizer vocabulary size once during session initialization. In
  tokenizers 0.22.2, `get_vocab_size(true)` clones the vocabulary; previously it
  ran for every output token. EOS/padding handling and LaTeX decoding are unchanged.
- Default native configuration and the WASM example to PP-FormulaNet_plus-S.
  Its pinned ONNX input is `float32 [batch, 1, 384, 384]` and its token output is
  `int64 [batch, length]`. Plus-L remains available through explicit artifacts,
  with its original 768-pixel preprocessing.
- Select preprocessing and engine names from the verified graph hash, not filenames.
  Both model/tokenizer pairs retain provenance and SHA-256 verification.
- Keep the WASM Formulas switch enabled by default and retain native Apple's
  explicit CPU compatibility policy. This change does not establish CoreML acceleration.

## Native release measurements

Apple M4, CPU compatibility executor, one intra-op thread. Each row measures the
same real English formula crop. Reported values are medians of three single-crop
runs after earlier single/batched inference and a cancellation/reuse check.
Model loading is excluded. Raw records, outputs and crop SHA-256 are included in
the adjacent JSON files and `summary.json`.

| Case | Preprocess | Model inference | Token decoding |
| --- | ---: | ---: | ---: |
| Plus-L, before tokenizer optimization | 1.650 ms | 1528.807 ms | 226.644 ms |
| Plus-L, cached vocabulary size | 1.765 ms | 1483.120 ms | 0.020 ms |
| Plus-S, cached vocabulary size | 0.566 ms | 144.417 ms | 0.024 ms |

The Plus-L output was byte-for-byte identical before and after the tokenizer
optimization. The model switch changes the recognizer and its output; it is not
an accuracy-equivalent transformation. These single-crop timings do not establish
corpus throughput or recognition accuracy.

## Production browser verification

The release SDK was built with Binaryen 132.0.0 `wasm-opt -O4`, then tested in
Chrome 153.0.8010.48 using the production example server, SDK and configured
WebGPU backend. Default layout, TSR, OCR and formula settings were retained.
The real document `2410.05779v3.pdf` has 16 pages and yielded 24 formula results,
including inline and display formulas, with nonempty LaTeX/Markdown and no
formula failures.

| Run | UI parse wall time | Preparation included? |
| --- | ---: | --- |
| First Plus-S parse | 31.7 s | No; may include first-inference kernel work |
| Same sessions, repeated Plus-S parse | 30.5 s | No |
| Formula disabled, newly prepared sessions | 24.6 s | No |

In the warm run, the eight formula batches spent 5.508 s in model inference,
37.0 ms in preprocessing, and 0.4 ms in token decoding for all 24 formulas.
The 30.5 s document total also includes layout, OCR, table processing and PDF work.

The repeated Plus-S run produced identical LaTeX strings. The earlier Plus-L
UI run took 102.4 s on the same document; it was performed in a separate run,
so this is historical context rather than a randomized, warmed A/B comparison.
Stage records include initialization separately; nested stage totals must not
be added together. Successful generation is not a semantic accuracy benchmark.

The browser check also passed default-on state, model identity, LaTeX/Markdown
inspection and clipboard actions, narrow viewport layout, session teardown on
setting changes, and absence of formula downloads/inference while disabled.

## Reproduction

```sh
rtk uv run --locked scripts/download_models.py --model pp-formulanet-plus-s
rtk proxy env FORMULA_TEST_CROP=/absolute/path/to/formula.png \
  FORMULA_BENCH_OUTPUT=/absolute/path/to/result.json \
  cargo test --release --locked -p docparse-formula --features coreml \
  --test model real_formula_batches_preserve_cardinality_and_content \
  -- --ignored --nocapture
```

Set `FORMULA_TEST_MODEL=pp-formulanet-plus-l` to test the large model. For browser
acceptance, build the SDK and example, start `scripts/serve-example.mjs`, then run:

```sh
rtk proxy node crates/web/tests/formula_ui.mjs /absolute/path/to/2410.05779v3.pdf \
  http://127.0.0.1:8768/example/ target/formula-plus-s-ui
```

Validation also passed 30 formula/config tests, 11 core formula/render tests,
10 Python provisioning tests, native/WASM warnings-as-errors Clippy, and the
platform compatibility boundary check. Real native Plus-S and Plus-L tests
covered batches 1/2/3, cancellation and session reuse.
