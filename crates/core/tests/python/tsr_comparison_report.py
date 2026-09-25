"""Build a portable visual report from the real production TSR comparison captures."""
import base64
import hashlib
import html
import json
from pathlib import Path


def semantic_checks(table, annotation):
    """Evaluate only manually reviewed dimensions, cell text and spans, without claiming full accuracy."""
    checks = [bool(table and (table["row_count"], table["column_count"]) == (annotation["rows"], annotation["columns"]))]
    for expected in annotation["checks"]:
        cell = next((c for c in table["cells"] if (c["row"], c["column"]) == (expected["row"], expected["column"])), None) if table else None
        checks.append(bool(cell and all(
            ("".join(cell["text"].split()) == "".join(value.split()) if key == "text" else cell.get(key) == value)
            for key, value in expected.items() if key not in ("row", "column")
        )))
    return {"passed": sum(checks), "total": len(checks), "all_passed": all(checks)}


def table_html(table):
    """Render exact model-derived cells with escaped source text and retained merge spans."""
    if table is None:
        return '<p class="failure">Structure unavailable; original source text was retained.</p>'
    rows = []
    for row in range(table["row_count"]):
        cells = []
        for cell in sorted((c for c in table["cells"] if c["row"] == row), key=lambda c: c["column"]):
            tag = "th" if cell["is_header"] else "td"
            text = html.escape(cell["text"]).replace("\n", "<br>")
            cells.append(f'<{tag} rowspan="{cell["row_span"]}" colspan="{cell["column_span"]}">{text}</{tag}>')
        rows.append("<tr>" + "".join(cells) + "</tr>")
    return '<div class="table-scroll"><table>' + "".join(rows) + "</table></div>"


def main():
    """Join all variants, assert identical crop pixels and publish inspectable data plus a standalone report."""
    root = Path(__file__).resolve().parents[4]
    runs = root / "packages/wasm-web/test-results/tsr-comparison"
    output = root / "docs/reports/2026-09-14-tsr-model-comparison"
    annotations = json.loads((output / "annotations.json").read_text())
    gold = {(a["case"], a["table"]): a for a in annotations["cases"]}
    variants = ["baseline", "cells-wired", "cells-wireless", "upgraded-wired", "upgraded-wireless"]
    labels = {"baseline": "SLANet+", "cells-wired": "SLANet+ + wired cells", "cells-wireless": "SLANet+ + wireless cells", "upgraded-wired": "SLANeXt wired + cells", "upgraded-wireless": "SLANeXt wireless + cells"}
    data, metrics = {}, []
    for variant in variants:
        report = json.loads((runs / variant / "report.json").read_text())
        process = json.loads((runs / variant / "process.json").read_text())
        assert process["exit_code"] == 0
        pages = report["pages"]
        timings = [t for page in pages for t in page["timings"]]
        score = {"passed": 0, "total": 0, "tables_passed": 0, "tables_reviewed": len(gold)}
        for page in pages:
            case = page["directory"]
            result = json.loads((runs / variant / case / "result.json").read_text())
            for block in result["blocks"]:
                if block["label"] != "table":
                    continue
                key = case, block["id"]
                stem = block["id"].replace(":", "-")
                capture = json.loads((runs / variant / case / (stem + ".json")).read_text())
                actual_pixels = (runs / variant / case / (stem + ".png")).read_bytes()
                baseline_pixels = (runs / "baseline" / case / (stem + ".png")).read_bytes()
                assert hashlib.sha256(actual_pixels).digest() == hashlib.sha256(baseline_pixels).digest()
                check = semantic_checks(block.get("table"), gold[key]) if key in gold else None
                if check:
                    score["passed"] += check["passed"]
                    score["total"] += check["total"]
                    score["tables_passed"] += check["all_passed"]
                data.setdefault(key, {"file": page["file"], "page": page["page"], "pixels": baseline_pixels, "variants": {}})["variants"][variant] = {
                    "table": block.get("table"), "check": check, "prediction": capture["prediction"],
                    "warnings": [w for w in result["warnings"] if block["id"] in w["message"]],
                }
        metrics.append({"variant": variant, "label": labels[variant], "engine": report["engine"], "pages": len(pages),
            "tables": sum(p["tables"] for p in pages), "structured": sum(p["structured"] for p in pages),
            "semantic": score, "initialization_ms": report["initialization_ms"], "warmup_ms": report["warmup_ms"],
            "process": process, "stage_ms": {stage: sum(t["duration_ms"] for t in timings if t["stage"] == stage) for stage in sorted({t["stage"] for t in timings})}})
    parity = json.loads((runs / "parity.json").read_text())
    manifest = {"hardware": "Apple M4, 16 GiB RAM", "backend": "ONNX Runtime CPU, one intra-op thread per model", "method": "One instrumented release-process run per variant; two real TSR warmup rounds; OS file cache not cleared; wall time includes loading, warmup and diagnostic PNG/JSON writes; RSS samples cover the process tree every 200 ms.", "semantic_method": annotations["method"], "variants": metrics, "parity": parity}
    (output / "metrics.json").write_text(json.dumps(manifest, indent=2) + "\n")
    header = "| Variant | Structured | Reviewed checks | Reviewed tables fully passing | Structure inference | Cell inference | Process wall | Peak RSS |"
    rows = [header, "|---|---:|---:|---:|---:|---:|---:|---:|"]
    for m in metrics:
        rows.append(f'| {m["label"]} | {m["structured"]}/{m["tables"]} | {m["semantic"]["passed"]}/{m["semantic"]["total"]} | {m["semantic"]["tables_passed"]}/7 | {m["stage_ms"].get("tsr_inference", 0)/1000:.2f} s | {m["stage_ms"].get("table_cell_inference", 0)/1000:.2f} s | {m["process"]["wall_seconds"]:.2f} s | {m["process"]["peak_process_tree_rss_bytes"]/1048576:.0f} MiB |')
    summary = """# TSR model comparison - 2026-09-14

The wireless RT-DETR detector improves a real gate-ablation header assignment while preserving all seven reviewed tables. Replacing SLANet+ with SLANeXt is not an overall improvement in the current integrated pipeline. Keep model selection explicit; do not treat successful structure validation as semantic accuracy.

## Scope and measurements

19 pages from six native-text PDFs under `/Volumes/Yage/Downloads/docs` produced 29 table crops. All five variants received identical crop pixels (SHA-256 checked). Seven tables have 49 manually reviewed dimension, text-placement and span assertions. The other 22 tables are included in pipeline coverage and visual inspection output but are not assigned a semantic accuracy score.

Apple M4, 16 GiB RAM; native release build, ONNX Runtime CPU, one intra-op thread per model. Every process loaded its selected models and executed two real model warmup rounds. The OS file cache was not cleared. These are single instrumented runs, not statistical throughput benchmarks. Wall time includes model loading, warmup and diagnostic PNG/JSON writes. Peak RSS is an approximate process-tree sample every 200 ms. Model inference totals exclude the surrounding PDF/render/capture work.

""" + "\n".join(rows) + """

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
"""
    (output / "report.md").write_text(summary)
    summary_rows = "".join(f'<tr><td>{html.escape(m["label"])}</td><td>{m["structured"]}/29</td><td>{m["semantic"]["passed"]}/49</td><td>{m["semantic"]["tables_passed"]}/7</td><td>{m["stage_ms"].get("tsr_inference",0)/1000:.2f}s</td><td>{m["stage_ms"].get("table_cell_inference",0)/1000:.2f}s</td></tr>' for m in metrics)
    sections = []
    for (case, table_id), entry in sorted(data.items(), key=lambda item: (list(gold).index(item[0]) if item[0] in gold else len(gold), item[0])):
        annotation = gold.get((case, table_id))
        title = annotation["title"] if annotation else "Unscored table"
        panels = []
        for variant in variants:
            result = entry["variants"][variant]
            table = result["table"]
            status = f'{table["row_count"]} rows × {table["column_count"]} columns' if table else "Unavailable"
            if result["check"]:
                status += f' · reviewed checks {result["check"]["passed"]}/{result["check"]["total"]}'
            warning = "<br>".join(html.escape(w["message"]) for w in result["warnings"])
            panels.append(f'<article data-variant="{variant}"><h3>{html.escape(labels[variant])}</h3><p>{status}</p>{table_html(table)}<p class="failure">{warning}</p><details><summary>Raw model output</summary><pre>{html.escape(json.dumps(result["prediction"], ensure_ascii=False, indent=2))}</pre></details></article>')
        image = base64.b64encode(entry["pixels"]).decode()
        sections.append(f'<section><h2>{html.escape(title)}</h2><p>{html.escape(entry["file"])} · page {entry["page"]} · {table_id}</p><details open><summary>Original crop</summary><img alt="Original table crop" src="data:image/png;base64,{image}"></details><div class="outputs">{"".join(panels)}</div></section>')
    document = '''<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>DocParse TSR comparison</title>
<style>body{font:15px/1.5 system-ui;margin:0;background:#f3f5f7;color:#182433}main{max-width:1500px;margin:auto;padding:28px}h1{font-size:32px}h2{font-size:21px}h3{font-size:17px}section,header{background:white;border:1px solid #dbe1e7;border-radius:10px;padding:22px;margin:22px 0}img{max-width:100%;max-height:1100px;object-fit:contain;display:block;margin:12px auto}table{border-collapse:collapse;width:100%;font-size:13px}th,td{border:1px solid #cdd5df;padding:5px 8px;vertical-align:top}th{background:#edf2f8}.outputs{display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:18px}article{min-width:0}.table-scroll{max-height:650px;overflow:auto}.failure{color:#a32828}pre{white-space:pre-wrap;overflow-wrap:anywhere;max-height:350px;overflow:auto;font-size:12px}.controls{position:sticky;top:0;background:#182433;color:white;padding:14px;z-index:2;border-radius:8px}select{font:inherit;padding:5px;margin-left:8px}@media(max-width:800px){.outputs{grid-template-columns:1fr}main{padding:12px}}</style>
<main><header><h1>DocParse TSR model comparison</h1><p>29 identical real table crops · 19 pages · 6 PDFs · Apple M4 CPU</p><p><strong>Independent wireless cells improve the reviewed header mapping. SLANeXt is not ready to replace the baseline in this pipeline.</strong></p><p>The 49 reviewed checks cover seven tables, not dataset-wide accuracy. Failures and unscored tables are retained below. Two real model warmup rounds; one instrumented run per variant; OS file cache not cleared.</p><table><thead><tr><th>Variant</th><th>Structured</th><th>Reviewed checks</th><th>Reviewed tables</th><th>Structure inference</th><th>Cell inference</th></tr></thead><tbody>''' + summary_rows + '''</tbody></table></header><div class="controls">Compare baseline with <select id="variant"><option value="cells-wireless">SLANet+ + wireless cells</option><option value="cells-wired">SLANet+ + wired cells</option><option value="upgraded-wireless">SLANeXt wireless + cells</option><option value="upgraded-wired">SLANeXt wired + cells</option></select></div>''' + "".join(sections) + '''</main><script>
// Show the same baseline beside the selected measured variant for every source crop.
function selectVariant(){const selected=document.getElementById('variant').value;for(const panel of document.querySelectorAll('[data-variant]'))panel.hidden=panel.dataset.variant!=='baseline'&&panel.dataset.variant!==selected;}
document.getElementById('variant').addEventListener('change',selectVariant);selectVariant();</script></html>'''
    (output / "comparison.html").write_text(document)
    print(json.dumps({"tables": len(data), "report": str(output / "comparison.html"), "bytes": len(document.encode())}))


if __name__ == "__main__":
    main()
