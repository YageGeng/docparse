"""Serve Texo through a single Optimum CUDA owner with bounded ready-image batching."""
import asyncio
from collections import defaultdict
from contextlib import asynccontextmanager, suppress
from dataclasses import dataclass
import io
import logging
import os
import time
from typing import Literal

from fastapi import FastAPI, File, Form, HTTPException, UploadFile
from PIL import Image, UnidentifiedImageError

from runtime import load_model, generate

LOG = logging.getLogger("texo.service")
logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s %(message)s")
BATCH_SIZE = int(os.environ.get("TEXO_BATCH_SIZE", "16"))
QUEUE_SIZE = 128
MAX_UPLOAD_BYTES = 16 * 1024 * 1024
assert 1 <= BATCH_SIZE <= 32


@dataclass
class Job:
    """Retain one caller's image, generation budget, and response ownership."""
    image: Image.Image
    max_length: int
    future: asyncio.Future
    admitted: float


@asynccontextmanager
async def lifespan(app):
    """Load verified CUDA sessions once and release the bounded worker on shutdown."""
    app.state.model, app.state.tokenizer = await asyncio.to_thread(load_model)
    app.state.queue = asyncio.Queue(maxsize=QUEUE_SIZE)
    app.state.active = 0

    async def consume():
        """Batch only ready callers, preserve individual budgets, and skip canceled requests."""
        queue = app.state.queue
        while True:
            jobs = [await queue.get()]
            # A short admission window lets concurrent HTTP uploads form a real tensor batch.
            await asyncio.sleep(0.003)
            while len(jobs) < BATCH_SIZE:
                try:
                    jobs.append(queue.get_nowait())
                except asyncio.QueueEmpty:
                    break
            groups = defaultdict(list)
            for job in jobs:
                if not job.future.cancelled():
                    groups[job.max_length].append(job)
            try:
                for max_length, group in groups.items():
                    started = time.perf_counter()
                    app.state.active = len(group)
                    try:
                        results = await asyncio.to_thread(
                            generate, app.state.model, app.state.tokenizer,
                            [job.image for job in group], max_length,
                        )
                        if len(results) != len(group):
                            raise RuntimeError("model result count mismatch")
                        elapsed_ms = (time.perf_counter() - started) * 1000
                        for job, result in zip(group, results):
                            if job.future.done():
                                continue
                            if "error" in result:
                                LOG.warning("Texo rejected unfinished output: %s", result["error"])
                                job.future.set_exception(HTTPException(422, result["error"]))
                            else:
                                job.future.set_result({
                                    "model": "texo-optimum", **result,
                                    "batch_size": len(group), "max_length": max_length,
                                    "inference_time_ms": elapsed_ms,
                                    "queue_ms": (started - job.admitted) * 1000,
                                })
                        LOG.info("Completed Texo batch of %d images in %.1f ms", len(group), elapsed_ms)
                    except Exception:
                        LOG.exception("Texo CUDA batch failed for %d images", len(group))
                        for job in group:
                            if not job.future.done():
                                job.future.set_exception(HTTPException(503, "Texo inference failed"))
                    finally:
                        app.state.active = 0
            finally:
                for _ in jobs:
                    queue.task_done()

    worker = asyncio.create_task(consume())
    LOG.info("Texo ready on CUDA with batch limit %d and queue capacity %d", BATCH_SIZE, QUEUE_SIZE)
    try:
        yield
    finally:
        worker.cancel()
        with suppress(asyncio.CancelledError):
            await worker
        LOG.info("Texo service stopped")


app = FastAPI(title="Texo Optimum CUDA API", version="1.0.0", lifespan=lifespan)


@app.get("/v1/health")
@app.get("/health")
async def health():
    """Report the actual model providers and queue occupancy."""
    return {
        "status": "ready", "model_id": "alephpi/FormulaNet",
        "backend": "Optimum ORTModelForVision2Seq.generate",
        "providers": app.state.model.providers, "use_cache": True,
        "use_io_binding": app.state.model.use_io_binding,
        "batch_limit": BATCH_SIZE, "queued": app.state.queue.qsize(),
        "active_images": app.state.active,
    }


@app.get("/v1/models")
async def models():
    """Expose a model identifier for clients without implying chat-completions support."""
    return {"object": "list", "data": [{"id": "texo-optimum", "object": "model", "owned_by": "alephpi"}]}


@app.post("/v1/predictions/upload")
async def recognize(
    image: UploadFile = File(...),
    max_tokens: int = Form(1024, ge=2, le=1024, description="Maximum sequence length including BOS; incomplete output is rejected."),
    task: Literal["formula", "ocr", "ocr_plain"] = Form("formula"),
    query: str = Form("", description="Accepted for compatibility; Texo always recognizes formulas."),
):
    """Validate an uploaded crop and await its result from the shared CUDA batch owner."""
    content = await image.read(MAX_UPLOAD_BYTES + 1)
    await image.close()
    if not content or len(content) > MAX_UPLOAD_BYTES:
        LOG.warning("Rejected image upload with %d bytes", len(content))
        raise HTTPException(413, "Image must contain 1..16777216 bytes")
    try:
        with Image.open(io.BytesIO(content)) as source:
            if source.width * source.height > 16_777_216:
                raise ValueError("Image exceeds the 16-megapixel limit")
            crop = source.convert("RGB")
    except (UnidentifiedImageError, OSError, ValueError, Image.DecompressionBombError) as error:
        LOG.warning("Rejected invalid formula image: %s", error)
        raise HTTPException(400, "Invalid or oversized formula image") from error
    future = asyncio.get_running_loop().create_future()
    job = Job(crop, max_tokens, future, time.perf_counter())
    try:
        app.state.queue.put_nowait(job)
    except asyncio.QueueFull:
        LOG.warning("Texo request queue is full")
        raise HTTPException(503, "Texo request queue is full")
    try:
        return await asyncio.wait_for(future, timeout=120)
    except asyncio.TimeoutError as error:
        LOG.warning("Texo request timed out")
        raise HTTPException(504, "Texo request exceeded 120 seconds") from error
