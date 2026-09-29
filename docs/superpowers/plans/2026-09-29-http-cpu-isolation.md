# HTTP 与 CPU 任务隔离实施计划

**目标：** 修复本轮审查发现的 async 线程计算、共享 blocking 池等待、同步日志和文件清理问题，保持解析与 HTTP 输出协议。

**方案：** 复用 Tokio 建立进程级、按可用 CPU 数限流的独立 CPU 执行池。模型队列增加异步入队和回复等待；原生线程清理继续保留取消安全。将 IPC、页面与模型前后处理移入 CPU 边界，gzip 按次 poll 转移计算且保留流式背压。日志使用标准非阻塞写入组件，文件锁采用非阻塞重试。

**约束：** 当前目录内修改，不使用 worktree，不创建 commit；注释英文，本文中文；不修改数据库。操作系统层面的 CPU 抢占仍需部署资源隔离，不能承诺绝对响应时间。

## 实施与验收

- [x] common：独立有限 CPU 池、异步模型队列、异步回复、线程退出清理。先验证单线程 runtime / 单 blocking 线程下 CPU 与模型等待不拖住普通 blocking 工作；保留取消、页许可、初始化失败和线程亲和测试。
- [x] core / layout / ocr / tsr：IPC JSON 和图片转换、页面准备、公式准备裁剪投影、OCR 旋转、TSR 解码切图离开 async 线程；验证现有公式、表格、页面回归与响应性。
- [x] server：结果产物计算使用 CPU 池，压缩 body 每次 poll 在 CPU 池执行，保留编码协商、流式背压及取消；检查缓存与 gzip 字节一致性。
- [x] storage / logging：清理锁竞争异步退避，清理许可随实际执行持有；ready 临时文件析构及图像目录清理不阻塞 async；非阻塞日志在退出时 flush。
- [x] 汇总验证：cargo fmt、相关 crate 测试、server HTTP / tracing / IPC 测试与 clippy；检查最终 diff，无未经允许的 commit。

## 重点边界

1. 取消后后台任务仍持有 PageLease，不提前放行下一页。
2. 原生 session 不在自己的线程上 join，初始化失败也释放已有 session。
3. CPU 池满时 HTTP blocking 文件工作仍可执行。
4. 压缩流的 Pending、错误、trailer 和客户端取消不丢失唤醒、不缓冲整份结果。
5. 文件锁竞争、日志管道背压不阻塞 async 工作线程。

## 执行记录

- 初始工作区干净。上一轮 core 三个响应性探针完成，但未覆盖生产 IPC、公式和 gzip。
- 已授权直接实施审查修复；不再要求重复方案确认。无可用 subagent 工具，在本会话实施并自行复查。
- CPU 与模型等待隔离两条回归、取消删除回归均先观察失败，再修复并通过。
- 最终复查补充 session 环境 PageLease 取消回归，先观察提前释放，再随原生回复保留许可并通过。
- 原有 Texo 部分初始化失败测试发现异步清理返回过早；新增显式异步 shutdown，Texo 与 PP 的失败路径等待清理，回归已通过。
- `cargo test -p docparse-core --lib --tests --no-fail-fast -- --quiet`：352 项通过；需要真实模型、人工测量或外部语料的测试维持忽略。
- common、layout、OCR、TSR、formula、Texo、formula-http 的相关单元与集成测试通过；server 的 HTTP、压缩、缓存、日志、监控、静态资源及真实 PDFium 进程池测试通过。
- `cargo clippy --tests --examples -- -D warnings`、`cargo check -p docparse-web --target wasm32-unknown-unknown`、平台边界脚本、格式与 diff 检查通过。native 检查使用默认成员；`--workspace` 会包含明确拒绝 native target 的 docparse-web。
- 未执行依赖可丢弃 PostgreSQL 的忽略测试，也未进行配置模型的生产负载压测；测试通过不代表操作系统层面的 CPU 配额隔离。
- 日志队列每个输出最多 8192 行；队列满时丢弃新日志，避免把磁盘或 stdout 背压传回 HTTP。此取舍已记录到 server README。

## 二次审查修复

- [x] HTTP 压缩改用独立于解析 CPU 配额的单次 poll 限流；在压缩层内标记已编码响应，预压缩资源直接走 I/O。验收占满解析池后的 identity/gzip 健康检查、预压缩资源完整正文，以及 HTTP 压缩槽位满时预压缩响应仍可完成。
- [x] CPU 配额跟随未领取输出，析构结束后才释放；新增阻塞返回值析构的取消回归，验证 drain 不提前返回。
- [x] 在线程切换前排除当前线程的 join；同时验证 drop 和显式 shutdown 都能让自拥有的异步工作线程退出。
- 三个主回归均先运行并复现失败，再实施对应修复；保留原有工作区改动，不创建 commit。
- 二次修复验证：common 31 项、server 的中间件/HTTP/日志/静态资源 36 项、core 的公式/流水线/失败回收/外部表格 17 项测试通过，共 84 项；默认 native 成员 clippy 零警告，WASM 编译、平台边界、格式与 diff 检查通过。
- 重新运行审查时的仓库外探针：占满 9 个解析 CPU 槽位后，identity/gzip 健康检查及预压缩 JS 正文均完成；取消输出析构尚未释放时 drain 保持等待；自拥有异步工作线程实际退出。
- HTTP 压缩每次 poll 只占用 HTTP blocking 池中的一个线程，使用独立限流；不会等待解析 CPU 配额，且等待客户端/I/O 时不占用压缩额度。此轮未增加依赖。

## 完成后取消与 HTTP 计算隔离修复

- [x] 补充精确控制 JoinHandle 完成时序的回归，旧实现确认会在 async 线程析构未领取输出。
- [x] 使用统一所有权包装，将取消的输出及等待入池的输入转交原 CPU 池清理；正常领取结果无需增加调度。复用已有 tokio-util 的 TaskTracker，退出时等待输入、执行、输出及清理全流程。
- [x] 新增最多两个线程的 HTTP CPU 池，上传分批/最终哈希、分页索引解码、响应前缀及缓存生成与解析配额分离，沿用同一取消安全实现。
- [x] 使用本机隔离的临时 PostgreSQL 和真实 server 路由验证：解析池满载时，已有索引的分页、304、Markdown 冷热缓存、删除及小/大 PDF 上传可以完成；旧实现首先在缓存分页读取处超时，修复后通过。
- [x] 完成存储、HTTP 压缩、日志、核心流水线、静态检查和 WASM 兼容性回归。
- 本轮验证：common 32 项、core 流水线 16 项、server 中间件/存储/日志/静态资源/接口文档等 53 项，以及临时 PostgreSQL 上 HTTP 4 项，共 105 项通过。其他需要模型、人工测量或独立环境的忽略测试未扩展执行。
- `cargo clippy -p docparse-common -p docparse-core -p docparse-server --tests -- -D warnings`、WASM 编译、平台边界脚本、格式和 diff 检查通过。首次扩展 server 测试缺少 PDFium worker 可执行文件，构建后重跑通过。
- 取消清理保留原调用方 runtime 上下文，避免原生资源析构触发的后续清理被调度到没有 async 驱动的 CPU runtime；对应回归先确认失败，再通过。
- 此处“满载”指占满计算槽位的确定性回归；没有真实模型负载的 p95/p99 或吞吐量 benchmark，不据此声称生产延迟得到硬保证。
