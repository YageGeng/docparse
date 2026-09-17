# Attribution

The Pillow-compatible preprocessing in `src/preprocess.rs` and `src/resample.rs`
is adapted from [studio-ransom/best-ocr-rust](https://github.com/studio-ransom/best-ocr-rust),
revision `5e00bb7ac26c3f8ce9b09f857725c1d17f61e7ae`, under AGPL-3.0-only.
The three PNG regression fixtures originate from that repository's `tests/images/`.
Its README attributes those images to [alephpi/Texo](https://github.com/alephpi/Texo).

The model is the author's ONNX export from
[alephpi/FormulaNet](https://huggingface.co/alephpi/FormulaNet), revision
`63e04c86fc96c2324811114351eeea8118bf6b28`, under AGPL-3.0.
This is the 687-token vocabulary-transfer checkpoint. It is not the 1264-token
GGUF checkpoint used by best-ocr-rust. Weights are downloaded explicitly and are
not embedded in this crate or checked into this repository.

`tests/fixtures/reference.json` was generated independently with Python ONNX
Runtime and Pillow by `tests/reference.py`; its runtime version and model
revision are recorded alongside the outputs.
