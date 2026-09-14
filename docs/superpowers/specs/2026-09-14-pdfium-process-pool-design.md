# PDFium worker 二进制与 server 进程池

日期：2026-09-14。状态：已实现；本轮按用户要求交付本地验证，CUDA 性能矩阵另行执行。

## 1. 目标与部署边界

core 在 `crates/core/src/bin/pdfium_worker.rs` 提供 `docparse-pdfium-worker` 二进制；`crates/pdfium` 继续作为 PDFium 库。server 使用 `tokio::process` 启动并管理 worker，通过 `ipc-channel` 调用 PDFium。配置限定 server 内共享池的最大进程数。

worker 与 server 一同发布，由 server 自动管理生命周期；不要求库调用方实现 worker 入口，也不重新执行宿主程序。普通 `docparse-core` 库调用及 Web 路径保留进程内实现。进程模式由 server 显式注入，不成为所有库调用者的隐式部署依赖。

Layout、OCR、TSR 引擎仍在 server 进程加载和共享。worker 只负责 PDFium 文档打开、原生文本/几何事实提取、页面渲染和关闭，不建立数据库连接、启动 HTTP 服务或初始化推理模型。

## 2. 当前代码与依赖边界

当前事实：

- `crates/pdfium/src/library.rs` 在 `Library` 生命周期中持有全局锁；PDFium 已使用 `Once` 初始化一次。
- `crates/pdfium-sys/src/dynamic.rs` 已使用 `OnceLock` 缓存动态库绑定，重复加载并不是本次瓶颈。
- `crates/core/src/pdfium/executor.rs` 当前为每份文档建立专属线程。文档和 `Library` 一直存活到命令循环退出。
- `crates/core/src/runtime/pipeline.rs` 要等最后一页送入有界渲染队列才关闭 executor；下游背压可能延长持锁。
- 页面提取和渲染操作位于 core，字符恢复还依赖 `GlyphResolver`、文本规则和表格几何逻辑。
- `crates/server/src/worker.rs` 已让不同文档任务共享 `Arc<DocParser>`。
- core 已依赖 `docparse-pdfium`，因此不能再让同一个 `docparse-pdfium` Cargo 包反向依赖 core。

PDFium 的输入、平台 worker、executor、provider 和 IPC 实现集中在 core 顶层 pdfium 模块内。runtime 仅消费 provider/session 接口编排流水线，worker 入口使用同一 Cargo 包的 library：

```text
crates/core/src/pdfium/
  mod.rs                      module declarations and exports
  input.rs                    native/browser PDF sources
  worker.rs                   local actor startup and join per platform
  executor.rs                 local PDFium document execution
  provider.rs                 provider/session contracts
  ipc.rs                      native IPC protocol and worker loop
crates/core/src/bin/pdfium_worker.rs

server -> core library -> pdfium library -> pdfium-sys
core worker binary -> core library
```

core 注册 `docparse-pdfium-worker` bin target，要求启用已有的 `pdfium-ipc` feature；其 native 日志依赖也随该 feature 启用。没有独立 worker Cargo 包或新增 workspace 成员。普通 core 库和 Web 构建不会默认构建 worker。

`pdfium/mod.rs` 仅承担模块声明与导出；`input.rs` 和 `worker.rs` 管理 native/browser 差异。平台边界检查只允许这三个明确的文件出现条件编译，不放宽 executor、provider 或整个目录。已有 `docparse_core::pdfium_ipc` 公开入口保留为重导出，server 的 IPC 调用方式保持不变。

## 3. 配置、发现与作用域

进程池归 server 所有，因此配置放在 server 段：

```toml
[server]
worker_concurrency = 2
pdfium_max_workers = 2

[runtime]
page_concurrency = 4
render_queue_capacity = 2
blocking_task_limit = 4
```

`server.pdfium_max_workers` 默认 1，要求整数且至少为 1。`server.worker_concurrency` 限制同时处理的完整文档数，默认 2，取值范围为 1–128；环境变量 `DOCPARSE_SERVER__WORKER_CONCURRENCY` 可覆盖文件值，显式传入 `--worker-concurrency` 时再覆盖配置，省略该参数则保留配置值。配置在 server 启动时生效，首版不支持热扩缩容。示例中的 PDFium worker 数 2 是起测值，不是已验证的最优值。非法配置在建立数据库连接或启动任何 PDFium 子进程之前返回错误。

server 使用自身可执行文件所在目录中的固定文件名 `docparse-pdfium-worker`；平台需要时附加可执行文件后缀。`current_exe()` 仅用于定位安装目录，不重新执行 server。没有 worker 路径配置，不搜索 PATH，不运行时下载或解压 worker。

发布产物必须在同一目录包含匹配版本的 server 和 PDFium worker，沿用现有 PDFium 动态库分发方式。文件缺失、不可执行、协议或包版本不匹配时返回明确启动错误，不回退到进程内解析。

- `role=all` 和 `role=worker` 建立一份共享池。
- `role=api` 不建立池，也不要求 PDFium worker 已安装。
- 一个 server 进程中，所有任务和解析器克隆共享同一池。
- 不同 server 进程各自拥有上限，不把该字段解释为整台机器的配额。
- `server.worker_concurrency` 限制完整文档任务数，`--worker-concurrency` 提供显式覆盖；`page_concurrency` 限制每份文档各分析阶段的在途页数。二者不能扩大 PDFium 池。
- 普通 core、CLI 和 Web 不消费这个 server 配置，也不因没有二进制而失去原有解析能力。

## 4. server 与 core 的连接方式

server 持有 `Arc<PdfiumPool>`，负责启动、监督和关闭。构建 `DocParser` 时通过 PDFium provider 注入该池的客户端；core 的解析流水线仍负责预扫描、布局、OCR、TSR、融合及结果生成。

新增的最小接口包含两层职责：

| 接口 | 责任 |
| --- | --- |
| `PdfiumProvider::open` | 为输入建立一个文档会话；本地实现打开线程 executor，server 实现等待并取得池租约 |
| `PdfiumSession` | 提供固定页数、逐页预扫描、渲染和关闭；所有结果为拥有所有权的数据 |

复用现有 `PdfInput`、`PreScannedPage`、`RenderedPage` 和 PDFium 错误语义，按需要调整可见性；不新增内容等价的另一套业务类型。异步 trait 沿用现有 `WasmBoxedFuture` / 平台兼容约束，提供本地和 IPC 两个实际实现。

`DocParserBuilder` 默认使用现有本地 provider；server 必须显式注入 IPC provider。克隆解析器只克隆共享句柄，不创建新池。server 不得在未成功注入池时静默采用默认 provider。

core 在 native 专用的 `pdfium-ipc` feature 下暴露内部通信类型和 worker 入口，供 server 与 binary crate 共用；正常 core/Web 构建不启用该 feature。core 不依赖 server。

关闭方法属于 server 的 `PdfiumPool::shutdown().await`，无需给所有 `DocParser` 调用者增加新的进程关闭要求。

## 5. 进程上限与生命周期

首版采用固定 N 个槽位，启动时建立 N 个常驻进程。每个槽位最多拥有一个 `tokio::process::Child`，由唯一监督任务持有。所有进程握手成功后才开始领取解析任务；中途失败则关闭并回收已启动进程。

槽位状态为 `Starting -> Idle -> Leased -> Idle`；异常或关闭进入 `Stopping -> Reaped`。必须始终满足：

```text
starting + idle + leased + stopping_not_reaped <= pdfium_max_workers
```

1. 在 spawn 之前保留槽位；没有空闲槽位的文档异步等待，不额外启动进程。
2. 一份文档租用一个进程，打开、预扫描、渲染和关闭都发送到这个进程，PDFium 句柄不跨进程。
3. 最后一页交付后发出 Close；worker 释放文档、输入映射和相关资源并确认，才能归还租约。剩余 OCR/后处理不继续占用 PDFium 租约。
4. 取消等待者不影响正在执行的文档。取消已租用文档时，解析器持有的渲染生产任务随 Future 一起取消，由监督任务清理会话；先尝试有时限的优雅退出，无法确认资源状态时终止并回收 worker，不能直接归还。
5. 崩溃或强制终止后先 `wait()` 确认退出并回收，再在原槽位补建，不能短暂超过 N。补建还必须等待旧桥接线程退出，避免累积线程。
6. 失败文档返回明确错误；池不自动重放请求，文档重试沿用 server 的租约和重试策略。格式错误或零页 PDF 的 Open 拒绝属于已知干净状态，直接归还健康 worker；主动取消可回收并替换 worker，但既不消耗也不恢复已有崩溃重试额度。
7. 每次故障最多尝试一次补建；替代进程完成一份文档后恢复补建额度。启动失败或替代进程在完成首份文档之前再次退出时，池进入不可用状态并通知 server 停止领取任务、取消在途任务及关闭整个池。

正常关闭时 server 先停止领取任务并排空现有任务，再关闭池。服务启动失败、运行故障或强制停止时，取消在途任务后关闭池。

worker 使用独立进程组，避免终端 Ctrl-C 同时直接中断 PDFium。server 接收退出信号后负责排空任务并通过 IPC 关闭 worker。

`shutdown()` 幂等，并发调用等待同一次清理。关闭后拒绝所有新租约，唤醒等待者，发送 Shutdown，等待全部子进程及桥接线程结束。正常退出等待默认 5 秒，超时则终止子进程并显式 wait。`kill_on_drop(true)` 仅作兜底，不代替回收。

文档取消也采用同一套优雅停机流程。Shutdown 排在正在执行的请求之后，worker 在文档打开时也接受进程级 Shutdown，先释放文档和输入映射再确认，并正常退出。等待确认和确认后等待进程退出各限 5 秒；任一阶段超时才强制终止。已崩溃或传输已损坏的 worker 直接回收。协议版本升为 2，server 与 worker 必须一起更新。

启动握手默认超时 30 秒，Close 默认超时 5 秒；首版使用内部常量。正常提取和渲染继续受 server 文档任务总体超时控制，不对合法大页任意设置很短的操作超时。

## 6. IPC 协议与数据

### 6.1 启动握手

worker 创建 `IpcOneShotServer`，通过 stdout 写出一行有限长度的引导信息，包含协议版本、crate 包版本及连接名称。server 使用 Tokio 限制读取长度为 4 KiB，并受启动超时控制。

server 检查版本后，通过阻塞适配建立命令和响应 channel。可能长期等待的 accept 在 worker 中执行，不在 server 留下取消后无法结束的 accept 任务。完成 channel 交换后，server 还必须收到 PDFium 初始化成功的 Ready，才能开放槽位。

stdout 只用于引导；日志使用 stderr。引导连接名称不进入普通日志。父进程为 worker 创建并持有私有临时目录，通过子进程环境指定其临时文件位置，回收后清理目录；因此握手未完成就终止也不会留下 Unix rendezvous 文件。父进程不得持有会阻止对端关闭检测的多余 channel 副本。

### 6.2 消息语义

普通命令为 Open、PreScan、Render、Close、Shutdown。响应为对应结果、结构化错误或字形恢复请求。租约标识、请求编号和进程代次用于确认响应归属，旧进程或上一租约的响应不能完成新请求。

- 路径输入使用绝对路径；内存输入使用 `IpcSharedMemory`，worker 保留映射直到文档关闭。
- 提取结果通过 Serde 传递纯数据，包括文本、几何、字体、表格证据及原有警告。不能传递 PDFium 指针、借用、trait 对象或 Arc 地址。
- 页面像素第一版使用 `IpcSharedMemory::from_bytes`，发送后保持只读。进入现有 `PageImage` 表示时允许必要的拷贝，不承诺端到端零拷贝。
- 接收端验证租约、页号、尺寸、格式、整数溢出及像素长度，使用现有 `TryFrom` 构造 `PageImage` / `PageTransform` 等类型。
- wire error 保留操作、页号、错误类别和可诊断消息。页面操作错误遵守既有 `continue_on_page_error`；进程失联和协议错误必须作为进程边界故障处理，不能伪装成合法空白页。

### 6.3 异步适配和背压

每个 worker 对应一个受控的阻塞桥接线程；server/core 的异步调用通过有界请求通道和 oneshot 取得响应。IPC 阻塞发送、接收都不直接占用 Tokio 执行线程。

桥接请求通道容量为 1，每个文档会话最多一个普通 PDFium 命令等待响应。worker 按请求生产页面，现有 `render_queue_capacity` 继续限制预取；不能把 IPC channel 当成业务内存上限。

同一会话使用异步 admission 锁串行提交普通命令。取消尚未取得 admission 的请求不影响当前命令；取消已提交命令的 Future，即使会话对象仍保留，也必须通知监督任务回收不确定状态的 worker。取消和关闭通过监督任务终止必要的子进程、关闭端点并结束桥接线程。不能仅丢弃等待 Future 后就把进程或线程算作已经回收。

### 6.4 GlyphResolver

复用 core 的字符恢复逻辑。worker 需要自定义 resolver 时，发送拥有所有权的字形轮廓段；server 的桥接线程调用当前租约携带的 resolver，并回传结果。原有字体/字形缓存继续生效，不逐字符重复无意义的 IPC。

字形往返属于当前 PreScan 命令内部的交互，协议循环必须能处理它，避免双方都等待普通命令结束。调用用户 resolver 时不持有池管理锁。

unwind panic 或通道错误必须传播并触发会话清理；abort panic 策略无法保证 server 继续运行。已有同步 resolver 没有取消协议，若它永不返回，不能安全强制结束对应 Rust 线程。此时终止 worker、将池置为不可用、停止补建桥接线程，并报告清理未完成，不能宣称线程已回收或静默关闭字符恢复功能。

## 7. 构建、发布与库兼容性

- `docparse-pdfium-worker` 是 core 包内的 feature-gated bin target，入口位于 `crates/core/src/bin/pdfium_worker.rs`，不依赖 server。`crates/pdfium` 不增加可执行入口或进程管理职责。
- 不增加 workspace 成员；删除旧 worker 包及其 `default-members` 条目。所有依赖的版本或路径在根声明，core 继承，遵守依赖分组规则。
- server 增加 Tokio process 所需 feature；server 与 worker 启用 core 的 native IPC feature。普通库和 Web 不引入子进程管理依赖。
- worker 只要求 pdfium-ipc feature，不调用解析器模型构建流程。同一次 workspace 构建可能统一 core 的其他 features；需要最小 worker 产物时，单独使用不带推理 provider feature 的 core bin 构建命令。链接依赖不等于初始化模型；worker 入口不创建推理模型 session。
- 构建及发布流程同时生成 server 和 core 的 worker bin，并放入同一安装目录。单独安装 server 不会安装依赖包的 binary，发布命令必须显式构建/安装 core 的 `docparse-pdfium-worker` target。
- 本地开发同样使用标准 target 目录中的配套产物，不用自定义 worker 路径参数掩盖发布问题。
- 保留 core 的默认本地解析行为和 Web Worker 生命周期。需要评估 server 进程池的 benchmark 放在 server example 层，复用同一池实现；不能在 core 生产依赖中反向引入 server。

## 8. 实施落点

| 位置 | 责任 |
| --- | --- |
| `crates/config` | `server.pdfium_max_workers`、`server.worker_concurrency` 默认值和验证 |
| `crates/core/src/parser.rs` | 注入 PDFium provider，默认保留本地实现 |
| `crates/core/src/pdfium/` | 集中管理本地执行、provider/session、IPC 和 worker 循环 |
| `crates/core/src/bin/pdfium_worker.rs` | core 包内的最小 worker main 入口 |
| `crates/server` | 唯一共享池、进程监督、IPC 客户端 provider、启动和关闭集成 |
| core Cargo、构建和发布说明 | 声明受 feature 控制的 bin，配套构建/安装两个产物 |
| timing 类型和 TypeScript 声明 | 新增池排队阶段及兼容说明 |
| server benchmark 和测试 | 真实进程上限、输出一致性、故障回收及性能验证 |

该设计不修改 HTTP 请求/响应结构、数据库 schema 或解析算法。新增函数和非平凡实现注释使用英文，遵守 typed-builder、Arc 克隆和日志规范；不创建 commit。

## 9. 日志与性能计时

日志覆盖池启动/关闭、worker Ready、租约获取/归还、取消、退出和补建失败。正文包含 slot、PID、租约、代次、耗时和错误原因；使用完全限定的 `tracing::info!("... {}", value)` 风格，不记录 PDF/像素/字形载荷、IPC 连接名称或凭据。

新增 `PdfiumQueue`，只计等待池租约的时间。`PdfOpen` 从获取租约之后开始，记录远端打开往返。`TextExtract`、`PdfRender` 包含 IPC 往返，报告明确其口径；启动、预热及共享内存拷贝单独观测。

当前结果不承诺吞吐改善幅度。验证固定文档并发足够大，再分别比较 N=1、2、4，不能同时改变文档并发和进程数而把影响混为一谈。各 worker、共享模型都完成预热，文件缓存状态明确。

## 10. 验收标准

1. 默认 N=1，0/非法类型在 spawn 前报错；配置覆盖顺序正确，API-only 不启动 worker。
2. 多个任务和解析器克隆共享 server 唯一池，在 N=1、2、4 的启动、繁忙、取消、退出和补建阶段，实际未回收进程数始终不超过 N。
3. N=2 时不同 PID 的两份文档提取/渲染能够真实重叠，不只是两个任务在等待同一个进程内锁。
4. 本地 provider 与 IPC provider 的输出在现有真实语料上保持一致，覆盖路径/内存输入、表格、旋转/CropBox/UserUnit、警告和自定义 GlyphResolver。
5. worker 缺失、不可执行、版本不匹配、握手超时、文档取消及进程崩溃均返回可诊断错误；不复用状态不明的会话，不无限补建，不泄漏进程或桥接线程。
6. Close 确认之后才归还租约；前一文档资源及旧响应不影响下一文档。停止服务后子进程和端点全部回收。
7. worker 不初始化 GPU 模型/数据库/HTTP；server 仍只共享一套既有模型配置和实例集合。
8. 普通 core 库调用、CLI 和 Web 不要求安装 worker、不要求宿主修改 main，也不要求提前初始化。WASM 目标编译及浏览器生命周期无退化。
9. 所有测试放在 crate-level tests 或精确的 `#[cfg(test)] mod tests` 内；WebUI 验收继续使用真实 app backend。
10. server 与 worker 配套构建、安装和版本检查通过；Cargo 依赖图无环，core/pdfium library 不反向依赖 server；worker 使用同包 core library。
11. CUDA 复测沿用原报告 12 份 PDF / 368 页及原模型配置，每档至少三次，记录整批墙钟、单文档延迟、阶段排队、IPC 字节和耗时、GPU 使用及宿主/全部 worker 峰值内存。进程计数必须与实际 PID/退出状态核对，不能仅检查 permit 数。

同步自定义 resolver 不返回、宿主被强制终止以及 OS 无法及时终止的进程属于明确的清理边界，不能以成功状态掩盖；正常回收验收要求回调能返回且操作系统完成终止。

## 11. 依据

- [原 CUDA 报告](../../reports/2026-09-14-cuda-performance.md)
- [PDFium 官方调用线程约束](https://pdfium.googlesource.com/pdfium/+/refs/heads/main/public/fpdfview.h)
- [维护者关于多进程的答复，2016 年](https://groups.google.com/g/pdfium/c/HeZSsM_KEUk/m/D3d2HeaBAgAJ)
- [Tokio process 的取消和回收行为](https://docs.rs/tokio/latest/tokio/process/)
- [ipc-channel 消息与连接建立](https://docs.rs/ipc-channel/latest/ipc_channel/)
- [IpcSharedMemory](https://docs.rs/ipc-channel/latest/ipc_channel/ipc/struct.IpcSharedMemory.html)

## 12. 本轮验证

详见 [本地验证报告](../../reports/2026-09-14-pdfium-process-pool-validation.md)。该报告明确区分已通过的本地检查与未执行的 CUDA 复测。
