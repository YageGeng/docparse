"""Exercise the cfg boundary checker on real temporary Rust source trees."""
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[4]


class CompatibilityBoundaryTest(unittest.TestCase):
    """Reject attributes and macros that hide platform choices in business code."""

    def check_source(self, path: str, source: str) -> subprocess.CompletedProcess:
        """Run the real checker against one independently constructed source tree."""
        with tempfile.TemporaryDirectory() as directory:
            target = Path(directory) / path
            target.parent.mkdir(parents=True)
            target.write_text(source)
            return subprocess.run(
                ["python3", str(ROOT / "scripts/check_wasm_compat.py"), "--root", directory],
                capture_output=True, text=True,
            )

    def test_multiline_platform_attribute_is_rejected(self):
        """A multiline cfg in a business module must fail the policy check."""
        result = self.check_source("crates/core/src/page.rs", '#[cfg(\n target_arch = "wasm32"\n)]\nfn render() {}')
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_exact_test_module_and_quoted_attributes_are_accepted(self):
        """Comments and strings must not produce false positives or hide real test modules."""
        source = '/* outer /* nested #[cfg(foo)] */ comment */\nconst TEXT: &str = r#"#[cfg(target_os = "windows")]"#;\n#[cfg(test)]\nmod tests {}'
        result = self.check_source("crates/core/src/page.rs", source)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_test_only_helper_outside_test_module_is_rejected(self):
        """A cfg(test) free function does not qualify as an allowed test module."""
        result = self.check_source("crates/core/src/page.rs", '#[cfg(test)]\nfn helper() {}')
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_hidden_platform_macro_is_rejected(self):
        """A compatibility macro must not bypass the explicit file boundary."""
        result = self.check_source("crates/layout/src/engine.rs", 'if_wasm! { fn run() {} }')
        self.assertEqual(result.returncode, 1, result.stderr)

    def test_only_named_compatibility_files_are_accepted(self):
        """The whitelist must not allow arbitrary files merely sharing the basename."""
        source = '#[cfg(target_arch = "wasm32")]\nfn run() {}'
        self.assertEqual(self.check_source("crates/layout/src/wasm_compat.rs", source).returncode, 0)
        self.assertEqual(self.check_source("crates/layout/src/extra/wasm_compat.rs", source).returncode, 1)
        self.assertEqual(self.check_source("crates/web/src/wasm_compat.rs", source).returncode, 1)
        self.assertEqual(self.check_source("crates/web/src/lib.rs", source).returncode, 1)

    def test_runtime_compatibility_files_are_explicitly_scoped(self):
        """Allow the extracted adapters without opening their directory to arbitrary cfgs."""
        source = '#[cfg(target_arch = "wasm32")]\nfn run() {}'
        for path, expected in (
            ("crates/core/src/wasm_compat/task_set.rs", 0),
            ("crates/core/src/wasm_compat/pdfium_worker.rs", 0),
            ("crates/core/src/wasm_compat/pdf_input.rs", 0),
            ("crates/layout/src/wasm_compat/session_pool.rs", 0),
            ("crates/core/src/wasm_compat/extra.rs", 1),
            ("crates/core/src/wasm_compat/session_pool.rs", 1),
            ("crates/layout/src/wasm_compat/extra.rs", 1),
            ("crates/core/src/runtime/task_set.rs", 1),
            ("crates/core/src/runtime/pdf_input.rs", 1),
            ("crates/layout/src/wasm_compat/task_set.rs", 1),
        ):
            with self.subTest(path=path):
                result = self.check_source(path, source)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
