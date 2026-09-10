# 视觉区域图片提取与输出方案

日期：2026-09-10  
调研基线：`45ef362`  
状态：方案建议；尚未修改生产实现。文中配置、字段与命令均为拟议接口。

## 1. 推荐结论与范围

复用现有 PDFium、布局模型、页面渲染器和 PNG 编码能力，增加独立的区域图片处理阶段：

**最终视觉块 → 定位完整模型区域 → 匹配 PDF 图片对象 → 提取可独立使用的图片 → 失败或不完整时裁剪页面栅格 → 按目录或 Base64 模式输出。**

用户已确认范围为 `chart`、`image`、`header_image`、`footer_image` 四类，每个最终输出 Block 对应一张完整区域图片。使用显式标签集合，不按字符串包含 image 模糊匹配。

“提取成功”的含义是能代表整个识别区域，而不是 PDFium 返回了任意一段图片字节。图表内的图标、背景图或某个子图不能代替完整图表。

建议原生端同时支持目录和 Base64；普通浏览器/WASM 首先支持 Base64。浏览器目录下载不能承诺真实的操作系统绝对路径，需要原生宿主才能满足这一输出约束。

## 2. 已核实的项目现状

| 部位 | 当前能力 | 对本需求的影响 |
|---|---|---|
| `crates/pdfium/src/page.rs:541` | `image_objects()` 读取位置、像素尺寸、JPEG/原始流 | 只枚举顶层对象，不能直接用于完整图片发现 |
| `crates/pdfium/src/page.rs:733` | `render_image_object()` 调用 `FPDFImageObj_GetRenderedBitmap` | 只接受顶层图片序号，输出分辨率受页面放置矩阵影响 |
| `crates/pdfium/src/page.rs:776` | 路径对象已递归进入 Form XObject，并组合变换 | 可以复用遍历、坐标变换思路，无须另加 PDF 解析器 |
| `crates/pdfium/src/bitmap.rs:113` | BGRA 到 RGBA 转换 | 可以保留透明通道；不能用丢弃 alpha 的 `to_rgb()` 处理透明图片 |
| `crates/core/src/semantic/mod.rs:313` | 普通模型块的 bbox 由文本内容决定；原模型范围另存 `source_region(s)` | 图片范围不能直接采用当前 `Block.bbox` 或文字生成的 polygon |
| `crates/core/src/runtime/pipeline.rs` | PDFium actor 负责文档/页面生命周期，页面分析已有 RGB 栅格 | 能复用现有截图，但图片提取必须安排在 actor 关闭之前 |
| `crates/core/src/types.rs:484` | `Block` 有文字、模型区域、可选 table，没有图片资源 | 需要新增可选的资源字段 |
| `crates/core/src/render/json.rs` | 配置化 JSON 使用手写的借用序列化视图 | 除 Rust 数据类型外，还必须修改此序列化入口，避免漏字段 |
| `packages/web/src/worker.ts` | 已将页面栅格编码为 PNG Blob，用于预览 | 可以参考编码与消息传递，但页面预览 Blob 不是可持久化的 JSON 图片资源 |

项目锁定 PDFium `chromium/8028`，并已有 PNG 编码依赖；`Cargo.lock` 中已有 Base64 0.23.1。正式使用 Base64 时仍须在根 `workspace.dependencies` 声明，并由子 crate 继承。

邻近 LiteParse 项目已有 JPEG 优先、位图转 PNG、字节共享和目录写出流程，可作为参考。但其图片索引和顶层枚举方式，以及对象渲染分辨率假设，不能原样用于本需求。

## 3. 对当前 PDF 的只读探测

输入：`/Volumes/Yage/Downloads/2303.18223v16.pdf`。

### 3.1 现有生产解析器的真实结果

使用当前 WebGPU、PDFium/WASM 和真实布局模型解析全部 144 页：

- `image`：17 个。
- `chart`：5 个。
- `header_image`、`footer_image`：均为 0。
- 22 个目标区域分布于 16 页。
- 其中 13 个目标区域所在页面没有内嵌位图；5 个 chart 都属于这一情况。
- 部分 `Block.bbox` 比保留的模型区域明显小，例如第 59 页一个 image 的面积约为模型区域的 67.6%。

因此，“没有直接图片时回退截图”是这份 PDF 的主要路径之一，不能当作罕见异常。直接裁 `Block.bbox` 还可能丢掉非文字图形。

### 3.2 PDF 对象层的辅助探测

Poppler `pdfimages -list` 统计到：132 个图片绘制实例、27 个 soft-mask 条目、63 个不同的图片资源对象。重复实例不是重复的识别区域，soft mask 也不是需要单独导出的图片。

使用环境内的 pypdfium2 5.13.0 / PDFium 153.0.7999.0 递归检查，132 个图片实例全部位于 Form XObject 内，顶层图片数为 0。

同时测试了第 9 页一个 225×225 图片对象：

| 方式 | 输出尺寸 |
|---|---|
| 原始像素位图 | 225×225 |
| 按原放置矩阵调用对象渲染 | 15×15 |
| 保持显示形态、临时放大到接近原始分辨率后渲染 | 225×225 |

这项辅助探测使用的是 PDFium 7999，不是项目的 8028 构建；它证明需要验证的分辨率和对象层问题，不能替代正式的 native/WASM 接入验收。

调研产物位于 `packages/web/test-results/region-image-research/`：`model-regions.json`、`region-summary.json`、`pdf-object-inventory.json`。它们是本地调研证据，不是正式回归夹具。

## 4. 提取策略

### 4.1 区域和对象匹配

1. 在布局归属、包含关系合并完成后，筛选最终 `label` 为目标类型的 Block。
2. 从 `source_regions` 或 `source_region` 中取上述四类对应的视觉模型区域。合并块只合并相关视觉来源，不把其他文字来源的范围无条件加入。
3. 将模型区域裁到页面 viewport，保存为独立的 `image.region`；不通过修改旧的文本 bbox 来实现图片提取。
4. 递归遍历 Form XObject，累积祖先变换；每次绘制用页面对象路径表示，例如 `[12, 3, 0]`。资源对象 ID 只能标识资源，不能区分同图的多次放置。
5. 同时检查区域内的图片、可见文字、路径和 shading，判断单个图片对象能否完整代表该区域。

匹配以同一 viewport 坐标系的可见范围为基础，不能只按 raw PDF 坐标或全页图片面积过滤。现有“过滤过大图片”的规则也不能照搬，否则整页扫描图里的目标区域无法正确处理。

首版可以使用偏保守的双向覆盖和边界误差门槛，但门槛不是独立的正确性证明：还要排除额外文字、矢量标记、多图拼接、明显裁剪、复杂透明叠加等情况。不能确认时走区域截图。具体阈值应由图片夹具确定，不按这份 PDF 的页码调参。

### 4.2 推荐的三级路径

| 路径 | 适用条件 | 输出 |
|---|---|---|
| `embedded_stream` | 单一、完整、无需额外视觉合成的 JPEG；流和颜色/变换条件均确认可直接使用 | 原 JPEG 字节，`image/jpeg` |
| `embedded_bitmap` | 单图片完整代表区域，但编码不能直接作为图片文件使用，或需要解码/掩码处理 | PDFium 图片对象位图，编码为 PNG |
| `page_crop` | 没有位图、复合图表、多图、外部文字/矢量、整页扫描图局部、复杂剪裁或前两级失败 | 完整区域的无调试覆盖层 PNG |

PDFium 的 Raw/Decoded image-data 接口返回的是流数据，不能把任意返回值直接保存成 `.png`。官方还明确区分了忽略 mask/matrix 的 `GetBitmap` 和考虑 mask/matrix 的 `GetRenderedBitmap`。[PDFium 图片 API](https://pdfium.googlesource.com/pdfium/+/refs/heads/main/public/fpdf_edit.h)

原 JPEG 保留属于优化路径；若无法可靠验证 ICC、CMYK、Decode 参数、Exif 方向或透明语义，就输出 PDFium 处理后的 PNG。JPX、Flate、JBIG2、CCITT 等不必分别增加公开输出格式，能解码时统一 PNG。

当前 upstream 的对象渲染实现也处理对象自身 clip path，但不能由此推断所有祖先 Form 裁剪、混合模式、其他对象遮挡都被独立渲染完整重现。[PDFium 对象渲染实现](https://pdfium.googlesource.com/pdfium/+/refs/heads/main/fpdfsdk/fpdf_editimg.cpp)

对象位图提取需要显式控制原始分辨率，不能直接把现有 `render_image_object()` 的输出尺寸当作 `pixel_width/pixel_height`。如果临时调整对象矩阵，应在同一 actor 内保存、恢复，错误路径也必须恢复；不保存或修改源 PDF。复杂嵌套变换首版可直接回退区域渲染。

### 4.3 截图回退

“截图”指 PDFium 页面内容的局部栅格化，不是操作系统截图，也不是带识别框的 SVG/PNG 诊断图。

- 默认复用本页已有 `PageImage`，减少重复渲染和跨线程复制。
- 使用实际栅格宽高映射坐标：`sx = raster_width / viewport_width`，`sy = raster_height / viewport_height`；不能假设请求 DPI 等于实际 DPI。
- 左上取 floor，右下取 ceil，再裁到图像边界；空矩形必须作为失败处理。
- 页面 Rotate、CropBox、UserUnit 已通过现有变换表达，不能二次旋转。
- 多边形区域以外接矩形承载，必要时将多边形之外置透明；不能用文字 polygon 代替模型 polygon。
- 若用户要求高于现有页面栅格的质量，只重渲染目标区域，不放大低分辨率截图。

PDFium 提供带变换矩阵和设备空间裁剪矩形的页面渲染接口，可用于只分配目标输出位图。[PDFium 页面渲染 API](https://pdfium.googlesource.com/pdfium/+/refs/heads/main/public/fpdfview.h)

图像质量、注释开关和背景颜色要形成明确策略。首版截图与当前页面预览使用相同渲染语义；原始 PDF 的注释可能被包含，DocParse 自己的 overlay 永远不包含。

## 5. 解析链路与生命周期

推荐把图片处理放在每页 `PageAnalyzer` 完成之后、页面结果返回之前：

`text/layout finish → visual region resolution → PDF image extraction / raster crop → encode → output destination → PageResult`

现有 PDFium executor 被渲染 producer 持有。需要向页面任务提供可克隆的请求句柄，用已有 actor 的消息通道发起图片请求；不把 PDFium 文档、页面、对象句柄带到其他线程或 JS。

- actor 内完成对象枚举、匹配所需的 PDF 原生操作、解码/局部渲染，并返回完全拥有的元数据或字节。
- PNG 编码尽量放在 actor 外。原生端使用既有 blocking 限流，避免图片编码长期占住 PDFium。
- 元数据先行，只读取选中的候选图片字节，避免在全页预扫描时保留所有 raw/decoded 数据。
- 等全部页面图片请求完成后再关闭 actor；取消、编码失败、落盘失败都要能释放像素缓冲和临时文件。
- `parse_page(PageInput)` 没有原 PDF，不能伪造原图提取成功；它使用已有页面栅格裁剪，并明确标记 `page_crop`。

更简单的备选是解析结束后重新打开 PDF，批量提取/裁剪。这能减少 actor 改动，但失去页面栅格复用，产生第二次打开和渲染。可作为实现打样，长期默认链路推荐页内完成。

## 6. JSON 与输出契约

建议新增 `Block.image: Option<ImageAsset>`。`chart` 也使用同一个字段；保留原有 `text`、`lines`、标签、顺序和表格结构。

### 6.1 目录模式

```json
{
  "label": "chart",
  "image": {
    "source": "page_crop",
    "mime_type": "image/png",
    "width": 1200,
    "height": 800,
    "region": {"left": 48.0, "top": 80.0, "right": 540.0, "bottom": 408.0},
    "data": {
      "kind": "file",
      "path": "/absolute/output/images/sha256-of-encoded-image.png"
    }
  }
}
```

上例尺寸和路径为接口示意，不是本轮导出的图片。`width/height` 始终是最终图片文件的像素尺寸；`region` 是页面 viewport 点坐标。

- 输出目录可由相对路径配置，但先创建并 canonicalize，JSON 中写实际存在的绝对路径。
- 文件名使用内容哈希或受控 ID，不直接使用 PDF 文字或含冒号的 Block ID。
- 相同编码字节可共用文件；去重按最终图片字节，而不是仅按原始 PDF stream。相同资源在不同 mask/变换下可能显示不同。
- 重复页眉/页脚 Logo 保留每个 Block 的关联，内部可共享编码字节，目录模式共用已校验文件。Base64 按块内联仍会重复序列化，文档体积预算按实例计算；首版不为去重额外改变为全局资源引用表。
- 文件先以临时文件写完并发布，最后才发布引用它们的最终 JSON。目录模式的写入失败不能静默改成 Base64，也不能返回不存在的路径。
- 单次运行目录或内容寻址文件便于隔离并发。失败清理只处理本次拥有的临时文件，不删除原有目录内容。
- 绝对路径是导出机器上的位置；若在服务器运行，它不是浏览器客户端本地路径。

### 6.2 Base64 模式

```json
{
  "label": "image",
  "image": {
    "source": "embedded_bitmap",
    "mime_type": "image/png",
    "width": 225,
    "height": 225,
    "region": {"left": 100.0, "top": 100.0, "right": 200.0, "bottom": 200.0},
    "data": {"kind": "base64", "value": "<encoded image bytes>"}
  }
}
```

- Base64 编码的是完整 JPEG/PNG 文件字节，不是像素数组或 PDF 压缩流。
- 建议采用标准带 padding 的 Base64，不混入 `data:image/...;base64,` 前缀；前端需要时再由 MIME 和 value 组合 data URL。
- 文件和 Base64 使用互斥枚举，避免两个可空字段同时有值或同时缺失。
- 内存中保留编码后的字节，序列化边界再 Base64；不要让 Rust 的 `Vec<u8>` 默认变成 JSON 数字数组。
- Base64 长度为 `4 × ceil(n/3)`，编码本身约膨胀 1/3，序列化和 JS 字符串还可能增加额外内存。

### 6.3 接口分层和兼容性

- 图片提取/编码与输出目的地分开；原生文件 I/O 留在 native 适配层，JSON renderer 保持纯序列化。
- 输出目的地在解析请求开始前选定。目录模式每页资源写完后可释放编码字节；Base64 模式在结果中保留字节待序列化。
- 优先使用两种明确的目的地枚举，不引入尚无使用者的通用云存储插件层。
- `Block.image` 用 `serde(default)` 和缺省省略支持读取旧结果。旧 JSON 缺少 image 不构成历史数据错误；新解析的图片完整性由处理阶段单独保证。
- `ConfiguredBlock`、普通 JSON renderer、WASM 结果序列化和 TS `Block` 都要同步支持。
- 关闭 `include_evidence` 或 diagnostics 不能删掉图片资源，它是业务结果。
- 本次保持增量字段扩展；是否升级 schema 版本应遵循项目现有的 2.0 兼容策略，不单独发明一个旧 reader 无法识别的版本字符串。

## 7. 建议配置与平台差异

拟议 CLI 用法：

```sh
rtk docparse parse input.pdf --format json --images base64 --output result.json
rtk docparse parse input.pdf --format json --images directory --image-dir ./images --output result.json
```

建议默认 `base64`，满足四类目标块都有图片且不隐式写目录；目录模式必须提供 `--image-dir`。模式也应提供 Rust 配置入口，CLI 只是覆盖配置。默认启用图片后，JSON 大小和解析成本会变化，必须在发布说明中列出。

建议先保留少量必要设置：输出模式/目录、截图 DPI、单图像素上限、单图编码字节上限和全篇资源上限。像素/字节上限的默认值应通过 native/WASM 峰值内存测试确定，不能把未经验证的数值称为安全默认值。

截图 DPI 首版推荐跟随现有渲染设置，当前默认 144；需要更清晰图表时单独提高图片 DPI，不改变布局模型输入分辨率。严格定义显式分辨率请求遇到上限时是报告降采样还是失败，不能无记录地降低质量。

| 环境 | Base64 JSON | 目录与真实绝对路径 |
|---|---|---|
| 原生 CLI / Rust | 支持 | 支持 |
| 普通浏览器 / WASM | 支持 | 无法提供这一完整语义 |
| 带原生文件桥接的桌面宿主 | 支持 | 可以由宿主负责写出和返回真实路径 |

浏览器文件系统 API 提供用户授权的 handle、名称和相对目录路径，不能据此编造系统绝对路径；OPFS 也不对应可供用户使用的同名磁盘路径。[Chrome 文件系统说明](https://developer.chrome.com/docs/capabilities/web-apis/file-system-access) [FileSystemHandle.name](https://developer.mozilla.org/en-US/docs/Web/API/FileSystemHandle/name)

浏览器下载 ZIP 或写入用户选择的目录可以另行支持，但相对路径清单不等价于本次要求的绝对路径模式。首版不将它冒充成同一功能。

## 8. 失败处理与可观测性

- 找不到完整内嵌图、矢量图、复合图转截图是正常分支，不应给用户显示“原图提取错误”。在 source/调试原因中区分即可。
- 原图解码失败后尝试截图；截图也失败时必须记录 `VisualAssetUnavailable`，按明确的解析错误策略中止或部分返回，不能悄悄漏图。
- 目录/权限/磁盘空间错误归为输出失败，最终 JSON 不发布无效路径。
- 日志只记录文档/页/块 ID、候选数、选用路径、像素尺寸、字节数、耗时和失败原因，不记录 Base64、完整图片字节或图内文本。
- 使用 `tracing::debug!("... {}", value)` 等项目约定，关键错误返回前记录上下文。
- 增加 `image_extract`、`image_crop`、`image_encode`、`image_write` 阶段计时与成功/回退/失败计数。阶段可能重叠，不能把各阶段累加值当作总耗时。

## 9. 实施拆分与验收

### 9.1 实施拆分

1. 补齐 PDFium 图片对象递归发现、祖先矩阵和稳定绘制路径；核对 native 动态符号加载和 wasm 静态绑定。
2. 实现共享区域选择/候选匹配，接入单图片提取、分辨率控制和干净栅格裁剪。
3. 接入页任务与 actor 请求生命周期；处理取消和资源上限。
4. 增加 typed 图片数据、目录/Base64 目的地及所有 JSON/WASM 序列化入口。
5. 增加 CLI/SDK 配置和浏览器预览；完成真实 PDF 与跨平台验收。

新增超过三字段的 Rust struct 使用 typed-builder；Option 字段遵循默认值约定。复用现有 PNG、几何和错误处理方式，避免添加大量简单自由 helper。

### 9.2 功能验收矩阵

| 用例 | 必须验证 |
|---|---|
| 四类目标标签、重复页眉/页脚 Logo | 均执行提取，保留每次区域关联，重复资源不会覆盖其他区域 |
| 独立 JPEG、Flate/RGB、灰度图片 | 可打开的图片文件、MIME/实际尺寸正确 |
| SMask、透明 PNG、ICC/CMYK | 不丢 mask、不错误反色；不满足直出条件时有可靠回退 |
| Form 多层嵌套、重复资源多次放置 | 枚举不遗漏，祖先变换与绘制实例不混淆 |
| 矢量 chart、文字+图片、多个子图 | 输出完整识别区域，不只返回其中一个图标 |
| 整页扫描、图片局部裁剪 | 不把整页图片当成目标小区域的最终资源 |
| Rotate/CropBox/UserUnit/非均匀缩放 | 与页面实际显示位置一致，无重复旋转和错误偏移 |
| 目录输出 | 每个 path 为存在的绝对路径，解码尺寸和哈希符合元数据 |
| Base64 输出 | 解码为同一编码字节，MIME 正确，不是数字数组或非法流 |
| include_evidence=false、JSON 往返 | 图片仍完整保留，旧 JSON 仍可读取 |
| 失败、取消、长文档、重复解析 | 没有悬挂 actor、失效句柄、无界缓存或错误的成功计数 |

使用固定布局的最小 PDF 夹具检验提取逻辑，再用真实模型进行浏览器验收；不使用替代布局后端冒充 WebUI 验收。

本份 144 页 PDF 应作为真实验收样本：当前模型的 22 个 chart/image 块逐一有图片或明确失败（本文件没有 header_image/footer_image，应另加重复页眉页脚夹具）；所在页面没有内嵌位图的 13 个目标区域正确使用截图；第 9/12/17 页的复合图不能只导出局部图标。其余已验证表格、公式、文本顺序应保持回归通过。

### 9.3 尚未验证的事项

- 新提取链路在项目 PDFium 8028 native/WASM 下的像素一致性；辅助 7999 探测不能代替它。
- JPEG 直出资格判定及完整 clip/透明/颜色处理范围。
- 高分辨率对象渲染、区域重渲染的耗时与峰值内存，默认资源上限。
- 任意文档的语义区域判定准确率；“对象覆盖接近”不等于图表内容完整，仍需要保守回退和视觉验收。
- 页眉、页脚图片已确定纳入范围，但当前真实 PDF 没有对应检测结果，必须补充该类样本。

## 10. 方案选择

| 方案 | 评价 |
|---|---|
| 全部直接截图 | 实现最少、视觉完整，但不满足原图优先，也损失原始像素优势 |
| 原始流优先 + 对象位图 + 区域截图 | **推荐**；满足需求并复用现有栈，但必须补齐完整性判断和对象遍历 |
| 换用另一个 PDF 引擎负责图片 | 带来额外依赖、坐标/字体/颜色一致性成本；目前没有必要 |

交付目标应是“每个识别区域得到完整且可消费的图片”，而不是提高原始流提取的命中率。原始流直出可以保守，截图回退必须可靠。
