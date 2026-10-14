# docparse-cli

Command-line entry points for `docparse parse` and `docparse inspect-model`. Supports JSON and Markdown output, raw/semantic Markdown views, atomic file output, and PNG/SVG diagnostics.

The package builds the `docparse` executable, not a `docparse-cli` executable.
`cargo build --locked --release -p docparse-cli` writes it to `target/release/`;
use that path or install it explicitly before calling it from `PATH`.

Native `cuda`, `tensorrt`, `coreml`, `metal`, and `openvino` features select the
shared model backend; omitting them selects CPU. TOML does not accept
`execution_provider`. Model files and native runtime libraries remain separate
from the executable.

See the [CLI deployment steps](../../DEPLOYMENT.md#cli)
([中文](../../DEPLOYMENT.zh-CN.md#cli)) for model provisioning, commands, and
portable packaging, and `docparse parse --help` for the complete option list.
