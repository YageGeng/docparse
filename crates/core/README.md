# docparse-core

DocParse 的原生 PDF 文本提取、文档上下文、版面/文字融合、异步 parser、稳定 schema、关系和 JSON/Text/Markdown/SVG 输出 crate。

主要入口是 `DocParser`/`DocParserBuilder`。自定义 layout/OCR 通过 `Arc<dyn LayoutEngine>` 与 `Arc<dyn OcrEngine>` 注入；默认解析运行 production PP-DocLayoutV3。完整 API 和 E2E 命令见仓库根目录 `README.md`。
