"""Verify HTTP disconnect propagation through the real multipart upload endpoint."""

import asyncio
import io
import sys
import threading
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

import requests
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from configuration import Settings
from service import app
from session_manager import SessionManager


class UploadTests(unittest.IsolatedAsyncioTestCase):
    """Keep transport cancellation and listener cleanup covered without CUDA models."""

    async def test_upload_disconnect_and_completion_cleanup(self):
        """Disconnected queued work is skipped; successful replies stop their transport listener."""
        for mode in ["disconnect", "success", "timeout", "cancel"]:
            with self.subTest(mode=mode):
                await self._check_upload(mode)

    async def _check_upload(self, mode: str) -> None:
        """Give each transport scenario its own callback scope and independently owned queue."""
        loop = asyncio.get_running_loop()
        entered, queued, disconnected = (
            asyncio.Event(),
            asyncio.Event(),
            asyncio.Event(),
        )
        listener_stopped = asyncio.Event()
        listener_started = asyncio.Event()
        release = threading.Event()
        calls, replies, statuses = [], [], []

        def generate(model, tokenizer, images, max_length):
            """Hold the owner so the HTTP crop remains queued until assertions finish."""
            calls.append(len(images))
            loop.call_soon_threadsafe(entered.set)
            if not release.wait(5):
                raise RuntimeError("test release timed out")
            return [{"text": "x", "output_tokens": 1} for _ in images]

        manager = SessionManager(Settings(1, 4, 1))
        with (
            patch(
                "session_manager.load_model",
                return_value=(
                    SimpleNamespace(
                        providers=["CUDAExecutionProvider"], use_io_binding=True
                    ),
                    None,
                ),
            ),
            patch("session_manager.generate", generate),
            patch.object(app.state, "manager", manager, create=True),
        ):
            await manager.start()
            blocker = manager.submit(Image.new("RGB", (1, 1)), 32)
            await asyncio.wait_for(entered.wait(), 2)
            submit = manager.submit

            def observe(image, limit):
                """Capture the production future without altering queue behavior."""
                future = submit(image, limit)
                replies.append(future)
                queued.set()
                return future

            data = io.BytesIO()
            Image.new("RGB", (1, 1)).save(data, format="PNG")
            prepared = requests.Request(
                "POST",
                "http://test/v1/predictions/upload",
                files={"image": ("crop.png", data.getvalue(), "image/png")},
            ).prepare()
            delivered = False

            async def receive():
                """Supply a real multipart body, then wait for disconnect or listener cancellation."""
                nonlocal delivered
                if not delivered:
                    delivered = True
                    return {
                        "type": "http.request",
                        "body": prepared.body,
                        "more_body": False,
                    }
                listener_started.set()
                try:
                    await disconnected.wait()
                    return {"type": "http.disconnect"}
                finally:
                    listener_stopped.set()

            async def send(message):
                """Observe the endpoint's response status without retaining response bodies."""
                if message["type"] == "http.response.start":
                    statuses.append(message["status"])

            scope = {
                "type": "http",
                "asgi": {"version": "3.0"},
                "http_version": "1.1",
                "method": "POST",
                "scheme": "http",
                "path": "/v1/predictions/upload",
                "raw_path": b"/v1/predictions/upload",
                "query_string": b"",
                "root_path": "",
                "headers": [
                    (
                        key.lower().encode(),
                        value if isinstance(value, bytes) else value.encode(),
                    )
                    for key, value in prepared.headers.items()
                ],
                "server": ("test", 80),
                "client": ("test", 1234),
            }
            wait = asyncio.wait

            async def bounded_wait(
                futures, *, timeout=None, return_when=asyncio.ALL_COMPLETED
            ):
                """Exercise the production deadline branch without waiting two minutes."""
                if mode == "timeout" and timeout == 120:
                    timeout = 0.01
                return await wait(futures, timeout=timeout, return_when=return_when)

            with (
                patch.object(manager, "submit", observe),
                patch("service.asyncio.wait", bounded_wait),
            ):
                request = asyncio.create_task(app(scope, receive, send))
                try:
                    await asyncio.wait_for(queued.wait(), 2)
                    await asyncio.wait_for(listener_started.wait(), 2)
                    if mode == "disconnect":
                        disconnected.set()
                    elif mode == "success":
                        release.set()
                    elif mode == "cancel":
                        request.cancel()
                    done, _ = await asyncio.wait([request], timeout=1)
                    self.assertIn(
                        request,
                        done,
                        "the HTTP handler must finish without waiting for abandoned inference",
                    )
                    if mode == "cancel":
                        with self.assertRaises(asyncio.CancelledError):
                            await request
                    else:
                        await request
                    self.assertTrue(listener_stopped.is_set())
                    self.assertEqual(replies[0].cancelled(), mode != "success")
                finally:
                    release.set()
                    await asyncio.wait_for(
                        asyncio.gather(request, blocker, return_exceptions=True),
                        3,
                    )
                    await manager.queue.join()
                    await manager.close()
            self.assertEqual(len(calls), 2 if mode == "success" else 1)
            self.assertEqual(
                statuses,
                {
                    "disconnect": [499],
                    "success": [200],
                    "timeout": [504],
                    "cancel": [],
                }[mode],
            )


if __name__ == "__main__":
    unittest.main()
