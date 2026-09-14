# PDFium 进程池实施计划

> 执行方式：使用 superpowers:executing-plans 在当前会话逐项实现；使用真实边界测试验证。

**目标：** 新增独立 pdfium-worker，由 server 通过 tokio::process + ipc-channel 管理最大进程数。

**架构：** core 默认使用本地 PDFium provider；server 注入共享进程池。worker 复用 core 页面操作，模型只在 server 初始化。

**技术栈：** Rust、Tokio、ipc-channel、Serde、typed-builder。

**Spec：** [PDFium 进程池设计](../specs/2026-09-14-pdfium-process-pool-design.md)

## 全局约束

- 不创建 worktree 或 commit；代码注释英文，计划和 spec 中文。
- server.pdfium_max_workers 为实际未回收子进程数硬上限，默认 1。
- 普通库、CLI 和 Web 不依赖 worker 二进制；server 不静默回退。
- 依赖只在 workspace 根声明版本，其他 crate 继承。
- 测试仅放在 crate tests 或精确的 cfg(test) mod tests 中。

## 任务 1：配置与 PDFium 会话边界

文件：crates/config/src/config.rs、validate.rs、tests/validation.rs；core 的 parser.rs、pdfium/executor.rs、runtime/pipeline.rs；layout 计时和类型声明。

- [x] 添加配置加载/验证用例，运行并确认新配置尚不能加载。
- [x] 加入 server.pdfium_max_workers 默认值和正数校验。
- [x] 定义 PdfiumProvider::open 和 PdfiumSession，复用既有 PdfInput/RenderedPage/PreScannedPage；为本地 executor 实现接口。
- [x] DocParser/ParseRuntime 共享 provider，扫描和渲染通过会话执行；本地默认保持原行为。
- [x] 运行配置与现有 PDFium 生命周期/提取回归。

## 任务 2：IPC 与独立 worker

文件：core 的 pdfium/{mod.rs,executor.rs,provider.rs,ipc.rs}；core/Cargo.toml、src/bin/pdfium_worker.rs。

- [x] 添加共享内存图像传输的几何/长度验证和错误往返测试。
- [x] 引入 ipc-channel 的 native feature；共享握手、带租约/请求编号的协议。
- [x] worker 单线程持有 Library 和 Document，复用现有提取/渲染；实现关闭确认和 resolver 反向请求。
- [x] 新 binary 仅启动 worker 服务循环，stdout 引导、stderr 日志。
- [x] 构建 worker，运行协议和真实 PDF 读写测试。

## 任务 3：server 有界进程池

文件：crates/server/src/pdfium_pool/mod.rs、process.rs；server tests/pdfium_pool.rs。

- [x] 先写真实进程测试：N=1 时第二个会话等待；N=2 时可同时打开两份文档；关闭后 PID 消失。
- [x] 固定 N 个槽位，每槽唯一 Child 所有者；启动失败清理；通过有界请求通道和阻塞桥接线程交换 IPC。
- [x] 租约 Drop 触发取消，Close 确认才归还；异常先 kill/wait/join 再补建；循环故障关闭池。
- [x] 增加取消、崩溃、排队者唤醒和协议失配用例，验证实际 PID 上限及输出一致性。

## 任务 4：集成与发布

文件：server main.rs/lib.rs；根 default-members、docparse.toml、发布说明；server benchmark example 和 scripts/benchmark.py。

- [x] all/worker 角色在领取任务前构建池并注入 parser；api-only 不启动池。
- [x] 正常排空和异常退出都显式关闭池；池不可用触发 server 停止领取任务。
- [x] 配套构建/安装 server 和 worker，固定同目录定位；更新配置及启动说明。
- [x] server benchmark 复用生产进程池与配置，预热每个 worker 后测量。

## 任务 5：验证与收尾

- [x] cargo fmt --all -- --check。
- [x] 配置、core 本地回归、IPC、server 真实进程测试通过。
- [x] cargo check/clippy 覆盖 server、worker 与 wasm32 Web 目标。
- [x] 真实 PDF 单进程/多进程输出一致，N=1/2/4 的资源上限和清理有证据。
- [x] 确认当前机器为 macOS，用户已明确本轮只交付本地验证；已执行 CPU 冒烟矩阵，不把它描述成 CUDA 验收。
- [x] 审阅最终 diff，记录实际完成项和无法在当前硬件执行的验证。

## 后续目录归并

- [x] 将 executor/provider/IPC 归入 core/src/pdfium，以 mod.rs 统一声明和导出。
- [x] worker 入口迁至 core/src/bin/pdfium_worker.rs，使用已有 pdfium-ipc feature；删除独立 worker crate。
- [x] 更新模块引用、Cargo workspace/lockfile、构建安装命令及平台边界声明。
- [x] 重新验证 worker 构建、原有真实进程测试、默认库和 WASM 编译、格式与 lint。

## PDFium 子系统归并

- [x] 将 pdfium 模块移到 core/src/pdfium，input.rs 与 worker.rs 一同归入该目录。
- [x] 更新内部引用与显式外部 ::pdfium 路径，保留已有公开类型及兼容性重导出。
- [x] 收紧平台条件编译白名单到新路径，并更新其正/负边界测试。
- [x] 验证 worker 构建、Core/Server 回归、默认 core 和 Native/WASM lint。
