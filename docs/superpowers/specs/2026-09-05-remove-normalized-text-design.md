# 删除 normalized_text 设计规格

## 背景

当前 `TextItem` 同时公开 `raw_text` 与 `normalized_text`。提取层把 PDFium 字符写入 `raw_text`，同时根据字符墨迹框间距推断缺失空格并写入 `normalized_text`；行组装和渲染又会在相邻 `TextItem` 之间进行第二层空格推断。这套机制不仅名称容易被误解为 Unicode normalization，而且已因字体空格宽度单位错误，把正常字符间距误判为单词边界。

用户明确不需要任何 `normalized_text` 或几何空格恢复机制，并接受公开 JSON schema 的破坏性变更。

## 目标

- 从公开 `TextItem` schema 中彻底删除 `normalized_text`。
- 从内部 draft、segment、OCR、语义和测试数据中删除对应字段。
- 删除字符级和跨 `TextItem` 的几何空格推断。
- 删除仅用于该机制的 `RepairAction::InsertedSpace` 与字体空格宽度读取 API。
- 所有文本判断、行文本构造和 renderer 直接使用 `raw_text`。
- 将输出 schema 主版本升级为 `2.0`，明确表达字段删除。
- 删除 `docparse.toml.example`，以根目录 `docparse.toml` 作为唯一默认配置和文档示例。

## 非目标

- 不修改 PDFium 返回的字符顺序、Unicode 映射或源空白字符。
- 不新增 Unicode NFC、NFKC、大小写折叠、断词或语言模型文本修复。
- 不重新设计段落、双向文字、OCR 或 layout 模型。
- 不保留 `normalized_text = raw_text` 的兼容字段或隐藏 alias。
- 不提供 v1 到 v2 的自动迁移器。

## 方案比较

### 方案 A：彻底删除并升级 schema 2.0（采用）

删除字段和全部推断代码，下游统一读取 `raw_text`。优点是数据语义唯一、没有重复字符串内存、不会再出现启发式篡改；代价是所有依赖 v1 JSON 的消费者必须迁移。

### 方案 B：保留字段但始终复制 raw_text（拒绝）

兼容旧消费者，但仍保留重复数据、歧义和维护成本，不符合“去掉机制”的明确要求。

### 方案 C：保留 v1 并仅修正空格阈值（拒绝）

能修复当前样例，却继续保留用户不需要的启发式机制，也无法消除未来字体和坐标系差异带来的误判。

## 数据模型与 schema

- 删除 `TextItem.normalized_text`。
- 删除 `TextItemDraft.normalized_text` 与 `CurrentSegment.normalized_text`。
- 删除 `TextCharFact.space_width`；提取阶段不再调用 `TextChar::font_space_width()`。
- 删除 `TextChar::font_space_width()`，但保留通用公开字体 glyph width API。
- 删除 `RepairAction::InsertedSpace`；其余修复动作保持不变。
- 新增 `SchemaVersion::V2_0` 并让新文档固定输出 `2.0`。
- schema 反序列化只接受主版本 2；v1 文档由调用方显式迁移。
- JSON 中 `TextItem` 只保留 `raw_text` 作为唯一文本事实。

## 文本数据流

### Native 提取

PDFium 可见字符直接追加到 `CurrentSegment.raw_text`。PDFium 提供的空白字符继续由 `push_source_space` 合并为一个 ASCII 空格；控制字符仍按现有规则删除并记录相应修复动作。显式换行、几何换行、回退、旋转或样式变化仍可切分 segment，但不再根据普通字符间距插入空格。

### OCR

OCR `TextItem` 只保存 OCR engine 返回的原始文本。native/OCR 去重、覆盖率和缺失区域逻辑改为读取 `raw_text`，不引入第二套文本。

### 行与语义

`Line.text` 按最终 `TextItem` 顺序直接拼接 `raw_text`，不在 item 边界插入推断空格。双向文字分类、字符计数、段落特征、标题和重复页眉指纹全部改为读取 `raw_text`。

### Renderer

Raw renderer 直接按顺序追加 `TextItem.raw_text`。Semantic renderer 继续使用已构建的 `Line.text`，其内容同样源于 raw 文本。JSON renderer 自动不再输出已删除字段。

## 错误处理与兼容性

- 删除字段是预期的 breaking change，不通过保留默认值掩盖。
- 遇到 schema `1.x` 时返回 unsupported schema major 错误。
- 不因为缺失几何空格而报错；PDF 原始文本没有空格时，v2 输出也不自行补空格。
- 所有稳定 ID、bbox、provenance、char codes、reading order 和 evidence 结构保持不变。

## 配置文件收敛

- 删除根目录 `docparse.toml.example`，保留现有 `docparse.toml`。
- README 不再要求复制示例文件，CLI 省略 `--config` 时继续直接读取 `./docparse.toml`。
- `crates/config/README.md`、配置集成测试、layout Python parity 测试和 CLI fake 测试全部改为引用根目录 `docparse.toml`。
- 不增加自动复制、模板生成或父目录配置搜索。

## 测试策略

- 先修改 schema 测试，要求序列化结果不含 `normalized_text` 且版本为 `2.0`，确认旧实现失败。
- 增加字符级测试：有视觉间距但没有源空白时输出 `AB`，存在源空白时输出 `A B`。
- 增加跨 `TextItem` 测试：相邻 item 不因几何间距自动插入空格。
- 更新 OCR、bidi、line、semantic、render、validator、parser 和 fixture builders。
- 删除 `docparse.toml.example` 并增加/更新测试，确认根目录 `docparse.toml` 可由严格 schema 直接加载。
- 对 `~/Downloads` 全部 PDF 运行 release CUDA E2E，并检查目标标题只保留 `raw_text` 且 JSON 中不存在 `normalized_text`。
- 串行与并行 canonical 结果必须完全一致；由于 schema 与文本内容按设计变化，不与 v1 hash 要求相等。
- 运行 workspace tests、Clippy、CUDA check/Clippy、Python parity 与 pre-commit。

## 验收标准

- `rg "normalized_text" crates/core/src README.md` 在生产代码和当前公共文档中无结果；schema 测试可保留一次旧字段缺失断言，本迁移规格可保留变更名称。
- `rg "InsertedSpace|font_space_width"` 在相关实现中均无结果。
- `SynCode: LLM Generation with Grammar Augmentation` 在 v2 JSON 中作为 `raw_text` 原样出现，且同一对象没有第二个文本字段。
- `README.md`、`crates/`、`scripts/` 与 `tests/` 中不存在 `docparse.toml.example` 引用；迁移规格可保留删除记录。
- 所有编译、测试、静态检查和真实 PDF E2E 通过。
- 不创建 commit，不使用 SubAgent 或 worktree。
