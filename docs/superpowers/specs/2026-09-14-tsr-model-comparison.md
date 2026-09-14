# TSR 单元格检测与结构模型对照

目标是在现有解析流程内验证两个问题：独立单元格检测是否改善文字落格，SLANeXt 是否改善行列与合并关系。沿用现有模型下载、固定 revision/hash、配置路径解析、Native/WASM session 与取消生命周期，不创建服务，不创建 commit 或 worktree。

## 配置与边界

- `tsr_only` 是跳过前置规则重建的配置名称，已将旧名称 `external_only` 完整重命名，不保留别名。原始文字填充和完整性验证始终保留。
- TSR 支持 SLANet+、SLANeXt wired、SLANeXt wireless，并可配置独立 RT-DETR wired/wireless 单元格检测。
- 默认组合为 SLANet+ 加无线单元格检测；主配置使用 `tsr_only`，库默认保留 `fallback`。`tsr-baseline` 配置通过 `cell_detection.enabled = false` 关闭检测，`tsr-upgraded` 保留 SLANeXt 无线模型对照。
- 结构模型与检测器分别使用显式模型路径、配置路径和 manifest 路径。未启用的模型不加载。
- 结构 tokens、结构位置头和独立检测框分别保留；SLANeXt 的无效位置输出不参与文字匹配。
- 检测框和逻辑单元格经过显式匹配后进入现有 CellGrid。不能假设检测数量与 tokens 数量相同或顺序一致，不能用任意最近框填充缺失结果。
- 最终结果仍保留原始文字引用、完整字符覆盖、无重复归属、合法行列跨度和失败 warning。

## 验证

从 `/Volumes/Yage/Downloads/docs` 的真实 PDF 获取固定表格裁图和原始文字，覆盖有线表、无线表、长表、合并表头及多行单元格。比较当前 SLANet+、SLANet+ 加检测器、SLANeXt 加检测器。实验显式指定 wired/wireless，避免分类误差混入模型比较。

复用现有真实模型采集与重放测试，记录初始化、预热、预处理、排队、推理、匹配和总耗时。检查人工标注的行列、跨度及文字落格，并展示原图与结果。结构化成功率和文字完整性单独报告，不能充当语义准确率。不同模型加载完成后显式预热，相同裁图测量重复推理；保留失败和退化，不能筛掉新模型失败样本。

新增回归覆盖模式重命名、默认组合与显式禁用、模型路径解析、manifest 固定身份、检测框乱序/数量不匹配、合并单元格、无有效位置的 SLANeXt、取消与模型生命周期。运行相关测试、Clippy 与 WASM 编译/边界检查。
