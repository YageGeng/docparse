# DocParse 阶段 D：真实模型与 PDF 验收实施计划

> **供当前会话执行者使用：** 必须使用 superpowers:executing-plans，按任务逐项执行本计划。禁止使用 SubAgent 或 worktree；步骤使用复选框（- [ ]）跟踪。

**目标：** 用固定 PP-DocLayoutV3 artifact、Python 官方流水线 oracle 和 `~/Downloads/` 当前全部 5 份、110 页真实 PDF，验证 DocParse 的模型一致性、文本守恒、稳定阅读顺序、视觉 overlay、分发内容与许可证合规。

**架构：** 先扩充独立 Python oracle，锁住预处理与检测结果；再由已跟踪的 e2e-corpus.toml 精确选择本地 PDF。Python/uv 驱动器负责模型与语料预检、启动真实 Rust ignored tests、监控资源和汇总报告；Rust harness 负责逐页结构不变量和规范结果输出。

**技术栈：** Rust 2024、Tokio、ort 2.0.0-rc.13、PP-DocLayoutV3、Python 3、uv、PaddleOCR 3.6.0、psutil、pypdf、serde/JSON/TOML

**设计规范：** docs/superpowers/specs/2026-09-04-layout-text-fusion-design.md

**前置条件：** 阶段 A、B、C 门禁全部通过；本地模型由 scripts/download_models.py 安装并匹配 revision 46bbdf188bb0a772c08aed74882ce7e51a8f1ea6；真实 PDF 只存在于用户指定目录，不进入 Git。

## 全局约束

- 不使用 SubAgent 或 worktree，不创建 commit。
- 每个新增函数必须有英文函数级注释；修改非平凡旧逻辑时必须用英文注释说明变动原因。
- 私有实现的测试只能放在对应源码文件或模块内声明为 `#[cfg(test)] mod tests`；crate 级 `tests/` 仅通过公开 API 测试，禁止为测试在生产代码中增加独立的 `#[cfg(test)]` 字段、函数、实现或支持模块。
- 真实 PDF、ONNX 模型、页面位图和 E2E 报告不得被 Git 跟踪。
- 测试必须扫描 `~/Downloads/` 顶层所有扩展名大小写不敏感的常规 PDF，并与 manifest 的 basename、SHA-256、size_bytes、page_count 完全匹配；缺失或额外 PDF 都必须使预检失败，禁止静默忽略。
- 所有 110 页运行机器不变量；视觉检查使用确定性抽样集合。
- 真实测试不得注入 fake LayoutEngine/OcrEngine，不得复用预先生成 detection 绕过 ORT。
- 性能只记录，不设跨机器硬阈值。
- 规范 DocumentResult 不包含时间、耗时、绝对路径或其他易变字段。
- 所有超过 3 字段的 struct 使用 typed-builder；Option 字段使用 builder default，并按调用方类型决定 strip_option；所有共享 Arc 显式克隆。

---

### 任务 D1：扩展 Python oracle 为多样本模型一致性基线

**文件：**
- 修改：scripts/reference_layout.py
- 修改：scripts/reference_layout.py.lock
- 新建：crates/layout/tests/fixtures/model/portrait.png
- 新建：crates/layout/tests/fixtures/model/landscape.png
- 新建：crates/layout/tests/fixtures/model/blank.png
- 新建：crates/layout/tests/fixtures/model/dense-overlap.png
- 新建：crates/layout/tests/fixtures/model/python_outputs.json
- 修改：crates/layout/tests/python_parity.rs

**接口：**
- 产出：多样本 tensor/detection oracle。
- 输入：固定模型 revision、paddleocr==3.6.0、PaddleX commit ffb64904d23708863ff5b8da312a5cbd52a7f462。

- [ ] **步骤 1：固定四张输入图片**

图片覆盖竖版、多栏横版、空白页和密集/重叠视觉区域。每张图片在 oracle JSON 中记录 basename、width、height、color_mode 与 PNG SHA-256；不得记录生成时间或绝对路径。

- [ ] **步骤 2：写多输入 oracle RED 测试**

先扩展 python_parity.rs 读取 python_outputs.json，逐样本断言 tensor contract、detection 数量、class、label、score、bbox、order_seq 和 mask 输出 shape/dtype；Python oracle 自身记录 mask SHA-256 供 artifact 诊断。此时 fixture 尚未生成，测试必须失败。

- [ ] **步骤 3：扩展 reference_layout.py**

脚本接受可重复 --input 或 --input-dir，按 basename 排序。每个样本输出：

~~~json
{
  "input_sha256": "...",
  "tensor": {
    "dtype": "float32",
    "shape": [1, 3, 800, 800],
    "min": 0.0,
    "max": 1.0,
    "sha256": "..."
  },
  "image_size": [800.0, 800.0],
  "scale_factor": [1.25, 0.78125],
  "postprocess_profile": {
    "layout_nms": false,
    "layout_unclip_ratio": null,
    "layout_merge_bboxes_mode": null,
    "filter_overlap_boxes": false
  },
  "detections": []
}
~~~

scale_factor 数值仅示意字段类型，实际按每张输入图计算。使用官方 predictor/processors 与 PPDocLayoutV3PostProcess，不手写另一套近似后处理；显式关闭 NMS/unclip/merge/filter_overlap_boxes，保留 reference、inline_formula 与重叠候选，并输出过滤前固定的 source_detection_index。

- [ ] **步骤 4：锁定依赖并生成 oracle**

运行：

~~~bash
rtk uv lock --script scripts/reference_layout.py
rtk uv run scripts/reference_layout.py --model-dir models/pp-doclayout-v3 --input-dir crates/layout/tests/fixtures/model --output crates/layout/tests/fixtures/model/python_outputs.json
~~~

预期：paddleocr 精确为 3.6.0，lock 无浮动；重复生成 JSON byte-for-byte 一致。

- [ ] **步骤 5：运行 Rust/Python parity**

运行：

~~~bash
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
~~~

预期：class/label/order 完全相同；float score/bbox 使用 fixture 中声明的绝对与相对容差。

---

### 任务 D2：固化可复现的真实 PDF 语料 manifest

**文件：**
- 新建：tests/e2e-corpus.toml
- 修改：crates/core/tests/common/mod.rs
- 新建：crates/core/tests/common/e2e_manifest.rs
- 新建：crates/core/tests/e2e_manifest.rs

**接口：**
- 产出：精确 5 文档/110 页语料身份，以及可由多个 integration test 复用的测试侧 manifest loader。
- 输入：用户本机 `~/Downloads/` 顶层全部 PDF。

- [ ] **步骤 1：写 manifest 解析 RED 测试**

在 `tests/common/e2e_manifest.rs` 定义 E2eCorpusManifest/E2eDocument，在 `tests/e2e_manifest.rs` 通过 `mod common` 使用。测试 logical_id 唯一、basename 唯一、SHA-256 为 64 位小写十六进制、size/page_count 非零，并断言总文档数 5、总页数 110。测试还要证明目录中缺少或额外出现一个 PDF 时集合校验失败，确保所有 PDF 都会进入真实 E2E。

- [ ] **步骤 2：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test e2e_manifest
~~~

- [ ] **步骤 3：写入精确 manifest**

tests/e2e-corpus.toml 内容固定为：

~~~toml
schema_version = 1

[[documents]]
logical_id = "arxiv-2403-01632v4"
basename = "2403.01632v4.pdf"
sha256 = "8f6df134543b1ff7ca841b4bf266dfe16884ba000d1ee24800b3aae96dad9431"
size_bytes = 3111797
page_count = 47

[[documents]]
logical_id = "arxiv-2412-05210v1"
basename = "2412.05210v1.pdf"
sha256 = "b10bfd72684d588666635c853ba5dffbcc1e145a4486ced67baa55b0ac3f37b6"
size_bytes = 2792697
page_count = 12

[[documents]]
logical_id = "arxiv-2609-04180v1"
basename = "2609.04180v1.pdf"
sha256 = "1e7121f407cfa6a1a2793ffd5762fbc858a984cb6d626482f38738d354d55eb7"
size_bytes = 1218716
page_count = 23

[[documents]]
logical_id = "arxiv-2609-04184v1"
basename = "2609.04184v1.pdf"
sha256 = "7c5ebfb41043bb9755cdf616700a44bd9f55e4fd04fff74a69ad7bc1876b6728"
size_bytes = 867625
page_count = 8

[[documents]]
logical_id = "arxiv-2609-04203v1"
basename = "2609.04203v1.pdf"
sha256 = "f7fc412ae9d7ae792981505f22f7de0d2e32efd742ebba305bccc394ec0ba7c5"
size_bytes = 15514420
page_count = 20
~~~

- [ ] **步骤 4：实现 manifest 类型校验**

在 `tests/common/e2e_manifest.rs` 使用 TryFrom<RawE2eCorpusManifest>，按 logical_id 稳定排序，禁止 path traversal basename。`E2eDocument` 超过 3 个字段，必须派生 TypedBuilder 并通过 builder 构造。提供带英文函数级注释的测试侧接口：

~~~rust
pub(crate) struct VerifiedE2eDocument {
    pub(crate) manifest: E2eDocument,
    pub(crate) path: PathBuf,
}

pub(crate) fn load_manifest(path: &Path) -> Result<E2eCorpusManifest, E2eManifestError>;
pub(crate) fn verify_pdf_directory(
    manifest: &E2eCorpusManifest,
    pdf_dir: &Path,
) -> Result<Vec<VerifiedE2eDocument>, E2eManifestError>;
~~~

`VerifiedE2eDocument` 只组合已校验的 manifest 记录与本地路径，不参与 DocumentResult 序列化。`verify_pdf_directory` 只扫描目录顶层的常规文件，扩展名使用 ASCII 大小写不敏感的 `pdf` 比较；发现集合必须与 manifest basename 集合完全相等，并按 logical_id 返回已验证文档。共享类型只存在于 crate 级 `tests/common`，不进入公开 core API，也不在生产源码中增加 test-only 项。

- [ ] **步骤 5：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-core --test e2e_manifest
~~~

---

### 任务 D3：实现 uv E2E 驱动器与严格预检

**文件：**
- 新建：scripts/run_real_pdf_e2e.py
- 新建：scripts/run_real_pdf_e2e.py.lock
- 新建：crates/core/tests/python/run_real_pdf_e2e_test.py
- 修改：.gitignore

**接口：**
- 产出：经过验证的 E2E 环境、子进程状态、资源 metrics。
- 输入：--pdf-dir、--model-dir、tests/e2e-corpus.toml。

- [ ] **步骤 1：写预检 RED 测试**

覆盖缺文件、重复 basename、错误 size、错误 hash、错误页数、模型 manifest/hash 错误和目录中存在额外 PDF。runner 必须扫描目录顶层所有扩展名大小写不敏感的常规 PDF；发现集合与 manifest 不完全相等时失败并列出 missing/extra basename，任何 PDF 都不能被静默忽略。

- [ ] **步骤 2：写安全命令 RED 测试**

runner 使用 subprocess 参数数组，不拼接 shell；测试在临时目录创建带空格的 PDF basename，证明参数无需 shell quoting 也能正常传递。输出目录限制在显式 --output-dir，默认 target/docparse-e2e。--write-overlays 是显式开关，默认关闭。

- [ ] **步骤 3：实现 PEP 723 脚本**

PEP 723 声明 psutil 与 pypdf，uv lock 精确固定完整依赖；TOML 使用 Python 标准库 tomllib。runner 先枚举并排序 `--pdf-dir` 顶层的全部 PDF，验证集合相等后流式计算 SHA-256，并使用 pypdf 校验页数；随后调用模型 --verify-only，再生成临时 e2e config。所有新增 Python 函数同样写英文函数级 docstring。

- [ ] **步骤 4：生成 config 与环境**

临时配置通过 Figment 指向真实 inference.onnx/yml，include_diagnostics=true。只设置 DOCPARSE_E2E_PDF_DIR、DOCPARSE_E2E_MANIFEST、DOCPARSE_E2E_OUTPUT_DIR 和 DOCPARSE_E2E_CONFIG；不把绝对路径写进规范结果。

- [ ] **步骤 5：锁定并运行脚本测试**

运行：

~~~bash
rtk uv lock --script scripts/run_real_pdf_e2e.py
rtk uv run crates/core/tests/python/run_real_pdf_e2e_test.py
~~~

- [ ] **步骤 6：更新 ignore**

明确忽略 models/、target/docparse-e2e/ 和本地临时 config。tests/e2e-corpus.toml、Python lock 与小型 oracle 图片必须保持 tracked。

---

### 任务 D4：实现真实 Rust E2E harness 与 110 页机器不变量

**文件：**
- 复用：crates/core/tests/common/mod.rs
- 复用：crates/core/tests/common/e2e_manifest.rs
- 新建：crates/core/tests/real_pdfs.rs
- 修改：crates/core/Cargo.toml

**接口：**
- 产出：每文档规范 JSON、page diagnostics、机器不变量结果。
- 输入：真实 docparse-pdfium、PpDocLayoutV3Engine、ort Session、Figment、DocParser。

- [ ] **步骤 1：写 ignored harness 入口**

测试标记 #[ignore = "requires fixed PP-DocLayoutV3 model and local E2E corpus"]。四个环境变量缺任一项时明确失败并打印 runner 命令，不能伪装成 skipped success。

- [ ] **步骤 2：强制真实依赖路径**

通过 ConfigLoader 读取 runner 配置，并使用 DocParser::from_config；测试代码不得引用 fake LayoutEngine、reference detection JSON 或 OcrEngine 实现。启动时把 engine name/revision/manifest hash 写入非规范 summary。

- [ ] **步骤 3：实现逐文档预检**

Rust 侧复用 D2 的 `verify_pdf_directory`，再次检查目录 PDF 集合以及每份文件的 basename、size/hash/page_count，形成双重防线。文档按 manifest logical_id 排序运行，每页结果按 page_number 排序落盘。

- [ ] **步骤 4：实现逐页机器不变量**

每页至少断言：

- page number/size/rotation 合法，所有坐标有限且在明确容差内；
- ResultValidator 通过；
- 预扫描 Native TextItemId multiset 与最终扁平 Native TextItemId multiset 完全相等；
- 每个 TextItem 一个 Line owner，每个 Line 一个 Block owner；
- final_order 连续且与 Block Vec 顺序一致；
- model detection 漏盖的文字进入 fallback；
- InlineSpan range 合法且 Missing 不伪造内容；
- Page warnings/errors 与降级原因一致；
- DocumentResult JSON serialize/deserialize/validate round-trip。

- [ ] **步骤 5：写失败诊断**

断言失败正文包含 logical_id、page_number、node path 和 invariant 名，不打印整页文本。即使某页失败，也先安全释放 PDFium/ORT 资源再让测试退出。

- [ ] **步骤 6：运行单文档 smoke**

runner 支持 --only logical_id；smoke 例外地只运行 manifest 中指定文档，先选当前语料最小的 8 页 `arxiv-2609-04184v1`：

~~~bash
rtk uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 --only arxiv-2609-04184v1
~~~

预期：8 页真实 ORT 推理、融合、relations 和 JSON 全部通过；未选文档只允许在显式 smoke 模式跳过，完整门禁禁止使用 `--only`。

---

### 任务 D5：验证全量确定性并生成可比较汇总

**文件：**
- 修改：crates/core/tests/real_pdfs.rs
- 修改：scripts/run_real_pdf_e2e.py
- 新建：scripts/compare_e2e_runs.py
- 新建：crates/core/tests/python/compare_e2e_runs_test.py
- 新建：crates/core/tests/e2e_summary.rs

**接口：**
- 产出：canonical-hashes.json、summary.json、overlay-selection.json，以及串行/并发结果不一致时返回非零状态的比较器。
- 输入：两次全量 E2E 运行结果。

- [ ] **步骤 1：定义规范与易变输出边界**

canonical-hashes.json 只含 schema_version、corpus manifest SHA-256、模型 revision/hash、影响规范输出的配置指纹、logical_id、document result SHA-256、逐页 canonical SHA-256 和计数，必须可重复且不得含本地绝对路径。summary.json 可含 wall time、峰值 RSS 和环境信息，不参与相等断言。

- [ ] **步骤 2：写汇总 RED 测试**

断言汇总包含文件数、页数、Native/TextItem/Line/Block 数、模型/fallback Block 数、未覆盖文字比例、assignment conflict、removed edge、label、warning/error 计数。map 输出按 key 排序。

- [ ] **步骤 3：写运行比较 RED 测试**

`compare_e2e_runs_test.py` 在临时目录构造两份 canonical-hashes.json，覆盖完全相同、corpus/model/config 指纹不同、缺文档、缺页、document hash 不同和 page hash 不同。除完全相同外，比较器都必须返回非零状态，并打印首个稳定 mismatch path，不打印 PDF 文本。

- [ ] **步骤 4：实现资源监控**

runner 使用 psutil 采样 cargo/test 进程树的 RSS 并记录峰值；只记录，不设阈值。中断时终止子进程并保留已完成文档报告。最终 parallel run 在每份 DocumentResult 完成后重新串行渲染各页并生成 PNG/SVG overlay，不重跑 ONNX，也不同时持有多页位图；serial determinism run 不生成 overlay。

- [ ] **步骤 5：实现确定性页面抽样**

overlay-selection.json 包含每份文档首页、中间页、末页，并追加全局 fallback ratio、assignment conflict、removed edge 各最高的页面。去重后按 logical_id/page 排序；指标同分使用 logical_id/page tie-break。

- [ ] **步骤 6：实现显式结果比较器**

`compare_e2e_runs.py` 只使用 Python 标准库，接收两个 canonical-hashes.json 路径，先校验 schema 和 corpus/model/config 身份，再比较按 logical_id/page 排序后的全部 hash 与计数。完全相同时退出 0；任何差异、缺字段或重复 key 都退出非零。运行脚本测试：

~~~bash
rtk uv run crates/core/tests/python/compare_e2e_runs_test.py
~~~

- [ ] **步骤 7：运行两次完整 corpus**

第一次 page_concurrency=1，第二次 page_concurrency=4；两次均重新执行真实 ORT，不复用 detection cache。

运行：

~~~bash
rtk uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 --page-concurrency 1 --run-id serial
rtk uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 --page-concurrency 4 --run-id parallel --write-overlays
~~~

预期：两次均精确处理当前全部 5 份、110 页，所有机器不变量零失败。

- [ ] **步骤 8：强制比较两次结果**

运行：

~~~bash
rtk uv run scripts/compare_e2e_runs.py \
  target/docparse-e2e/serial/canonical-hashes.json \
  target/docparse-e2e/parallel/canonical-hashes.json
~~~

预期：退出状态为 0，并明确报告 5 documents、110 pages、0 canonical mismatches；该命令未成功前，确定性门禁不得通过。

---

### 任务 D6：生成 overlay 并完成视觉验收报告

**文件：**
- 新建：scripts/build_visual_review.py
- 新建：scripts/build_visual_review.py.lock
- 修改：scripts/run_real_pdf_e2e.py
- 修改：README.md

**接口：**
- 产出：target/docparse-e2e/visual-review/index.html、review.md、PNG/SVG overlay。
- 输入：overlay-selection.json、规范结果、页面渲染图。

- [ ] **步骤 1：写 selection 消费 RED 测试**

断言每个 logical_id 至少包含 first/middle/last；所有自动追加页面可追溯到 metric/rank；重复页面只生成一次 overlay。

- [ ] **步骤 2：实现静态 review 页面**

从 final parallel run 已生成的 overlay 中按文档/页码展示原页、模型 Region、最终 Block/order、Line baseline、fallback、inline formula、冲突与 removed edge。build_visual_review.py 只组装静态报告，不重新解析 PDF 或运行模型。HTML/Markdown 中所有 PDF 文本和 label 必须转义。

- [ ] **步骤 3：实现人工检查表**

每页检查：跨栏顺序、全宽标题、表格视觉顺序、caption 位置、inline formula 位置、页眉页脚、漏字、重复字和 order number。状态只能是 pass、fail 或 needs-investigation，并允许写最短说明。

- [ ] **步骤 4：锁定脚本并生成报告**

运行：

~~~bash
rtk uv lock --script scripts/build_visual_review.py
rtk uv run scripts/build_visual_review.py --run-dir target/docparse-e2e/parallel
~~~

- [ ] **步骤 5：完成人工视觉 gate**

逐项查看 selection 中全部页面。发现明显问题时先创建不含第三方 PDF 内容的最小合成回归 fixture，再回到 B/C 对应算法修复并重跑全量；不能只在真实 PDF 上加 basename/page 特判。

预期：review.md 无 fail；needs-investigation 有明确结论后才能通过 gate。

---

### 任务 D7：完成用户文档、来源声明与第三方许可证

**文件：**
- 修改：README.md
- 修改：NOTICE
- 修改：THIRD_PARTY_NOTICES.md
- 修改：crates/config/README.md
- 新建：crates/config/LICENSE
- 新建：crates/config/NOTICE
- 新建：crates/config/THIRD_PARTY_NOTICES.md
- 修改：crates/layout/README.md
- 新建：crates/layout/LICENSE
- 新建：crates/layout/NOTICE
- 新建：crates/layout/THIRD_PARTY_NOTICES.md
- 修改：crates/core/README.md
- 新建：crates/core/LICENSE
- 新建：crates/core/NOTICE
- 新建：crates/core/THIRD_PARTY_NOTICES.md
- 修改：crates/cli/README.md
- 新建：crates/cli/LICENSE
- 新建：crates/cli/NOTICE
- 新建：crates/cli/THIRD_PARTY_NOTICES.md

**接口：**
- 产出：可审查、可发布的 Apache-2.0 包内容。
- 输入：LiteParse 派生来源、PDFium、PP-DocLayoutV3、ort/ONNX Runtime 的许可证与固定来源。

- [ ] **步骤 1：更新 README**

记录 uv 模型下载、Figment 覆盖顺序、配置相对路径、async/blocking API、自定义 LayoutEngine/OcrEngine、CLI、输出 schema、首版非目标、模型不随 crate 分发，以及真实 E2E 固定扫描 `~/Downloads/` 顶层全部 PDF 的规则。README 必须说明目录集合与 manifest 不一致时预检失败、如何显式更新 manifest、完整门禁禁止 `--only`，并给出串行/并发运行和 `compare_e2e_runs.py` 比较命令。

- [ ] **步骤 2：固定来源与许可证声明**

NOTICE/THIRD_PARTY_NOTICES 记录：

- DocParse 基于/派生自 LiteParse 的具体文件与固定 commit，保留 Apache-2.0 notice；
- docparse-pdfium/docparse-pdfium-sys 的 PDFium 来源和适用许可证；
- PP-DocLayoutV3 repository/revision/hash/Apache-2.0；
- ort crate 与 ONNX Runtime 的版本、来源和许可证；
- geo crate 0.33.1 的 MIT OR Apache-2.0 许可证；
- Python oracle 依赖只用于开发/测试，不进入 Rust runtime 包。

- [ ] **步骤 3：做品牌词 allowlist 审计**

代码标识、crate/package/repository URL 和普通用户文案中不得残留 liteparse 品牌。LiteParse 只允许出现在 LICENSE、NOTICE、THIRD_PARTY_NOTICES、来源注释和说明“参考/派生来源”的文档段落。

运行：

~~~bash
rtk rg -n -i 'liteparse|run-llama/liteparse' --glob '!target/**' --glob '!Cargo.lock'
~~~

逐条审查命中，不能用全局替换破坏法定归属声明。

- [ ] **步骤 4：检查 metadata**

所有新 crate 的 license=Apache-2.0、repository=https://github.com/YageGeng/docparse.git、version 和 edition 从 workspace 继承；每个新 crate 显式设置本地 readme = "README.md"。依赖只在根 Cargo.toml 定义。

---

### 任务 D8：执行最终离线、真实与 package 门禁

**文件：**
- 修改：only files required by failures found below

**接口：**
- 产出：第一版验收证据。
- 输入：所有 阶段 结果。

- [ ] **步骤 1：格式、编译和离线测试**

运行：

~~~bash
rtk cargo fmt --all -- --check
rtk cargo check --locked --workspace --all-targets
rtk cargo test --locked --workspace
rtk cargo clippy --locked --workspace --all-targets -- -D warnings
~~~

随后核对匹配宿主 CI 中 cuda、coreml、openvino 三个互斥 feature 的独立 check 结果；不得用 --all-features 代替该矩阵。

- [ ] **步骤 2：模型合同与多样本 parity**

运行：

~~~bash
rtk cargo test -p docparse-layout --test model_contract -- --ignored --nocapture
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
~~~

- [ ] **步骤 3：全量真实 E2E**

运行：

~~~bash
rtk uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 --page-concurrency 1 --run-id final-serial
rtk uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 --page-concurrency 4 --run-id final-parallel --write-overlays
~~~

预期：两次均扫描并处理目录中全部 5 documents、110 pages，且为 0 invariant failures。

- [ ] **步骤 4：比较串行与并发规范结果**

运行：

~~~bash
rtk uv run scripts/compare_e2e_runs.py \
  target/docparse-e2e/final-serial/canonical-hashes.json \
  target/docparse-e2e/final-parallel/canonical-hashes.json
~~~

预期：身份字段和全部 document/page canonical hash 完全一致，退出状态为 0；差异必须阻断最终门禁。

- [ ] **步骤 5：视觉 gate**

运行：

~~~bash
rtk uv run scripts/build_visual_review.py --run-dir target/docparse-e2e/final-parallel
~~~

人工完成 review.md，要求 0 fail、0 unresolved needs-investigation。

- [ ] **步骤 6：pre-commit**

运行：

~~~bash
rtk pre-commit run --all-files
~~~

- [ ] **步骤 7：package 内容检查**

运行：

~~~bash
rtk cargo package -p docparse-config --allow-dirty --list
rtk cargo package -p docparse-layout --allow-dirty --list
rtk cargo package -p docparse-core --allow-dirty --list
rtk cargo package -p docparse-cli --allow-dirty --list
~~~

逐包确认 README、LICENSE、NOTICE、THIRD_PARTY_NOTICES 存在，且没有 ONNX、真实 PDF、E2E output、绝对本地路径。

- [ ] **步骤 8：最终工作树审计**

运行：

~~~bash
rtk git status --short
rtk git diff --check
rtk rg -n '/Volumes/Yage|/Users/|/home/isbest' --glob '!docs/superpowers/**' --glob '!target/**'
~~~

预期：只有计划内文件发生变化；发布源码和配置不含开发机绝对路径。

---

## 阶段 D 完成门禁

- Python/Rust 多样本前后处理 parity 通过。
- manifest 精确锁定 `~/Downloads/` 当前全部 5 份 PDF、110 页及其 hash/size/page count，目录与 manifest 集合完全相等。
- 串行和并发两次真实全量结果 canonical hash 一致。
- 所有 110 页文本守恒、唯一所有权、有限坐标和 JSON round-trip 通过。
- overlay 确定性抽样完成人工审查，0 未解决问题。
- README、来源声明、品牌 allowlist 与第三方许可证审计通过。
- 四个 crate 的 package 清单完整且不打包模型/真实 PDF。
- 全 workspace fmt/check/test/clippy/pre-commit 通过。

本计划不包含 commit。提交需用户单独授权。
