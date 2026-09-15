# Formula recognition and conversion research — 2026-09-15

## Conclusion

OAR-OCR already implements formula-image recognition into LaTeX, formula-region cropping and batching inside its document pipeline, tokenizer decoding, LaTeX normalization, Markdown math wrappers, and insertion into table cells. It does not provide a general LaTeX-to-MathML/OMML conversion engine or a browser math renderer in the inspected classic pipeline.

For DocParse, retain the current layout, source-text ownership and inline-span matching, and evaluate only the missing image-to-LaTeX recognition path. Compare **PP-FormulaNet_plus-S** and **PP-FormulaNet_plus-M** first. Treat CoreML speed, generated-formula correctness and browser compatibility as unverified until measured on actual formula crops.

This is a source and documentation investigation. No formula model was downloaded or benchmarked, no recognition capability was added to production, and no commit was created.

## Evidence snapshot

- DocParse: `366ebdc0674397c9e2be269d721cc440910ed528`.
- OAR-OCR upstream `main`, fetched on 2026-09-15: `7feb044d74be09e3e2078a89cec0f0f8688e942b`; workspace version 0.9.3. The checked source declares Apache-2.0 and pins `ort` to `2.0.0-rc.13`.
- Upstream sources are pinned to that revision in the links below. The local read-only reference checkout is under `target/research/oar-ocr/`.
- The workload count uses the completed CoreML HTTP run's downloaded JSON, not new model predictions. See [workload.json](workload.json) and the [HTTP report](../2026-09-15-coreml-http-release/README.md).

## 1. What DocParse already does

The layout vocabulary distinguishes `display_formula`, `inline_formula`, and `formula_number`. Formula regions constrain native glyph ordering and superscript/subscript grouping. The semantic matcher attaches non-owning inline spans to existing text-item ranges. `InlineSpan` stores extracted source text, geometry, detector confidence and a content-coverage status, but no recognized LaTeX.

`InlineContentStatus::Complete` is selected when overlapping text-item area covers at least 80% of the formula box; otherwise non-empty overlap is `Partial`, and no overlap is `Missing`. This is **geometric source coverage, not mathematical correctness or recognition confidence**. The renderer emits native text and inserts the configured placeholder for missing inline content. It does not reconstruct general LaTeX expressions or render math.

Code references: [layout labels](../../../crates/layout/src/types.rs), [InlineSpan](../../../crates/core/src/types.rs), [formula matcher](../../../crates/core/src/semantic/formula.rs), [source ordering](../../../crates/core/src/line/formula.rs), [line rendering](../../../crates/core/src/render/mod.rs), [Markdown rendering](../../../crates/core/src/render/markdown.rs).

The latest 20-PDF / 694-page HTTP results contain:

| Existing annotation | Count |
| --- | ---: |
| Display-formula blocks | 656 |
| Attached inline-formula spans | 4,328 |
| Inline spans marked Partial | 4,161 |
| Inline spans marked Complete | 141 |
| Inline spans marked Missing | 26 |
| Separate formula-number blocks | 228 |

Eighteen PDFs contain formula annotations. The 4,984 formula blocks/spans are detector-derived annotations, not a manually verified count of distinct mathematical expressions. Recognizing every `Partial` span would already cover 96.1% of inline spans. Conversely, recognizing only the 26 missing spans would leave most image-to-LaTeX conversion undone.

For budgeting only: if a recognizer averaged 200ms per crop, 4,984 serial crops would require 996.8s of recognition time before batching or overlap. This is a hypothetical cost calculation, **not an M4 measurement**.

## 2. OAR-OCR implementation map

| Capability | Inspected implementation |
| --- | --- |
| Standalone formula crops to LaTeX | `FormulaRecognitionPredictor::predict(Vec<RgbImage>)` |
| Model families | PP-FormulaNet S/L, Plus S/M/L, UniMERNet |
| Model input/output | Preprocessed image tensor; exported ONNX model returns token IDs |
| Decoding | Matching `tokenizer.json`, BOS/EOS filtering, vocabulary checks, normalization |
| Document integration | `OARStructureBuilder::with_formula_recognition`, crop recognized formula regions, batch inference |
| Result | `FormulaResult { bbox, latex, confidence }` |
| Markdown | Context-based `$...$` or `$$...$$` emission |
| Table formulas | Inject LaTeX-wrapped regions into cell-content matching |
| HTML | Escape LaTeX and wrap it in a formula paragraph with `$$`; no actual typesetting engine is included |

Primary sources: [predictor](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/src/predictors/formula_recognition.rs#L85), [ONNX model adapter](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/src/models/recognition/pp_formulanet.rs#L119), [document crop/batch path](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/src/oarocr/structure.rs#L1952), [Markdown renderer](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/src/domain/structure.rs#L610), [table stitching](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/src/oarocr/stitching.rs#L482), [HTML output](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/src/domain/structure.rs#L949).

## 3. Candidate models

The ONNX artifacts and tokenizer links exist in OAR-OCR's model guide; release asset sizes were also checked through the GitHub release API. These are storage sizes, not runtime memory requirements.

| Model | OAR ONNX size | Suggested role in an initial comparison |
| --- | ---: | --- |
| PP-FormulaNet_plus-S | 221.1 MiB | Speed candidate, especially English mathematical content |
| PP-FormulaNet_plus-M | 564.9 MiB | Chinese text inside formulas and complex expressions |
| PP-FormulaNet_plus-L | 699.5 MiB | Additional quality candidate if M remains insufficient |
| UniMERNet | About 1.72 GiB | Larger comparison model; not the first deployment candidate |

All PP variants require their matching tokenizer; UniMERNet has a separate tokenizer. See [OAR model assets](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/docs/models.md#formula-recognition).

PaddleOCR documents Plus-M/L improvements for Chinese formulas and support for longer predictions, while Plus-S emphasizes English formula recognition. Its published latency measurements use a Tesla T4 and Xeon system with Paddle inference, so they must not be presented as Rust/CoreML/M4 measurements. [PaddleOCR formula documentation](https://www.paddleocr.ai/main/en/version3.x/module_usage/formula_recognition.html)

## 4. Important implementation limits

### Confidence and maximum length

The classic adapter receives token IDs without model probabilities. It returns `None` scores for accepted formulas; `score_threshold` therefore does not filter them. The structure pipeline converts absent confidence to `0.0`, which must not be interpreted as a calibrated zero-probability result. Keep unknown recognition confidence optional in DocParse and distinguish it from layout-detector confidence.

`max_length` truncates tokens **after the ONNX inference call**. It is not a model generation budget and cannot be assumed to reduce inference cost. Truncation may also remove required closing syntax. The example CLI declares `--max-length` and target dimensions but does not pass those arguments into the predictor builder; direct API calls are needed for a trustworthy parameter experiment with this revision. [Adapter execution](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/src/domain/adapters/formula_recognition_adapter.rs#L190), [example builder call](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/examples/formula_recognition.rs#L184)

### Formula identity, numbering and normalization

OAR maps both inline and display labels into one `Formula` type and infers display style later from neighboring text. Its `is_formula()` also includes `FormulaNumber`, so the crop path can recognize equation numbers; its Markdown renderer then skips separate formula-number elements. DocParse already has distinct labels and explicit inline spans: retain those distinctions and preserve equation numbering separately.

The normalizer is a model-output cleanup routine, not a LaTeX parser or mathematical equivalence checker. It removes some Chinese text wrappers, quote characters and spaces. Preserve original decoded LaTeX alongside any normalized form until visual/semantic checks establish that normalization is safe for the target examples. [Label mapping and formula classification](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/src/domain/structure.rs#L2103), [normalizer](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/src/processors/formula_preprocess.rs#L268)

### CoreML, CUDA and browser execution

OAR's formula path uses exported ONNX inference rather than a Rust-managed token-by-token decoder with an exposed KV-cache loop. Its CUDA workaround explicitly refers to PP-FormulaNet's autoregressive ONNX `Loop`; enabling CUDA/TensorRT formula recognition may set `CUDA_LAUNCH_BLOCKING=1` before session creation when the environment does not already define it. This process-wide setting needs separate throughput evaluation rather than automatic reuse in DocParse. [Workaround implementation](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/src/core/inference/ort_infer_builders.rs#L72)

The shared example CoreML configuration sets `subgraphs: Some(false)`. ONNX Runtime documents `EnableOnSubgraphs` as the switch controlling CoreML execution inside `Loop`, `Scan` and `If` bodies. Thus registration alone does not establish that a formula decoder is accelerated. Inspect the specific ONNX artifact and compare subgraph execution disabled/enabled, graph format, partitioning, output parity and latency. Dynamic shapes and compilation/readback costs remain model-specific. [OAR device options](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/examples/utils/device_config.rs#L156), [ORT CoreML options](https://onnxruntime.ai/docs/execution-providers/CoreML-ExecutionProvider.html#available-options-new-api)

OAR's Cargo `webgpu` feature enables the native `ort` execution provider. It is not evidence that these exported models and tokenizers already work inside DocParse's ONNX Runtime Web Worker. Browser operator/control-flow coverage, tokenizer packaging and download/runtime memory need a separate production-WASM test. [Core dependencies and features](https://github.com/GreatV/oar-ocr/blob/7feb044d74be09e3e2078a89cec0f0f8688e942b/oar-ocr-core/Cargo.toml)

## 5. Recognition versus conversion

Recognition determines a formula from pixels. Conversion operates on already recognized LaTeX and cannot repair an incorrectly recognized exponent, symbol, fraction or matrix entry.

| Desired output | Suitable path |
| --- | --- |
| Canonical machine-readable result | Store LaTeX with geometry, source identity and recognition status |
| Markdown | Emit `$...$` for inline and `$$...$$` for display, preserving the known layout kind |
| Web display / accessible MathML | KaTeX can produce HTML, MathML or both; supported syntax still needs validation |
| SVG display/export | MathJax provides an SVG output processor |
| Editable Word or PowerPoint math | Pandoc converts recognized TeX math to OMML in DOCX/PPTX |

Primary documentation: [KaTeX output options](https://katex.org/docs/options.html), [MathJax output components](https://docs.mathjax.org/en/latest/web/components/output.html), [Pandoc math conversion](https://pandoc.org/MANUAL.html#math).

Keep LaTeX as the recognition result and generate presentation/export formats when requested. Successful rendering is a syntax/compatibility check, not proof that the recognized formula matches its source image.

## 6. Recommended next evaluation

1. Reuse DocParse's detected formula boxes and PDFium rendering. Begin with a representative subset of the 656 display-formula blocks, then include short/long inline expressions, fractions, nested scripts, matrices, multiline alignment, Chinese text and formulas inside tables. Keep equation-number boxes out of the recognition input.
2. Compare Plus-S and Plus-M on the same labeled crops, starting with CPU and CoreML release builds. Check model/tokenizer pairing and preprocessing parity before interpreting quality. Compare batches 1/2/4/8 only when the graph supports them.
3. Record initialization/compilation, warmup, crop/preprocess, inference, token decoding/normalization and rendering separately. Report p50/p95 seconds per formula, formulas/s, token length, memory and provider partitioning. Inspect truncated/empty output explicitly.
4. Assess normalized exact match and rendered visual correctness on annotated crops; manually audit semantically dangerous errors. Do not use optional/absent confidence, the current geometric `Partial` flag or render success as an automatic quality gate.
5. Only after choosing a model, integrate recognized formula metadata while retaining original native text items and their ownership. Render one selected representation so inline formulas and table cells do not duplicate both native glyphs and generated LaTeX.
6. Run the full HTTP corpus again and report added document latency against the same concurrency, warmup and timing boundary. Validate the browser separately with the release WASM artifact before claiming WebGPU parity.

This path reuses the existing parser and ONNX lifecycle while taking OAR's model/preprocessing/tokenizer work as a reference. It avoids importing unrelated OCR/layout/table orchestration merely to obtain formula recognition.
