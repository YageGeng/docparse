# /// script
# requires-python = "==3.12.*"
# dependencies = ["torch==2.9.1+cpu", "torchvision==0.24.1+cpu", "transformers==4.57.6", "onnx==1.20.1", "pillow==12.3.0", "numpy==2.5.2"]
# [tool.uv.sources]
# torch = { index = "pytorch-cpu" }
# torchvision = { index = "pytorch-cpu" }
# [[tool.uv.index]]
# name = "pytorch-cpu"
# url = "https://download.pytorch.org/whl/cpu"
# explicit = true
# ///
"""Export the pinned TATR checkpoint on CPU; the installer verifies the resulting digest."""
import argparse
from pathlib import Path

import onnx
import torch
from transformers import TableTransformerForObjectDetection


class ExportModel(torch.nn.Module):
    """Expose only the two inference outputs consumed by structure reconstruction."""

    def __init__(self, model):
        """Keep the loaded evaluation model as the export module."""
        super().__init__()
        self.model = model

    def forward(self, pixel_values, pixel_mask):
        """Return class logits and normalized center-format bounding boxes."""
        result = self.model(pixel_values=pixel_values, pixel_mask=pixel_mask)
        return result.logits, result.pred_boxes



def export(directory, destination):
    """Export dynamic dimensions without GPU dependencies or application test fixtures."""
    torch.set_num_threads(4)
    model = ExportModel(TableTransformerForObjectDetection.from_pretrained(
        directory, use_pretrained_backbone=False, attn_implementation="eager")).eval()
    # Only the sample shape affects tracing; no model data depends on pixel values.
    pixels = torch.zeros((1, 3, 435, 800), dtype=torch.float32)
    mask = torch.ones((1, 435, 800), dtype=torch.int64)
    with torch.inference_mode():
        torch.onnx.export(model, (pixels, mask), destination,
                          input_names=["pixel_values", "pixel_mask"],
                          output_names=["logits", "pred_boxes"], opset_version=17,
                          dynamo=False, dynamic_axes={
                              "pixel_values": {0: "batch", 2: "height", 3: "width"},
                              "pixel_mask": {0: "batch", 1: "height", 2: "width"},
                              "logits": {0: "batch"}, "pred_boxes": {0: "batch"}})
    onnx.checker.check_model(str(destination))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("directory", type=Path)
    parser.add_argument("destination", type=Path)
    args = parser.parse_args()
    export(args.directory, args.destination)
