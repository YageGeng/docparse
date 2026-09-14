"""Regression for interrupting a benchmark whose parent supervises worker processes."""

import importlib.util
import os
import signal
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest.mock import patch

import psutil


class SlowStop(subprocess.Popen[bytes]):
    """Expose the supervisor's restart window before the parent receives its stop signal."""

    def terminate(self) -> None:
        """Allow a live supervisor to react to an already killed worker before stopping it."""
        time.sleep(0.2)
        super().terminate()


@unittest.skipUnless(os.name == "posix", "requires POSIX signals")
class BenchmarkCleanupTest(unittest.TestCase):
    """Register process cleanup coverage for discovery and direct script execution."""

    def test_interrupted_supervisor_reaps_workers(self) -> None:
        """An interrupted supervisor must stop before its workers, leaving no replacement behind."""
        root = Path(__file__).resolve().parents[4]
        spec = importlib.util.spec_from_file_location("benchmark", root / "scripts/benchmark.py")
        assert spec is not None and spec.loader is not None
        runner = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(runner)
        with tempfile.TemporaryDirectory(prefix="docparse-benchmark-stop-") as temporary:
            directory = Path(temporary)
            marker = directory / "children.txt"
            binary = directory / "supervisor"
            worker_command = [sys.executable, "-c", "import time; time.sleep(30)"]
            binary.write_text(
                f"#!{sys.executable}\n"
                "import subprocess, sys\n"
                "from pathlib import Path\n"
                f"marker = Path({str(marker)!r})\n"
                "while True:\n"
                f"    child = subprocess.Popen({worker_command!r})\n"
                "    with marker.open('a') as output:\n"
                "        output.write(str(child.pid) + '\\n')\n"
                "    child.wait()\n"
            )
            binary.chmod(0o700)

            def interrupt() -> None:
                """Wait for a real worker before interrupting the process-monitor loop once."""
                deadline = time.monotonic() + 5
                while not marker.exists() and time.monotonic() < deadline:
                    time.sleep(0.01)
                time.sleep(0.05)
                os.kill(os.getpid(), signal.SIGINT)

            timer = threading.Thread(target=interrupt)
            timer.start()
            try:
                with patch.object(runner.shutil, "which", return_value=None), patch.object(runner.subprocess, "Popen", SlowStop):
                    with self.assertRaises(KeyboardInterrupt, msg="the test must interrupt an active benchmark"):
                        runner.run_case(binary, directory / "config", directory, directory, 1, 1, None)
                timer.join()
                self.assertTrue(marker.exists(), "supervisor did not create a worker")
                pids = [int(value) for value in marker.read_text().splitlines()]
                live = []
                for pid in pids:
                    try:
                        if psutil.Process(pid).status() != psutil.STATUS_ZOMBIE:
                            live.append(pid)
                    except psutil.NoSuchProcess:
                        pass
                self.assertFalse(live, f"interruption leaked replacement workers: {live}")
                print("Interrupted supervisor left no workers")
            finally:
                timer.join()
                # Clean up even when the old implementation reproduces the leak.
                if marker.exists():
                    for value in marker.read_text().splitlines():
                        try:
                            process = psutil.Process(int(value))
                            if process.cmdline() == worker_command:
                                process.kill()
                        except (psutil.NoSuchProcess, psutil.ZombieProcess):
                            pass


if __name__ == "__main__":
    unittest.main()
