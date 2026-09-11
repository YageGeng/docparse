# 字符恢复与清理实现计划

目标：按已授权规格引入全部字符处理能力，保持几何与来源证据有效。

规格：[字符恢复设计](../specs/2026-09-11-character-recovery-design.md)。直接在当前工作区顺序实施，不创建 commit、SubAgent 或 worktree。

- [x] 为字符展开、标点与输出清理增加回归测试，确认旧实现失败。
- [x] 引入 AGL 与反向 cmap 模块，扩展现有 GlyphNormalizer 的字体判定、缓存和多字符结果；维护原始编码、修复原因及表格词范围。
- [x] 接入 GlyphResolver、Native MessagePack 字形数据库、builder 和 PDFium 请求链；补充注入与分片测试。
- [x] 在输出层实现空白清理与正文去断词；扩大列表符号和 RTL 覆盖并验证 Raw、表格、算法、公式边界。
- [x] 补充英文公共文档和来源声明，运行相关测试、workspace/WASM 编译和 pre-commit，检查最终差异。

验证结果：native 默认成员测试 323 项通过、11 项忽略；其中 core 254 项通过、7 项忽略。WASM 编译和全部 pre-commit hooks 通过。代码审查结论见 `../reports/2026-09-12-character-recovery-review.md`。

## 第二轮组织审查修复

- [x] 用真实 PDF 回归测试复现连字修复记录不一致及字体缓存受逐字条件影响的问题。
- [x] 将归一化统一到 ResolvedGlyph，将字体候选缓存与逐字选择分开；保留连字去重及来源几何。
- [x] 将列表及纯字符规则下移至共享文本规则模块，解除 renderer 对 semantic 内部规则的依赖。
- [x] 将 PDF 对象序列化、xref 和 trailer 合并到 tests/common/pdf.rs。
- [x] 运行 native 测试、WASM 编译、pre-commit，更新审查结论。

第二轮验证：native 默认成员测试 326 项通过、11 项忽略；core 257 项通过、7 项忽略。WASM 编译、全部 pre-commit hooks、共享 PDF 构造器格式检查通过。
