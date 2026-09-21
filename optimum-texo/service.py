"""Serve formula uploads through configurable, independent Optimum CUDA consumers."""

import asyncio
import io
import logging
import os
from contextlib import asynccontextmanager
from pathlib import Path
from typing import Annotated, Literal

from configuration import Settings
from fastapi import FastAPI, File, Form, HTTPException, Request, UploadFile
from PIL import Image, UnidentifiedImageError
from session_manager import SessionManager

LOG = logging.getLogger("texo.service")
logging.basicConfig(
    level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s %(message)s"
)
MAX_UPLOAD_BYTES = 16 * 1024 * 1024


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Validate deployment settings and retain all native owners until service shutdown."""
    config = Path(
        os.environ.get("TEXO_CONFIG", Path(__file__).with_name("config.toml"))
    )
    manager = SessionManager(Settings.load(config))
    app.state.manager = manager
    await manager.start()
    LOG.info(
        "Texo ready with %d sessions, batch limit %d, and queue capacity %d",
        manager.settings.session_size,
        manager.settings.batch_size,
        manager.settings.queue_size,
    )
    try:
        yield
    finally:
        await manager.close()


app = FastAPI(title="Texo Optimum CUDA API", version="1.0.0", lifespan=lifespan)


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
        "data": [{"id": "texo-optimum", "object": "model", "owned_by": "alephpi"}],
    }


@app.post("/v1/predictions/upload")
# Keep FastAPI metadata in annotations so parameter defaults remain ordinary values.
async def recognize(
    request: Request,
    image: Annotated[UploadFile, File()],
    max_tokens: Annotated[
        int,
        Form(
            ge=2,
            le=1024,
            description="Maximum sequence length including BOS; incomplete output is rejected.",
        ),
    ] = 1024,
    task: Annotated[Literal["formula", "ocr", "ocr_plain"], Form()] = "formula",
    query: Annotated[
        str,
        Form(
            description="Accepted for compatibility; Texo always recognizes formulas."
        ),
    ] = "",
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
            crop = source.convert("RGB")
    except (
        UnidentifiedImageError,
        OSError,
        ValueError,
        Image.DecompressionBombError,
    ) as error:
        LOG.warning("Rejected invalid formula image: %s", error)
        raise HTTPException(400, "Invalid or oversized formula image") from error
    manager: SessionManager = app.state.manager

    async def wait_for_disconnect() -> None:
        """Observe transport closure after FastAPI has consumed the complete multipart body."""
        while (await request.receive())["type"] != "http.disconnect":
            pass

    try:
        future = asyncio.create_task(manager.submit(crop, max_tokens))
        disconnected = asyncio.create_task(wait_for_disconnect())
        try:
            # Admission and inference share one deadline and the same disconnect cancellation.
            done, _ = await asyncio.wait(
                (future, disconnected), timeout=120, return_when=asyncio.FIRST_COMPLETED
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
        LOG.warning("Texo request timed out")
        raise HTTPException(504, "Texo request exceeded 120 seconds") from error
    except RuntimeError as error:
        LOG.warning("Texo request failed: %s", error)
        raise HTTPException(503, str(error)) from error
    if "error" in result:
        LOG.warning("Texo rejected formula: %s", result["error"])
        raise HTTPException(422, result["error"])
    return result
