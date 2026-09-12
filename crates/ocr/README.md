# docparse-ocr

Independent PaddleOCR ONNX detection, perspective crop, text-line orientation,
recognition and CTC decoding. The crate uses the shared DocParse ONNX execution
provider boundary and has no dependency on OAR or docparse-core.

Download the pinned official artifacts with `scripts/download_models.py` using
`pp-ocrv6-medium-det`, `pp-ocrv6-medium-rec` and `pp-lcnet-textline-ori`.
`PaddleOcrEngine::from_config` loads native directories;
`PaddleOcrEngine::from_artifacts` accepts immutable bytes for native and Web.

Features `cuda`, `coreml`, `metal`, `openvino` and `wasm` select available runtime
backends. Browser sessions use the configured WebGPU or CPU WASM backend. Model
outputs are validated before decoding, and cancelled callers cannot release
buffers still used by ONNX Runtime.
