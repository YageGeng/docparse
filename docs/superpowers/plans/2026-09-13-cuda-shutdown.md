# CUDA 停机段错误修复

- 根因已确认，与此前 GDB 证据一致：SessionWorker 丢弃原生线程 JoinHandle，进程退出时 CUDA session 仍在析构，与 CUDA 库全局析构并发。修复前真实 release CUDA server 空闲 SIGINT 复现退出码 -11。
- 在共享兼容层为模型创建独立原生线程。SessionOwner 持有队列和 JoinHandle；最后一个持有者先关闭队列，再等待线程析构和 TLS 清理完成。保持有界队列和线程亲和性。
- Tokio blocking task 只等待有限的初始化和单次推理，不覆盖空闲 session 生命周期。等待任务持有 SessionOwner，即使异步调用方取消也要等原生工作释放捕获变量后才释放 owner，避免 layout lease 最后一个引用在模型线程中触发 self-join。
- 补充确定性回归：runtime 等待 session 析构，以及等待已取消的初始化。保留在途取消、排队取消、错误传播和 scoped tracing 测试。
- 用独立临时 PostgreSQL、真实模型与 release CUDA 复现并验证：多次 SIGINT/SIGTERM、启用 OCR、在途 PDF 解析、初始化失败；同时验证默认构建和 WASM 编译。
- 空闲模型不占用 blocking worker；允许模型离开构造 runtime 后继续被同步或其他 runtime 调用。最后一个持有者同步等待回收；在途任务的回收发生在有限 blocking task 中。

## 验证结果

- Review 修复回归：单 blocking worker 下，空闲模型不得阻塞 CPU 工作或后续模型初始化；模型离开构造 runtime 后，旧 runtime 必须能关闭，模型可继续推理。这两项测试在长期 blocking task 实现中失败，在独立线程实现中通过。
- 取消回归增加模型请求自身捕获 worker 引用的情形，覆盖 layout lease 的 self-join 风险。

- 生命周期回归已确认可以捕获对应旧实现的问题；最终原生 workspace 共 383 项通过、22 项忽略，严格 Clippy 通过。
- release CUDA 进程矩阵共 12 个场景：5 次空闲 SIGINT、3 次空闲 SIGTERM、启用 OCR 的空闲 SIGINT、普通及 OCR 在途解析 SIGINT，全部退出 0；两个在途场景均完成并保存 3 页无错误结果。缺失 TSR 文件的初始化失败场景正常退出 1。
- WASM release 构建通过，浏览器执行层未改动。
- 使用真实模型补验公开同步 API：在 max_blocking_threads(1) 的临时 runtime 中构建 DocParser，关闭该 runtime，再调用 parse_path_blocking 完成一页真实 PDF，正常释放模型，耗时 3.28 秒。
- 最终真实 CUDA 进程日志与结果：`target/cuda-shutdown-check/fixed-1789310213490212088/`。本轮使用独立的临时 PostgreSQL 容器，验证后已移除；不修改用户正在编辑的 docparse.toml。
