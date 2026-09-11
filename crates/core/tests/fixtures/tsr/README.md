# Captured TSR regressions

`survey-model-captures.json.gz` contains all 24 table inputs from a real native SLANet_plus ONNX run on `2303.18223v16.pdf`. It retains model tokens, position-head boxes, crop transforms, measured native words, and source separators. Repeated page diagnostics and PDF character provenance are omitted; text, geometry, baselines, and font facts remain unchanged.

The default unit test replays postprocessing without model files or the original PDF. Expected dimensions and selected cell assertions come from inspecting the PDF, not from accepting a successful model return. The illustrated prompt panels on page 48 have no fixed dimension assertion; source completeness and model provenance are still required.

Regenerate captures with the ignored `real_pdfs_use_configured_table_model`, `capture_tsr_source_facts`, and `refresh_tsr_captured_predictions` tests. These captures are regression inputs, not a replacement for the production browser acceptance test.

`terminal-universe-configurations.json.gz` captures the real SLANet_plus response
and immutable PDFium words/rules for page 32, Table 20 of Terminal-Universe.
The regression requires a 16-by-7 table, a seven-column section heading and four
values spanning the six model columns. Removing the enclosing rules or inserting
a real column divider must reject the inferred spans. The companion statistics
header fixture lives in `../table/terminal-universe-page-32-statistics.json`.

`coarse-numeric-columns.json.gz` combines the real Terminal-Universe statistics
source with a controlled four-column prediction. It is not a model accuracy
measurement: the regression checks that complete word coverage cannot suppress
recovery of the separate Turns and Tool calls numeric columns.
