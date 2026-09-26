"""Serve chart uploads through configurable, independent ONNX CUDA consumers."""

import asyncio
import io
import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated

from fastapi import FastAPI, File, Form, HTTPException, Request, UploadFile
from PIL import Image, UnidentifiedImageError

from configuration import Settings
from preprocessing import bounded
from runtime import MAX_NEW_TOKENS
from session_manager import SessionManager

LOG = logging.getLogger("chart.service")
logging.basicConfig(
    level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s %(message)s"
)
MAX_UPLOAD_BYTES = 16 * 1024 * 1024
REQUEST_TIMEOUT_SECONDS = 120


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Validate deployment settings and retain all native owners until service shutdown."""
    config = Path(
        os.environ.get("CHART_CONFIG", Path(__file__).with_name("config.toml"))
    )
    manager = SessionManager(Settings.load(config))
    app.state.manager = manager
    await manager.start()
    LOG.info(
        "OneChart ready with %d sessions, batch limit %d, and queue capacity %d",
        manager.settings.session_size,
        manager.settings.batch_size,
        manager.settings.queue_size,
    )
    try:
        yield
    finally:
        await manager.close()


app = FastAPI(
    title="OneChart Optimum ONNX CUDA API", version="1.0.0", lifespan=lifespan
)


@app.get("/v1/health")
@app.get("/health")
async def health():
    """Report the actual model providers and queue occupancy."""
    manager: SessionManager = app.state.manager
    return manager.health()


@app.get("/v1/models")
async def models():
    """Expose a model identifier for clients without implying chat-completions support."""
    return {
        "object": "list",
        "data": [{"id": "onechart-optimum", "object": "model", "owned_by": "kppkkp"}],
    }


@app.post("/v1/predictions/upload")
# Keep FastAPI metadata in annotations so parameter defaults remain ordinary values.
async def extract(
    request: Request,
    image: Annotated[UploadFile, File()],
    max_new_tokens: Annotated[
        int,
        Form(
            ge=2,
            le=MAX_NEW_TOKENS,
            description="Maximum generated tokens; the chart dictionary needs the whole budget.",
        ),
    ] = MAX_NEW_TOKENS,
):
    """Validate an upload and await its individual reply from the shared consumer queue."""
    content = await image.read(MAX_UPLOAD_BYTES + 1)
    await image.close()
    if not content or len(content) > MAX_UPLOAD_BYTES:
        LOG.warning("Rejected image upload with %d bytes", len(content))
        raise HTTPException(413, "Image must contain 1..16777216 bytes")
    try:
        with Image.open(io.BytesIO(content)) as source:
            if source.width * source.height > 16_777_216:
                raise ValueError("Image exceeds the 16-megapixel limit")
            # Downscale before admission so a queue of pending uploads holds one 1024x1024
            # chart each instead of the full-size image the caller sent.
            chart = bounded(source)
    except (
        UnidentifiedImageError,
        OSError,
        ValueError,
        Image.DecompressionBombError,
    ) as error:
        LOG.warning("Rejected invalid chart image: %s", error)
        raise HTTPException(400, "Invalid or oversized chart image") from error
    manager: SessionManager = app.state.manager

    async def wait_for_disconnect() -> None:
        """Observe transport closure after FastAPI has consumed the complete multipart body."""
        while (await request.receive())["type"] != "http.disconnect":
            pass

    try:
        future = asyncio.create_task(manager.submit(chart, max_new_tokens))
        disconnected = asyncio.create_task(wait_for_disconnect())
        try:
            # Admission and inference share one deadline and the same disconnect cancellation.
            done, _ = await asyncio.wait(
                (future, disconnected),
                timeout=REQUEST_TIMEOUT_SECONDS,
                return_when=asyncio.FIRST_COMPLETED,
            )
            if future in done:
                result = future.result()
            elif disconnected in done:
                disconnected.result()
                raise HTTPException(499, "Client disconnected")
            else:
                raise TimeoutError
        finally:
            # Cover success, timeout, disconnect, and cancellation of the enclosing request task.
            future.cancel()
            disconnected.cancel()
            await asyncio.gather(future, disconnected, return_exceptions=True)
    except TimeoutError as error:
        LOG.warning("OneChart request timed out")
        raise HTTPException(504, "OneChart request exceeded 120 seconds") from error
    except RuntimeError as error:
        LOG.warning("OneChart request failed: %s", error)
        raise HTTPException(503, str(error)) from error
    if "error" in result:
        LOG.warning("OneChart rejected chart: %s", result["error"])
        raise HTTPException(422, result["error"])
    return result
