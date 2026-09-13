# Python 工具清理与模型下载

- 删除旧水印审计、静态视觉报告（及其测试）和无人调用的许可证同步脚本；把 layout 图片 fixture 生成器移到对应 fixture 目录，保留正在使用的独立模型 oracle、原生 E2E 对比及 WASM 验收服务。
- 根目录统一使用 pyproject.toml、uv.lock 和 Python 3.12。默认环境无第三方依赖；dev 组承载 PDF fixture/E2E 工具，reference 组锁定现有模型对照环境。移除分散的 PEP 723 依赖声明和单脚本 lock。
- 下载器默认检查全部五个模型，默认目录为仓库 models；使用 --model 选择单个模型、--models-dir 指定模型根目录。已有且校验通过的文件保持不变，只下载缺失或损坏文件，最后写入 manifest；失败不发布未验证下载。
- 同步 README、pre-commit、Web E2E 启动器和 Python 子进程，统一由 uv 或其已管理的 Python 解释器执行。
- 验证默认全部选择、部分缺失修复、不重复下载、hash 错误、verify-only 和 force；实际执行默认下载命令确认本机已有模型均跳过，并跑保留脚本的必要检查。

## 完成与验证

- 删除 4 个过时 Python 文件（含旧视觉报告测试）和 4 个单脚本 lock；迁移 layout fixture 生成器，保留 21 个仍有用途的 Python 文件。
- 下载器 8 项测试、兼容边界 7 项测试、真实模型 oracle 4 项测试通过；E2E 预检及规范结果比较测试通过。
- 默认命令实际检查了本机 5 个模型，全部跳过；在临时目录真实下载缺失 YAML，确认权重不被改写，第二次执行跳过。
- 统一环境成功安装所有依赖组；移动后的 4 张 layout fixture 与原始文件逐字节一致；uv 管理的 Web 验收服务可正常启动、提供页面并退出。
- 修正保留 E2E 工具的 TSR 路径、运行目录影响配置指纹及 Cargo 错误输出问题。真实 PDF 的 release 串行/并行各运行一次，规范结果零差异。
- 预提交检查通过；兼容检查排除 uv 的第三方 .venv 目录。未创建新的 commit。
