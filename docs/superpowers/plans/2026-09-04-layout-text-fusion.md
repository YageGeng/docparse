# DocParse 版面与文本融合总路线图

> 本文件是跨阶段总路线图，不直接作为逐步执行清单。实施时必须进入下列对应阶段计划，并在当前会话中使用 superpowers:executing-plans；禁止使用 SubAgent 或 worktree。

**目标：** 在现有 PDFium crates 上构建 Native Rust 文档解析器，使用 PP-DocLayoutV3、页内双轨融合和 LiteParse 风格启发式输出 Block -> Line -> TextItem，并通过 `~/Downloads/` 当前全部 5 份、110 页真实 PDF 端到端验收。

**架构：** PDFium 提供完整文本事实，docparse-layout 封装 ort 和模型检测，docparse-core 在冻结的 DocumentContext 下逐页独立融合。模型标签优先，残余文字使用 XY-cut 补全，文档级逻辑只生成旁路 DocumentRelations。

**技术栈：** Rust 2024、Tokio、Figment、serde、typed-builder、thiserror、ort 2.0.0-rc.13、ONNX Runtime 1.28、PDFium、Python/uv、Hugging Face Hub

**设计规范：** docs/superpowers/specs/2026-09-04-layout-text-fusion-design.md

**权威阶段计划：**

- 阶段 A：docs/superpowers/plans/2026-09-04-layout-text-fusion-phase-a-foundation.md
- 阶段 B：docs/superpowers/plans/2026-09-04-layout-text-fusion-phase-b-core-fusion.md
- 阶段 C：docs/superpowers/plans/2026-09-04-layout-text-fusion-phase-c-runtime-cli.md
- 阶段 D：docs/superpowers/plans/2026-09-04-layout-text-fusion-phase-d-e2e.md

## 全局约束

- 不使用 SubAgent 或 worktree；所有任务在当前会话内顺序执行。
- 未经用户再次明确允许不创建 commit。
- 计划、spec 和用户文档使用中文；源码注释使用英文。
- 每个新增函数必须有英文函数级注释；修改非平凡旧逻辑时必须用英文注释说明变动原因。
- 私有实现的测试只能放在对应源码文件或模块内声明为 `#[cfg(test)] mod tests`；crate 级 `tests/` 仅通过公开 API 测试，禁止为测试在生产代码中增加独立的 `#[cfg(test)]` 字段、函数、实现或支持模块。
- 第一版仅支持 Native Rust，不支持 WASM。
- 第一版不内置 OCR，只公开异步 OcrEngine trait。
- 第一版不恢复表格单元格，不识别公式内容。
- 每个有效 TextItem 恰好属于一个 Line，每个 Line 恰好属于一个 Block。
- Layout 失败或漏检不能删除已提取的 Native TextItem。
- 融合以 Page 为单位；PageAnalyzer 只读共享的 Arc<DocumentContext>。
- 所有超过 3 字段的结构使用 typed-builder，Option 字段使用 builder default；依赖版本和路径只在根 Cargo.toml 声明。
- 共享所有权克隆统一写 Arc::clone(&value)，不使用 value.clone() 隐藏 Arc 克隆。
- 日志使用 tracing 完整宏路径和正文格式参数，并覆盖第三方调用、关键分支及错误返回。
- 所有行为变更先写失败测试，再写最小实现。
- 默认测试必须离线；真实模型测试和真实 PDF E2E 显式运行。

---

## 文件结构

计划完成后的主要结构：

~~~text
docparse/
├── Cargo.toml
├── docparse.toml.example
├── scripts/
│   ├── build_visual_review.py
│   ├── compare_e2e_runs.py
│   ├── download_models.py
│   ├── reference_layout.py
│   └── run_real_pdf_e2e.py
├── tests/e2e-corpus.toml
├── crates/
│   ├── config/
│   │   ├── Cargo.toml
│   │   ├── src/{lib.rs,error.rs,loader.rs,types.rs,validate.rs}
│   │   └── tests/{loading.rs,validation.rs}
│   ├── layout/
│   │   ├── Cargo.toml
│   │   ├── examples/inspect_model.rs
│   │   ├── src/{lib.rs,engine.rs,error.rs,geometry.rs,types.rs}
│   │   ├── src/pp_doclayout_v3/{mod.rs,pool.rs,preprocess.rs,postprocess.rs,schema.rs,session.rs}
│   │   └── tests/{engine.rs,fixtures/model/,model_contract.rs,python_parity.rs}
│   ├── core/
│   │   ├── Cargo.toml
│   │   ├── src/{lib.rs,error.rs,parser.rs,page.rs,types.rs,validate.rs,ocr.rs,diagnostics.rs}
│   │   ├── src/runtime/{mod.rs,pdfium_executor.rs,pipeline.rs}
│   │   ├── src/extract/{mod.rs,text.rs,metadata.rs}
│   │   ├── src/context/{mod.rs,builder.rs,relations.rs}
│   │   ├── src/line/{mod.rs,assemble.rs,bidi.rs,metrics.rs}
│   │   ├── src/fusion/{mod.rs,assign.rs,fallback.rs,order.rs}
│   │   ├── src/semantic/{mod.rs,paragraph.rs,formula.rs,label_policy.rs}
│   │   ├── src/render/{mod.rs,json.rs,text.rs,markdown.rs,overlay.rs}
│   │   └── tests/{common/,degradation.rs,e2e_manifest.rs,e2e_summary.rs,ocr_trait.rs,parser.rs,parser_api.rs,pdfium_surface.rs,real_pdfs.rs,render.rs,types.rs}
│   ├── cli/
│   │   ├── Cargo.toml
│   │   ├── src/{args.rs,lib.rs,main.rs}
│   │   └── tests/{cli.rs,cli_with_fake.rs}
│   ├── pdfium/
│   └── pdfium-sys/
└── target/docparse-e2e/
~~~

---

### 任务 1：建立新 crate 与统一依赖

**文件：**
- 修改：Cargo.toml
- 新建：crates/config/Cargo.toml
- 新建：crates/config/README.md
- 新建：crates/config/src/lib.rs
- 新建：crates/layout/Cargo.toml
- 新建：crates/layout/README.md
- 新建：crates/layout/src/lib.rs
- 新建：crates/core/Cargo.toml
- 新建：crates/core/README.md
- 新建：crates/core/src/lib.rs
- 新建：crates/cli/Cargo.toml
- 新建：crates/cli/README.md
- 新建：crates/cli/src/main.rs

**接口：**
- 产出：docparse-config、docparse-layout、docparse-core、docparse-cli 四个可编译 crate。
- 输入：已有 docparse-pdfium 和 docparse-pdfium-sys。

- [ ] **步骤 1：在根 workspace 声明所有内部依赖**

根 Cargo.toml 的 workspace.dependencies 增加：

~~~toml
# workspace crates
docparse-config = { version = "0.1.0", path = "crates/config" }
docparse-core = { version = "0.1.0", path = "crates/core" }
docparse-layout = { version = "0.1.0", path = "crates/layout" }
pdfium = { package = "docparse-pdfium", version = "1.9.0", path = "crates/pdfium" }
pdfium-sys = { package = "docparse-pdfium-sys", version = "1.9.0", path = "crates/pdfium-sys" }

# error
thiserror = "2"

# async
async-trait = "0.1"
futures = "0.3"
tokio = { version = "1", features = ["fs", "macros", "rt", "rt-multi-thread", "sync", "time"] }

# config and serialization
figment = { version = "0.10", features = ["env", "toml"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
serde_path_to_error = "0.1"
serde_yml = "0.0.12"
typed-builder = "0.23"

# inference and image
geo = "0.33.1"
image = { version = "0.25", default-features = false, features = ["png"] }
ndarray = "0.17"
ort = { version = "=2.0.0-rc.13", default-features = false, features = ["api-28", "copy-dylibs", "download-binaries", "ndarray", "std", "tls-rustls", "tracing"] }
sha2 = "0.10"

# cli and diagnostics
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# testing
assert_cmd = "2"
predicates = "3"
proptest = "1"
tempfile = "3"
~~~

- [ ] **步骤 2：创建最小 crate manifests**

每个子 crate 的 version、edition、license、repository 使用 workspace 继承，依赖使用 workspace=true。四个新 crate 显式设置 readme = "README.md"，因为继承的 workspace readme 路径相对 workspace 根解析。docparse-cli 声明名称为 docparse-cli，并定义名称为 docparse 的 binary；docparse-core 依赖 config、layout、pdfium；layout 依赖 config、ort、image、geo、ndarray、serde_yml 和 sha2，但公共 API 不泄漏 geo/ort 类型；config 依赖 figment、serde、typed-builder。

docparse-layout features 定义 cuda、coreml、openvino，并分别转发到 ort 对应 feature。docparse-core features 定义 layout-cuda、layout-coreml、layout-openvino，再转发给 docparse-layout。默认 feature 只包含 CPU 所需能力；三个可选 EP 在第一版互斥，同时启用时 compile_error，因此不运行 --all-features。

- [ ] **步骤 3：删除无使用者的旧架构依赖**

用 rg 逐项确认后移除 wasm-bindgen-futures、arc-swap、nanoid 以及未被现有 PDFium/新 crate 使用的格式转换或 mime 依赖。保留 PDFium 实际引用的 build/dev dependency。

- [ ] **步骤 4：创建最小 lib 与 CLI 入口**

lib.rs 只包含 crate 级文档注释。四个 crate README 写最小职责与实施状态，保证 manifest 的本地 readme 路径有效。CLI main 暂时只返回 Result，并打印 clap 生成的帮助，不增加解析逻辑。

- [ ] **步骤 5：验证 workspace 拓扑**

运行：

~~~bash
rtk cargo metadata --no-deps --format-version 1
rtk cargo check --workspace
~~~

预期：六个 workspace crate 全部可解析并编译。

---

### 任务 2：实现 docparse-config

**文件：**
- 新建：crates/config/src/error.rs
- 新建：crates/config/src/types.rs
- 新建：crates/config/src/loader.rs
- 新建：crates/config/src/validate.rs
- 修改：crates/config/src/lib.rs
- 新建：crates/config/tests/loading.rs
- 新建：crates/config/tests/validation.rs
- 新建：docparse.toml.example

**接口：**
- 产出：ConfigLoader、RawConfig、ValidatedConfig、LayoutConfig、RuntimeConfig、RenderConfig、FusionConfig、OcrConfig、OutputConfig。
- 输入：Figment providers、配置文件路径、DOCPARSE_PROFILE 和 DOCPARSE_ 环境变量。

- [ ] **步骤 1：写配置加载失败测试**

在 loading.rs 写测试，创建临时 docparse.toml 和 docparse.dev.toml，设置隔离的 Figment Env provider，并断言优先级为默认值 < 主文件 < profile 文件 < env < 显式 override。测试还要断言相对 model_path、model_config_path、model_manifest_path 基于主配置目录解析。

关键断言：

~~~rust
assert_eq!(config.layout.score_threshold, 0.72);
assert_eq!(
    config.layout.model_path,
    config_dir.join("models/layout/inference.onnx")
);
~~~

- [ ] **步骤 2：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-config --test loading
~~~

预期：FAIL，因为 ConfigLoader 和配置类型尚不存在。

- [ ] **步骤 3：定义 RawConfig 与子配置**

在 types.rs 定义 serde Deserialize 原始配置，并在根/子配置拒绝未知字段，错误保留完整 key path。所有超过 3 字段的结构派生 TypedBuilder。ExecutionProviderConfig 使用 snake_case 枚举 Cpu、Cuda、CoreMl、OpenVino。所有具有安全默认值的字段使用 serde default。RuntimeConfig 默认值固定为 page_concurrency=4、render_queue_capacity=2、blocking_task_limit=4、continue_on_page_error=true。

- [ ] **步骤 4：实现 ConfigLoader**

loader.rs 提供：

~~~rust
pub struct ConfigLoader {
    config_path: PathBuf,
    profile: Option<String>,
    env_provider: Option<Figment>,
    explicit_overrides: Option<figment::value::Dict>,
}

impl ConfigLoader {
    pub fn new(config_path: impl Into<PathBuf>) -> Self;
    pub fn with_profile(self, profile: impl Into<String>) -> Self;
    pub fn with_env_provider(self, provider: Figment) -> Self;
    pub fn with_overrides(self, overrides: figment::value::Dict) -> Self;
    pub fn load(self) -> Result<ValidatedConfig, ConfigError>;
}
~~~

Figment 合并顺序必须与 spec 一致，环境变量使用 DOCPARSE_ 和双下划线嵌套。

- [ ] **步骤 5：写校验失败测试**

validation.rs 覆盖：非有限浮点、阈值不在 0 到 1、assignment 权重非负且总和偏离 1 超过 1e-6、session_pool_size 为 0、runtime 三个容量为 0、render_queue_capacity 大于 page_concurrency、DPI 为 0、max_long_edge_pixels 小于 800、主配置/profile 文件缺失、非法 profile 名和相对路径解析错误。profile 只允许 ASCII 字母数字/下划线/连字符，禁止路径分隔符和 ..。通用 ValidatedConfig 不检查模型文件是否存在。

- [ ] **步骤 6：实现 TryFrom 校验**

validate.rs 实现 TryFrom<RawConfig> for ValidatedConfig。ValidatedConfig 内路径为绝对 PathBuf；构造成功后不再需要重复检查基本范围。三个模型路径的文件存在性、manifest 来源/hash、YAML 合同和 ONNX schema 仅由 PpDocLayoutV3Engine::from_config 检查，使自定义/fake LayoutEngine 不依赖默认模型。

- [ ] **步骤 7：运行 config 全部测试**

运行：

~~~bash
rtk cargo test -p docparse-config
rtk cargo clippy -p docparse-config --all-targets -- -D warnings
~~~

预期：所有配置测试和 Clippy 通过。

---

### 任务 3：建立可复现模型下载与 manifest

**文件：**
- 新建：scripts/download_models.py
- 新建：scripts/download_models.py.lock
- 新建：crates/layout/src/model_manifest.rs
- 修改：.gitignore
- 修改：THIRD_PARTY_NOTICES.md

**接口：**
- 产出：inference.onnx、inference.yml、model-manifest.json。
- 输入：固定 Hugging Face revision 46bbdf188bb0a772c08aed74882ce7e51a8f1ea6。

- [ ] **步骤 1：写 model manifest 解析失败测试**

在 model_manifest.rs 末尾的唯一 `#[cfg(test)] mod tests` 中创建临时 JSON，直接测试私有校验逻辑并断言以下结构可以读取及校验，不扩大生产 API：

~~~rust
struct ModelManifest {
    repository: String,
    revision: String,
    files: BTreeMap<String, String>,
    license: String,
}
~~~

测试必须拒绝 revision 不一致、缺失文件和 hash 不一致。

- [ ] **步骤 2：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-layout --lib model_manifest::tests
~~~

预期：FAIL，因为 ModelManifest 尚未定义。

- [ ] **步骤 3：在 layout crate 实现 ModelManifest**

将与具体模型解耦的类型放入 model_manifest.rs，提供 load_and_verify(model_path, config_path, manifest_path)。使用流式读取计算 SHA-256，错误正文包含失败文件路径但不包含文件内容。pp_doclayout_v3/schema.rs 在 任务 5 复用该验证结果。

- [ ] **步骤 4：编写 uv PEP 723 下载脚本**

download_models.py 固定：

~~~text
repository = PaddlePaddle/PP-DocLayoutV3_onnx
revision = 46bbdf188bb0a772c08aed74882ce7e51a8f1ea6
inference.onnx = 45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba
inference.yml = 506fcfac13b3b546ae40d7886b44126420f392adb694e3f8bb6a6286a1f90fdc
~~~

脚本参数为 --output、--force、--verify-only。下载到同级临时目录，验证后使用 os.replace 原子安装。

- [ ] **步骤 5：生成 script lock 并验证命令**

运行：

~~~bash
rtk uv lock --script scripts/download_models.py
rtk uv run scripts/download_models.py --help
~~~

预期：lock 文件生成，帮助输出包含三个参数。

- [ ] **步骤 6：验证真实下载**

运行：

~~~bash
rtk uv run scripts/download_models.py --output models/pp-doclayout-v3
rtk uv run scripts/download_models.py --output models/pp-doclayout-v3 --verify-only
~~~

预期：两个文件和 manifest hash 全部通过。models 目录保持 gitignored。

---

### 任务 4：定义 LayoutEngine、检测类型与坐标变换

**文件：**
- 新建：crates/layout/src/error.rs
- 新建：crates/layout/src/types.rs
- 新建：crates/layout/src/geometry.rs
- 新建：crates/layout/src/engine.rs
- 修改：crates/layout/src/lib.rs
- 新建：crates/layout/tests/engine.rs

**接口：**
- 产出：LayoutEngine、LayoutRequest、LayoutDetection、LayoutLabel、PageImage、PageTransform。
- 输入：RGB 页面像素与 canonical viewport geometry。

- [ ] **步骤 1：写 PageTransform 往返失败测试**

测试 Letter、A4、90 度旋转页和非等比 800 x 800 resize。断言 viewport -> model -> viewport 每个坐标误差小于 0.01 point。

- [ ] **步骤 2：写 LayoutEngine fake 测试**

定义测试引擎返回两个 detection，断言 trait object 可通过 Arc<dyn LayoutEngine> 异步调用，Unknown 标签保留原始字符串。

- [ ] **步骤 3：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-layout --test engine
~~~

预期：FAIL，因为 trait 和类型尚不存在。

- [ ] **步骤 4：实现通用类型**

核心签名：

~~~rust
#[async_trait::async_trait]
pub trait LayoutEngine: Send + Sync {
    fn name(&self) -> &str;
    fn model_revision(&self) -> &str;
    async fn detect(
        &self,
        request: LayoutRequest,
    ) -> Result<Vec<LayoutDetection>, LayoutError>;
}
~~~

LayoutDetection 必须携带从原始输出行得到的 source_detection_index、raw_label、class_id、confidence、bbox、可选 polygon、model_order 和 engine_metadata。source_detection_index 在阈值/几何过滤前固定。

- [ ] **步骤 5：实现 PageTransform**

使用显式 affine 参数和 TryFrom 验证有限值。Polygon 通过 geo::Validation 校验，并以私有 geo 转换实现 bbox、area、intersection_area、含边界 contains_point 和 baseline_inside_length；退化、NaN 或自交 polygon 返回错误。

- [ ] **步骤 6：运行 layout 基础测试**

运行：

~~~bash
rtk cargo test -p docparse-layout --test engine
rtk cargo clippy -p docparse-layout --all-targets -- -D warnings
~~~

---

### 任务 5：捕获真实 ONNX schema 并实现 PP-DocLayoutV3 推理

**文件：**
- 新建：crates/layout/examples/inspect_model.rs
- 新建：crates/layout/tests/fixtures/model/pp_doclayout_v3_schema.json
- 新建：crates/layout/tests/fixtures/model/input.png
- 新建：crates/layout/tests/fixtures/model/python_output.json
- 新建：crates/layout/src/pp_doclayout_v3/mod.rs
- 新建：crates/layout/src/pp_doclayout_v3/pool.rs
- 新建：crates/layout/src/pp_doclayout_v3/schema.rs
- 新建：crates/layout/src/pp_doclayout_v3/preprocess.rs
- 新建：crates/layout/src/pp_doclayout_v3/postprocess.rs
- 新建：crates/layout/src/pp_doclayout_v3/session.rs
- 新建：crates/layout/tests/model_contract.rs
- 新建：crates/layout/tests/python_parity.rs
- 新建：scripts/reference_layout.py
- 新建：scripts/reference_layout.py.lock
- 修改：crates/layout/src/lib.rs

**接口：**
- 产出：PpDocLayoutV3Engine::from_config、真实 LayoutDetection。
- 输入：Validated LayoutConfig、模型文件和 PageImage。

- [ ] **步骤 1：先生成官方 Python oracle**

reference_layout.py 固定 paddleocr==3.6.0，并通过 uv lock 固定完整依赖。输出预处理 tensor 的 dtype、shape、min/max、SHA-256、image size、scale factor，以及 lossless fusion profile 下最终 bbox row、source_detection_index 和 order。

运行：

~~~bash
rtk uv lock --script scripts/reference_layout.py
rtk uv run scripts/reference_layout.py \
  --model-dir models/pp-doclayout-v3 \
  --input crates/layout/tests/fixtures/model/input.png \
  --output crates/layout/tests/fixtures/model/python_output.json
~~~

预期：fixture 在任何 Rust 预处理/后处理生产代码之前生成。

- [ ] **步骤 2：用真实模型导出 schema fixture**

使用独立 inspection example 打开模型，只打印 input/output 名称、元素类型、维度和模型 metadata。将结果保存为 pp_doclayout_v3_schema.json，不保存张量。

运行：

~~~bash
rtk cargo run -p docparse-layout --example inspect_model -- \
  models/pp-doclayout-v3/inference.onnx \
  crates/layout/tests/fixtures/model/pp_doclayout_v3_schema.json
~~~

预期：fixture 明确列出 image、scale_factor 和所有输出，不含动态时间字段。

- [ ] **步骤 3：写模型契约和 parity 失败测试**

model_contract.rs 对真实模型运行 Session introspection，断言与 fixture 完全一致；使用错误模型路径时断言返回 ModelNotFound；修改 fixture 维度时断言返回 UnsupportedModelSchema。python_parity.rs 在实现后比较 tensor hash、bbox、score、class 和 order_seq；Python fixture 记录 mask hash，Rust 只验证未消费 mask 的 dtype/shape。

- [ ] **步骤 4：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-layout --test model_contract -- --ignored --nocapture
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
~~~

预期：FAIL，因为真实 engine 尚未实现。

- [ ] **步骤 5：实现预处理**

preprocess.rs 固定到 PaddleX ffb64904d23708863ff5b8da312a5cbd52a7f462：RGB、关闭 optimized/IPP 且单线程的 OpenCV 4.10.0 generic CPU INTER_CUBIC 语义、800 x 800 resize、float32 乘 1/255、mean 0、std 1、HWC 转 NCHW。image size 固定 [800,800]，scale factor 为 [800/original_render_height,800/original_render_width]；实现首先通过 Python tensor hash fixture，不得直接假定 image crate 的 CatmullRom 等价。

- [ ] **步骤 6：实现 Session 初始化**

session.rs 使用 Session::builder、图优化和配置选择的 Execution Provider。校验 model manifest 与 schema 后再构建 Session。Session 不暴露出 crate。

- [ ] **步骤 7：实现 LayoutSessionPool**

pool.rs 创建 session_pool_size 个独立 Session，并用 Tokio semaphore 管理借用。PpDocLayoutV3Engine 内部持有 pool；docparse-core 只能看到 Arc<dyn LayoutEngine>，不能接触 ort Session。

- [ ] **步骤 8：解析固定版本导出后处理结果**

postprocess.rs 读取 bbox、bbox_num 和 mask。每个 bbox row 语义为 [class_id,score,xmin,ymin,xmax,ymax,order_seq]；order_seq 已由 ONNX 图内 global-pointer 后处理生成，固定 artifact 不导出 order_votes，Rust 不重复计算或伪造。bbox 已由图内后处理还原到原始渲染像素，Rust 只能继续转换为 viewport，不能再次做 800 x 800 逆缩放。外层使用严格 > 阈值、NumPy ties-to-even 取整和边界裁剪；lossless fusion profile 关闭 NMS/unclip/merge/filter_overlap_boxes。mask 仅校验 dtype/shape，第一版不近似生成 polygon；polygon 保持 None，矩形 quad 标记 DerivedFromBbox。

- [ ] **步骤 9：运行真实模型契约和 parity 测试**

运行：

~~~bash
rtk cargo test -p docparse-layout --test model_contract -- --ignored --nocapture
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
~~~

预期：schema、tensor hash、单页推理、bbox、score 和模型顺序均与固定 Python oracle 一致。

---

### 任务 6：定义 docparse-core 事实结果类型

**文件：**
- 新建：crates/core/src/types.rs
- 新建：crates/core/src/error.rs
- 修改：crates/core/src/lib.rs
- 新建：crates/core/tests/types.rs

**接口：**
- 产出：PdfInput、PageInput、稳定 ID newtypes、DocumentResult、PageResult、Block、Line、TextItem、InlineSpan、Evidence、DocumentRelations。
- 输入：docparse-layout 的 Polygon、LayoutLabel 和 EngineMetadata。

- [ ] **步骤 1：写嵌套结构与 round-trip 失败测试**

构造一个 Block、两个 Line、三个 TextItem，序列化再反序列化。断言数组顺序、raw_text、normalized_text、model_region_id、InlineSpan 和 schema_version 不变。

- [ ] **步骤 2：写唯一所有权校验失败测试**

测试重复 TextItemId、重复 LineId、空稳定 ID 和非有限 bbox 均被 ResultValidator 拒绝。

- [ ] **步骤 3：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test types
~~~

- [ ] **步骤 4：实现公共类型**

Block 直接拥有 Vec<Line>，Line 直接拥有 Vec<TextItem>。PageResult 不重复保存 lines/text_items，只提供 iter_lines 和 iter_text_items。Block.bbox 表示最终内容 union，SourceRegionEvidence 独立保留模型/fallback 原始 geometry，避免 Region 拆段后伪造子 Block polygon。TextItem 不含 Glyph，PDF object provenance 使用 Option<u32> text_object_index，禁止保存裸 handle。PdfInput 明确区分 Path 与 Arc<[u8]>；PageInput 表示调用方提供的单页像素和可选预提取 TextItem。

ID 固定使用 page/extraction index、page/source detection index、fallback XY-cut path 和 split/line ordinal；禁止随机 UUID、内存地址和 HashMap 迭代位置。schema_version 初始为 1.0，并按 spec 的 major/minor 规则校验。

- [ ] **步骤 5：实现 ResultValidator**

Validator 深度遍历文档，检查唯一 ID、有限 geometry、页码、顺序索引和父子 bbox 合理性。错误返回精确 node path。

- [ ] **步骤 6：运行类型测试**

运行：

~~~bash
rtk cargo test -p docparse-core --test types
~~~

---

### 任务 7：移植并扩展 PDFium 文本提取

**文件：**
- 新建：crates/core/src/extract/mod.rs
- 新建：crates/core/src/extract/text.rs
- 新建：crates/core/src/extract/metadata.rs
- 新建：crates/core/tests/fixtures/pdf/extraction_metadata.pdf
- 修改：crates/pdfium/src/font.rs
- 修改：crates/pdfium/src/page.rs
- 修改：crates/pdfium/src/text_page.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：ExtractedPage，包含 Native TextItem、页面尺寸、旋转、内容边界和轻量信号。
- 输入：docparse-pdfium Document/Page/TextPage。

- [ ] **步骤 1：写真实 PDF 提取失败测试**

在 crates/core/tests/fixtures/pdf 保存确定性的最小 PDF fixture，并在 text.rs/metadata.rs 各自唯一的 `#[cfg(test)] mod tests` 中直接测试私有提取逻辑。fixture 包含两行文本、字体变化、链接、旋转文字和 generated space；测试断言 TextItem 原文、bbox、font、matrix、char_codes、MCID、link、rotation 和 source。fixture 只包含合成内容，不使用 Downloads 中的真实 PDF。

- [ ] **步骤 2：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib extract::
~~~

- [ ] **步骤 3：移植 LiteParse SegmentBuilder**

从 LiteParse extract.rs 移植字符遍历、显式换行、Y 变化、回跳、大间距、dot leader、缺失空格恢复和连续片段构建。保留并更新 Apache 来源声明。

- [ ] **步骤 4：扩充 metadata**

metadata.rs 统一提取字体名、字号、font height/ascent/descent/weight/flags、text matrix、颜色、char codes、MCID、可选 text_object_index、Unicode mapping 状态、generated-space、link 和 strike。无法映射到稳定 page-object enumeration 时 text_object_index 为 None。

- [ ] **步骤 5：确保原始与规范化数据分离**

raw_text 和原始 geometry 永不修改。normalized_text、repair_actions 和 viewport geometry 独立保存。

- [ ] **步骤 6：运行提取测试与 PDFium 回归**

运行：

~~~bash
rtk cargo test -p docparse-core --lib extract::
rtk cargo test -p docparse-pdfium
~~~

---

### 任务 8：构建冻结 DocumentContext

**文件：**
- 新建：crates/core/src/context/mod.rs
- 新建：crates/core/src/context/builder.rs
- 新建：crates/core/src/context/relations.rs

**接口：**
- 产出：DocumentContextBuilder::build -> Arc<DocumentContext>，DocumentLinker::link -> DocumentRelations。
- 输入：Vec<PageProbe>、outline 和文档 metadata。

- [ ] **步骤 1：写上下文统计失败测试**

在 builder.rs 与 relations.rs 各自唯一的 `#[cfg(test)] mod tests` 中直接测试私有实现。构造三页 probe，包含重复顶部标题、页码、9pt 正文和 16pt 标题，断言 body_font_size、repeated_header、page_number_pattern 和 heading candidates；不为测试公开 builder/linker 内部细节。

- [ ] **步骤 2：写冻结与关系测试**

使用 Arc<DocumentContext> 并发读取，断言 API 不暴露 mutation。构造前页开放句和后页首段，断言 DocumentRelations 只保存 BlockRef，不改变 PageResult。

- [ ] **步骤 3：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib context::
~~~

- [ ] **步骤 4：实现 builder**

使用字符加权字号直方图、相对页面 Y、规范化文本和出现页比例识别全局信号。所有 HashMap tie-break 显式排序，避免随机迭代影响结果。

- [ ] **步骤 5：实现 DocumentLinker**

只生成 repeated chrome、paragraph continuation、heading relation 和 table continuation candidate。输入 PageResult 为不可变引用。

- [ ] **步骤 6：运行 context 测试**

运行：

~~~bash
rtk cargo test -p docparse-core --lib context::
~~~

---

### 任务 9：实现保守组行、Bidi 与局部 XY-cut

**文件：**
- 新建：crates/core/src/line/mod.rs
- 新建：crates/core/src/line/assemble.rs
- 新建：crates/core/src/line/bidi.rs
- 新建：crates/core/src/line/metrics.rs
- 新建：crates/core/src/fusion/fallback.rs

**接口：**
- 产出：Vec<LineFragment>、LineMetrics、RegionTree。
- 输入：Vec<TextItem> 和 FusionConfig。

- [ ] **步骤 1：写保守组行失败测试**

在 line 各源码文件唯一的 `#[cfg(test)] mod tests` 中直接覆盖同一行多个片段、双栏同 Y 不合并、字号异常 bbox、RTL 数字混排、竖排文字和旋转侧栏；私有算法不为测试提升可见性。

- [ ] **步骤 2：写 XY-cut 失败测试**

覆盖单栏不切、双栏左右顺序、全宽标题先水平切、表格/模型区域障碍物不被垂直切开。

- [ ] **步骤 3：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib line::
~~~

- [ ] **步骤 4：移植并收敛 LiteParse 组行算法**

assemble.rs 使用 Y band、垂直重叠、可信行高和水平 gap 生成 LineFragment。先拆后合，不在全页阶段跨大 gap 拼接。

- [ ] **步骤 5：实现 Bidi 与 Vertical**

强 RTL/LTR 字符投票决定 base direction；数字和标点中立。RTL 只反转 fragment 顺序。Vertical 按旋转和主轴排序。

- [ ] **步骤 6：移植 XY-cut**

fallback.rs 实现密度 valley、banner cut、column histogram、障碍物和稳定前序遍历。阈值来自 ValidatedConfig。

- [ ] **步骤 7：运行组行测试**

运行：

~~~bash
rtk cargo test -p docparse-core --lib line::
~~~

---

### 任务 10：实现模型映射、唯一归属和 residual fallback

**文件：**
- 新建：crates/core/src/fusion/mod.rs
- 新建：crates/core/src/fusion/assign.rs
- 修改：crates/core/src/fusion/fallback.rs

**接口：**
- 产出：Vec<BlockSeed>，每个 LineFragment 最多一个 primary owner。
- 输入：LayoutDetection、LineFragment、DocumentContext、FusionConfig。

- [ ] **步骤 1：写映射失败测试**

在 assign.rs 与 fallback.rs 各自唯一的 `#[cfg(test)] mod tests` 中直接测试私有实现，覆盖：完整覆盖、部分覆盖、两个重叠区域、中心点命中但面积低、面积命中但中心点越界、模型区域无文字、文字无模型区域。

- [ ] **步骤 2：写文本不丢失属性测试**

生成随机非重叠 TextItem 集合和随机检测框，融合后收集所有 TextItemId，断言与输入集合完全相等且无重复。

- [ ] **步骤 3：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::
~~~

- [ ] **步骤 4：实现 candidate score**

先应用 eligibility gate：coverage 至少 0.30，或中心点位于 Region 且 coverage 至少 0.10。AssignEvidence 保存 coverage、center_inside、baseline_intersection、model_confidence 和 specificity。assignment_score 精确使用 spec 中 0.55/0.20/0.10/0.10/0.05 权重。相同分数依次比较 coverage、confidence、较小区域、model_order 和 source_detection_index。label compatibility 只记录诊断，不参与评分。

- [ ] **步骤 5：实现唯一归属**

inline_formula detection 不参与整行 owner 竞争。其他 fragment 选择一个 primary BlockSeed，其余候选写 evidence。

- [ ] **步骤 6：实现 residual fallback**

未归属 fragment 运行 XY-cut；模型区域作为障碍物。fallback Block label_source 为 Fallback，raw_label 为空。

- [ ] **步骤 7：运行融合测试**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::
~~~

---

### 任务 11：实现 Block 内组行、段落拆分和 inline formula

**文件：**
- 新建：crates/core/src/semantic/mod.rs
- 新建：crates/core/src/semantic/paragraph.rs
- 新建：crates/core/src/semantic/formula.rs
- 新建：crates/core/src/semantic/label_policy.rs

**接口：**
- 产出：Vec<Block> 与 InlineSpan。
- 输入：Vec<BlockSeed>、LineMetrics、模型 label policy。

- [ ] **步骤 1：写模型 Region 一对多测试**

在 paragraph.rs、formula.rs 与 label_policy.rs 各自唯一的 `#[cfg(test)] mod tests` 中直接测试私有实现。一个 text detection 内构造两个紧密行和一个大间距段落，断言输出两个 Block，均继承同一 model_region_id 和模型 label。

- [ ] **步骤 2：写 paragraph_flow 测试**

覆盖真实/估算字号容差、Center mismatch、粗体切换、首行缩进、后续右缩进、1.5 倍行高、软连字符、中文句末和列表 hint。

- [ ] **步骤 3：写 inline_formula 测试**

覆盖 Complete、Partial、Missing、行内 X 插入顺序，以及没有匹配 Line 时生成独立 Formula Block。

- [ ] **步骤 4：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib semantic::
~~~

- [ ] **步骤 5：实现 label policy**

按 FlowText、Title、Atomic、Formula、Chrome、Structured 分类。主 label 永远继承模型；heuristic 只写 semantic_hints。

- [ ] **步骤 6：实现 paragraph_flow**

移植 LiteParse 核心规则并补充中文、RTL 和模型 Region 边界。软连字符只影响 Line/Block 派生 text。

- [ ] **步骤 7：实现 inline formula**

根据垂直 overlap、baseline 距离和 X 位置挂载 InlineSpan。没有原生文字时使用 Missing，不伪造公式。

- [ ] **步骤 8：运行 semantic 测试**

运行：

~~~bash
rtk cargo test -p docparse-core --lib semantic::
~~~

---

### 任务 12：实现页内阅读顺序 DAG

**文件：**
- 新建：crates/core/src/fusion/order.rs

**接口：**
- 产出：final_order 已填充且按阅读序排列的 Vec<Block>。
- 输入：模型 order、几何关系、XY-cut path 和 edge confidence。

- [ ] **步骤 1：写稳定拓扑排序失败测试**

在 order.rs 末尾唯一的 `#[cfg(test)] mod tests` 中直接测试私有 OrderGraph，覆盖双栏、全宽标题加双栏、模型漏块、fallback 插入、模型顺序与几何冲突、三节点环、相同坐标 tie-break；不为测试公开 OrderGraph。

- [ ] **步骤 2：写并发确定性测试**

将同一 PageInput 以不同 detection 输入排列重复运行 100 次，断言包含 diagnostics 的规范 JSON 完全相同；耗时等易变数据不得进入 DocumentResult。

- [ ] **步骤 3：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::order::tests
~~~

- [ ] **步骤 4：实现 OrderGraph**

OrderEdge 保存 from、to、source、source confidence、preservation_weight 和 reason。模型只连接按 (order_seq,source_detection_index) 排序后的相邻 Region；拆分子 Block 先建立 IntraRegion 边。各边使用 spec 固定权重。

- [ ] **步骤 5：实现消环**

检测强连通分量，按 preservation_weight、source 删除优先级和稳定 edge key 删边；若只剩 StrongVertical/IntraRegion 环则返回 InternalOrderConflict。记录 removed_edges。

- [ ] **步骤 6：实现稳定拓扑排序**

零入度节点优先队列 key 为 XY-cut path、floor(block_bbox.top/0.5pt)、Y、X、source priority、stable BlockId。写回 final_order。

- [ ] **步骤 7：运行 ordering 测试**

运行：

~~~bash
rtk cargo test -p docparse-core --lib fusion::order::tests
~~~

---

### 任务 13：定义无内置实现的 OcrEngine

**文件：**
- 新建：crates/core/src/ocr.rs
- 新建：crates/core/tests/ocr_trait.rs
- 修改：crates/core/src/lib.rs

**接口：**
- 产出：OcrEngine、OcrRequest、OcrResult、OcrRegion、OcrContentStatus。
- 输入：PageImage、PageTransform 和缺失区域。

- [ ] **步骤 1：写 trait object 失败测试**

Fake OcrEngine 异步返回两个结果，断言 Arc<dyn OcrEngine> 可注入；未注入时缺失区域状态为 OcrUnavailable。

- [ ] **步骤 2：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test ocr_trait
~~~

- [ ] **步骤 3：实现异步 trait**

OcrEngine 使用 async_trait，输入只包含图像、DPI、PageTransform、page number、缺失区域和原生覆盖统计。输出只包含文字与几何，不允许直接构造 Block。

- [ ] **步骤 4：实现 OCR 合并边界**

Native 可用文字优先；OCR 与 Native 重叠时只保留 Native。OCR 失败生成区域 warning，不删除已有内容。

- [ ] **步骤 5：运行测试**

运行：

~~~bash
rtk cargo test -p docparse-core --test ocr_trait
~~~

---

### 任务 14：实现 Tokio DocParser 与受控运行时

**文件：**
- 新建：crates/core/src/runtime/mod.rs
- 新建：crates/core/src/runtime/pdfium_executor.rs
- 新建：crates/core/src/runtime/pipeline.rs
- 新建：crates/core/src/parser.rs
- 修改：crates/core/src/lib.rs
- 新建：crates/core/tests/parser.rs
- 新建：crates/core/tests/common/mod.rs

**接口：**
- 产出：DocParser、DocParserBuilder、parse_path、parse_bytes、parse_page、parse_path_blocking。
- 输入：ValidatedConfig、LayoutEngine、可选 OcrEngine、PDF bytes/path。

- [ ] **步骤 1：写 async parser 失败测试**

使用真实 PDFium fixture 和 fake LayoutEngine，断言预扫描、冻结 context、页内并发、按页码汇总和 DocumentRelations。

- [ ] **步骤 2：写降级与阻塞包装测试**

Fake LayoutEngine 返回错误，断言 PageResult 使用纯几何 fallback。Tokio runtime 内调用 parse_path_blocking，断言 BlockingInsideRuntime。

- [ ] **步骤 3：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test parser
~~~

- [ ] **步骤 4：实现 PdfiumExecutor**

所有 PDFium FFI 通过单一受控 spawn_blocking 边界串行执行。预扫描只保留 TextItem 和 probe，不保留位图。

- [ ] **步骤 5：调用已封装 Session Pool 的 LayoutEngine**

ParseRuntime 持有 Arc<dyn LayoutEngine>，不持有 ort 类型。内置 PpDocLayoutV3Engine 在 docparse-layout 内部借用 Session；页面图像经有界 channel 进入 detect，队列容量不超过 page concurrency。

- [ ] **步骤 6：实现三阶段 parser**

parse_document 依次执行预扫描、DocumentContext freeze、并发 PageAnalyzer、按页码汇总、DocumentLinker。每页错误遵守 continue_on_page_error。

- [ ] **步骤 7：实现单页与 blocking 语义**

公开 parse_page 构建单页 context；内部 analyze_page 接受整本文档 Arc<DocumentContext>。blocking wrapper 检测已有 Tokio runtime。

- [ ] **步骤 8：运行 parser 测试**

运行：

~~~bash
rtk cargo test -p docparse-core --test parser
~~~

---

### 任务 15：实现 JSON、Raw/Semantic 渲染和诊断 overlay

**文件：**
- 新建：crates/core/src/render/mod.rs
- 新建：crates/core/src/render/json.rs
- 新建：crates/core/src/render/text.rs
- 新建：crates/core/src/render/markdown.rs
- 新建：crates/core/src/render/overlay.rs
- 新建：crates/core/tests/render.rs

**接口：**
- 产出：JsonRenderer、TextRenderer、MarkdownRenderer、OverlayRenderer。
- 输入：不可变 DocumentResult。

- [ ] **步骤 1：写渲染失败测试**

构造含重复页眉、跨页 continuation、inline formula Missing、RTL 和 fallback Block 的 DocumentResult。断言 Raw 保留全部，Semantic 只在输出层隐藏 chrome/连接段落。

- [ ] **步骤 2：写不可变测试**

渲染前后序列化 DocumentResult，断言完全相同。

- [ ] **步骤 3：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-core --test render
~~~

- [ ] **步骤 4：实现 renderer**

JSON 直接序列化规范结构。Text/Markdown 遍历 Block -> Line -> TextItem。Formula Missing 根据配置输出 formula_placeholder。

- [ ] **步骤 5：实现 overlay**

在页面截图上绘制模型 detection、最终 Block、order、Line baseline、fallback 和冲突。输出文件路径由调用方传入，core 不决定目录。

- [ ] **步骤 6：运行 render 测试**

运行：

~~~bash
rtk cargo test -p docparse-core --test render
~~~

---

### 任务 16：实现 docparse CLI

**文件：**
- 修改：crates/cli/src/main.rs
- 新建：crates/cli/src/args.rs
- 新建：crates/cli/tests/cli.rs

**接口：**
- 产出：docparse parse、docparse inspect-model。
- 输入：ConfigLoader、DocParser 和 renderers。

- [ ] **步骤 1：写 CLI 失败测试**

使用 assert_cmd 风格集成测试覆盖 --help、缺失模型错误、JSON 文件输出、raw/semantic view 和 inspect-model schema 摘要。

- [ ] **步骤 2：运行测试确认 RED**

运行：

~~~bash
rtk cargo test -p docparse-cli --test cli
~~~

- [ ] **步骤 3：实现 clap 参数**

parse 参数包含 input、可选 config、profile、format、view、output、continue-on-page-error、overlay-dir。inspect-model 接受可选 config 和 profile。省略 config 时只读取当前目录 ./docparse.toml，不向父目录搜索。首版不提供页面范围筛选，避免只分析部分页面时破坏 DocumentContext 语义。

- [ ] **步骤 4：实现 Tokio main**

main 初始化 tracing，加载配置，构建 parser，调用 async API。所有错误在返回前使用正文日志并保留 source chain。

- [ ] **步骤 5：运行 CLI 测试**

运行：

~~~bash
rtk cargo test -p docparse-cli --test cli
~~~

---

### 任务 17：扩展 Python/Rust 多样本模型一致性回归

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
- 产出：固定参考 detection 集。
- 输入：同一模型 revision 和固定的竖版、横版、空白、密集重叠四张输入图片。

- [ ] **步骤 1：编写参考脚本**

复用任务 5 已固定的 paddleocr==3.6.0 oracle，再增加横版、竖版、空白页和含重叠区域的样本。输出包含 tensor contract、class_id、label、score、coordinate、source_detection_index、order_seq、mask shape/dtype/SHA-256 和 lossless postprocess profile。

运行：

~~~bash
rtk uv lock --script scripts/reference_layout.py
~~~

- [ ] **步骤 2：生成 Python fixture**

运行：

~~~bash
rtk uv run scripts/reference_layout.py \
  --model-dir models/pp-doclayout-v3 \
  --input-dir crates/layout/tests/fixtures/model \
  --output crates/layout/tests/fixtures/model/python_outputs.json
~~~

- [ ] **步骤 3：写 Rust parity 失败测试**

使用 PpDocLayoutV3Engine 推理同一图片，断言 detection 数量和 label/order 完全相同，score 与坐标在明确容差内。

- [ ] **步骤 4：运行并修正后处理**

运行：

~~~bash
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
~~~

预期：Rust 与 Python 官方流水线一致。

---

### 任务 18：实现真实 PDF E2E 与视觉抽样

**文件：**
- 新建：crates/core/tests/real_pdfs.rs
- 修改：crates/core/tests/common/mod.rs
- 新建：crates/core/tests/common/e2e_manifest.rs
- 新建：tests/e2e-corpus.toml
- 新建：scripts/run_real_pdf_e2e.py
- 新建：scripts/run_real_pdf_e2e.py.lock
- 新建：scripts/compare_e2e_runs.py
- 修改：README.md

**接口：**
- 产出：target/docparse-e2e/summary.json、页面 overlay 和 E2E 退出状态。
- 输入：DOCPARSE_E2E_PDF_DIR、真实模型、真实 PDF。

- [ ] **步骤 1：写 E2E harness**

real_pdfs.rs 使用 ignore 属性标记真实测试，并在通过 --ignored 显式运行时读取 DOCPARSE_E2E_PDF_DIR。环境变量缺失时返回带设置示例的测试失败，不能把配置错误伪装成成功跳过。

- [ ] **步骤 2：实现全部机器不变量**

每页调用 ResultValidator，并额外断言页数、无任务丢失、Native TextItem 集合守恒、fallback 在漏检页出现、JSON round-trip 和重复运行一致。

- [ ] **步骤 3：实现汇总与页面挑选**

summary.json 记录文件/页数、耗时、峰值内存、模型与 fallback Block 数、未覆盖率、冲突边、消环和 warnings。挑选每份首页/中间/末页，以及全局 fallback、冲突、消环最高页。

- [ ] **步骤 4：实现 E2E 驱动脚本**

run_real_pdf_e2e.py 校验模型，扫描 `~/Downloads/` 顶层所有 PDF，并要求发现集合与 tests/e2e-corpus.toml 完全相等，再逐项校验 basename、SHA-256、文件大小和页数，设置环境变量，运行 cargo test 并读取 summary。缺失或额外 PDF 都必须失败，不能静默忽略；语料变化必须显式更新已跟踪 manifest 并经过审查。

运行：

~~~bash
rtk uv lock --script scripts/run_real_pdf_e2e.py
~~~

- [ ] **步骤 5：运行真实 E2E**

运行：

~~~bash
rtk uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 --page-concurrency 1 --run-id serial
rtk uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 --page-concurrency 4 --run-id parallel --write-overlays
~~~

预期：两次都处理目录中全部 5 份 PDF、110 页，机器不变量零失败。

- [ ] **步骤 6：比较串行与并发结果**

运行：

~~~bash
rtk uv run scripts/compare_e2e_runs.py \
  target/docparse-e2e/serial/canonical-hashes.json \
  target/docparse-e2e/parallel/canonical-hashes.json
~~~

预期：corpus/model/config 身份和全部 document/page canonical hash 相同，比较器退出 0；任何差异阻断门禁。

- [ ] **步骤 7：视觉检查 overlay**

打开自动挑选的 overlay，逐页检查跨栏、标题、表格、inline formula、页眉页脚、漏字、重复字和 order number。任何明显错误先增加最小回归 fixture，再修复对应算法。

---

### 任务 19：文档、许可证与最终验证

**文件：**
- 修改：README.md
- 修改：THIRD_PARTY_NOTICES.md
- 修改：crates/layout/README.md
- 新建：crates/layout/LICENSE
- 新建：crates/layout/NOTICE
- 新建：crates/layout/THIRD_PARTY_NOTICES.md
- 修改：crates/core/README.md
- 新建：crates/core/LICENSE
- 新建：crates/core/NOTICE
- 新建：crates/core/THIRD_PARTY_NOTICES.md
- 修改：crates/config/README.md
- 新建：crates/config/LICENSE
- 新建：crates/config/NOTICE
- 新建：crates/config/THIRD_PARTY_NOTICES.md
- 修改：crates/cli/README.md
- 新建：crates/cli/LICENSE
- 新建：crates/cli/NOTICE
- 新建：crates/cli/THIRD_PARTY_NOTICES.md

**接口：**
- 产出：可发布且可复现的第一版。
- 输入：全部前置任务。

- [ ] **步骤 1：更新用户文档**

README 记录 uv 下载、Figment 优先级、async API、CLI、OCR 注入、首版非目标、模型许可证，以及真实 E2E 扫描 `~/Downloads/` 顶层全部 PDF、严格匹配 manifest、分别运行串行/并发并显式比较 canonical hash 的命令。

- [ ] **步骤 2：更新第三方许可证**

记录 PP-DocLayoutV3 Apache-2.0、ort 和 ONNX Runtime 许可证及固定来源。将根 LICENSE、NOTICE 和适用的 THIRD_PARTY_NOTICES 原样或按适用范围复制到每个新 crate，使 crate package 清单包含 LICENSE、NOTICE、README 和 THIRD_PARTY_NOTICES。

- [ ] **步骤 3：运行格式和 workspace 编译**

运行：

~~~bash
rtk cargo fmt --all -- --check
rtk cargo check --locked --workspace --all-targets
~~~

- [ ] **步骤 4：运行全部离线测试与 Clippy**

运行：

~~~bash
rtk cargo test --locked --workspace
rtk cargo clippy --locked --workspace --all-targets -- -D warnings
~~~

- [ ] **步骤 5：运行模型一致性**

运行：

~~~bash
rtk cargo test -p docparse-layout --test model_contract -- --ignored --nocapture
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
~~~

- [ ] **步骤 6：运行真实 110 页 E2E 并比较确定性**

运行：

~~~bash
rtk uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 --page-concurrency 1 --run-id final-serial
rtk uv run scripts/run_real_pdf_e2e.py --pdf-dir ~/Downloads --model-dir models/pp-doclayout-v3 --page-concurrency 4 --run-id final-parallel --write-overlays
rtk uv run scripts/compare_e2e_runs.py target/docparse-e2e/final-serial/canonical-hashes.json target/docparse-e2e/final-parallel/canonical-hashes.json
~~~

- [ ] **步骤 7：运行 pre-commit**

运行：

~~~bash
rtk pre-commit run --all-files
~~~

- [ ] **步骤 8：检查 package 内容与工作树**

运行：

~~~bash
rtk cargo package -p docparse-config --allow-dirty --list
rtk cargo package -p docparse-layout --allow-dirty --list
rtk cargo package -p docparse-core --allow-dirty --list
rtk cargo package -p docparse-cli --allow-dirty --list
rtk git status --short
~~~

预期：所有验证通过，真实 E2E 覆盖 `~/Downloads/` 当前全部 5 份/110 页且串并行结果相同，package 包含许可证，工作树只包含本实施计划范围内的改动。

---

## 执行顺序与审查门

- 阶段 A，基础：任务 1-5。门禁为配置测试、模型 manifest、真实 schema 和单页 inference。
- 阶段 B，事实与融合：任务 6-13。门禁为唯一所有权、文本守恒和稳定阅读序。
- 阶段 C，运行与输出：任务 14-16。门禁为多页 async、降级、renderer 不可变和 CLI。
- 阶段 D，真实验收：任务 17-19。门禁为 Python parity、`~/Downloads/` 当前全部 5 份/110 页 E2E、串并行显式比较和视觉检查。

每个 任务 完成后先运行该 任务 指定测试，再运行受影响 crate 的完整测试。发现失败时停在当前 任务，不叠加后续改动。

本计划不包含 git commit 步骤。若用户之后授权提交，应在提交前重新读取 .gitmessage、运行 pre-commit，并按用户指定范围暂存。
