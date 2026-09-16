# TSR model comparison - 2026-09-14

The wireless RT-DETR detector improves a real gate-ablation header assignment while preserving all seven reviewed tables. Replacing SLANet+ with SLANeXt is not an overall improvement in the current integrated pipeline. Keep model selection explicit; do not treat successful structure validation as semantic accuracy.

## Scope and measurements

19 pages from six native-text PDFs under `/Volumes/Yage/Downloads/docs` produced 29 table crops. All five variants received identical crop pixels (SHA-256 checked). Seven tables have 49 manually reviewed dimension, text-placement and span assertions. The other 22 tables are included in pipeline coverage and visual inspection output but are not assigned a semantic accuracy score.

Apple M4, 16 GiB RAM; native release build, ONNX Runtime CPU, one intra-op thread per model. Every process loaded its selected models and executed two real model warmup rounds. The OS file cache was not cleared. These are single instrumented runs, not statistical throughput benchmarks. Wall time includes model loading, warmup and diagnostic PNG/JSON writes. Peak RSS is an approximate process-tree sample every 200 ms. Model inference totals exclude the surrounding PDF/render/capture work.

| Variant | Structured | Reviewed checks | Reviewed tables fully passing | Structure inference | Cell inference | Process wall | Peak RSS |
|---|---:|---:|---:|---:|---:|---:|---:|
| SLANet+ | 29/29 | 44/49 | 6/7 | 1.39 s | 0.00 s | 9.53 s | 1571 MiB |
| SLANet+ + wired cells | 29/29 | 40/49 | 5/7 | 1.36 s | 5.81 s | 15.03 s | 1965 MiB |
| SLANet+ + wireless cells | 29/29 | 49/49 | 7/7 | 1.37 s | 9.35 s | 18.61 s | 2182 MiB |
| SLANeXt wired + cells | 15/29 | 13/49 | 1/7 | 14.76 s | 5.82 s | 30.17 s | 2660 MiB |
| SLANeXt wireless + cells | 20/29 | 23/49 | 3/7 | 14.71 s | 9.41 s | 33.71 s | 2315 MiB |

## Verified effects and limits

- `2609.13141v1.pdf`, page 6: the baseline splits `Gate Pos.`, `Gate Act.`, `Rank Pres.` and `Train Sco.` across neighboring header cells. Independent cell geometry fixes all nine headers; a captured native-source regression now enforces this behavior.
- `2303.18223v16.pdf`, page 24: SLANet+ retains 16 method/equation rows. Both SLANeXt variants collapse the body into four configuration groups (five rows including the header), losing the finer cell-level method/equation relationships.
- The wired detector still changes the long comparison table to 57 rows and the task taxonomy to 22 rows. The wireless detector preserves the reviewed 58-row and 21-row structures and their tested associations.
- SLANeXt failures include unbalanced tokens and disagreement between logical row starts and observed cell geometry. The current topology-only matcher deliberately rejects ambiguous row correspondence. These failures measure the current integrated pipeline, not the standalone model's accuracy.
- The long comparison crop produces 726 SLANet+ position-head boxes, while each RT-DETR export is limited to 300 detections. Independent geometry cannot be assumed complete on such tables. The integrated SLANet+ path retains finer structure anchors where coarse detections would cross another logical row.
- Classifier routing was not introduced: wired and wireless variants were measured explicitly on the same corpus. These results do not establish behavior on scans, rotated tables or every table in the 20-PDF directory.

## Runtime parity

On the real page-6 crop, OpenCV/Python ONNX reference inference matches all three structure-token sequences exactly. After matching unordered detections, the wired detector's maximum coordinate difference is 0.001465 crop pixels and the wireless detector's difference is zero. Reference versions: OpenCV 4.10.0, NumPy 2.3.5, ONNX Runtime 1.29.0. This checks the integration's tensor and output handling; it does not certify model accuracy.

## Reproduction

The main configuration uses `tsr_only` with SLANet+ and wireless RT-DETR cell detection. Library defaults use the same combination with rules-first fallback.

For a pure SLANet+ comparison, replace the existing TSR sections in `docparse.toml`:

```toml
[tsr]
mode = "tsr_only"
model = "slanet_plus"
model_path = "models/slanet-plus/inference.onnx"
model_config_path = "models/slanet-plus/inference.yml"
model_manifest_path = "models/slanet-plus/model-manifest.json"

[tsr.cell_detection]
enabled = false
```

Alternatively, use SLANeXt wireless with independent wireless cell detection:

```toml
[tsr]
mode = "tsr_only"
model = "slanext_wireless"
model_path = "models/slanext-wireless/inference.onnx"
model_config_path = "models/slanext-wireless/inference.yml"
model_manifest_path = "models/slanext-wireless/model-manifest.json"

[tsr.cell_detection]
enabled = true
model = "wireless"
score_threshold = 0.3
model_path = "models/rtdetr-table-cell-wireless/inference.onnx"
model_config_path = "models/rtdetr-table-cell-wireless/inference.yml"
model_manifest_path = "models/rtdetr-table-cell-wireless/model-manifest.json"
```

```sh
rtk uv run --locked scripts/download_models.py
rtk cargo run -p docparse-cli --release -- parse input.pdf
rtk proxy env TSR_COMPARE_VARIANT=cells-wireless rtk cargo test -p docparse-core --release --test tsr_comparison -- --ignored --nocapture
```

The five experiment values are `baseline`, `cells-wired`, `cells-wireless`, `upgraded-wired` and `upgraded-wireless`. `TSR_COMPARE_PDFS` overrides the corpus directory. Raw captures and reports are under `packages/wasm-web/test-results/tsr-comparison`; `round-1` retains the initial integration results before the generic row-order and coarse-box guards were repaired. No independent service or commit was created.
