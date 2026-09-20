"""Independent native model owners consume one bounded asynchronous crop queue."""

import asyncio
import logging
import time
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass

from configuration import Settings
from PIL import Image
from runtime import generate, load_model

LOG = logging.getLogger("texo.sessions")
Response = dict[str, str | int | float]


@dataclass
class Job:
    """Retain one caller's image, generation budget, and independently cancelable reply."""

    image: Image.Image
    max_length: int
    future: asyncio.Future[Response]
    admitted: float


class SessionManager:
    """Own model lifetimes and dispatch crops without sharing mutable model state."""

    def __init__(self, settings: Settings) -> None:
        """Create bounded admission without loading CUDA or starting owner threads."""
        self.settings = settings
        self.queue: asyncio.Queue[Job] = asyncio.Queue(maxsize=settings.queue_size)
        self._executors: list[ThreadPoolExecutor] = []
        self._tasks: list[asyncio.Task[None]] = []
        self._active = [0] * settings.session_size
        self._providers: list[str] = []
        self._io_binding = False
        self._accepting = False

    async def start(self) -> None:
        """Load every model on its own thread before exposing readiness; unwind partial startup."""
        if self._executors:
            raise RuntimeError("Texo sessions are already started")
        loop = asyncio.get_running_loop()
        try:
            for index in range(self.settings.session_size):
                executor = ThreadPoolExecutor(
                    max_workers=1, thread_name_prefix=f"texo-{index}"
                )
                self._executors.append(executor)
                # Sequential loading avoids racing global model registration and startup allocations.
                model, tokenizer = await loop.run_in_executor(executor, load_model)
                self._providers = list(model.providers)
                self._io_binding = bool(model.use_io_binding)
                self._tasks.append(
                    asyncio.create_task(
                        self._consume(index, executor, model, tokenizer)
                    )
                )
                LOG.info(
                    "Loaded Texo session %d of %d",
                    index + 1,
                    self.settings.session_size,
                )
            self._accepting = True
        except BaseException:
            await self.close()
            raise

    def submit(self, image: Image.Image, max_length: int) -> asyncio.Future[Response]:
        """Admit without another waiting queue; callers own timeout and cancellation of replies."""
        if not self._accepting:
            raise RuntimeError("Texo sessions are not accepting requests")
        future: asyncio.Future[Response] = asyncio.get_running_loop().create_future()
        self.queue.put_nowait(Job(image, max_length, future, time.perf_counter()))
        return future

    async def _consume(
        self, index: int, executor: ThreadPoolExecutor, model, tokenizer
    ) -> None:
        """Drain ready crops per owner, grouping equal token budgets and isolating caller failures."""
        # A separate coroutine scope drops every job/group reference before the next queue wait.
        while True:
            await self._consume_batch(index, executor, model, tokenizer)

    async def _consume_batch(
        self, index: int, executor: ThreadPoolExecutor, model, tokenizer
    ) -> None:
        """Own exactly one batch so completed crop pixels cannot survive in the idle consumer."""
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
                    groups[job.max_length].append(job)
            for max_length, group in groups.items():
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
                        tokenizer,
                        [job.image for job in group],
                        max_length,
                    )
                    if len(results) != len(group):
                        raise RuntimeError("model result count mismatch")
                    elapsed_ms = (time.perf_counter() - started) * 1000
                    for job, result in zip(group, results):
                        if not job.future.done():
                            job.future.set_result(
                                {
                                    "model": "texo-optimum",
                                    **result,
                                    "batch_size": len(group),
                                    "max_length": max_length,
                                    "inference_time_ms": elapsed_ms,
                                    "queue_ms": (started - job.admitted) * 1000,
                                }
                            )
                    LOG.info(
                        "Session %d completed %d images in %.1f ms",
                        index,
                        len(group),
                        elapsed_ms,
                    )
                except Exception:
                    LOG.exception(
                        "Texo session %d failed for %d images", index, len(group)
                    )
                    for job in group:
                        if not job.future.done():
                            job.future.set_exception(
                                RuntimeError("Texo inference failed")
                            )
                finally:
                    self._active[index] = 0
        finally:
            # Cancellation releases each original reply even when a native call still has to finish.
            for job in jobs:
                if not job.future.done():
                    job.future.set_exception(RuntimeError("Texo service stopped"))
                self.queue.task_done()

    async def close(self) -> None:
        """Stop admission, release pending callers, and join native work without blocking the event loop."""
        self._accepting = False
        for task in self._tasks:
            task.cancel()
        await asyncio.gather(*self._tasks, return_exceptions=True)
        self._tasks.clear()
        while not self.queue.empty():
            job = self.queue.get_nowait()
            if not job.future.done():
                job.future.set_exception(RuntimeError("Texo service stopped"))
            self.queue.task_done()
        # Canceling an asyncio waiter cannot terminate ORT; wait for each actual native owner to return.
        await asyncio.gather(
            *(
                asyncio.to_thread(executor.shutdown, wait=True, cancel_futures=True)
                for executor in self._executors
            )
        )
        self._executors.clear()
        LOG.info("Texo sessions stopped")

    def health(self) -> dict[str, object]:
        """Report aggregate occupancy and actual provider metadata across independent consumers."""
        return {
            "status": "ready" if self._accepting else "stopped",
            "model_id": "alephpi/FormulaNet",
            "backend": "Optimum ORTModelForVision2Seq.generate",
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
