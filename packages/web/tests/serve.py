"""Serve real browser artifacts and persist test evidence; never perform PDF parsing."""
import argparse
import json
import re
import threading
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

ROOT = Path(__file__).resolve().parents[3]
DIST = ROOT / "packages/web/dist"
REPORTS = ROOT / "packages/web/test-results"
LOCK = threading.Lock()


class Handler(SimpleHTTPRequestHandler):
    """Serve the production SDK, including a renamed directory used for deployment acceptance."""

    def __init__(self, *args, **kwargs):
        """Constrain ordinary static resources to the repository root."""
        super().__init__(*args, directory=str(ROOT), **kwargs)

    def translate_path(self, path):
        """Map the relocation test prefix without allowing it to escape the SDK directory."""
        url = unquote(urlsplit(path).path)
        if url.startswith("/relocated-sdk/"):
            result = (DIST / url.removeprefix("/relocated-sdk/")).resolve()
            return str(result if result.is_relative_to(DIST.resolve()) else DIST / "missing")
        return super().translate_path(path)

    def end_headers(self):
        """Prevent a local rebuild from mixing cached JavaScript with a newer WASM binary."""
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def do_POST(self):
        """Save ordered JSON test reports only in the ignored test-results directory."""
        if self.path != "/__docparse_test_report":
            self.send_error(404)
            return
        try:
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= 4 * 1024 * 1024:
                raise ValueError("invalid report length")
            report = json.loads(self.rfile.read(length))
            name = report["runId"]
            if not re.fullmatch(r"[a-zA-Z0-9_-]{1,80}", name):
                raise ValueError("invalid report name")
            with LOCK:
                REPORTS.mkdir(parents=True, exist_ok=True)
                path = REPORTS / f"{name}.json"
                old = json.loads(path.read_text()) if path.exists() else {}
                incoming = (report["startedAt"], report["sequence"])
                previous = (old.get("startedAt", 0), old.get("sequence", -1))
                if incoming >= previous:
                    path.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
            self.send_response(200)
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"{}")
        except (ValueError, KeyError, TypeError):
            self.send_error(400, "Invalid test report")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=8767)
    ThreadingHTTPServer(("127.0.0.1", parser.parse_args().port), Handler).serve_forever()
