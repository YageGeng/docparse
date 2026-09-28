# 在 Axum 服务中托管工作台：将 `packages/web` 嵌入服务的方案调研

日期：2026-09-28
状态：调研 + 已实施（磁盘与嵌入两种来源均已落地，路由为挂载式）。已确认决策：不引入 nginx、本机/受信内网明文 HTTP、UI 挂载在 `{api_prefix}/webui`（见 §9）。
范围：如何让构建产物 Vite 工作台（`packages/web/dist`）成为服务端部署的一部分，而不再依赖独立的 nginx 文档根目录。

> 说明：本文档位于 `docs/` 下，按 `AGENTS.md` 第 1 条用中文撰写；代码注释仍为英文。代码、命令、标识符、HTTP 头与链接保持英文原文。

## 1. 一段话结论

保留现有 Axum 服务，把构建好的 SPA 挂载进去，并且分两个阶段做。阶段 1 通过 `tower-http` 的 `ServeDir`/`ServeFile` 从磁盘提供 `dist` —— **零新依赖**（tower-http 0.7.1 已是依赖，根 `Cargo.toml` 已启用 `fs` feature），并且与 [`jobs/source.rs`](../crates/server/src/routers/jobs/source.rs) 中已经使用的 `Range`/`HEAD` 写法一致。阶段 2 增加一个**非默认**的 `embed-web` Cargo feature，用 `rust-embed` 把 `dist` 嵌入二进制，并通过同一个 `ServeDir` 的可插拔 `Backend` trait 提供服务 —— tower-http 0.7 的文档明确说明该 trait 就是为 "rust-embed、include_dir、S3" 这类场景设计的。**不要**把某个专门的"embed + axum"crate 当作基础：最流行的 `axum-embed` 已停止维护且锁定在 `axum-core 0.4`/axum 0.7，其余替代品要么仅支持 Axum 0.8 但用户量极小（下载量几百），要么根本不处理 HTTP。也**不要**在 `build.rs` 里调用 `npm`。

成本估算（基于当前构建实测）：SPA 未压缩文件 6.90 MB，另有 1.27 MB 的 `.gz` 同伴文件；两者全部嵌入实测使 75 MB 的 release `docparse-server` 增加 **8,972,472 字节**（约 +8.6 MiB，即约 +12%）；如果只嵌入预压缩版本则约 +3.6 MB。详见 §5.5。

**部署前提（已确定）：不引入 nginx。** 因此进程内托管 SPA 是必选项，模式 D（nginx/独立静态站点）出局，§3.2 那条 nginx 缺陷随之作废。**但去掉 nginx 不等于所有能力都自动消失**：压缩、缓存头、上传体积上限、SSE 不缓冲、以及 TLS/HTTP2/HTTP3 都必须有人承担，其中前三项服务端已经具备或按 §5.2 补齐，TLS 与 HTTP 版本则需要另行决定，见 §3.3。

剩下的唯一选择是资源放在**磁盘**（阶段 1）还是**嵌入二进制**（阶段 2）；两者共享同一套路由、响应头与测试，所以先用阶段 1 落地、再按需加阶段 2 是最低风险的路径。下面的方案在不引入第三方 embed-and-serve crate、也不让默认 `cargo build` 依赖 Node 的前提下达成该目标。

## 2. 当前拓扑与约束（实测）

| 事实 | 证据 |
| --- | --- |
| SPA 是 Vite 8 / React 19 应用，构建到 `packages/web/dist` | `packages/web/package.json`、`packages/web/vite.config.ts` |
| 当前生产形态 = nginx 文档根 + API 反向代理（本次改动要替换掉的部分） | `packages/web/nginx.conf.example`；`packages/web/README.md`（"Production needs only a static web server and a same-origin proxy to Axum"） |
| 需要 `index.html` 兜底的 SPA 路由：`/`、`/monitoring`、`/document` | `packages/web/src/app.tsx`（`BrowserRouter`） |
| 浏览器 API 地址为同源，前缀来自 `VITE_API_PREFIX` | `packages/web/src/api/client.ts`（`apiUrl` 使用 `window.location.origin`） |
| 服务端路由：`api_prefix` 子树（jobs、monitoring、health、ready，以及生成的 `/docs` 与 `/openapi.json`），外加根路径的 `/metrics` | `crates/server/src/app.rs`、`crates/server/src/routers/`（其 `utoipa::path` 值 `/docs`、`/openapi.json`、`/health`、`/ready` 都被嵌套在该前缀之下） |
| 未知路由目前返回带类型的 JSON 404 信封 | `crates/server/src/app.rs`（`.fallback(...)`、`RequestSnafu`） |
| tower-http 已是服务端直接依赖，且 `fs` 已启用 | `crates/server/Cargo.toml`；根 `Cargo.toml`（`tower-http = { version = "0.7", features = [..., "fs"] }`）；`Cargo.lock` 锁定 0.7.1 |
| 服务端已在使用 `tower_http::services::ServeFile` 处理 `Range`/`HEAD`/条件请求 | `crates/server/src/routers/jobs/source.rs` |
| 现有 `CompressionLayer` 绝不会二次压缩已带 `Content-Encoding` 的响应 | tower-http 0.7.1 `src/compression/service.rs`："Responses that are already compressed ... will _never_ be recompressed" |
| `dist` 被 gitignore；`dist/pdfjs/**` 由 `scripts/prepare-pdfjs.mjs` 在 `prebuild` 阶段生成 | `.gitignore`；`packages/web/package.json` |
| `npm run build` 已为 html/js/css/json/svg/wasm（≥ 1 KiB）生成 `.gz` 同伴文件 | `packages/web/scripts/compress-assets.mjs` |
| 实测产物 | 未压缩（非 `.gz`）**6.90 MB** / `.gz` 同伴 **1.27 MB** / 共 285 个文件：`assets/` 3.52 MB 原始，`pdfjs/` 3.37 MB 原始（cmaps 1.7 MB、wasm 2.1 MB、standard fonts 0.8 MB） |

有两个约束决定了绝大部分设计：

1. **构建顺序。** `dist` 不在仓库里，因此任何编译期嵌入都会让 `cargo build`/`cargo test --locked` 依赖一次 Node 构建。所以嵌入必须用 feature 门控，永远不能进默认特性。
2. **两个服务、两个命名空间。** API 前缀（`/api/v1/docparse`）与 SPA 共用一个 origin。SPA 占有根命名空间；API 保留自己的命名空间，并且必须继续返回 JSON 404，而不是 HTML。

## 3. 分类：SPA 可以放在哪里（通用模式）

| 模式 | 工作方式 | 收益 | 代价 | 适用场景 |
| --- | --- | --- | --- | --- |
| **A. 反向代理 / 独立静态站点**（原方案） | nginx/Caddy 提供 `dist`，反代 API 前缀 | 静态性能最佳、`gzip_static`、暴力缓存、HTTP/3、TLS、CDN 就绪、不重启 API 即可换资源 | 两个产物、两步部署、SPA 与 API 版本错配 | **已排除**（决策：不引入 nginx） |
| **B. 同进程、资源在磁盘** | 把 `ServeDir` 挂成路由 fallback，路径来自配置 | 单一产物形态（二进制 + 资源目录）、HTTP 语义与 API 一致、改 UI 无需重编译 | 资源必须随二进制一起分发；需要配置路径 | 本地/桌面式安装；以镜像为产物的容器部署 |
| **C. 同进程、资源嵌入** | 用 `rust-embed`/`include_dir`/宏代码生成在编译期把 `dist` 嵌入 | 真正单文件、不依赖文件系统、资源不可变、无读取系统调用 | 编译前必须先跑 Node 构建；二进制变大；每次改 UI 都要重编译 Rust | "一个文件拷过去"是硬需求 |
| **D. 混合（嵌入 + 磁盘覆盖）** | 默认用嵌入资源；若配置目录存在则改从磁盘提供 | 兼顾调试/热修与单文件默认值 | 接线略多，两条代码路径 | 发布二进制 + 应急热修通道 |
| **E. CDN / 对象存储 + 纯 API 后端** | 把 `dist` 上传 CDN，后端只服务 `/api` | 全球性能与缓存经济性最佳 | 版本管理/CORS/缓存失效的额外工作与凭据 | 面向公网的产品；需要引入外部 CDN，与"不引入外部静态托管"的决策相冲突 |

### 3.1 真实先例（已核实）

| 项目 | 模式 | 备注 |
| --- | --- | --- |
| Meilisearch mini-dashboard | A | `build.rs` 下载预构建的 `build.zip` 发布产物、校验 SHA-1、解包到 `OUT_DIR`，再用 `static_files::resource_dir` 嵌入。CI 从不在 Rust 构建里调用 JS 工具链。 |
| Grafana | B | `static_root_path` 默认 `public`；文档明确警告二进制必须以安装路径为工作目录运行。另有 `serve_from_sub_path` 支持子路径挂载。 |
| Vaultwarden | B | `WEB_VAULT_FOLDER=web-vault/` + `WEB_VAULT_ENABLED=true`；路径写错就是白屏。 |
| Spring Boot | A | `src/main/resources/static` 下的内容自动对外提供。 |
| Go 服务 | A | `//go:embed` + `embed.FS`。 |
| Express 5 应用 | A | Express 5 中 `app.get('*')` 已非法，SPA 兜底必须写 `/*splat`（要同时匹配 `/` 则用 `/{*splat}`）。 |

两点启示：(1) Rust 生态中成熟的嵌入先例都是通过消费**已发布的 SPA 产物**来避免在 `build.rs` 里跑 `npm`；(2) 磁盘提供资源是大型安装的常态，因为它能热换资源。

### 3.2 原 nginx 形态中曾存在的一个缺陷（已随文件删除作废，仅存档）

原先的 `packages/web/nginx.conf.example` 只代理了 `location /api/v1/docparse/`，其余全部交给 `try_files $uri $uri/ /index.html`。除 `/metrics` 外，所有服务端路由都在 `api_prefix` 之下，而 `/metrics` 由 `app.rs` 挂在根路径，因此**经该代理访问 `GET /metrics` 会返回 `index.html`**。该示例文件已随"不引入 nginx"的决策删除（可从 git 历史恢复）；但它说明的规则在进程内方案里同样成立并已落实：**fallback 必须显式区分非 SPA 路径**，否则 `/metrics`、`/api/...` 等都会被 SPA 外壳吞掉。§5.1 的边界判断正是为此实现，并经 §附录 C 的真实服务验证。

### 3.3 去掉 nginx 后，服务端必须接管的能力

| 原本由 nginx 承担 | 现在的承担者 | 状态 |
| --- | --- | --- |
| 提供 `dist` 静态文件（`try_files ... /index.html`） | `ServeDir` + 带前缀判断的 SPA fallback（§5.1） | **已实现** |
| `gzip_static on`（直接发 `.gz`） | `ServeDir::precompressed_gzip()` + `Vary: accept-encoding` | **已实现** |
| 其余压缩 | 现有 `CompressionLayer`（gzip/Fastest，排除 SSE 与 PDF） | 已具备 |
| 缓存头（原由静态服务器默认/额外配置） | `workbench::cache_policy`：`/assets/*` 且 2xx/304 用 `immutable`，其余用 `no-cache`（§5.2） | **已实现** |
| `client_max_body_size 513m` | `DefaultBodyLimit::max(max_upload_bytes + 64 KiB)` | 已具备 |
| `proxy_buffering off`（SSE 不缓冲、上传不缓冲） | axum 原生流式响应，无中间代理 | 自动满足 |
| `proxy_read_timeout 360s` | axum/hyper 默认无读取超时限制 | 自动满足（如需上限需自行加超时层） |
| TLS 终止、HTTP/2、HTTP/3 | `axum::serve`；本服务定位为**本机/受信内网、明文 HTTP**，不做 TLS | **已决策：无需处理**（若将来要对外暴露，需在服务内建 rustls 与另加 TLS 终结器之间选择） |
| 访问日志 / 限流 / IP 白名单 | 目前无 | 视部署环境而定 |

也就是说：不引入 nginx 会把 TLS 与 HTTP 版本协商的问题摆到台面上，但**本服务的定位是本机/受信内网、明文 HTTP**（当前默认 `host = "127.0.0.1"`，且 README 的 ingress 说明同样指向受信边界），因此这一项已按"无需处理"结案。若将来要暴露到公网或要求 HTTPS，需要重新决策。

适用于所有"进程内"模式的补充说明：

- `compress-assets.mjs` 已经写出的 `.gz` 同伴文件，正是 `ServeDir::precompressed_gzip()` 期望的输入，因此无需新增压缩步骤即可启用预压缩服务；现有 `CompressionLayer` 不会对其二次压缩。
- `ServeDir` 会设置 `Content-Type`（MIME 猜测）、基于 size + mtime 的强 `ETag`、`Last-Modified`、`Accept-Ranges`，并处理 `If-None-Match`、`If-Modified-Since`、`If-Match`、`If-Unmodified-Since`、单/多段 Range（`206`/`416`）以及 `HEAD`。它**不会**设置 `Cache-Control` —— 这必须由应用补上。
- SPA 兜底不得吞掉 API 的 404，也不应把缺失的哈希资源用 `index.html` 应答。

## 4. 现成方案（版本核实于 2026-09-28）

| 方案 | 版本 / 最近发布 | Axum 0.8 | 嵌入 | 磁盘 | 预压缩 | ETag / 304 | Range | SPA 兜底 | 结论 |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| [`tower-http` `ServeDir`/`ServeFile`](https://docs.rs/tower-http/0.7.1/tower_http/services/struct.ServeDir.html) | 0.7.1，2026-08-31 | 是 | 经 `with_backend` | 是 | gzip/br/deflate/zstd | 是（强 ETag、Last-Modified） | 是 | 经 `fallback`/`not_found_service` | **推荐作为基础。** 已是依赖，且与 `jobs/source.rs` 的写法一致 |
| [`tower-http` `Backend` trait](https://docs.rs/tower-http/0.7.1/tower_http/services/fs/trait.Backend.html) | 0.7.x | 是 | 是 | n/a | n/a | n/a | n/a | n/a | 文档注释点名 rust-embed/include_dir/S3：这就是为嵌入预留的接口 |
| [`rust-embed`](https://docs.rs/rust-embed/8.12.0/rust_embed/) | 8.12.0，2026-07-08 | n/a（纯数据） | 是 | debug 构建读磁盘 | `compression` = deflate/zstd | metadata 提供 SHA-256 + mtime | 否 | 否 | 最佳嵌入数据源；与 `ServeDir::with_backend` 组合使用 |
| [`rust-embed-for-web`](https://crates.io/crates/rust-embed-for-web) | 11.4.1，2026-07-01 | n/a | 是 | debug 构建读磁盘 | 预置 gzip + br（+zstd） | 预置 ETag/Last-Modified | 否 | 否 | 若愿意自己写 handler 则很强；`#[allow_missing]` 对 gitignore 的 `dist` 有用 |
| [`static-serve`](https://crates.io/crates/static-serve) | 0.6.4，2026-09-06 | 是（0.8） | `embed_assets!` | 否 | gzip + zstd | ETag / `If-None-Match` | 是 | 未内置 | 最接近"开箱即用"的 Axum 0.8 crate；增加 2 个依赖；生态很小 |
| [`axum-asset`](https://crates.io/crates/axum-asset) | 0.3.0，2026-02-24 | 是（0.8.8） | derive 宏 | 否 | 否 | ETag、Last-Modified、`Cache-Control` | 否 | 否 | 特性集正确，但总下载量约 360 |
| [`axum-frontend`](https://crates.io/crates/axum-frontend) | 0.1.3，2026-09-18 | 是 | 是 | 是 | 未说明 | 未说明 | 否 | Vite 感知 | 正是为本需求而做，但仅 3 周大、下载量约 650 |
| [`heisenberg`](https://crates.io/crates/heisenberg) | 0.5.0，2026-06-07 | 是 | 是 | 是（代理模式） | 否 | 否 | 否 | 开发代理 vs 嵌入 | 双模式代理/嵌入，与框架无关 |
| [`axum-embed`](https://docs.rs/axum-embed/0.1.0/axum_embed/) | 0.1.0，**2023-12-17** | **否**（`axum-core ^0.4`、axum 0.7） | 是 | 否 | br/gzip/deflate 同伴文件 | ETag | 否 | 是 | **否决：已停止维护且版本不兼容** |
| [`static-files`](https://crates.io/crates/static-files) | 0.3.1，2025-08-24 | **否** | 仅 `build.rs` 代码生成（`generated.rs`） | n/a | 否 | 否 | 否 | 否 | 不是 HTTP 层；特性只有 `change-detection`/`sort`；需自己写 handler |
| [`include_dir`](https://crates.io/crates/include_dir) | 0.7.4，2024-06-17 | 否 | 是 | 否 | 否 | 否 | 否 | 否 | 只能配手写 backend/handler；项目已停更 |

其他评估后排除的：`axum-static-embed`（0.0.1，28 次下载）、`axum-embed-files`（2025）、`axctl-core`（2026-09）、`rust-silos`、`fs-embed`、`embed_it` —— 都太新或太小，不适合作为生产解析服务的依赖。`axum-embed-hashed-asset` 只处理哈希 URL 装饰。`bake` 已死（0.16.0 被 yank，"no longer used"）；`embed-resource` 嵌入的是 Windows `.rc` 资源而非 Web 资源；`rust_embed_axum` / `axum-rust-embed` / `embed-static` 在 crates.io 上不存在。`actix-web-rust-embed-responder` 是模式 C 的成熟实现，但面向 actix-web 而非 axum。路径查找库（`phf`、`matchit`）只在手写 handler 时才有意义。

重要的结构性结论：**不存在维护良好的、可直接落地的 "embed-my-SPA-in-axum" crate。** 生态分裂为 (a) 只做数据嵌入、不碰 HTTP 的 crate，和 (b) 基于文件系统的 HTTP 层。tower-http 0.7 的 `Backend` trait 是两者之间的桥，而 SPA 兜底策略无论如何都属于应用逻辑。

## 5. 本仓库的推荐设计

### 5.1 路由与优先级（挂载式）

需求在实施后变更为"前端也要尊重 `api_prefix`"，因此工作台不再是"前缀之外的一切"，而是**挂载在 `{api_prefix}/webui`** 下的一个服务（第五轮改动）：

```text
/                              -> 307 -> {api_prefix}/webui/
{api_prefix}/webui             -> SPA 外壳（带/不带尾斜杠都直接返回 index.html）
{api_prefix}/webui/**          -> 静态资源；无扩展名的导航回退到 index.html
{api_prefix}/** 其余            -> 带类型的 JSON 404（裸前缀、未知路径、WebUI 邻近写法）
/metrics                       -> 根路径 Prometheus 处理器
其他一切                        -> 带类型的 JSON 404
```

收益是**从结构上消除了整类边界缺陷**。旧设计让 SPA 拥有"前缀之外的一切"，于是服务端必须自行推断"某条路径算不算 API"，进而要处理百分号解码、多重编码、`//`、`/./`、`..` 与非法 UTF-8；评审据此找出过 `%252F`、`%FF` 一类会漏进外壳的输入。挂载之后外壳只在 `{api_prefix}/webui` 下可达，**任何 API 路径都不可能拿到 HTML**，`decode_request_path`、`canonical_path`、`is_api_path`、`DECODE_PASSES`、`reject_shadowing_prefix` 与 `top_level_names` 全部删除（净减约 120 行）。

关键实现点：

- `axum::Router::nest_service(mount, service)` 会先剥掉挂载前缀再调用服务，因此 `ServeDir` 看到的是挂载内路径（`/assets/...`），缓存策略与 `/assets` 判定直接基于它。
- **目录 307 必须补回前缀**：tower-http 的 `maybe_redirect_or_append_path` 用它实际看到的 URI 生成 `Location`（`uri.path()`），剥离后就是 `/assets/`，也就是 API 命名空间。因此 `ServeDir::redirect_path_prefix(mount)` 是必需的；实测 `/…/webui/assets` → `307 Location: /…/webui/assets/`。
- 全局 `fallback` 回归纯 JSON 404：不再需要 `fallback_service` 去抢占同一个 fallback 槽位（旧设计里"嵌套路由会继承外层 fallback"的坑随之消失）。
- 根路径 `307` 跳转到挂载点，保证 UI 仍然可发现；`Role::Worker` 不安装工作台，仍只有 `/metrics`。
- **空前缀也可用**：`api_prefix = ""` 或 `"/"` 时挂载点是 `/webui`，SPA 有明确落脚点，因此删除了旧的"必须非空前缀"校验。

示意代码（`crates/server/src/app.rs`）：

```rust
let mut router = router
    .route("/metrics", axum::routing::get(crate::routers::monitoring::metrics))
    .fallback(|| async { ApiError::route_not_found() })
    .method_not_allowed_fallback(/* 405 且保留 Allow 头 */);

if let Some(workbench) = workbench::Workbench::resolve(config)? {
    let mount = workbench.mount().to_owned();
    let workbench = Arc::new(workbench);
    let index = format!("{mount}/");
    router = router
        .route(
            "/",
            axum::routing::get(move || async move { Redirect::temporary(&index) }),
        )
        .nest_service(
            &mount,
            service_fn(move |request| {
                let workbench = Arc::clone(&workbench);
                async move { Ok::<_, std::convert::Infallible>(workbench.respond(request).await) }
            }),
        );
}
```

`workbench.respond` 只处理挂载内的请求：挂载根与无扩展名的路径回退到入口文档，`/assets` 命名空间始终按文件查找，内容哈希资源不可变缓存，其余（含 404）回源校验。

### 5.2 缓存与压缩头

`ServeDir` 有意不设置 `Cache-Control`。请只用一段小中间件覆盖 Web 服务（不要覆盖 API）：

- `assets/*`（Vite 生成内容哈希文件名）→ `public, max-age=31536000, immutable`
- `index.html` → `no-cache`（必须回源校验，否则新部署不会被拾取）

Vite 也提出了同样的要求（"set `Cache-Control: no-cache` on the HTML file, otherwise the old assets will be still referenced"），并提供 `vite:preloadError` 事件用于处理过期 chunk。若将来引入 Service Worker，在不可变哈希资源下"过期的预缓存外壳"就是失败模式；目前没有 Service Worker。

- `pdfjs/cmaps/*`、`pdfjs/standard_fonts/*`、`pdfjs/wasm/*` → 长缓存；它们只在升级 PDF.js 时变化，而回退到 `ServeDir` 已发出的 `ETag`/`Last-Modified` 校验也是正确的。

压缩：调用 `ServeDir::precompressed_gzip()`（可选再加 `precompressed_br()`），让 `compress-assets.mjs` 已产出的 `.gz` 同伴文件以 `Content-Encoding: gzip` + `Vary: accept-encoding` 提供。`.br` 不需要额外的 tower-http feature（`Encoding::Brotli` 由 `fs` 本身就启用），只需在 `compress-assets.mjs` 里加一步 `brotliCompress`。只有在未压缩文件同时存在时才会使用预压缩变体，本仓库始终满足。tower-http 的 Range 语义是**对选中的表示取范围**：不带 `Accept-Encoding` 时范围作用于原文件并移除 `Content-Encoding`；带 `Accept-Encoding: gzip` 时范围作用于 `.gz` 表示并保留 `Content-Encoding`（RFC 9110 §14.2 的表述即如此）。本仓库真正需要断点续传的是 `/jobs/source` 的 PDF，该路径不使用预压缩变体，且 `CompressionLayer` 会跳过带 `Content-Range` 的响应。现有 `CompressionLayer` 可继续作为没有预压缩同伴文件的资源的兜底；它绝不会再压缩已带编码的响应（`should_compress = !headers.contains_key(CONTENT_ENCODING)`）。

不要把否定结果标记为可缓存：仅依据请求路径设置 `immutable`，会让缺失哈希资源的 `404` 被缓存一年。该头必须按响应状态码判定（附录 B 的原型已验证）。

### 5.3 嵌入模式（`embed-web` Cargo feature）

阶段 2 在 `docparse-server` 上增加一个**非默认** feature：

```toml
[features]
embed-web = ["dep:rust-embed"]
```

嵌入模式实现 `tower_http::services::fs::Backend`，数据来自 `#[derive(rust_embed::Embed)] #[folder = "../../packages/web/dist"] struct WebAssets`，并用 `ServeDir::with_backend("", WebAssetsBackend::new())` 安装，于是路由、MIME、ETag、Range 与预压缩协商都与阶段 1 完全一致。以下要点均已对照源码核实：

- `Backend` 需要 `Metadata`（`is_dir`、`modified`、`len`）以及 `File: AsyncRead + AsyncSeek` —— `Cursor<&'static [u8]>` 满足后者，rust-embed 提供 `sha256_hash()`/`last_modified()` 满足前者。
- `ServeFile` **没有** `with_backend`，因此嵌入模式下的 SPA 兜底必须是一小段 `service_fn`，返回嵌入的 `index.html`（或一个基于 `WebAssets::get("index.html")` 的 axum handler）。预压缩协商在嵌入 backend 下**是可行的**：`ServeDir` 会通过 `Backend::metadata` 查找 `foo.js.gz`，所以只要把现有 `.gz` 同伴文件一并嵌入就够了。
- 优先嵌入已有的 `.gz` 同伴文件，而不是使用 rust-embed 的 `compression` feature。后者每次 `get()` 都会解压，只提供 deflate/zstd（从不提供 gzip/br）且需通过 `compressed()` 获取，debug 动态模式下不可用，在 `compression` 下拒绝绝对路径 `#[folder]`，还会引入 `include-flate` → `proc-macro-error2`（cargo 当前标记为 future-incompatible）。
- 目录缺失：rust-embed 本身就会给出可操作的错误（`#[derive(RustEmbed)] folder '<abs>/dist' does not exist`）。其 `#[allow_missing = true]` 属性（或 `allow_missing` feature）会改为生成**空集合**，这正是让 `--all-features` CI 在没有前端构建时也能通过的方式 —— 代价是应用必须在启动时检测"嵌入了零个资源"并显式失败。
- Debug 行为：未启用 `debug-embed` 时，debug 二进制在运行时读磁盘，但读的是**编译期绝对路径**（README 中"相对于二进制运行目录解析"的说法是错的），因此移动检出目录会破坏 debug 服务。启用 `debug-embed` 则改为快照字节。
- 嵌入过期是最隐蔽的失效：rust-embed 会按文件的绝对规范路径展开一条 `include_bytes!`，因此*内容变化*会触发重编译，但*新增文件*（例如新的未哈希 `pdfjs/**` 资源）只有在 proc macro 重新执行时才会被发现。一个只做 `println!("cargo:rerun-if-changed=../../packages/web/dist")` 的 build script 可以补上这一环；在推荐流水线里 CI 是发布产物的唯一生产者，所以它是可选项而非必需项。
- 优先级必须显式且响亮：默认使用嵌入资源；配置了 `server.web_root`（或 `DOCPARSE_WEB_ROOT`）时，仅当它**存在且目录可用**才覆盖；配置了但路径缺失应视为启动错误，而不是静默回退。Grafana 的"二进制必须以安装路径运行"正是这类坑。
- 供应链：rust-embed 很成熟（5500 万次下载），但已离开 GitHub —— crate 的 `repository` 字段现在指向 `pyrossh.dev/repos/rust-embed`，旧的 GitHub 链接全部 404。对于发布关键路径上的依赖，建议固定版本或 vendor。

如果"代码量最少"优先于"HTTP 语义完全对齐"，阶段 2 的替代方案是 `static-serve` 0.6.4（`embed_assets!` → `static_route(router, ...)`，构建期 gzip+zstd，ETag/304，206/416 Range，`cache_busted_paths`，目录缺失或扩展名无法识别时编译报错）。它是 Axum 0.8 原生、MIT/Apache-2.0 双许可，但**没有 SPA 兜底**，且很年轻（24 star、0.6.x），还会额外引入两个 crate 加 `range-requests`。与 `with_backend` 方案相比，它用约 80 行 backend 代码换来一个新依赖，而该依赖的用户基数要小得多。

### 5.4 配置（最终形态：单个枚举键）

两个互斥的键已合并为一个 `server.webui` 键（serde 外部标记枚举，`WebUi::Disk(PathBuf)` 与 `WebUi::Embedded`）：

```toml
[server]
api_prefix = "/api/v1/docparse"
# 从目录提供：相对路径按配置文件所在目录解析，目录必须含可读的 index.html。
webui = { disk = "packages/web/dist" }
# 或者使用 `--features embed-web` 编译进二进制的构建：
# webui = "embedded"
```

- 挂载点由前缀推导：`{api_prefix}/webui`（空前缀或 `/` 时为 `/webui`），因此不再需要"必须非空前缀"的校验。
- 校验集中在 `ServerConfig::validate()` 与 `Workbench::resolve()`：目录不可用或嵌入集合为空都在启动阶段报错（`field = "server.webui"`），不会静默降级；旧的 `web_root` 键因 `deny_unknown_fields` 被直接拒绝。
- 相对路径与 `log.file`、模型路径一样按配置文件所在目录重定位（`wasm_compat.rs::resolve_paths`）。
- 启动日志同时记录模式与挂载点：

```
INFO docparse_server::workbench: serving the workbench from /…/packages/web/dist at /api/v1/docparse/webui
INFO docparse_server::workbench: serving the workbench embedded in this binary at /api/v1/docparse/webui
```

- 前端不再单独配置挂载路径：`packages/web/vite.config.ts` 用 `VITE_API_PREFIX` 推导 `base = {VITE_API_PREFIX}/webui/`（`VITE_BASE_PATH` 可覆盖），构建产物因此引用 `/api/v1/docparse/webui/assets/...`；dev 代理只转发 API 路由（`jobs`、`monitoring`、`health`、`ready`、`docs`、`openapi.json` 与 `/metrics`），`/…/webui/**` 留给 Vite 自己提供。

之所以仍然要求显式配置来源：只要二进制带 `embed-web` 编译就自动开始提供 UI 的话，任何未显式配置的部署与测试都会悄悄改变 fallback 行为；显式枚举保证"仅 API"在任何特性组合下都是默认值。

### 5.5 体积与许可

基于真实 `dist` 实测（release 构建、strip、rust-embed 8.12）：

- 不压缩嵌入：二进制增加 **8,972,472 字节**（约 +8.6 MiB），构建约 6 秒 —— 相对 75 MB 的 release `docparse-server` 约 +12%。之所以比 6.90 MB 源文件更大，是因为 1.27 MB 的 `.gz` 同伴文件也被嵌入了。
- `rust-embed` 的 `compression = "zstd"`：6,129,736 字节（省 2.8 MiB / 32%），但构建约 16 秒，另有 `include-flate` 的 future-incompat 警告与每次请求的解压开销。
- 仅嵌入预压缩版本（对每个文件做 gzip-6，不保留原文件）约 3.55 MB，代价是只支持 gzip 客户端、没有 `br`。
- 嵌入之前就能省的部分：`dist/assets` 目前同时打包 KaTeX 的 `.woff2`、`.woff`、`.ttf`（1.2 MB）；去掉旧格式并裁剪 `pdfjs/standard_fonts` 可以显著减小体积，与本次决策无关。
- PDF.js 资源（Apache-2.0）将从"nginx 提供"变为"随二进制再分发"。嵌入的 `dist/pdfjs/LICENSE*` 必须保留，并且 `THIRD_PARTY_NOTICES.md` 应注明服务端二进制会再分发这些资源。

## 6. 构建与发布流水线

两步，由编排器串起来 —— 永不通过 `build.rs`：

```sh
npm ci --prefix packages/web && npm run build --prefix packages/web
cargo build -p docparse-server --release --features embed-web --locked
```

### 6.1 为什么不能在 `build.rs` 里跑 `npm`

- Cargo 文档说明 build script 在构建包之前运行；若不写 `rerun-if-*`，cargo 会扫描整个包目录来检测变化；因此会改动自身包目录的 build script 每次构建都会重跑（一类已被复现的重建循环缺陷）。
- `cargo check` 同样会执行 build script，所以 Node 步骤会拖慢每一次 `cargo check`/`cargo test`，以及 rust-analyzer 的每次调用。
- build script 运行在**宿主**上，交叉编译时就需要宿主有 Node，而嵌入的资源本身与目标平台无关。
- `cargo package` 会校验"build script 未修改任何源文件"，然后构建解包后的 `.crate`；crates.io 对 `.crate` 有 10 MB 上限。用 build script 生成资源的做法与打包不兼容，而 7 MB 的资源树本身就逼近上限。
- 社区共识（不是 Cargo 的规则）认为 build script 里的网络访问会破坏离线与 hermetic（`--network=none`）构建。

可用的编排器：`crates/xtask` 二进制（workspace 已是 `members = ["crates/*"]`，而 `default-members` 不含它，所以普通 `cargo build` 会忽略它 —— 在已存在的 `.cargo/config.toml` 里加 `[alias] xtask = "run --package xtask --"`）、`just`、`cargo-make`、`Makefile` 或 CI。Tauri 的 `beforeBuildCommand` 是"由工具接管前端步骤"的最接近先例。注意 `required-features` 只对 `[[bin]]`/`[[test]]`/`[[bench]]`/`[[example]]` 生效，**对 `[lib]` 无效**，所以 lib 代码要用 `#[cfg(feature = "embed-web")] mod web_assets;` 门控。

### 6.2 嵌入的过期问题与失效模式

- `rust-embed` 会把相对的 `#[folder]` 按 `CARGO_MANIFEST_DIR` 解析，并在目录缺失时给出清晰错误（`#[derive(RustEmbed)] folder '<abs>/dist' does not exist`）。`include_dir!` 会 panic 且信息不如它明确，也没有"目录缺失"的逃生开关，这是优先选 `rust-embed` 的又一理由。
- `#[allow_missing = true]` 会把目录缺失变成**空资源集合**，因此应用仍必须在启动时检测"没有资源"并显式失败，而不是返回一片 404。
- 嵌入会让每个文件成为一条 `include_bytes!` 依赖，因此*内容变化*会触发重编译；*新增文件*（例如新的未哈希 `pdfjs/**` 资源）只有在 proc macro 重新执行时才被发现。若在意这一点，可加一个只做 `println!("cargo:rerun-if-changed=../../packages/web/dist")` 的 build script。它是可选项：在推荐流水线中 CI 是发布产物的唯一生产者，因此不会出现过期嵌入；该守卫的唯一代价是让一个（只打印指令的）build script 在 `cargo check` 时也会执行。
- 有意保留：由于 `cargo package` 会跳过被 gitignore 的文件，`dist` **不会**被打包。若加 `include = [..., "/../../packages/web/dist"]`，每次发布都会被要求 `--allow-dirty`，因此服务端 crate 应保持不发布（`publish = false`），而不是试图把 SPA 塞进 crate。

### 6.3 容器镜像

Node 阶段（`npm ci` + `npm run build`）→ Rust 阶段（`cargo build --release -p docparse-server --features embed-web --locked`，`dist` 从阶段 1 复制）→ 最终 `gcr.io/distroless/cc-debian12` 镜像，嵌入模式下只含二进制。若不用嵌入，最终阶段还必须复制 `dist`（含 `.gz` 同伴文件）并设置 `server.web_root`。`.dockerignore` 没有 `.gitignore` 兜底，因此必须显式列出 `/target`、`**/node_modules` 和 `packages/web/dist`。

### 6.4 CI 形态（待引入 CI 后）

- Job A：`cargo check --locked` + 测试（不需要 Node），带 Rust 缓存。
- Job B：Node，使用 `actions/setup-node` 的 npm 缓存、`npm ci`、`npm run build`，把 `dist` 作为 artifact 上传。
- Job C：下载那份确切的 `dist` artifact，构建 `--features embed-web --locked`。直接传 artifact（而不是重新构建）才能让被嵌入的字节无歧义；注意对 gitignore 的目录做 `git diff --exit-code` 过期检查是无效的。
- 校验步骤：启动二进制并断言 §5.1 的路由矩阵 —— `/document` 与 `/` 返回外壳，`/api/v1/docparse/unknown` 以及裸前缀/尾斜杠前缀返回 JSON 404，缺失的 `/assets/*.js` 返回真实 404，`.gz` 同伴文件返回 `Content-Encoding: gzip` + `Vary`，`Range` 请求返回 `206`。

### 6.5 可选增强

- 在 `vite.config.ts` 中开启 `build.manifest: true` 会生成 `.vite/manifest.json`（当前没有），可用于在启动时推导入口/CSS 的 preload 头。正确提供服务并不需要它。
- 若需要可复现的嵌入元数据，可固定构建时间戳（`rust-embed` 的 `deterministic-timestamps` / `SOURCE_DATE_EPOCH`），让 ETag 在相同重建之间保持稳定；Vite 的内容哈希已让资源字节稳定。
- Vite 8 没有 SRI 构建选项，因此哈希文件名只提供缓存失效能力，不提供完整性校验。

## 7. 风险与坑

| 风险 | 为什么会发生 | 缓解 |
| --- | --- | --- |
| API 客户端收到 HTML | 嵌套路由会继承外层 fallback，且裸前缀/尾斜杠不会命中嵌套路由自身路由 | Web fallback 必须自带前缀判断，同时 API 子树内保留通配 JSON 404（先例：某真实项目曾让 `/api/v2` 返回 `200 text/html` 的 Web 外壳） |
| 缺失资源的 404 被缓存一年 | 仅按请求路径设置 `immutable` | 仅在 2xx 状态下设置 `immutable`（原型发现） |
| 现有 JSON 404 静默消失 | `fallback` 与 `fallback_service` 共用一个 `catch_all_fallback` 槽位 | 增加断言 `GET /api/…/unknown` 返回 JSON 信封的测试 |
| SPA 深链因 4xx 被中间件改写（仅当将来再加 ingress 时） | 部分代理会拦截上游 4xx（Traefik `errors`、nginx `proxy_intercept_errors`、Caddy `handle_errors`、Cloudflare 错误页） | 本项目不引入 ingress，故默认选 200 + 外壳；若将来加代理，需重新评估 404 策略 |
| `/metrics` 等非 SPA 路径被外壳吞掉 | fallback 若不判断路径，任何未匹配请求都会拿到 `index.html` | 前缀判断 + 对 `/metrics` 等路径显式放行（§5.1、附录 B） |
| TLS / HTTP 版本无人承担 | 去掉 nginx 后 TLS 终止与 HTTP/2、HTTP/3 不再有人做 | 明确决策：内网明文够用则不动；需要 HTTPS 则选 `axum-server` + rustls 或另加 TLS 终结器（§3.3） |
| 缺失的哈希资源返回 200 + `index.html` | 天真地使用 `not_found_service(ServeFile::new(index))` | 兜底前检查扩展名/`Accept` |
| `api_prefix = ""` | API 与 SPA 都宣称拥有 `/` | 在配置校验中拒绝该组合 |
| release 里嵌入了过期的 `dist` | 构建顺序被忽略；新增文件名只有在 proc macro 重跑时才可见 | build script 加 `cargo:rerun-if-changed=packages/web/dist`；feature 开启但 `dist` 缺失时构建失败；在 CI 中校验 |
| 嵌入模式在 debug 下正常、release 下全 404 | tower-http 以 `""` 为 base 解析出的路径带 `./` 前缀；debug 构建从文件系统读取并容忍它，release 构建按精确文件名匹配则查不到 | backend 必须把路径**按组件归一化**成 `assets/…` 形式，并拒绝 `.`/`..`/根；用单元测试锁住该规则（本轮真实缺陷，见附录 C） |
| 二次压缩 | 在 `CompressionLayer` 之下提供 `.gz` 同伴文件 | tower-http 0.7 下安全（带 `Content-Encoding` 的响应永不再压缩；旧的重复压缩缺陷已修复） |
| `index.html` 被永久缓存 | 哈希资源不可变，但入口文档不是 | `index.html` 用 `no-cache`，只有 `assets/` 下用 `immutable` |
| 二进制膨胀 | 6.9 MB 原始资源 | 嵌入预压缩变体；裁剪不需要的字体格式与字体资源 |
| SPA/API 版本错配 | 同源、两套产物 | 单一产物消除错配 —— 这是做嵌入的主要理由 |
| 新层序影响 SSE/上传 | 静态服务被放在 API 路由之前或之外 | 只作为路由 fallback 挂载；`ServeDir` 不会触碰这些路由，`CompressionLayer` 也已排除 `text/event-stream` 与 `application/pdf` |
| 许可与来源漂移 | PDF.js 声明现在随二进制分发 | 保留 `dist/pdfjs/LICENSE*` 嵌入；更新 `THIRD_PARTY_NOTICES.md` |

## 8. 分阶段计划

> 以下步骤**本轮不执行**（决策见 §9），仅作为将来实施时的依据。

1. **阶段 1（小改动、零新依赖）：** 从磁盘提供 SPA。新增 `server.web_root` 配置、`crates/server` 中的 `web` 模块（`crates/web` 名字已被 wasm 绑定 crate 占用）、API 通配 JSON 404、带前缀判断的 `ServeDir` fallback 服务并使用 `precompressed_gzip`、按状态码判定 `Cache-Control` 的中间件，以及覆盖附录 B 全部路由矩阵的集成测试。
2. **阶段 2（可选、feature 门控）：** `embed-web` feature + 基于 `rust-embed` 的 `Backend`、嵌入的 `index.html` 兜底、记录当前模式的启动日志。增加 `crates/xtask`（或等价物）编排器，把两步构建收敛为一条有文档的命令。
3. **阶段 3（仅在部署确实需要时）：** 处理 §3.3 遗留的 TLS 与 HTTP 版本问题（服务端内建 rustls，或另行引入 TLS 终结器），以及在需要 CDN 时才把 `dist` 外移。**不会引入 nginx。**

阶段 1 与阶段 2 共享同一套路由、响应头与测试，因此阶段 2 是增量而非重写 —— 这也是不要一上来就用第三方 embed 宏的主要论据。

## 9. 决策记录与实施结果

**已确认的决策（2026-09-28）：**

1. **不引入 nginx**（或任何外部静态托管/ingress）。因此采用进程内托管，模式 A/D/E 出局；`packages/web/nginx.conf.example` 已随该决策删除（可从 git 历史恢复）。
2. **服务定位为本机/受信内网，明文 HTTP**，不处理 TLS 与 HTTP/3（§3.3 对应行结案）。
3. **开始实施**：阶段 1（磁盘）与阶段 2（嵌入）均已实现。

**实施结果（与本文档原始设计的差异）：**

- 落地为两个**互斥**的配置键，而不是"嵌入作为 `web_root` 缺省时的兜底"：
  - `server.web_root = "packages/web/dist"`：从磁盘提供（可热换资源）。
  - `server.web_embedded = true`：使用 `--features embed-web` 编译进二进制的构建。
  之所以要求显式开关，是因为若"编译了特性就自动开始提供 UI"，那么在带 `embed-web` 编译的二进制上，任何未显式配置的部署/测试都会悄悄改变 fallback 行为；显式开关让默认行为在任何特性组合下都保持"仅 API"。
- 入口文档按来源分别处理（第七轮已统一为同一条路径）：**磁盘模式**每次导航都重新读取文件，因此替换 `index.html` 无需重启，并天然获得 `Last-Modified`/`If-Modified-Since`/`HEAD`；**嵌入模式**使用内存字节 + blake3 `ETag`。这一拆分来自代码评审：早期两个模式共用一份启动时读入的内存副本，导致磁盘模式热换 `index.html` 后 `/` 是新版本、`/document` 仍是旧版本（附录 C 有实测记录）。
- 资源请求走 `ServeDir` 的 `Service` 实现（而非 `try_call`），这样文件系统错误会按 tower-http 的规则变成 404/403/500 响应，而不是把原始 `io::Error` 抛给本模块。
- 配置校验落在 `ServerConfig::validate()`：`web_root` 与 `web_embedded` 不得同时设置；启用任一者时 `api_prefix` 必须非空。目录不可用或嵌入集合为空都在启动阶段报错。
- 覆盖路由矩阵的集成测试位于 `crates/server/tests/workbench.rs`（16 个用例，按行为拆分），并在两种特性组合下均通过；纯配置规则的两条用例移到 `crates/config/tests/validation.rs`。

**第五轮（需求变更：UI 挂载到 `{api_prefix}/webui`，配置合并为枚举）：**

13. **配置合并为一个枚举键**：`server.web_root` + `server.web_embedded` → `server.webui`（`{ disk = "..." }` 或 `"embedded"`）。旧键现在被 `deny_unknown_fields` 拒绝，`crates/config/tests/loading.rs` 中读取仓库配置的两个用例正好把这一点锁住。
14. **路由改为挂载式**：工作台经 `nest_service` 挂在 `{api_prefix}/webui`，`/` 以 307 跳转到挂载点，未匹配路径回到纯 JSON 404。随之删除前缀推断整条链路（`decode_request_path`、`canonical_path`、`is_api_path`、`DECODE_PASSES`、`reject_shadowing_prefix`、`top_level_names`）——`%252F`、`%FF` 这类输入不再可能落进外壳，因为它们根本到不了外壳。目录 307 用 `redirect_path_prefix(mount)` 补回挂载前缀。
15. **前端尊重前缀**：`vite.config.ts` 的 `base` 由 `VITE_API_PREFIX` 推导为 `{VITE_API_PREFIX}/webui/`，构建产物引用 `/api/v1/docparse/webui/assets/...`；dev 代理从"整个前缀"收窄为"API 路由白名单"，否则 Vite 会把 UI 路径也代理给后端。`.env*` 去掉 `VITE_BASE_PATH=/`，四个 e2e 脚本与 `packages/web/README.md` 的地址同步到挂载路径。

**第六轮（把例行分类提升为子树中间件）：**

16. **例行标记从手工插入改为子树中间件**：原先 `Workbench::respond` 在 2xx/304 响应上插入 `middlewares::RoutineCompletion`，属于业务代码里重复声明分类策略。现在改为 `middlewares::routine` 模块中的 `mark_routine`（`axum::middleware::from_fn` 形态，实现独立成 `crates/server/src/middlewares/routine.rs`，由 `mod.rs` re-export）——把"哪些状态码算例行"这条策略集中在中间件里，路由子树用 `.layer(middleware::from_fn(middlewares::mark_routine))` 声明一次，工作台不再感知标记。工作台挂载点用 `ServiceBuilder` 包一层，因此**只有挂载子树被标记**：实测 `…/webui/` 与 `…/webui/assets/*.js` 的完成日志为 `DEBUG`，缺失资源 404、未知 API 404 与 `/metrics` 仍为 `INFO`。集成测试新增 `routine_marking_follows_the_response_status`，直接断言 200/304 带标记、404 不带。
    - 为什么仍然用"响应扩展标记"而不是让追踪层按路径前缀自行跳过：同一条路径既可能成功也可能失败（资源存在 vs 缺失、200 vs 500），前缀规则会把失败一起降级；标记承载的是**响应状态**判断。也不存在 axum 的"逐路由元数据"API，子树级 layer 是等价且更贴合边界的写法。

**第七轮（评审修复）：**

17. **e2e 脚本的地址语义拆开**：上一轮把四个 `*.mjs` 的默认 `origin` 改成挂载路径，但脚本同时用 `origin` 拼 API URL，于是出现 `/api/v1/docparse/webui/api/v1/docparse/health` 这种路径——它落在挂载内、无扩展名，服务器按 SPA 深链返回 200 外壳，`health.json()` 直接抛异常。现在 `origin` 恢复为服务根（只用于 API），新增 `webui = origin + api_prefix + "/webui"`（只用于页面导航），README 的调用示例同步。
18. **README 启动命令与仓库配置一致**：仓库 `docparse.toml` 选择 `webui = "embedded"`，但 `crates/server/README.md` 与 `packages/web/README.md` 的启动命令都没有 `--features embed-web`，照文档启动会立即以 `server.webui: requires building docparse-server with the embed-web feature` 退出；两处已补上并说明何时可去掉。
19. **删除未使用依赖 `percent-encoding`**：它是上一轮"解码边界"逻辑的遗留，挂载式改造后源码 0 引用，已从根与 server 的依赖表中移除（`Cargo.lock` 同步更新，传递依赖不变）。
20. **删除死代码**：`Workbench::respond` 里的防御性 `strip_prefix` 在 `nest_service`（内部 `StripPrefix`）下永远是无操作，而在挂载文本重复时（`api_prefix = ""` 时 `GET /webui/webui/assets/x.js`）会二次剥离、把缓存策略算到错误的命名空间上；已删除并更正注释。
21. **入口文档统一走文件服务**：删掉 `Entry`/`EmbeddedEntry` 两套实现（约 -120 行），导航回退改为把请求重写成 `/index.html` 交给同一个 `ServeDir`。效果是两种来源在入口文档上完全一致（实测：`HEAD` 的 `Content-Length: 659`、`Range: bytes=0-9` → `206`、`If-Modified-Since` → `304`、`Content-Type: text/html`，在 `{mount}/` 与 `{mount}/document` 两个形态上都成立；修复前嵌入模式的对应值是"无 Content-Length / 200 全量 / 200"）。"入口文档启动后消失"仍然返回 `500` + ERROR 日志。
22. **`webui = { disk = "." }`（或空串）现在在配置加载阶段被拒绝**：相对路径按配置文件目录解析后，`.` 会让**配置文件所在目录**成为静态根，实测旧行为下 `GET {mount}/docparse.toml` 返回 200，内容含 `[database]` 连接串；现在报 `InvalidValue { field: "server.webui", reason: "must name a build directory rather than the current directory" }`。
23. **`/` 的 307 保留 query**：`GET /?job=42&page=3` → `Location: /api/v1/docparse/webui/?job=42&page=3`，旧方案留下的书签不再丢参数。
24. **三处过期文档/措辞**：`validate.rs` 的 `validate()` 文档不再声称校验"workbench source pairing"；`error.rs` 的 `route_not_found` 文档不再声称与工作台共用响应；`packages/web/README.md` 的 "needs no `dist` at runtime" 改为区分 release（编译进二进制）与 debug（rust-embed 仍读编译期目录），并把"every other path below the prefix"更正为"挂载点之外"。
25. **测试**：workbench 17 → 20（入口文档的 `HEAD` 长度与 `Range`、运行时入口消失 → 500、空前缀端到端挂载 `/webui`），config 13 → 14（拒绝 `.`/空 disk），嵌入模式测试补上 `If-None-Match` → 304、`Range` → 206、`HEAD` 长度断言（此前嵌入入口的条件请求分支从未被执行）。

**遗留问题：**

1. 是否顺带处理 §5.5 的体积优化（裁剪 KaTeX 的 `.woff`/`.ttf`、裁剪 `pdfjs/standard_fonts`）——与是否嵌入无关，但能显著减小两种模式的传输与镜像体积。
2. 开发流程继续沿用 Vite dev server + API 代理（当前行为），服务端不反向代理 Vite 进程。

## 附录 A：关键结论的核实方式

- crate 版本与发布日期：`crates.io` API（`rust-embed` 8.12.0、`axum-embed` 0.1.0/2023-12-17 且依赖 `axum-core ^0.4`、`static-serve` 0.6.4 依赖 axum ^0.8、`rust-embed-for-web` 11.4.1、`axum-asset` 0.3.0、`include_dir` 0.7.4、`tower-http` 0.7.1）。
- tower-http 行为：0.7.1 源码（`src/services/fs/serve_dir/{mod,headers,backend,open_file,future}.rs`、`src/compression/service.rs`）—— `Backend`/`with_backend`、通过 backend 解析预压缩变体并设置 `Vary`、由 size+mtime 生成的强 ETag、`Last-Modified`、Range 处理、不设置 `Cache-Control`、不重复压缩已编码响应。
- axum 行为：0.8.9 源码（`src/routing/mod.rs`）—— `nest` 保留嵌套路由的 fallback；`fallback` 与 `fallback_service` 共用唯一的 `catch_all_fallback` 槽位；`nest_service("/")` 会 panic 并指向 `fallback_service`。通配语义：`matchit` 0.8.4 —— `/{*rest}` 不匹配 `/`。
- utoipa 行为：`utoipa-axum` 0.3.0 `src/router.rs` —— `OpenApiRouter::route` 是直通方法，不修改 OpenAPI 文档；`nest` 会同时改写路由与文档路径。
- 本地数字：对 `packages/web/dist` 执行 `du`/`find`；`target/release/docparse-server` 为 75,075,648 字节。
- 嵌入成本用 rust-embed 8.12 在真实 `dist` 上实测（release、strip）：原始 8,972,472 字节；`compression = "zstd"` 为 6,129,736 字节，构建约 16 秒 vs 约 6 秒。
- 先例与坑均对照一手资料核实：Meilisearch 的
  [`build.rs`](https://github.com/meilisearch/meilisearch/blob/main/crates/meilisearch/build.rs)
  与 [Cargo.toml](https://github.com/meilisearch/meilisearch/blob/main/crates/meilisearch/Cargo.toml)；
  [Grafana 配置](https://grafana.com/docs/grafana/latest/setup-grafana/configure-grafana/)；
  [Vaultwarden `.env.template`](https://github.com/dani-garcia/vaultwarden/blob/main/.env.template)；
  [Spring Boot 静态内容](https://docs.spring.io/spring-boot/reference/web/servlet.html)；
  [`//go:embed`](https://pkg.go.dev/embed)；
  [Express 5 迁移指南](https://expressjs.com/en/guide/migrating-5.html)；
  [Vite 构建指南](https://vite.dev/guide/build.html)；
  [nginx `gzip_static`](https://nginx.org/en/docs/http/ngx_http_gzip_static_module.html)；
  [tower-http #138](https://github.com/tower-rs/tower-http/issues/138)；
  axum 的 [static-file-server 示例](https://github.com/tokio-rs/axum/blob/main/examples/static-file-server/src/main.rs)；
  [Cargo build-script 参考](https://doc.rust-lang.org/cargo/reference/build-scripts.html)
  与 [`cargo package`](https://doc.rust-lang.org/cargo/commands/cargo-package.html)；
  [cargo-xtask](https://github.com/matklad/cargo-xtask)；
  [Docker 多阶段构建](https://docs.docker.com/build/building/multi-stage/)；
  以及两起路由坑的真实案例
  （[gptme #2532](https://github.com/gptme/gptme/pull/2532)、
  [Mealie #7656](https://github.com/mealie-recipes/mealie/pull/7656)）。

## 附录 B：§5.1 接线的原型验证结果

在仓库之外搭了一个临时 crate（`/tmp/webembed-check`，axum 0.8 + tower-http 0.7，把 `ServeDir::precompressed_gzip()` 包在实现 SPA 策略的 `service_fn` 里），编译后用 `curl` 逐条探测。第一版仅靠路由做隔离，失败两次；修正版全部通过。这也是 §5.1 强制要求"带前缀判断的 fallback"、§5.2 要求"按状态码判定缓存头"的依据。

修正版实测响应：

| 请求 | 结果 |
| --- | --- |
| `GET /` | `200`、`text/html`、外壳、`ETag`、`Last-Modified`、`Cache-Control: no-cache` |
| `GET /document` | `200` 外壳（SPA 深链可用） |
| `GET /monitoring` | `200` 外壳（无扩展名的导航请求） |
| `GET /assets/app-DEADBEEF.js` + `Accept-Encoding: gzip` | `200`、`text/javascript`、`Content-Encoding: gzip`、`Vary: accept-encoding`、`Content-Length: 65`（即 `.gz` 同伴文件） |
| `GET /assets/app-DEADBEEF.js`（不带 gzip） | `200`、`Content-Length: 29`（原文件） |
| `GET /assets/missing.js` | `404` 且 `Cache-Control: no-cache` —— **不是**外壳，也不会被缓存 |
| `GET /api/v1/docparse/unknown` | `404 application/json` 带类型信封 |
| `GET /api/v1/docparse` 与 `/api/v1/docparse/` | `404 application/json` 带类型信封 |
| `HEAD /document` | `200` 带校验头、空 body |
| 资源的 `Range: bytes=0-6`（不带 `Accept-Encoding`） | `206`，`Content-Range: bytes 0-6/29`，无 `Content-Encoding` |
| 资源的 `If-None-Match` | `304`，带 `ETag` + `Last-Modified` |
| `POST /nope` | `405 Method Not Allowed`，`Allow: GET,HEAD` |

第一版暴露的两个缺陷（现已分别写入 §5.1 与 §5.2）：

1. **`GET /api/v1/docparse/` 返回了 `200 text/html` 的 SPA 外壳。**
   axum 的 `nest` 不会把*带尾斜杠*的裸前缀路由进嵌套路由自身的 `/` 路由，于是请求落到了外层 Web fallback。仅在 API 子树里加通配路由并不足够；fallback 自身必须把 `path == api_prefix || path.starts_with(api_prefix + "/")` 视为 API 路径。这与 gptme 案例报告的是同一类缺陷。
2. **缺失的哈希资源返回 `404`，却带上了
   `Cache-Control: public, max-age=31536000, immutable`**，因为响应头只依据请求路径决定。被缓存的否定结果会比修复本身活得更久。该头必须按成功状态码判定。

## 附录 C：真实服务的端到端验证（实施后）

用真实 `docparse-server` 二进制 + 真实 `packages/web/dist` + 本机 PostgreSQL 实测，而不是只用测试夹具。

**改造后（第五轮）**：真实二进制 + 真实 `packages/web/dist` + 本机 PostgreSQL，磁盘模式（debug 二进制、`webui = { disk = "..." }`、端口 8093）与嵌入模式（release 构建 `--features embed-web`、`webui = "embedded"`、端口 8091）**结果完全一致**：

| 请求 | 结果 |
| --- | --- |
| `GET /` | `307` → `Location: /api/v1/docparse/webui/` |
| `GET /api/v1/docparse/webui/`、`…/webui`（无尾斜杠） | `200 text/html`（真实外壳，`<title>DocParse · 文档工作台</title>`）、`ETag`、`Cache-Control: no-cache` |
| `GET /api/v1/docparse/webui/document`、`…/monitoring` | `200 text/html`，SPA 深链可用 |
| `GET /api/v1/docparse/webui/assets` | `307` → `Location: /api/v1/docparse/webui/assets/`（挂载前缀被保留） |
| `GET /…/webui/assets/index-BNoBnnLD.js` + `Accept-Encoding: gzip` | `200 text/javascript`、`Content-Encoding: gzip`、`Vary: accept-encoding`、`immutable` |
| `GET /…/webui/pdfjs/wasm/jbig2.wasm` | `200 application/wasm` |
| `GET /api/v1/docparse/unknown`、裸前缀、`/webui`、`/document` | `404 application/json`（带类型信封；`/document` 不再是外壳） |
| `GET /api/v1/docparse/health` | `200 application/json` |
| 启动日志 | `serving the workbench … at /api/v1/docparse/webui`（磁盘/嵌入两种文案） |

嵌入二进制（release）确认包含新 bundle（`grep -c index-BNoBnnLD.js` = 4），两种模式都从各自来源提供同一份构建；前端构建产物本身引用 `/api/v1/docparse/webui/assets/...`（由 `VITE_API_PREFIX` 推导）。

**与旧形态的对照**：下文第 1–4 轮的历史记录中的路径（`/`、`/document`、`/assets/...`）描述的是**挂载改造之前**的形态，保留作为评审过程记录，不再代表当前行为。

**代码评审又发现并修复的三项（均已验证）：**

1. **仓库门禁 `scripts/check_wasm_compat.py` 失败**：该脚本拒绝允许清单之外的所有 `#[cfg]`（原本用于约束平台条件编译），而本次新增的 9 处 `#[cfg(feature = "embed-web")]` 全部命中。修复方式是按现有先例（`crates/core/src/pdfium/mod.rs` 的"可选 IPC 实现"注释）把 `crates/server/src/workbench.rs` 与 `crates/server/tests/workbench.rs` 加入允许清单并注明原因；服务端 crate 是原生专用，这些 feature cfg 不会进入浏览器目标的编译。门禁现已通过。
2. **磁盘模式热换 `index.html` 后深链仍是旧版本**（实测：替换磁盘上的 `index.html` 后 `/` 返回新外壳，`/document` 仍返回旧外壳）。根因是入口文档在启动时读入内存。修复为上面的"按来源分别处理"，并新增回归测试 `replaced_entry_document_is_served_without_restart`；真实进程复测 `/`、`/document`、`/monitoring` 三者一致返回新外壳。
3. **静态资源占满 INFO 日志**：请求追踪中间件对每个响应都打一条 INFO（`body completed: N bytes`）。之前由 nginx 提供静态文件时不会经过该中间件，改为进程内托管后一次页面加载会写入几十条 INFO，而日志文件不轮转。修复为：给"例行"响应打上 `middlewares::RoutineCompletion` 标记（第六轮已改为由 `middlewares::routine` 中间件在子树级自动注入），追踪中间件对这类"例行完成"降到 `DEBUG`（失败仍为 `WARN`，API 请求仍为 `INFO`）。实测：一次请求 `/`、`/document`、两个资源、一个缺失资源、`/metrics` 与一个未知 API 路径后，INFO 日志只留下 `/metrics` 与 API 404 两条。

**代码评审（第二轮）提出并已修复的四项：**

4. **`server.web_root` 是唯一按进程工作目录解析的相对路径**：仓库既有策略（`crates/config/src/wasm_compat.rs` 的 `resolve_paths`）把 `log.file` 与所有模型路径重定位到配置文件所在目录，注释写明"与服务启动目录无关"；`web_root` 之前不在其中，systemd/容器/`--config /etc/...` 启动时会静默提供另一个目录。修复为加入 `resolve_paths`，并更新 `config.rs` 字段文档、`docparse.toml` 与 `packages/web/README.md`；`crates/config/tests/validation.rs` 新增用例断言相对路径被重定位。
5. **百分号编码的斜杠绕过 API 边界**（`GET /api/v1/docparse%2Funknown` 返回 `200 text/html` 外壳）：判断改为在**解码后**的路径上进行，并把 `%2F` 形式写进集成测试。为此把 `percent-encoding` 声明进根 `[workspace.dependencies]`（它本就在依赖图中，不增加编译成本）。
6. **静态资源的 4xx/5xx 被降到 DEBUG**：`RoutineCompletion` 之前标记了**所有** Web 响应，而追踪中间件只在"body 正常读完"时降级，与状态码无关——缺失资源 404 与入口文档 500 因此从 INFO/WARN 消失。修复为仅 2xx 与 304 标记为例行响应；实测一次页面加载 + API 请求后，INFO 日志只剩非例行响应。
7. **启动只检查 `is_file()`，不能证明入口文档可读**：改为 `File::open`，权限问题在启动即报错，而不是在请求时被 tower-http 映射成 404 后报"entry document disappeared"。

另外采纳的清理项：模块 `web` 更名为 `workbench`（避免与 `crates/web`（wasm 绑定）和 `packages/web`（前端）三者混淆）、`pub(crate)` 收窄可见性、删除 `crates/server/Cargo.toml` 中重复的 `tower` dev-dependency、把 `reason` 文案中的构建命令移到文档、`/assets/` 不再返回外壳、缓存策略把 304 视同已服务、测试按行为拆分并把 `TempDir` 与 router 生命周期对齐。

**第三轮（对抗性复核）修复的两项：**

8. **非规范路径仍可漏进外壳**：`//api/v1/docparse/unknown` 与 `/./api/v1/docparse/unknown` 之前会被当作非 API 路径并返回 `200 text/html`（tower-http 会折叠重复分隔符、跳过 `.` 段再打开文件）。修复为在**解码 + 规范化**（折叠空段/`.`、按 `..` 回退）后判定边界，任一形式命中前缀都保留 JSON 信封；同时 `/assets`（无尾斜杠）不再被当作导航。对应测试覆盖 `//api/...`、`/./api/...`、`//assets/`。
9. **前缀与工作台命名空间冲突**：例如 `api_prefix = "/assets"` 会把 SPA 自己的 bundle 判成 API 请求，结果是一张空白页而不是启动错误。修复为启动时比较前缀首段与工作台顶层条目（磁盘模式读目录、嵌入模式遍历嵌入键），冲突即以 `server.api_prefix` 报错。另将入口文档检查收紧为"必须是文件且可打开"，避免 `mkdir dist/index.html` 之类通过启动检查。

**第四轮（对抗性复核的残留项）修复的三项：**

10. **双重编码与非法转义仍会漏进外壳**：复核用 `curl --path-as-is` 证明 `%252F`、`%FF`、`%00` 会得到 `200 text/html`。修复为**有界迭代解码**（最多 3 轮）：任一阶段命中前缀、出现非法 UTF-8、迭代到上限仍有转义残留、或解码结果含控制字符时，一律保留 JSON 信封；只有可打印且规范的路径才可能成为客户端路由。真实进程复测：`%252F`、`%FF`、`%00` 现在都返回 `404 application/json`。
11. **嵌入模式的目录行为与磁盘不一致**：`/assets`（无尾斜杠）在磁盘模式是 `307 → /assets/`，嵌入模式却是 `404`，因为嵌入 `Metadata::is_dir()` 恒为 false。修复为按嵌入键推导目录前缀并返回目录元数据，两个模式现在都发出 `307`；两种模式的集成测试都加了断言。
12. **`canonical_path` 注释不准确**：原文说"像 file service 一样折叠 `..`"，而 tower-http 对 `.`/`..` 是直接拒绝。注释改为说明这是本模块的保守规范化，行为是安全超集。

另外，`AGENTS.md` 第 1 条已按用户要求更新为：**代码注释必须用英文，`docs/` 下的文档必须用中文**，其他项目文档（README、规范、计划）仍用英文。本文档因此不再需要豁免说明。

**关于 Range 与预压缩的一个记录**：带 `Accept-Encoding: gzip` 的范围请求会对**被选中的 gzip 表示**取范围并保留 `Content-Encoding`（RFC 9110 的范围语义如此）；真正的断点续传场景是 `/jobs/source` 的 PDF，该路径不使用预压缩变体，且 `CompressionLayer` 会跳过带 `Content-Range` 的响应。

**本轮真实缺陷（已修复并锁定）：** 嵌入模式首测时入口文档正常、其余资源全部 404。原因是 `ServeDir::with_backend("", backend)` 解析出的路径带 `./` 前缀（`./assets/index-….js`）：debug 构建由 rust-embed 从文件系统读取、容忍该前缀，而 release 构建按嵌入文件清单**精确匹配**，因此查不到。修复方式是在 backend 内按路径组件重建规范键（丢弃 `.`、拒绝 `..`/根/前缀），并补上单元测试；同时给嵌入模式的 `ServeDir` 加上 `precompressed_gzip()`，让嵌入的 `.gz` 同伴文件真正生效。

**验证命令：**

```bash
cargo fmt --all -- --check
cargo clippy -p docparse-server --locked --all-targets -- -D warnings
cargo clippy -p docparse-server --locked --all-targets --features embed-web -- -D warnings
cargo test -p docparse-config --locked
cargo test -p docparse-server --locked                    # 55 passed, 11 ignored（DB/PDFium 环境相关）
cargo test -p docparse-server --locked --features embed-web
```

注：`tests/pdfium_pool.rs` 需要同目录下存在 `docparse-pdfium-worker` 二进制；单独的 `cargo test -p docparse-server` 不会构建它。先执行 `cargo build -p docparse-core --features pdfium-ipc --bin docparse-pdfium-worker`，或按 README 运行整个工作区的 `cargo test --locked`，该目标即为 14 passed。
