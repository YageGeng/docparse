"""OneChart image, prompt, and visual-splice preparation copied from the pinned transform."""

import numpy as np
from numpy.typing import NDArray
from PIL import Image

# OneChartImageEvalProcessor always resizes a chart to this square before the vision tower.
IMAGE_SIZE = 1024
# OneChart replaces exactly this many `<imgpad>` embeddings with projected visual features.
IMAGE_TOKEN_LEN = 256
IMAGE_START_TOKEN = "<img>"
IMAGE_PATCH_TOKEN = "<imgpad>"
IMAGE_END_TOKEN = "</img>"
CHART_QUERY = "Convert the key information of the chart to a python dict:"
# The release ships the vicuna v1.1 system turn, joined to the user turn by a space.
SYSTEM_PROMPT = (
    "A chat between a curious user and an artificial intelligence assistant. "
    "The assistant gives helpful, detailed, and polite answers to the user's questions."
)


def bounded(image: Image.Image) -> Image.Image:
    """Return one chart at the deployment's fixed square, ready to be queued or encoded.

    The upload handler and :func:`preprocess` both call this so a chart is downscaled once, as
    early as possible. That stops an admission queue of full-size uploads from holding tens of
    megabytes per pending request, and resizing an already-square chart is a byte-exact copy.
    """
    return image.convert("RGB").resize(
        (IMAGE_SIZE, IMAGE_SIZE), Image.Resampling.BICUBIC
    )


def preprocess(image: Image.Image) -> NDArray[np.float32]:
    """Resize one chart with the upstream bicubic transform and add the batch axis."""
    pixels = np.asarray(bounded(image), dtype=np.float32).transpose(2, 0, 1)
    # The upstream ToTensor divides by 255 and its Normalize(mean 0, std 1) changes nothing.
    return (pixels / np.float32(255.0))[None]


def build_prompt() -> str:
    """Reproduce `Conversation.get_prompt` for the release's single fixed chart query."""
    question = (
        IMAGE_START_TOKEN
        + IMAGE_PATCH_TOKEN * IMAGE_TOKEN_LEN
        + IMAGE_END_TOKEN
        + CHART_QUERY
        + "\n"
    )
    # The TWO separator style ends the system turn with " " and leaves the assistant turn open.
    return f"{SYSTEM_PROMPT} USER: {question} ASSISTANT:"


def splice_image_features(
    inputs_embeds: NDArray[np.float32],
    input_ids: NDArray[np.int64],
    image_features: NDArray[np.float32],
    start_token_id: int,
    end_token_id: int,
) -> NDArray[np.float32]:
    """Replace each `<img>...</img>` placeholder run with the projected visual features.

    The upstream model keeps the `<img>` embedding, overwrites the following
    `image_token_len` `<imgpad>` embeddings, and resumes at the `</img>` embedding.
    """
    patches = image_features.shape[1]
    spliced = inputs_embeds.copy()
    for row in range(input_ids.shape[0]):
        positions = np.flatnonzero(input_ids[row] == start_token_id)
        if len(positions) != 1:
            raise ValueError("A chart prompt must carry exactly one image start token")
        position = int(positions[0])
        end = position + patches + 1
        # Bound the end index before reading it so a short prompt raises the intended error
        # instead of an IndexError from the row lookup.
        if end >= input_ids.shape[1] or input_ids[row, end] != end_token_id:
            raise ValueError("The image end token should follow the image start token.")
        spliced[row, position + 1 : end] = image_features[row]
    return spliced
