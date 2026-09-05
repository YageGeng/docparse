# Block 合成文本实施计划

> **执行要求：** 使用 `superpowers:executing-plans` 在当前工作区逐项执行。仓库禁止 SubAgent 和 worktree；未经用户明确授权不创建 commit。
>
> **后续修订（2026-09-05）：** 逐页真实 PDF 审核证明统一单空格策略会破坏 algorithm 结构，并与子集字体控制字形丢失共同产生断词。后续代码审查又证明 `0x02` 同时用于真实词内连字符，因此当前行为以同名设计规格为准：编码连字符作为独立事实保留且 canonical 摘要不删除字符，algorithm 保留换行和行首缩进；本计划下方保留的是最初实现记录。

**目标：** 为 schema 2.0 的每个 Block 增加按 LiteParse 风格单空格合成的必填 `text` 字段，同时保留完整 Lines。

**架构：** `Block::derive_text(&[Line])` 是唯一合成规则，semantic 构造阶段一次计算并存储。JSON 直接输出该字段，validator 使用无完整字符串分配的逐段比较防止 `text` 与 Lines 漂移。

**技术栈：** Rust 2024、Serde、typed-builder、现有 DocParse core/CLI/CUDA E2E。

**规格：** `docs/superpowers/specs/2026-09-05-block-text-design.md`

## 全局约束

- `Block.text` 取非空 `Line.text.trim()`，按阅读顺序用一个 ASCII 空格连接。
- 不修改 Line 内部文本，不删除 Lines，不做软连字符或 Unicode normalization。
- 空 Block 的 text 必须为 `""`。
- schema 保持当前尚未发布的 `2.0`。
- 新增函数必须有英文函数级注释；修改的非平凡逻辑必须有英文原因注释。
- 超过 3 个字段的结构体继续使用 `typed-builder`，Block.text 不设置 builder 默认值。
- 所有命令使用 `rtk`；不创建 commit，不使用 SubAgent 或 worktree。

---

### 任务 1：用行为测试锁定 Block.text 合约

**文件：**

- 修改：`crates/core/tests/types.rs`
- 修改：`crates/core/src/types.rs`

**接口：**

- `Block` 新增 `pub text: String`。
- `Block::derive_text(lines: &[Line]) -> String` 实现唯一合成规则。

- [x] **步骤 1：添加公开 JSON RED 测试**

使用现有两行 fixture，断言首个 Block 的序列化字段为：

```rust
assert_eq!(
    value.pointer("/pages/0/blocks/0/text").and_then(Value::as_str),
    Some("Hello x² World")
);
```

- [x] **步骤 2：运行测试确认 RED**

运行：`rtk cargo test -p docparse-core --test types block_serializes_single_space_text_summary`

预期：旧 schema 没有 Block.text，断言得到 `None` 并失败。

- [x] **步骤 3：实现数据模型与合成函数**

在 `Block` 中把 `text` 放在 `label` 后；实现：

```rust
pub(crate) fn derive_text(lines: &[Line]) -> String {
    lines
        .iter()
        .map(|line| line.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}
```

若 Clippy 或性能检查反对临时 Vec，改为预估容量并单遍 push，语义必须保持一致。

- [x] **步骤 4：增加合成边界单元测试**

在精确命名的 `#[cfg(test)] mod tests` 中覆盖：

```text
[" First line ", "", "Second  line"] -> "First line Second  line"
[] -> ""
```

### 任务 2：迁移构造、JSON 与消费者

**文件：**

- 修改：`crates/core/src/semantic/mod.rs`
- 修改：`crates/core/src/semantic/formula.rs`
- 修改：`crates/core/src/context/relations.rs`
- 修改：`crates/core/src/fusion/order.rs`
- 修改：`crates/core/src/render/json.rs`
- 修改：`crates/core/tests/types.rs`
- 修改：`crates/core/tests/render.rs`

**接口：**

- semantic model/fallback Block 在 Lines 完成后设置 `Block::derive_text(&lines)`。
- formula 和所有无文本 fixture 显式设置 `String::new()` 或按 Lines 派生。
- `ConfiguredBlock` 序列化 15 个字段，并在 label 后写 `text`。
- relation linker 使用 `block.text`，不再重新 join Lines。

- [x] **步骤 1：迁移所有 Block builder**

运行 `rtk grep -R -n "Block::builder()" crates/core crates/cli`，逐个构造点显式增加 `.text(...)`。生产 semantic 从 Lines 派生；测试 fixture 使用手工期望，避免通过同一函数计算断言。

- [x] **步骤 2：更新借用型 JSON serializer**

把 `serialize_struct("Block", 14)` 改为 15，并按 `id`、`label`、`text`、`raw_label` 顺序序列化。

- [x] **步骤 3：复用 Block.text**

将 `DocumentLinker::block_text` 的 Lines join 替换为 `block.text.clone()`，并更新英文注释说明直接读取已验证摘要。

- [x] **步骤 4：运行 core 测试**

运行：`rtk cargo test -p docparse-core`

预期：所有 builder 已迁移，JSON writer 与 canonical 字节一致。

### 任务 3：增加零分配 validator 不变量

**文件：**

- 修改：`crates/core/src/validate.rs`
- 修改：`crates/core/tests/types.rs`

**接口：**

- `ResultValidator` 拒绝与统一合成规则不一致的 Block.text。
- 错误路径为 `pages[i].blocks[j].text`。

- [x] **步骤 1：添加 validator RED 测试**

把有效 fixture 的 `block.text` 改成 `"wrong"`，要求 `ResultValidator::validate` 返回 `ValidationError::InvalidNode` 且 path 精确指向 Block.text。

- [x] **步骤 2：运行测试确认 RED**

运行：`rtk cargo test -p docparse-core --test types validator_rejects_block_text_not_derived_from_lines`

预期：旧 validator 接受篡改值，测试失败。

- [x] **步骤 3：实现逐段验证**

按 Lines 顺序 trim/跳空；在 `block.text` 上维护字节 offset，非首段先精确比较一个 ASCII 空格，再比较当前 segment；最后要求 offset 等于总长度。不得构造完整派生 String。

- [x] **步骤 4：运行 types 与 workspace 测试**

运行：`rtk cargo test -p docparse-core --test types`

运行：`rtk cargo test --workspace`

### 任务 4：更新文档并执行真实 E2E

**文件：**

- 修改：`docs/superpowers/specs/2026-09-04-layout-text-fusion-design.md`
- 生成：`target/docparse-e2e/block-text-final-serial/`
- 生成：`target/docparse-e2e/block-text-final-parallel/`

**接口：**

- 公共设计文档声明 Block.text 单空格派生且 Lines 保留。
- 最终全部 PDF Block 都包含 text。

- [x] **步骤 1：执行静态与完整测试门禁**

运行：`rtk cargo fmt --all -- --check`

运行：`rtk cargo test --workspace`

运行：`rtk cargo clippy --workspace --all-targets -- -D warnings`

运行：`rtk cargo check -p docparse-cli --features layout-cuda`

运行：`rtk cargo clippy -p docparse-cli --all-targets --features layout-cuda -- -D warnings`

运行：`DOCPARSE_LAYOUT__EXECUTION_PROVIDER=cpu rtk cargo test -p docparse-layout --release --test python_parity -- --ignored --nocapture`

- [x] **步骤 2：运行 release CUDA 串并行 E2E**

运行：`rtk uv --cache-dir /tmp/docparse-uv-cache run scripts/run_real_pdf_e2e.py --pdf-dir /home/isbest/Downloads --model-dir models/pp-doclayout-v3 --execution-provider cuda --cargo-profile release --page-concurrency 1 --run-id block-text-final-serial`

运行：`rtk uv --cache-dir /tmp/docparse-uv-cache run scripts/run_real_pdf_e2e.py --pdf-dir /home/isbest/Downloads --model-dir models/pp-doclayout-v3 --execution-provider cuda --cargo-profile release --page-concurrency 4 --run-id block-text-final-parallel`

- [x] **步骤 3：比较并检查目标 Block**

运行：`rtk python scripts/compare_e2e_runs.py target/docparse-e2e/block-text-final-serial/canonical-hashes.json target/docparse-e2e/block-text-final-parallel/canonical-hashes.json`

检查 SynCode 标题所属 Block：`text` 必须是 `SynCode: LLM Generation with Grammar Augmentation`，同时 `lines` 仍存在。

- [x] **步骤 4：执行最终仓库检查**

运行：`rtk pre-commit run --all-files`

运行：`rtk git diff --check`

运行：`rtk git status --short`

预期：全部通过；不创建 commit，不改变用户已有 staging 决策。
