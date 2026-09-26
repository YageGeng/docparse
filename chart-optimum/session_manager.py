"""Independent native model owners consume one bounded asynchronous chart queue."""

import asyncio
import logging
import time
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass

from PIL import Image

from configuration import Settings
from runtime import generate, load_model

LOG = logging.getLogger("chart.sessions")
Response = dict[str, str | int | float | dict | list | None]


@dataclass
class Job:
    """Retain one caller's chart, generation budget, and independently cancelable reply."""

    image: Image.Image
    max_new_tokens: int
    future: asyncio.Future[Response]
    admitted: float


class SessionManager:
    """Own model lifetimes and dispatch charts without sharing mutable model state."""

    def __init__(self, settings: Settings) -> None:
        """Create bounded admission without loading CUDA or starting owner threads."""
        self.settings = settings
        self.queue: asyncio.Queue[Job] = asyncio.Queue(maxsize=settings.queue_size)
        self._executors: list[ThreadPoolExecutor] = []
        self._tasks: list[asyncio.Task[None]] = []
        self._admissions: set[asyncio.Task[None]] = set()
        self._active = [0] * settings.session_size
        self._providers: list[str] = []
        self._io_binding = False
        self._accepting = False

    async def start(self) -> None:
        """Load every model on its own thread before exposing readiness; unwind partial startup."""
        if self._executors:
            raise RuntimeError("OneChart sessions are already started")
        loop = asyncio.get_running_loop()
        try:
            for index in range(self.settings.session_size):
                executor = ThreadPoolExecutor(
                    max_workers=1, thread_name_prefix=f"chart-{index}"
                )
                self._executors.append(executor)
                # Sequential loading avoids racing CUDA context creation and startup allocations.
                model = await loop.run_in_executor(executor, load_model)
                self._providers = list(model.providers)
                self._io_binding = bool(model.use_io_binding)
                self._tasks.append(
                    asyncio.create_task(self._consume(index, executor, model))
                )
                LOG.info(
                    "Loaded OneChart session %d of %d",
                    index + 1,
                    self.settings.session_size,
                )
            self._accepting = True
        except BaseException:
            await self.close()
            raise

    async def submit(self, image: Image.Image, max_new_tokens: int) -> Response:
        """Wait for bounded queue capacity and inference, canceling abandoned work at either stage."""
        if not self._accepting:
            LOG.warning(
                "Rejected chart request because sessions are not accepting requests"
            )
            raise RuntimeError("OneChart sessions are not accepting requests")
        future: asyncio.Future[Response] = asyncio.get_running_loop().create_future()
        submitted = time.perf_counter()
        waiting = self.queue.full()
        if waiting:
            LOG.debug(
                "OneChart queue capacity %d reached; waiting for space",
                self.settings.queue_size,
            )
        # Track puts separately so shutdown can wake producers before draining a Python 3.12 queue.
        admission = asyncio.create_task(
            self.queue.put(Job(image, max_new_tokens, future, submitted))
        )
        # Failed reply tracebacks retain this frame, so keep chart ownership only in the queued job.
        del image
        self._admissions.add(admission)
        try:
            try:
                await admission
            except asyncio.CancelledError:
                if not self._accepting:
                    raise RuntimeError("OneChart service stopped") from None
                raise
            finally:
                self._admissions.discard(admission)
                del admission
            if waiting:
                LOG.debug(
                    "OneChart request admitted after %.1f ms",
                    (time.perf_counter() - submitted) * 1000,
                )
            return await future
        finally:
            # Cancel both queued replies and replies whose native inference is already running.
            future.cancel()

    async def _consume(self, index: int, executor: ThreadPoolExecutor, model) -> None:
        """Drain ready charts per owner, grouping equal budgets and isolating caller failures."""
        # A separate coroutine scope drops every job/group reference before the next queue wait.
        while True:
            await self._consume_batch(index, executor, model)

    async def _consume_batch(
        self, index: int, executor: ThreadPoolExecutor, model
    ) -> None:
        """Own exactly one batch so completed chart pixels cannot survive in the idle consumer."""
        loop = asyncio.get_running_loop()
        jobs = [await self.queue.get()]
        try:
            if jobs[0].future.done():
                return
            # Full ready batches need no artificial delay; sparse uploads get the existing 3 ms window.
            if self.queue.qsize() < self.settings.batch_size - 1:
                await asyncio.sleep(0.003)
            while len(jobs) < self.settings.batch_size:
                try:
                    job = self.queue.get_nowait()
                except asyncio.QueueEmpty:
                    break
                if job.future.done():
                    self.queue.task_done()
                else:
                    jobs.append(job)
            groups: dict[int, list[Job]] = defaultdict(list)
            for job in jobs:
                if not job.future.done():
                    groups[job.max_new_tokens].append(job)
            for max_new_tokens, group in groups.items():
                # A budget group may have been canceled while an earlier group used the owner.
                group = [job for job in group if not job.future.done()]
                if not group:
                    continue
                started = time.perf_counter()
                self._active[index] = len(group)
                try:
                    results = await loop.run_in_executor(
                        executor,
                        generate,
                        model,
                        [job.image for job in group],
                        max_new_tokens,
                    )
                    if len(results) != len(group):
                        raise RuntimeError("model result count mismatch")
                    elapsed_ms = (time.perf_counter() - started) * 1000
                    for job, result in zip(group, results):
                        if not job.future.done():
                            job.future.set_result(
                                {
                                    "model": "onechart-optimum",
                                    **result,
                                    "batch_size": len(group),
                                    "max_new_tokens": max_new_tokens,
                                    "inference_time_ms": elapsed_ms,
                                    "queue_ms": (started - job.admitted) * 1000,
                                }
                            )
                    LOG.info(
                        "Session %d completed %d charts in %.1f ms",
                        index,
                        len(group),
                        elapsed_ms,
                    )
                except Exception:
                    LOG.exception(
                        "OneChart session %d failed for %d charts", index, len(group)
                    )
                    for job in group:
                        if not job.future.done():
                            job.future.set_exception(
                                RuntimeError("OneChart inference failed")
                            )
                finally:
                    self._active[index] = 0
        finally:
            # Cancellation releases each original reply even when a native call still has to finish.
            for job in jobs:
                if not job.future.done():
                    job.future.set_exception(RuntimeError("OneChart service stopped"))
                self.queue.task_done()

    async def close(self) -> None:
        """Stop admission, release pending callers, and join native work without blocking the event loop."""
        self._accepting = False
        LOG.info(
            "Stopping OneChart sessions with %d pending admissions",
            len(self._admissions),
        )
        for admission in self._admissions:
            admission.cancel()
        await asyncio.gather(*self._admissions, return_exceptions=True)
        for task in self._tasks:
            task.cancel()
        await asyncio.gather(*self._tasks, return_exceptions=True)
        self._tasks.clear()
        while not self.queue.empty():
            job = self.queue.get_nowait()
            if not job.future.done():
                job.future.set_exception(RuntimeError("OneChart service stopped"))
            self.queue.task_done()
        # Canceling an asyncio waiter cannot terminate ORT; wait for each actual native owner to return.
        await asyncio.gather(
            *(
                asyncio.to_thread(executor.shutdown, wait=True, cancel_futures=True)
                for executor in self._executors
            )
        )
        self._executors.clear()
        LOG.info("OneChart sessions stopped")

    def health(self) -> dict[str, object]:
        """Report aggregate occupancy and actual provider metadata across independent consumers."""
        return {
            "status": "ready" if self._accepting else "stopped",
            "model_id": "kppkkp/OneChart",
            "backend": "ONNX Runtime CUDA sessions with an Optimum-exported split decoder",
            "providers": self._providers,
            "use_cache": True,
            "use_io_binding": self._io_binding,
            "session_size": self.settings.session_size,
            "queue_size": self.settings.queue_size,
            "batch_size": self.settings.batch_size,
            "batch_limit": self.settings.batch_size,
            "queued": self.queue.qsize(),
            "active_images": sum(self._active),
            "active_sessions": sum(count > 0 for count in self._active),
        }
