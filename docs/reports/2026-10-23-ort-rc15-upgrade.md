# ORT rc.15 升级与验证

## 范围

- `ort` / `ort-sys`：`2.0.0-rc.13` → `2.0.0-rc.15`，原生预编译 ONNX Runtime：1.28 → 1.31，原生 API：31。
- 本地 `ort-web`：`0.3.0+1.27` → `0.5.0+1.30`，浏览器运行库：1.30.0，浏览器 API：24。
- 升级与三项审查修复均在当前工作目录完成，未使用 worktree 或 SubAgent。实现和复查阶段未创建 commit；之后按用户授权运行提交前检查并创建本地提交，不推送。

## 兼容迁移

1. 保留进程共享环境和线程池，由共享后端显式向每个 Session 传递 rc.15 环境。环境跨解析器释放和重新创建持续存在，避免上游禁止重复创建环境的错误；独立会话线程池明确选择独立模式。
2. WASM 版面、OCR 和表格输入移动原始数组，避免复制图像；Texo 在异步步骤中保留拥有所有权的输入切片，执行后恢复 hidden states 和 KV 缓存，不复制 GPU 张量。
3. 基于发布包及 SHA-256 重新应用 `ort-web` 本地补丁。上游已实现输出筛选、图优化和内存选项透传；继续保留 Worker ESM 加载、可增长内存视图和旧浏览器快照、资源释放诊断与 GPU 输出位置配置。
4. 增加 rc.15 的 WebGPU provider 规范名称兼容，以及 GPU 输出 allocator 所有权句柄。GPU 句柄不允许分配或释放 CPU 内存。
5. 公式消费者列表的带标签枚举需要显式接受 Uint8Array 字节表示，同时保留普通数组兼容；完整浏览器初始化测试覆盖此边界。
6. 同步版本清单、部署文档、第三方许可及模型级浏览器示例。修正现有浏览器测试中过期的公式消费者配置、TATR 模型选择和双阶段版面排队计数。

## 已完成验证

- `rtk cargo test --locked`：103 组测试结果，690 项通过，0 项失败，63 项按原有条件忽略。
- 原生和 WASM 的 Clippy（`-Dwarnings`）通过；Rust 格式和平台 cfg 边界检查通过。
- CLI / server 的 TensorRT 特性 `cargo check --all-targets` 通过，使用独立 GPU 构建目录；TensorRT CLI 完整链接构建和 `--version` 启动也通过。
- 真实 CUDA OCR：批次 1、4、8、16 的结果一致性测试通过。
- 真实原生 PDF：五份 PDF 的解析和浏览器参考结果生成通过。
- 原生 Texo：批次一致性和取消回归通过。
- 原生 TATR：与共享单元格检测模型的真实推理通过。
- 浏览器 Texo WASM：单张和双张批次与固定参考公式一致。
- 浏览器 Texo WebGPU：单张和双张批次与参考一致，1598 次 GPU 提交，hidden states 和非空 KV 缓存位置检查无违规。

- 最终生产 WASM SDK 完整构建通过：Binaryen `-O4` 优化，194 个 WASM imports 检查通过。
- 完整 WASM SDK 验收：14 项检查通过，三轮重复解析、错误 PDF 恢复、取消与重建、强制内存增长、输出筛选、零残留张量、页面图像及计时通过。
- 原生/Web 输出：嵌入字体、中文/旋转/CropBox/UserUnit、三类表格的严格对比差异均为 0；单页、多页结果也通过对比。
- 示例页面：选择、缩放、页切换、真实 PNG 导出、取消恢复、错误 PDF 恢复、切换后端全部通过。
- 导出竞态：新请求先完成、旧请求先完成、旧请求失败三个场景均通过。测试明确等待旧 PNG 编码启动，避免在开始编码前取消造成无效竞争。
- 模拟旧浏览器内存能力：14 项 SDK 检查通过，使用快照而非可增长内存视图，输出一致且无残留张量。
- 会话选项：布局五种配置、TATR、OCR、PP、Texo 共九组模型初始化，以及非法配置拒绝检查通过。

- NVIDIA 硬件 WebGPU：适配器为 `nvidia / lovelace`，`isFallbackAdapter=false`；完整 SDK、示例交互和三个导出竞态场景通过。SDK 的 14 项检查、三轮重复解析、原生结果严格对比、零输入中间复制和零残留张量通过。
- Texo 硬件 WebGPU：从 ORT 实际设备读取的适配器为 NVIDIA Lovelace、非回退；1598 次 GPU 提交，GPU 缓存位置检查无违规，单张和双张结果与固定参考一致。

初次 headless Chrome 选择了 SwiftShader 软件适配器，版面推理约 70 秒/次。该长耗时验收已主动结束并清理测试浏览器，未计为硬件验证。使用已验证的 Linux GL/Vulkan 启动参数后完成上述 NVIDIA 验收；测试脚本现会记录 ORT 的实际设备，避免混淆软件和硬件执行。参数仅作用于临时测试浏览器，未修改用户浏览器配置或系统驱动。

升级首轮 SDK WASM SHA-256：`d75ae9613cd0d81ea620e3253d02a5213a64309ba9f1fffdef11ae896db78a69`。

详细浏览器证据保存在忽略目录 `packages/wasm-web/test-results/ort-rc15/`（`wasm.json`、`legacy.json`、`nvidia.json`、`nvidia-sdk.json`）；终端日志保存在 `/tmp/docparse-ort-rc15/`。

## 验证边界

本机仍缺少 `libnvinfer.so.10` 和 `libnvonnxparser.so.10`。已经检查新版 provider 的实际 ELF 依赖；TensorRT 编译通过不代表 TensorRT 引擎构建或推理通过。本次不安装系统 TensorRT，也不修改旧 engine 缓存；安装匹配运行库后应使用新缓存目录重建引擎。

最后一次 NVIDIA SDK 复测中，未嵌入字体多页样例出现两个原始模型排序字段差异：同一块的 `source_region.model_order` 和 `model_order` 为 128，原生参考为 129。既有测试对该类跨平台样例记录差异；嵌入字体、中文几何、表格的严格比较仍为 0 差异。新增适配器记录确认 ORT 实际使用 NVIDIA Lovelace，且输入中间复制字节数为 0。

没有在 macOS、Windows 或真实旧浏览器上执行验证。完整测试保留的忽略项包含模型、数据库等外部条件测试；另行执行的真实模型测试以上述明细为准。

完整原生测试仍提示既有的多个 `recognize` 示例输出文件同名警告。本次未更改示例名称；测试结果及 Clippy 检查通过。


## 审查修复：问题 1 和 2

这一阶段只修复宿主 ORT 环境复用和重复输入快照。当时暂缓问题 3，`crates/formula-texo/src/wasm_compat.rs` 与该阶段开始时逐字节一致；后续处理见下节。

### 宿主环境复用

新增 `docparse_layout::OnnxBackend::use_environment(environment.clone())`，在创建任何 DocParse 会话前注册宿主已有环境。重复注册同一环境无副作用；DocParse 保留句柄，宿主释放自己的句柄后仍可创建新解析器。默认 CLI/server 仍惰性创建环境。

环境所有权与线程策略分开；宿主决定其全局线程池和遥测设置，DocParse 不重建环境、不修改宿主 provider 列表，并明确隔离会话的执行 provider。宿主若需要全局线程池，必须在创建环境时配置。

回归首先复现了 `only one environment is allowed per process`。修复后的独立集成测试通过真实 ONNX Add 图，验证重复注册、句柄释放后重建会话，以及宿主关闭 CPU arena 后仍保持其设置、DocParse 使用自己的会话分配策略。CPU 和 CUDA 构建均通过。

### 单份输入快照

Rust-backed 张量构造阶段只创建元数据视图；真正提交运行时才生成稳定视图或快照。Rust 存储保持权威，显式同步不再访问可能因堆增长而失效的元数据视图；外部张量的同步与 GPU 缓存所有权保持原逻辑。

测试分别记录实际提交的 CPU 输入字节数与全部中间快照字节数，避免只检查“发生过复制”而漏掉重复快照。修复前断言失败：复制 15,360,032 字节、提交 7,680,016 字节。

| 验证路径 | 单次提交字节数 | 单次中间复制字节数 | 结果 |
| --- | ---: | ---: | --- |
| WASM，可增长内存 | 7,680,016 | 0 | 通过 |
| WASM，模拟旧内存能力 | 7,680,016 | 7,680,016 | 通过 |
| NVIDIA WebGPU，可增长内存 | 7,680,016 | 0 | 通过 |
| NVIDIA WebGPU，模拟旧内存能力 | 7,680,016 | 7,680,016 | 通过 |

四条路径均完成 14 项 SDK 检查和三轮重复解析，覆盖强制堆增长、输出对比、取消恢复和零残留张量。正常 WASM 的示例页面及三个导出竞态场景通过。NVIDIA 适配器为 Lovelace、非软件回退；Texo 真机公式回归与 GPU 缓存驻留检查也通过。

### 本轮其他验证

- 完整原生测试：104 组结果，691 项通过，0 项失败，63 项按原有条件忽略。沙箱限制 IPC socket 的初次运行已在宿主环境重新运行并通过。
- 原生和 WASM Clippy（`-Dwarnings`）、Rust 格式、平台 cfg 边界和 `git diff --check` 通过。
- 生产 WASM SDK 完整构建通过，包含 Binaryen `-O4` 优化和 194 个 WASM imports 检查。
- 修复后生产 SDK WASM SHA-256：`1159ff7a5972724818d87327df753cbe7a129e6b7006d5a84a8d59ce89a7aaed`。
- 浏览器证据：`packages/wasm-web/test-results/ort-review-fixes/`；终端日志：`/tmp/docparse-ort-fixes/`。

本轮未执行 TensorRT 引擎构建/推理，也未在真实旧版本浏览器或其他操作系统验证；旧内存能力通过真实模型上的能力降级测试覆盖。

## 审查修复：问题 3

Texo 浏览器 decoder 在每个会话初始化时校验 11 个输入并建立完整、无重复的索引映射。逐 token 路径改用固定数组，移除原有的两个临时路由 Vec 和名称搜索；异步调用期间仍持有输入张量，推理后按索引恢复 hidden states 和 KV 缓存，继续复用缓存 Vec 的容量。

修改限定为 WASM 适配层、模型级浏览器回归和对应 README 说明。未新增依赖或公共抽象，原生解码路径不变。

修复与只读复查均完成以下验证：

- 缺失输入回归先确认旧实现在初始化时未拒绝错误契约；修复后，缺失、重复、未知输入均在模型加载阶段拒绝。
- WASM 与 NVIDIA WebGPU 的正常输入顺序及反转元数据顺序均通过真实模型的单张、双张输出对比；连同三个错误契约用例，共七组浏览器场景通过。
- 两组 WebGPU 场景均使用 NVIDIA Lovelace、非软件回退，各完成 1598 次 GPU 提交；hidden states 和非空 KV 缓存的输入、输出位置检查无违规。
- Texo 默认测试：10 项通过，1 项外部模型测试按原有条件忽略。
- 浏览器模型示例的 WASM release 构建、相关原生/WASM Clippy、Web crate 集成检查、格式与平台边界检查通过。

这一步验证使用重新构建的模型级浏览器示例。上节的完整 SDK 构建和 WebUI 验收对应问题 1、2 修复后的产物；问题 3 修复后未重新构建完整 SDK 发布包或重跑 WebUI 验收，也未做专项性能基准，不据此声称具体提速比例。

终端证据保存在 `/tmp/docparse-texo-input-order/`，包括修复前失败记录和七组 `rereview-*.log` 复查结果。

## 提交前检查

用户授权合并提交本次升级与问题 1、2、3 的修复，暂不推送。提交前对暂存内容执行 `pre-commit run --show-diff-on-failure`：平台兼容边界、Rust fmt、Rust Clippy 三项全部通过，hook 未改写源码。

补充检查同样通过：WASM Web crate 的 Clippy（`-D warnings`）、vendored ort-web 的 rustfmt、8 个变更 JavaScript 文件的语法检查、仓库根目录 `uvx ruff check .`，以及暂存 diff 的空白检查。本阶段没有重跑此前已完成的全部模型与浏览器验收。
