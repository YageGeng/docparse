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

Native OCR groups equal-width text lines for recognition, preserving each line's
single-input padding and restoring detection order after inference. Orientation
uses fixed-size inputs and fills its batches across recognition-width boundaries. Set
`ocr.batch_size` (1–32, default 16) to bound both recognition and orientation
calls independently of `ocr.max_in_flight`, which controls overlapping pages.
Batch 1 retains the previous line scheduling for comparisons. Only the current
batch's crops are allocated; browser inference remains capped at one line.
Recognition probabilities use an eight-lane CPU validation/max reduction before
CTC decoding. This retains exact first-index ties and rejects non-finite or
out-of-range values, without copying the full vocabulary tensor or adding threads.

The shared native CUDA builder uses heuristic cuDNN convolution selection to
avoid exhaustive searches for changing input shapes. CPU and browser providers
retain their existing execution settings.
