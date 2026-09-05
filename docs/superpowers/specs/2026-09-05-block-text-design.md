# Block 合成文本设计规格

## 背景

DocParse 当前在 `Block` 下只保存 `lines`，调用方需要遍历多层结构才能读取一个版面块的完整文本。用户要求在 Block 层增加可直接消费的合成 `text`，同时保留现有 Line 和 TextItem 事实。

本设计参考本地 LiteParse 的 layout block 实现：paragraph、heading 和 list item 的连续物理行默认以单个空格连接；换行主要用于不同 Block 的渲染边界，code/grid/table 则另行保留结构化行或单元格。DocParse 使用统一 Block schema，因此增加通用字符串摘要，同时继续保留 `lines` 作为精确结构。

## 目标

- 为每个 `Block` 增加必填 `text: String`。
- 让文本型 Block 可在不遍历 `lines` 的情况下直接读取完整内容。
- 保留 `Line.text`、`Line.text_items`、bbox、reading order 和 provenance，不删除或重排任何事实。
- 在构造和验证阶段保证 `Block.text` 与其 Lines 始终一致。
- 将该字段并入当前尚未提交的 schema `2.0`。

## 非目标

- 不删除 `Block.lines`。
- 不改变 `Line.text` 或 `TextItem.raw_text`。
- 不做 Unicode normalization、内部空白折叠或语言模型重写。
- 不在本阶段为 table、reference、formula 引入未经验证的专用结构。
- 不改变 Text/Markdown renderer 的 Block 间分隔规则。

## LiteParse 参考行为

- `markdown_layout::paragraphs::dehyphenate_join` 在普通物理行边界插入一个空格，并在有明确断行连字符证据时去掉连字符直接拼接。
- wrapped heading 与 list item 通过 `append_inline_continuation` 使用同一连接规则。
- 公共 `LayoutBlock.text` 只承载 heading、paragraph、list item；code/grid/table 保留各自结构。
- Markdown renderer 在不同 Block 之间使用空行，但这不属于 Block 内文本合成。

DocParse 的统一 Block schema 继续保留 `text`，但 canonical 摘要不猜测编码连字符究竟是断词还是词内连字符：普通文本只移除行分隔而保留连字符，`algorithm` 使用换行并保留行首缩进。table、reference 和 formula 在专用结构设计完成前维持原有摘要策略。

## 合成规则

按 `Block.lines` 当前数组顺序执行：

1. 普通标签对每个 `Line.text` 调用 `trim()`；`algorithm` 只调用 `trim_end()`，保留行首缩进。
2. 跳过处理后为空的 Line。
3. 普通文本使用一个 ASCII 空格 `U+0020` 连接其余 Line 文本。
4. `0x02` 恢复的连字符必须成为独立 TextItem 并记录 `EncodedHyphen`，不得标记为软连字符。
5. 仅 FlowText/Title 可在方向、旋转和纵向推进确认下一物理正文行且下一行以字母开头时，保留 `-` 并省略行间空格；不得删除 `-`。Structured、Formula、Atomic、Chrome 和 Unknown 不执行该正文规则。
6. `algorithm` 使用 `\n` 连接非空 Line，且不执行连字符推断。
7. 不修改每行内部的其他字符或空白。
8. 没有非空 Line 时输出空字符串 `""`。

示例：

```text
text lines = ["pro-" (EncodedHyphen), "grams"]
text block.text = "pro-grams"

algorithm lines = ["let value = 1;", "return value;"]
algorithm block.text = "let value = 1;\nreturn value;"
```

## 数据模型与构造

- `Block` 新增公开必填字段 `pub text: String`，继续通过 `typed-builder` 构造。
- `Block::derive_text(label: &LayoutLabel, lines: &[Line]) -> String` 作为唯一合成实现，并提供英文函数级注释。
- semantic assembler 在完成 Lines 后调用 `Block::derive_text(&label, &lines)`，随后同时写入 `text` 与 `lines`。
- 空模型 region、图片、图表等无文本 Block 显式写入 `String::new()`；测试 fixture 必须显式提供期望值，避免 builder 默认值掩盖遗漏。
- document relation 等只需完整 Block 文本的消费者改为读取 `block.text`，避免重复分配和重复合成。

## 序列化

- JSON 中 `text` 位于 `label` 之后、`raw_label` 之前，作为 Block 的核心内容字段。
- 借用型 configured JSON serializer 必须输出该字段，并把 Block 字段计数从 14 更新为 15。
- `text` 始终出现，包括空字符串，不使用 `skip_serializing_if`。
- 当前 schema 2.0 尚未提交或发布，本字段直接纳入同一 v2 合约，不再升级到 2.1。

## 验证

`ResultValidator` 对每个 Block 独立按上述标签与断行规则验证：

- 顺序读取 Lines；
- 按标签执行 trim/trim_end 并跳过空行；
- 要求分隔位置与标签策略一致，并保留编码连字符；
- 要求最终字节长度与 `Block.text` 完全一致。

验证应避免构造第二个完整 Block 字符串；使用偏移量逐段比较，降低真实 E2E 的峰值内存。错误路径固定为 `pages[i].blocks[j].text`。

## 测试策略

- 先增加 JSON 行为测试，要求两行 Block 输出 `"Hello x² World"`，确认旧代码因缺少字段而失败。
- 增加合成单元测试，覆盖首尾 trim、空行跳过、内部双空格保留、编码连字符、真实复合词、algorithm 换行/缩进、非正文标签和空 Block。
- 增加 validator 测试，篡改 `Block.text` 后必须返回精确 node path。
- 更新所有 Block builder fixture，编译器负责暴露遗漏构造点。
- 运行 workspace tests、Clippy、CUDA check/Clippy、Python parity 与 pre-commit。
- 使用 `~/Downloads` 全部 PDF 运行 release CUDA 串行/并行 E2E，要求 canonical 一致。
- 检查 SynCode 标题 Block 的 `text` 等于 `SynCode: LLM Generation with Grammar Augmentation`。

## 验收标准

- 每个序列化 Block 都包含字符串字段 `text`。
- 所有 `Block.text` 都严格符合标签感知的合成规则。
- `lines` 和嵌套 TextItem 完整保留。
- schema 仍为 `2.0`。
- 全部静态检查、自动测试和真实 PDF E2E 通过。
- 不创建 commit，不使用 SubAgent 或 worktree。
