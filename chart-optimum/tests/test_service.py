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
from service import MAX_UPLOAD_BYTES, app
from session_manager import SessionManager


def png_bytes() -> bytes:
    """Return one valid in-memory chart upload."""
    data = io.BytesIO()
    Image.new("RGB", (8, 8)).save(data, format="PNG")
    return data.getvalue()


class ValidationTests(unittest.IsolatedAsyncioTestCase):
    """Cover the upload guards and their status codes without loading CUDA models."""

    async def _status(
        self, submit, body: bytes, filename: str = "chart.png"
    ) -> list[int]:
        """Drive one upload through the real ASGI app and collect its response statuses.

        The transport delivers the multipart body once and then stays open, so only the
        manager's reply can complete the request and the status is deterministic.
        """
        prepared = requests.Request(
            "POST",
            "http://test/v1/predictions/upload",
            files={"image": (filename, body, "image/png")},
        ).prepare()
        statuses: list[int] = []
        delivered = False

        async def receive():
            """Supply the body, then hold the transport open until the handler cancels it."""
            nonlocal delivered
            if not delivered:
                delivered = True
                return {
                    "type": "http.request",
                    "body": prepared.body,
                    "more_body": False,
                }
            await asyncio.Event().wait()
            raise AssertionError("the disconnect listener must be cancelled")

        async def send(message):
            """Record the response status without retaining response bodies."""
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
        with patch.object(
            app.state, "manager", SimpleNamespace(submit=submit), create=True
        ):
            await app(scope, receive, send)
        return statuses

    async def test_rejects_invalid_and_oversized_uploads(self):
        """Return 400 for undecodable bytes and 413 for a body above the upload limit."""
        self.assertEqual(await self._status(None, b"not an image", "bad.png"), [400])
        oversized = b"x" * (MAX_UPLOAD_BYTES + 1)
        self.assertEqual(await self._status(None, oversized, "big.png"), [413])

    async def test_maps_model_rejections_and_failures(self):
        """Return 422 when the model rejects a chart and 503 when inference fails."""

        async def rejected(image, budget):
            """Report a per-chart rejection the way the runtime does."""
            return {"error": "the model returned no chart dictionary"}

        self.assertEqual(await self._status(rejected, png_bytes()), [422])

        async def failed(image, budget):
            """Fail the way a native inference error reaches the handler."""
            raise RuntimeError("OneChart inference failed")

        self.assertEqual(await self._status(failed, png_bytes()), [503])

    async def test_returns_the_model_reply_on_success(self):
        """Return the runtime's payload unchanged when the model accepts the chart."""

        async def accepted(image, budget):
            """Return one complete runtime reply."""
            return {
                "text": '{"title": "None"}',
                "table": {"S": [{"label": "0", "value": "1"}]},
                "magnitudes": [0.5],
                "output_tokens": 3,
            }

        self.assertEqual(await self._status(accepted, png_bytes()), [200])


class UploadTests(unittest.IsolatedAsyncioTestCase):
    """Keep transport cancellation and listener cleanup covered without CUDA models."""

    async def test_upload_disconnect_and_completion_cleanup(self):
        """Disconnected queued work is skipped; successful replies stop their transport listener."""
        for mode in ["disconnect", "success", "timeout", "cancel"]:
            with self.subTest(mode=mode):
                await self._check_upload(mode)

    async def test_full_queue_waits_for_capacity(self):
        """Saturated uploads wait for space and still honor transport cancellation and deadlines."""
        for mode in ["success", "disconnect", "timeout", "cancel"]:
            with self.subTest(mode=mode):
                await self._check_upload(mode, saturated=True)

    async def _check_upload(self, mode: str, saturated: bool = False) -> None:
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

        def generate(model, images, max_new_tokens):
            """Hold the owner so the HTTP chart remains queued until assertions finish."""
            calls.append(len(images))
            loop.call_soon_threadsafe(entered.set)
            if not release.wait(5):
                raise RuntimeError("test release timed out")
            return [{"text": "x", "output_tokens": 1} for _ in images]

        manager = SessionManager(Settings(1, 1, 1))
        with (
            patch(
                "session_manager.load_model",
                return_value=SimpleNamespace(
                    providers=["CUDAExecutionProvider"], use_io_binding=True
                ),
            ),
            patch("session_manager.generate", generate),
            patch.object(app.state, "manager", manager, create=True),
        ):
            await manager.start()
            blocker = asyncio.create_task(manager.submit(Image.new("RGB", (1, 1)), 32))
            await asyncio.wait_for(entered.wait(), 2)
            fillers = (
                [asyncio.create_task(manager.submit(Image.new("RGB", (1, 1)), 32))]
                if saturated
                else []
            )
            submit = manager.submit

            async def observe(image, limit):
                """Capture the request task without altering queue admission or inference."""
                replies.append(asyncio.current_task())
                queued.set()
                return await submit(image, limit)

            data = io.BytesIO()
            Image.new("RGB", (1, 1)).save(data, format="PNG")
            prepared = requests.Request(
                "POST",
                "http://test/v1/predictions/upload",
                files={"image": ("chart.png", data.getvalue(), "image/png")},
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
                    self.assertEqual(
                        statuses, [], "queue saturation must not return 503"
                    )
                    await asyncio.wait_for(listener_started.wait(), 2)
                    if saturated:
                        self.assertEqual(manager.health()["queued"], 1)
                        self.assertFalse(replies[0].done())
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
                        asyncio.gather(
                            request, blocker, *fillers, return_exceptions=True
                        ),
                        3,
                    )
                    await manager.queue.join()
                    await manager.close()
            self.assertEqual(len(calls), (2 if mode == "success" else 1) + len(fillers))
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
