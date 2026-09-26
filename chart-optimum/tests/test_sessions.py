"""Exercise real queue scheduling with deterministic stand-ins for GPU execution."""

import asyncio
import gc
import sys
import tempfile
import threading
import unittest
import weakref
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from configuration import Settings
from session_manager import SessionManager


class SettingsTests(unittest.TestCase):
    """Reject unbounded or misspelled deployment settings before loading models."""

    def test_toml_validation(self):
        """Load valid values and reject invalid types, ranges, caps, and unknown keys."""
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "config.toml"
            path.write_text("session_size = 2\nqueue_size = 8\nbatch_size = 4\n")
            self.assertEqual(Settings.load(path), Settings(2, 8, 4))
            for content in [
                "session_size = 0",
                "queue_size = 0",
                "batch_size = 33",
                # Owners duplicate the graph set and each pending chart holds an image, so a
                # typo in either capacity has to fail before anything is allocated.
                "session_size = 9",
                "queue_size = 4097",
                "session_size = true",
                "queue_size = 1.5",
                "workers = 2",
            ]:
                path.write_text(content)
                with self.subTest(content=content), self.assertRaises(ValueError):
                    Settings.load(path)

    def test_capacity_caps_accept_their_upper_bound(self):
        """Accept the documented maximum of every bounded capacity."""
        self.assertEqual(Settings(8, 4096, 32), Settings(8, 4096, 32))


def fake_model():
    """Stand in for a loaded owner, which the manager only queries for its providers."""
    return SimpleNamespace(providers=["CUDAExecutionProvider"], use_io_binding=True)


class SessionTests(unittest.IsolatedAsyncioTestCase):
    """Verify batching, independent owners, backpressure, and teardown without CUDA."""

    async def test_idle_consumers_release_completed_and_canceled_charts(self):
        """No batch pixels remain owned after success, failure, or queued cancellation."""

        def generate(model, images, max_new_tokens):
            """Complete one budget and reject another without retaining its image storage."""
            if max_new_tokens == 2:
                raise RuntimeError("test failure")
            return [{"text": "x", "output_tokens": 1} for _ in images]

        manager = SessionManager(Settings(1, 4, 3))
        with (
            patch("session_manager.load_model", return_value=fake_model()),
            patch("session_manager.generate", generate),
        ):
            await manager.start()
            try:
                images = [Image.new("RGB", (1, 1)) for _ in range(3)]
                references = [weakref.ref(image) for image in images]
                replies = [
                    asyncio.create_task(manager.submit(image, budget))
                    for image, budget in zip(images, [32, 2, 16])
                ]
                # Start submissions and their queue puts before canceling an admitted chart.
                await asyncio.sleep(0)
                await asyncio.sleep(0)
                replies[2].cancel()
                del images
                with self.assertLogs("chart.sessions", level="ERROR"):
                    await asyncio.gather(*replies, return_exceptions=True)
                await manager.queue.join()
                gc.collect()
                self.assertTrue(all(reference() is None for reference in references))
                self.assertEqual(manager.health()["active_images"], 0)
            finally:
                await manager.close()

    async def test_parallel_sessions_and_queue_limit(self):
        """Two native owners overlap while active and pending charts remain bounded."""
        loop = asyncio.get_running_loop()
        entered = asyncio.Queue()
        release = threading.Event()

        def load():
            """Record the native thread that owns each independently loaded model."""
            return SimpleNamespace(
                owner=threading.get_ident(),
                providers=["CUDAExecutionProvider"],
                use_io_binding=True,
            )

        def generate(model, images, max_new_tokens):
            """Hold both owners inside inference until queue saturation has been checked."""
            self.assertEqual(model.owner, threading.get_ident())
            loop.call_soon_threadsafe(entered.put_nowait, (model.owner, len(images)))
            if not release.wait(5):
                raise RuntimeError("test release timed out")
            return [
                {"text": str(image.getpixel((0, 0))), "output_tokens": 1}
                for image in images
            ]

        manager = SessionManager(Settings(2, 4, 2))
        with (
            patch("session_manager.load_model", load),
            patch("session_manager.generate", generate),
        ):
            await manager.start()
            pending = []
            try:
                pending = [
                    asyncio.create_task(
                        manager.submit(Image.new("L", (1, 1), index), 32)
                    )
                    for index in range(4)
                ]
                first = await asyncio.wait_for(entered.get(), 3)
                second = await asyncio.wait_for(entered.get(), 3)
                self.assertNotEqual(first[0], second[0])
                self.assertEqual(sorted([first[1], second[1]]), [2, 2])
                self.assertEqual(manager.health()["active_images"], 4)
                pending.extend(
                    asyncio.create_task(
                        manager.submit(Image.new("L", (1, 1), index), 32)
                    )
                    for index in range(4, 24)
                )
                # Let all submissions run while both native owners still hold their first batch.
                done, _ = await asyncio.wait(pending, timeout=0.03)
                self.assertFalse(
                    done, "full queues must suspend admission instead of rejecting it"
                )
                self.assertEqual(manager.health()["queued"], 4)
                release.set()
                results = await asyncio.wait_for(asyncio.gather(*pending), 3)
                self.assertEqual(
                    [result["text"] for result in results],
                    [str(index) for index in range(24)],
                )
                for result in results:
                    size = result["batch_size"]
                    assert isinstance(size, int)
                    self.assertLessEqual(size, 2)
            finally:
                release.set()
                await manager.close()
                await asyncio.gather(*pending, return_exceptions=True)
            self.assertEqual(manager.health()["status"], "stopped")

    async def test_cancellation_budgets_and_recovery(self):
        """Canceled charts are skipped, token budgets stay separate, and failures release owners."""
        calls = []

        def generate(model, images, max_new_tokens):
            """Fail one budget and return identifiable results for unaffected callers."""
            calls.append((len(images), max_new_tokens))
            if max_new_tokens == 2:
                raise RuntimeError("simulated inference failure")
            return [{"text": str(max_new_tokens), "output_tokens": 1} for _ in images]

        manager = SessionManager(Settings(1, 8, 4))
        with (
            patch("session_manager.load_model", return_value=fake_model()),
            patch("session_manager.generate", generate),
        ):
            await manager.start()
            try:
                image = Image.new("L", (1, 1))
                canceled = asyncio.create_task(manager.submit(image, 99))
                # Reach queue ownership before cancellation so the consumer must skip this chart.
                await asyncio.sleep(0)
                await asyncio.sleep(0)
                canceled.cancel()
                failed = asyncio.create_task(manager.submit(image, 2))
                valid = asyncio.create_task(manager.submit(image, 16))
                with self.assertLogs("chart.sessions", level="ERROR"):
                    outcomes = await asyncio.gather(
                        failed, valid, return_exceptions=True
                    )
                self.assertIsInstance(outcomes[0], RuntimeError)
                outcome = outcomes[1]
                assert isinstance(outcome, dict)
                self.assertEqual(outcome["text"], "16")
                recovered = await manager.submit(image, 32)
                self.assertEqual(recovered["text"], "32")
                self.assertEqual(calls, [(1, 2), (1, 16), (1, 32)])
            finally:
                await manager.close()
            with self.assertRaises(RuntimeError):
                await manager.submit(image, 32)

    async def test_failed_startup_releases_earlier_sessions(self):
        """A later model load failure cannot leave the first owner's thread alive."""
        threads = []

        def load():
            """Record owners and reject the second initialization."""
            threads.append(threading.current_thread())
            if len(threads) == 2:
                raise RuntimeError("load failed")
            return fake_model()

        manager = SessionManager(Settings(2, 4, 2))
        with (
            patch("session_manager.load_model", load),
            self.assertRaisesRegex(RuntimeError, "load failed"),
        ):
            await manager.start()
        self.assertTrue(all(not thread.is_alive() for thread in threads))
        self.assertEqual(manager.health()["status"], "stopped")

    async def test_shutdown_waits_for_native_work_and_releases_pending_callers(self):
        """Shutdown must join real worker threads while keeping the event loop responsive."""
        loop = asyncio.get_running_loop()
        entered = asyncio.Event()
        release = threading.Event()

        def generate(model, images, max_new_tokens):
            """Hold a native invocation beyond cancellation of its asynchronous waiter."""
            loop.call_soon_threadsafe(entered.set)
            if not release.wait(5):
                raise RuntimeError("test release timed out")
            return [{"text": "x", "output_tokens": 1}]

        manager = SessionManager(Settings(1, 2, 1))
        with (
            patch("session_manager.load_model", return_value=fake_model()),
            patch("session_manager.generate", generate),
        ):
            await manager.start()
            active = asyncio.create_task(manager.submit(Image.new("L", (1, 1)), 32))
            await asyncio.wait_for(entered.wait(), 3)
            pending = [
                asyncio.create_task(manager.submit(Image.new("L", (1, 1)), 32))
                for _ in range(6)
            ]
            done, _ = await asyncio.wait(pending, timeout=0.03)
            self.assertFalse(done)
            self.assertEqual(manager.health()["queued"], 2)
            closing = asyncio.create_task(manager.close())
            try:
                with self.assertRaises(TimeoutError):
                    await asyncio.wait_for(asyncio.shield(closing), 0.03)
                self.assertTrue(all(reply.done() for reply in [active, *pending]))
            finally:
                release.set()
                await asyncio.wait_for(closing, 3)
                outcomes = await asyncio.wait_for(
                    asyncio.gather(active, *pending, return_exceptions=True), 1
                )
            self.assertTrue(
                all(isinstance(outcome, RuntimeError) for outcome in outcomes)
            )
            await asyncio.wait_for(manager.queue.join(), 1)


if __name__ == "__main__":
    unittest.main()
