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

Detection, recognition, and orientation each own a shared queue with independently
configured `session_size` (positive integer, default 1), `batch_size` (1–32), and required
`queue_size` (pending inputs) under
`ocr.detection`, `ocr.recognition`, and `ocr.orientation`. Detection defaults to
batch 1; recognition and orientation default to 16. The former top-level
`ocr.batch_size` is rejected.

Callers submit individual pages or text lines. Each idle consumer drains only
ready inputs, runs short batches immediately, and groups equal tensor dimensions
without changing a line's original padding. Results return in detection order.
The retired `ocr.max_in_flight` setting is rejected. Pages enter the model
queues without an extra page semaphore on either native or browser builds.
Line crop preparation retains a window derived from active batch capacity plus
`queue_size`; queued model work is consumed by `session_size` owners. Native
sessions outlive their construction runtime.
Browser sessions share the same queue and retain the global ORT execution guard.

Recognition probabilities use an eight-lane CPU validation/max reduction before
CTC decoding. This retains exact first-index ties and rejects non-finite or
out-of-range values, without copying the full vocabulary tensor or adding threads.

The shared native CUDA builder uses heuristic cuDNN convolution selection to
avoid exhaustive searches for changing input shapes. CPU and browser providers
retain their existing execution settings.
