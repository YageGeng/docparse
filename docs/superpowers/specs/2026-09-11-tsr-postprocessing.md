# TSR 后处理与表格合成修复

## 目标与结果

`2303.18223v16.pdf` 的默认 `external_only` 流程必须对全部 layout table 执行真实 TSR，并为所有表格生成通过源文本校验的结构化结果。此次没有调用本地规则表格重建作为兜底；原文和已有分隔线用于校准模型结构。

已完成完整 144 页的实际运行：

| 环境 | 检测表格 | 结构化表格 | TSR 请求 | ONNX 推理次数 |
|---|---:|---:|---:|---:|
| Native，CoreML layout / CPU TSR | 24 | 24 | 24 | 32 |
| Chrome，WebGPU layout / WASM CPU TSR | 24 | 24 | 24 | 30 |

推理次数包含图像分段重试，因此与表格请求数不同。最终 Native 验收使用 release 构建，浏览器使用经过优化的生产 WASM。两个环境独立加载 layout 后端，耗时不作为硬件间的性能对比。

## 根因

1. SLANet_plus 的长表输出达到 501 步时可能没有 EOS。旧实现只能拒绝整张表；第 8 页是实际复现。
2. 一些复杂图示区域会让模型重复预测行或产生空框；第 48 页需要对真实裁剪做有界分段推理。
3. 独立位置预测被过早压成全表统一边界，导致长标签、分式和上下标跨格。单纯放大字号容差不能修复错位。
4. 模型的 rowspan/colspan 与位置不一致，造成占位空洞；也存在漏列、漏行或多行被压成单行。
5. 原验收只检查流程完成和已有结果来源，未要求所有表格有结构。

## 处理链

### 模型模块

`crates/tsr` 保持独立：只接收 RGB 图像，返回结构 tokens、位置和置信度。模型、词表和预处理版本保持固定。

- EOS 缺失和退化位置保留明确错误原因。
- 在可见横向分隔线或空白扫描线处分段，最多三层、八个叶段。
- 每段必须完整成功，不能把前半张表作为整表发布。
- 恢复段的原始裁剪坐标；连续段不重复创建全局表头。
- 队列、deadline 和实际执行资源生命周期继续由原有 runtime 管理。

### 输入解码与拓扑

`Declared` 继续拒绝不平衡 tokens、非法跨度、错误请求 ID、越界框、占位重叠及空洞。

内置模型使用 `Predicted`：先保留独立框，再以密集模型行提供的列中心校对 token 位置，单调放置单元格，修正跨行终点并补充明确的空位置。普通正文单元格不会仅因位置头框过宽而被扩为跨列单元格。

### 原文合成

`crates/core/src/table/grid/predicted/` 按职责分开：

- `axes.rs`：模型轴、文字间隙、完整锚点列、编号行及多行正文边界。
- `binding.rs`：模型位置与校准区域匹配、原生行基线、连续短语归属和最终墨迹边界。
- `spans.rs`：可见分隔线支持的跨度拆分、空位置合并和分类标签延展。
- `topology.rs`：模型列中心、单调位置匹配和占位修复。
- `recovery.rs`：重复数字间隙支持的漏列修复，以及复用现有表头逻辑。

修复必须有跨行重复证据或明确几何证据。例如，新增数字列需要每个正文行都支持同一个宽间隙；从单行恢复编号表需要连续编号、稳定的左对齐位置以及另一模型列的对应内容。无法确定归属的跨列原文仍然拒绝。

模型内容框可以相交，原文归属以明确的词引用为准。最终共享校验仍要求：

- 完整 UTF-8 字符覆盖，无重复引用；
- 每个词由所属单元格覆盖至少 80% 墨迹；
- 合并网格完整且不重叠；
- 原 Block 边界、原始行和原始文本事实不变。

## 回归与验收

默认测试内置了所有 24 张表的真实模型输出和原文事实，压缩存储，不依赖模型下载或原 PDF。除完整性校验外，还固定了人工核对的语义检查：

- 第 8 页：58 行、13 列；T5/mT5 与各自日期对应，保留分组表头。
- 第 24 页：16 行、3 列，保留公式原文引用。
- 第 33 页：6 行、13 列，分开 `36.6` 与下一列 `1`，LoRA 分组表头跨三列。
- 第 47 页：28 行、3 列，分类标签覆盖完整行组。
- 第 57 页：21 行、4 列，Basic/Advanced 跨度和多行数据集归属正确。
- 第 68 页：19 行、6 列，分类跨度与相邻行分数不混淆。
- 第 82 页：11 行、2 列，不能把十条公式压进单个正文行。

模型测试覆盖 EOS 截断、分段坐标、表头范围和重试深度。外部声明式输入及不确定的预测输入仍有拒绝回归。

`crates/web/tests/paddle_tsr.mjs` 默认要求 `external_only` 全部表格成功；仅显式 `--allow-unresolved` 才允许诊断性运行，报告标记为 `partial`。验收使用生产 SDK、Worker、PDFium、layout 和 ONNX 模型，不替换后端。

## 复现

```sh
rtk cargo test --locked
rtk cargo clippy -p docparse-core -p docparse-tsr --all-targets --locked -- -D warnings
rtk cargo clippy -p docparse-web --target wasm32-unknown-unknown --locked -- -D warnings
rtk proxy node crates/web/tests/paddle_tsr.mjs --output packages/wasm-web/test-results/paddle-tsr/browser-repair/report.json /Volumes/Yage/Downloads/2303.18223v16.pdf
```

浏览器命令需要先构建 `packages/wasm-web` 并启动真实 example。Native 使用 `crates/core/tests/paddle_tsr_e2e.rs`，通过 `TSR_E2E_PDFS` 提供 JSON 文件路径列表。

本次完成标准是全表结构化、源文本完整性及指定语义回归；置信度不作为人工标注级准确率。未能确定归属的输入继续保留原文并报告失败。

验证记录：workspace 298 个测试通过；Native 与 WASM Clippy、平台边界检查通过；原有规则模式完成 9 组真实浏览器回归，包含四份完整 PDF 共 223 页及 CPU/符号子集。真实模型的取消、取消后重建、重复解析、1ms deadline 验证均通过。未创建 commit。

同一 PDF 的真实 Web `fallback` 复测也为 24/24：23 张表使用既有规则结果，1 张表进入内置 TSR；该请求包含 5 次有界模型推理。
