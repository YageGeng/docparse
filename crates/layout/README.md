# docparse-layout

中立的 `LayoutEngine`/geometry API 与固定 PP-DocLayoutV3 ONNX Runtime 实现。支持 CPU，以及互斥 feature `cuda`、`coreml`、`openvino`。显式请求的 accelerator 无法注册时返回错误，不静默回退 CPU。

模型不随 crate 分发。固定 artifact、Python oracle 和多尺寸 tensor/detection parity 流程见仓库根目录 `README.md`。

Native model sessions own dedicated threads for creation, inference, destruction,
and thread-local cleanup. Idle sessions use no Tokio blocking capacity and can
outlive their construction runtime. Only finite initialization and inference waits
use the blocking pool; cancelled callers keep their native work owned until it
finishes. The final owner closes the queue and joins the thread synchronously.
