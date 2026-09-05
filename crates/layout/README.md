# docparse-layout

中立的 `LayoutEngine`/geometry API 与固定 PP-DocLayoutV3 ONNX Runtime 实现。支持 CPU，以及互斥 feature `cuda`、`coreml`、`openvino`。显式请求的 accelerator 无法注册时返回错误，不静默回退 CPU。

模型不随 crate 分发。固定 artifact、Python oracle 和多尺寸 tensor/detection parity 流程见仓库根目录 `README.md`。
