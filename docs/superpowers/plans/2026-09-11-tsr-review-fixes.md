# TSR review 修复计划

1. 用显式 ParserArtifacts 携带 layout 与 TSR 权重，将 Web 和原生的模型初始化统一到 DocParserBuilder；保留 rules_only 的单模型入口。
2. 将声明输入和模型预测的解码、拓扑归一化、网格验证统一放进 core；内置模型适配器只转换输出。
3. 消除预测网格“文字可分配就提前返回”的捷径，补上真实文字与受控粗网格回归，确保修复后仍保持原文唯一归属。
4. 将 fallback 完整性检查与 other 模式共用；只有显式允许未恢复结果时才报告 partial，默认缺表必须使验收失败。
5. 在 cfg(test) tests 下拆分模型捕获、fixture 读取和规则回归；不扩大生产 API 来服务测试。
6. 运行原生/WASM 检查、既有 badcase，以及真实浏览器 default fallback / external_only 端到端测试。

不创建 worktree，不创建 commit。
