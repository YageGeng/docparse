# DocParse 阶段 A：基础与模型合同实施计划

> **供当前会话执行者使用：** 必须使用 superpowers:executing-plans，按任务逐项执行本计划。禁止使用 SubAgent 或 worktree；步骤使用复选框（- [ ]）跟踪。

**目标：** 建立可编译的 crate 边界、实例级 Figment 配置、可复现模型资产，以及经 Python oracle 验证的 PP-DocLayoutV3 Rust 推理。

**架构：** docparse-config 只负责类型、覆盖和范围校验；docparse-layout 拥有模型 artifact 校验、ort Session Pool 和 PP-DocLayoutV3 前后处理。Python oracle 必须先于 Rust 算法实现产生。

**技术栈：** Rust 2024、Figment、serde、typed-builder、Tokio、ort 2.0.0-rc.13、image、ndarray、Python 3、uv、PaddleOCR 3.6.0

**设计规范：** docs/superpowers/specs/2026-09-04-layout-text-fusion-design.md

## 全局约束

- 不使用 SubAgent 或 worktree，不创建 commit。
- 每个新增函数必须有英文函数级注释；修改非平凡旧逻辑时必须用英文注释说明变动原因。
- 私有实现的测试只能放在对应源码文件或模块内声明为 `#[cfg(test)] mod tests`；crate 级 `tests/` 仅通过公开 API 测试，禁止为测试在生产代码中增加独立的 `#[cfg(test)]` 字段、函数、实现或支持模块。
- 所有超过 3 字段的 struct 使用 typed-builder；Option 字段使用 builder default，并按调用方类型决定 strip_option。
- 共享所有权克隆统一写 Arc::clone(&value)。
- 依赖统一声明在根 workspace.dependencies。
- 默认 CPU；EP 通过 Cargo feature 转发。
- 通用配置不得要求默认模型文件存在。
- 模型实现固定 PaddleX commit ffb64904d23708863ff5b8da312a5cbd52a7f462。
- 先生成 Python oracle，再写 Rust 前后处理。

---

### 任务 A1：创建 workspace crate 骨架

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
- 产出：四个新 crate 可被 Cargo metadata 识别。
- 输入：已有 pdfium/pdfium-sys。

- [ ] **步骤 1：在根 Cargo.toml 加内部 crate path/version**

加入 docparse-config、docparse-layout、docparse-core、pdfium，并保留现有 pdfium-sys alias。按 workspace crates 分组排序。

- [ ] **步骤 2：在根 Cargo.toml 加外部依赖**

加入 async-trait、figment、serde、serde_json、serde_path_to_error、serde_yml、typed-builder、tokio、ort、ndarray、image、geo=0.33.1、sha2、clap、tracing、assert_cmd、predicates、proptest。ort 精确使用 =2.0.0-rc.13，关闭默认 feature，显式启用 api-28、std、ndarray、tracing、download-binaries、copy-dylibs 和唯一 TLS 实现 tls-rustls；`download-binaries` 未搭配 TLS feature 时必须在编译期失败。

- [ ] **步骤 3：清理旧架构遗留 workspace 依赖**

先用 rg 确认每个 workspace dependency 的实际使用者，再删除无使用者。至少移除 wasm-bindgen-futures（首版无 WASM）、arc-swap（配置不是全局热更新）、nanoid（结果 ID 禁止随机）以及未被现有 PDFium 或新 crate 使用的格式转换/mime 依赖。保留 docparse-pdfium 实际需要的 build/dev dependencies，不能按名称批量删除。

- [ ] **步骤 4：创建四个最小 manifest**

所有依赖使用 workspace=true；version、edition、license、repository 继承 workspace.package。Cargo 对继承的 readme 路径按 workspace 根解析，因此四个新 crate 显式写 readme = "README.md"，对应文件在阶段 D 补齐。layout 依赖 geo，但公共 API 只暴露自有 geometry 类型。layout features 为 cuda/coreml/openvino；core features 为 layout-cuda/layout-coreml/layout-openvino 并转发。默认只启用 CPU。首版三个可选 EP 互斥，任意组合同时启用时 compile_error；因此禁止把 --all-features 当作验证命令。

- [ ] **步骤 5：创建最小入口**

每个 lib.rs 写 crate 级英文文档；每个 README 先写 crate 名称、职责和“实施中”状态，确保 manifest 路径从第一次 cargo check 起有效。CLI main 使用 Tokio 和 Result 返回，不加入解析行为。

- [ ] **步骤 6：验证 Cargo 拓扑**

运行：

~~~bash
rtk cargo metadata --no-deps --format-version 1
rtk cargo check --workspace
~~~

预期：六个 crate 全部可编译，Cargo 无 unused manifest key。

---

### 任务 A2：定义配置类型与覆盖顺序

**文件：**
- 新建：crates/config/src/error.rs
- 新建：crates/config/src/types.rs
- 新建：crates/config/src/loader.rs
- 修改：crates/config/src/lib.rs
- 新建：crates/config/tests/loading.rs
- 新建：docparse.toml.example

**接口：**
- 产出：RawConfig、ConfigLoader、LayoutConfig、RuntimeConfig、RenderConfig、FusionConfig、OcrConfig、OutputConfig。
- 输入：docparse.toml、profile 文件、DOCPARSE_ env 和显式 override。

- [ ] **步骤 1：写主文件/profile 覆盖测试**

在临时目录写 docparse.toml 与 docparse.dev.toml。断言 profile 值覆盖主文件，未设置 profile 时不读取 profile 文件；显式 with_profile 优先于 DOCPARSE_PROFILE，后者只选文件而不进入 RawConfig。model_path、model_config_path、model_manifest_path 都按主配置目录解析。

- [ ] **步骤 2：写缺失文件测试**

断言主文件缺失返回 ConfigFileNotFound；设置 dev profile 但 profile 文件缺失返回 ProfileFileNotFound；空 profile、../dev 和包含路径分隔符的 profile 返回 InvalidProfileName，不能读取配置目录之外的文件。

- [ ] **步骤 3：写 env/override 测试**

通过 ConfigLoader 可注入的 Figment Provider 提供隔离 env 数据，避免修改进程环境。断言 env 覆盖 profile、显式 Dict 覆盖 env；任一层出现未知字段时返回包含完整 key path 的错误。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-config --test loading
~~~

预期：ConfigLoader 尚不存在导致失败。

- [ ] **步骤 5：定义配置结构**

RawConfig 直接 Deserialize，并在根与所有子配置使用 serde deny_unknown_fields。每个超过 3 字段的结构派生 TypedBuilder；Option 字段 builder default。ExecutionProviderConfig 使用 Cpu/Cuda/CoreMl/OpenVino，OcrPolicy 只含 Disabled/MissingRegions。RuntimeConfig 默认值固定为 page_concurrency=4、render_queue_capacity=2、blocking_task_limit=4、continue_on_page_error=true。

- [ ] **步骤 6：实现 ConfigLoader**

公开接口：

~~~rust
pub struct ConfigLoader {
    config_path: PathBuf,
    profile: Option<String>,
    env_provider: Option<Figment>,
    explicit_overrides: Option<Dict>,
}

impl ConfigLoader {
    pub fn new(path: impl Into<PathBuf>) -> Self;
    pub fn with_profile(self, profile: impl Into<String>) -> Self;
    pub fn with_env_provider(self, provider: Figment) -> Self;
    pub fn with_overrides(self, values: Dict) -> Self;
    pub fn load_raw(self) -> Result<RawConfig, ConfigError>;
}
~~~

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-config --test loading
~~~

---

### 任务 A3：校验配置但不绑定模型 artifact

**文件：**
- 新建：crates/config/src/validate.rs
- 新建：crates/config/tests/validation.rs
- 修改：crates/config/src/lib.rs

**接口：**
- 产出：ValidatedConfig、TryFrom<RawConfig>。
- 输入：已解析 RawConfig 和配置文件 base directory。

- [ ] **步骤 1：写参数范围 RED 测试**

覆盖 NaN/Infinity、阈值越界、session_pool_size=0、page_concurrency/render_queue_capacity/blocking_task_limit 为 0、render_queue_capacity 大于 page_concurrency、dpi=0、max_long_edge_pixels<800、负权重和 assignment 权重总和偏离 1 超过 1e-6；允许误差不超过 1e-6，不能直接比较浮点相等。

- [ ] **步骤 2：写自定义引擎路径测试**

给出三个都不存在的 model_path/model_config_path/model_manifest_path，断言 TryFrom<RawConfig> 仍成功并把路径解析为绝对路径。该测试防止配置层错误绑定默认模型。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-config --test validation
~~~

- [ ] **步骤 4：实现 TryFrom**

ValidatedConfig 只校验类型、范围、权重总和和路径解析。模型存在/hash/schema 不在本 crate 检查。

- [ ] **步骤 5：运行 GREEN 与 Clippy**

运行：

~~~bash
rtk cargo test -p docparse-config
rtk cargo clippy -p docparse-config --all-targets -- -D warnings
~~~

---

### 任务 A4：实现模型 manifest 与 uv 下载

**文件：**
- 新建：crates/layout/src/model_manifest.rs
- 新建：scripts/download_models.py
- 新建：scripts/download_models.py.lock
- 修改：crates/layout/src/lib.rs
- 修改：.gitignore

**接口：**
- 产出：ModelManifest::load_and_verify、模型目录。
- 输入：固定 HF revision 和两个文件 hash。

- [ ] **步骤 1：在 model_manifest.rs 内写 manifest RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`。临时目录覆盖正确 manifest、错误 revision、缺文件、错误 hash，断言错误 variant 和路径；测试直接访问私有 manifest 校验实现，不扩大生产 API 可见性。

- [ ] **步骤 2：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-layout --lib model_manifest::tests
~~~

- [ ] **步骤 3：实现 ModelManifest**

结构含 repository、revision、license、BTreeMap<String,String> files。load_and_verify 流式计算 SHA-256；禁止读完整 ONNX 到内存。

- [ ] **步骤 4：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-layout --lib model_manifest::tests
~~~

- [ ] **步骤 5：写 PEP 723 下载脚本**

固定：

~~~text
revision=46bbdf188bb0a772c08aed74882ce7e51a8f1ea6
inference.onnx=45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba
inference.yml=506fcfac13b3b546ae40d7886b44126420f392adb694e3f8bb6a6286a1f90fdc
~~~

实现 --output、--force、--verify-only，临时目录校验成功后 os.replace。

- [ ] **步骤 6：锁定并真实验证**

运行：

~~~bash
rtk uv lock --script scripts/download_models.py
rtk uv run scripts/download_models.py --output models/pp-doclayout-v3
rtk uv run scripts/download_models.py --output models/pp-doclayout-v3 --verify-only
~~~

---

### 任务 A5：定义中立 LayoutEngine 与 geometry

**文件：**
- 新建：crates/layout/src/error.rs
- 新建：crates/layout/src/types.rs
- 新建：crates/layout/src/geometry.rs
- 新建：crates/layout/src/engine.rs
- 新建：crates/layout/tests/engine.rs
- 修改：crates/layout/src/lib.rs

**接口：**
- 产出：LayoutEngine、Arc<PageImage> LayoutRequest、PageTransform、LayoutDetection、Polygon。
- 输入：RGB8 页面像素。

- [ ] **步骤 1：写 geometry RED 测试**

覆盖 Letter/A4、90 度旋转、非等比 resize、NaN/退化 polygon。往返误差小于 0.01 point。

- [ ] **步骤 2：写 fake trait RED 测试**

Arc<dyn LayoutEngine> 返回 Known 和 Unknown label，断言 raw_label 不丢失。

- [ ] **步骤 3：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-layout --test engine
~~~

- [ ] **步骤 4：实现类型**

PageImage 内部持有 Arc<[u8]> RGB8 buffer，并以 TryFrom 校验尺寸乘法溢出和 buffer 长度；LayoutRequest 持有 Arc<PageImage>，为后续可选 OCR 保留同一份像素。LayoutDetection 字段为 source_detection_index、raw_label、class_id、confidence、bbox、Option<Polygon>、geometry_source、model_order、metadata。固定 artifact 不输出 order_votes，公共类型不得为其增加伪造值。source_detection_index 是引擎原始输出中从 0 开始的行号，在阈值过滤或几何过滤前确定。所有多字段 struct 使用 builder。

- [ ] **步骤 5：实现 PageTransform 与 Polygon 运算**

PageTransform 使用显式 affine 参数和 TryFrom 验证有限值。Polygon 构造时通过 geo::Validation 拒绝非有限、自交、点数不足或退化 geometry，并在内部使用 geo::BooleanOps/clip 提供 bbox、area、与矩形的 intersection_area、含边界 contains_point 和 baseline_inside_length；公共签名不得泄漏 geo 类型。

- [ ] **步骤 6：实现 trait**

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

- [ ] **步骤 7：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-layout --test engine
~~~

---

### 任务 A6：在 Rust 实现前固定 Python oracle 与 ONNX schema

**文件：**
- 新建：scripts/reference_layout.py
- 新建：scripts/reference_layout.py.lock
- 新建：crates/layout/examples/inspect_model.rs
- 新建：crates/layout/tests/fixtures/model/input.png
- 新建：crates/layout/tests/fixtures/model/python_output.json
- 新建：crates/layout/tests/fixtures/model/pp_doclayout_v3_schema.json

**接口：**
- 产出：预处理 tensor 和最终 detection 的不可变 oracle。
- 输入：paddleocr==3.6.0、固定模型和固定图片。

- [ ] **步骤 1：创建固定 RGB 测试图片**

使用明确像素值生成小图并保存 PNG；记录 PNG SHA-256。图片同时包含矩形、文本形状和不同颜色通道，能识别 RGB/BGR 颠倒。

- [ ] **步骤 2：编写 reference_layout.py**

脚本输出 JSON：

~~~json
{
  "paddlex_source_commit": "ffb64904d23708863ff5b8da312a5cbd52a7f462",
  "tensor": {
    "dtype": "float32",
    "shape": [1, 3, 800, 800],
    "min": 0.0,
    "max": 1.0,
    "sha256": "generated-by-script"
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

scale_factor 数值仅示意字段类型；实际 fixture 写入 [800/original_render_height,800/original_render_width]。tensor SHA-256 对 C-contiguous NCHW f32 little-endian bytes 计算。

- [ ] **步骤 3：锁定 Python 依赖**

运行：

~~~bash
rtk uv lock --script scripts/reference_layout.py
~~~

- [ ] **步骤 4：生成 oracle**

运行：

~~~bash
rtk uv run scripts/reference_layout.py \
  --model-dir models/pp-doclayout-v3 \
  --input crates/layout/tests/fixtures/model/input.png \
  --output crates/layout/tests/fixtures/model/python_output.json
~~~

- [ ] **步骤 5：导出 ONNX schema**

inspect_model 只输出 inputs/outputs 名称、dtype、dims 和 metadata。

运行：

~~~bash
rtk cargo run -p docparse-layout --example inspect_model -- \
  models/pp-doclayout-v3/inference.onnx \
  crates/layout/tests/fixtures/model/pp_doclayout_v3_schema.json
~~~

预期：两个 fixture 无时间戳和绝对路径，重复生成 byte-for-byte 一致。

---

### 任务 A7：实现并验证预处理

**文件：**
- 新建：crates/layout/src/pp_doclayout_v3/mod.rs
- 新建：crates/layout/src/pp_doclayout_v3/preprocess.rs

**接口：**
- 产出：ModelInputs。
- 输入：PageImage、PageTransform 和固定 Python tensor oracle。

- [ ] **步骤 1：在 preprocess.rs 内写 tensor parity RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，读取 input.png 和 python_output.json，直接验证私有 ModelInputs 的 dtype、shape、min/max、SHA-256、image size 和 scale factor 全部一致，不为测试公开预处理内部类型。

- [ ] **步骤 2：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-layout --lib pp_doclayout_v3::preprocess::tests
~~~

- [ ] **步骤 3：实现 RGB/BICUBIC resize**

输入固定 RGB8；Python oracle 显式关闭 OpenCV optimized/IPP 并固定单线程，Rust 逐项对齐 OpenCV 4.10.0 generic CPU INTER_CUBIC 的坐标映射、cubic coefficient、边界扩展、通道与定点量化语义。不得直接假定 image::imageops::FilterType::CatmullRom 等价。输出 800 x 800，并先通过非方形/边界像素 fixture 的 tensor hash。

- [ ] **步骤 4：实现数值变换**

逐像素乘 1/255，mean 0、std 1，HWC 转 NCHW float32。image size 固定 [800,800]，scale factor 固定 [800/original_render_height,800/original_render_width]。

- [ ] **步骤 5：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-layout --lib pp_doclayout_v3::preprocess::tests
~~~

---

### 任务 A8：实现并验证后处理、Session 与 Pool

**文件：**
- 新建：crates/layout/src/pp_doclayout_v3/schema.rs
- 新建：crates/layout/src/pp_doclayout_v3/postprocess.rs
- 新建：crates/layout/src/pp_doclayout_v3/session.rs
- 新建：crates/layout/src/pp_doclayout_v3/pool.rs
- 新建：crates/layout/tests/model_contract.rs
- 新建：crates/layout/tests/python_parity.rs

**接口：**
- 产出：PpDocLayoutV3Engine。
- 输入：ModelInputs、ONNX schema、bbox/bbox_num/mask outputs 和 Python detection oracle。

- [ ] **步骤 1：写 schema RED 测试**

断言固定模型的 im_shape/image/scale_factor 三个输入，以及 float32 [N,7] bbox、int32 [batch] bbox_num、int32 [N,200,200] mask 三个输出与 schema fixture 一致；错误模型、错误 output count 和错误 dtype 返回 UnsupportedModelSchema。

- [ ] **步骤 2：在 postprocess.rs 内写后处理 RED 测试**

在文件末尾声明唯一的 `#[cfg(test)] mod tests`，使用固定 7 列 bbox rows 直接验证私有后处理：threshold 严格 >、NumPy ties-to-even 取整、原始渲染像素边界裁剪、class label、source_detection_index、order_seq 和稳定 tie-break。ONNX bbox 已在原始渲染像素空间，不得再次做 800 x 800 逆缩放；只映射到 viewport。mask 只校验 shape/dtype/hash 后释放；polygon 为 None，矩形 quad 标记 DerivedFromBbox。

- [ ] **步骤 3：写 Python parity RED 测试**

真实推理结果与 python_output.json 比较数量、class、score、bbox、order_seq；Python fixture 额外记录未消费 mask 输出的 SHA-256，Rust 验证其 shape/dtype 后立即释放。

- [ ] **步骤 4：运行 RED**

运行：

~~~bash
rtk cargo test -p docparse-layout --test model_contract -- --ignored --nocapture
rtk cargo test -p docparse-layout --lib pp_doclayout_v3::postprocess::tests
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
~~~

- [ ] **步骤 5：实现 schema 与 Session**

PpDocLayoutV3Engine::from_config 依次读取三个显式路径，验证 manifest 来源/revision/file hash、inference.yml 合同、ONNX schema 和 EP feature，再构建 ort Session。只有此处检查模型 artifact。

- [ ] **步骤 6：实现 bbox row 后处理**

解析 [class_id,score,xmin,ymin,xmax,ymax,order_seq]，使用 bbox_num 切分 batch，并在任何过滤前保存 source_detection_index。第三个 int32 mask 输出只验证 schema，不进入公共 detection。应用严格阈值、ties-to-even 取整、render pixel 边界裁剪、render pixel -> viewport 变换和固定 label list。lossless profile 关闭 NMS/unclip/merge/overlap filter，并按 order_seq、source index 排序。

- [ ] **步骤 7：实现 Session Pool**

创建 session_pool_size 个独立 Session。用 Tokio semaphore 和 RAII lease 借用；lease Drop 时归还 index。不得跨 await 持有 std MutexGuard。

- [ ] **步骤 8：实现异步 detect**

detect 在 spawn_blocking 中借用 Session 并执行同步 ORT；返回前记录第三方调用完成或错误日志。

- [ ] **步骤 9：运行 GREEN**

运行：

~~~bash
rtk cargo test -p docparse-layout
rtk cargo test -p docparse-layout --test model_contract -- --ignored --nocapture
rtk cargo test -p docparse-layout --test python_parity -- --ignored --nocapture
rtk cargo clippy -p docparse-layout --all-targets -- -D warnings
~~~

预期：阶段 A 门禁全部通过。

可选 EP 在匹配宿主与 ONNX Runtime 产物的独立 CI job 中逐个运行，不能组合：

~~~bash
rtk cargo check -p docparse-layout --no-default-features --features cuda
rtk cargo check -p docparse-layout --no-default-features --features coreml
rtk cargo check -p docparse-layout --no-default-features --features openvino
~~~

---

## 阶段 A 完成门禁

- 配置测试不依赖模型文件。
- 下载脚本两次运行幂等。
- Python oracle 先于 Rust 实现存在。
- 预处理 tensor SHA-256 完全一致。
- ONNX schema 固定。
- bbox、score、order_seq 与 Python 一致，mask 输出 dtype/shape 与固定 schema 一致。
- 自定义 LayoutEngine 不触发默认模型校验。
- CPU 默认构建通过；CUDA/CoreML/OpenVINO 各自在匹配宿主中独立 compile check 通过。

本计划不包含 commit。提交需用户单独授权。
