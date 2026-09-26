"""Verify the ONNX deployment reproduces the released PyTorch pipeline on real charts.

This is the acceptance test for the conversion itself. It runs the published greedy
decoding path on the GPU and compares its cleaned text with the ONNX owner's, so a
regression in any graph, in the visual splice, or in the tokenizer setup fails here.

The exported graphs are float32, so the equivalent reference is the released model in
float32 as well. The model card's `chat()` runs the same weights in float16 on CUDA, which
agrees on the opening tokens but can pick a different token after several hundred
low-confidence steps, exactly as any reduced-precision run can.
"""

import argparse
import gc
import json
import sys
from pathlib import Path

import torch
from PIL import Image

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from preprocessing import build_prompt, preprocess
from runtime import MAX_NEW_TOKENS, clean_output, generate, load_model

MODEL_ROOT = Path(__file__).resolve().parents[1] / "models"
SOURCE = MODEL_ROOT / "OneChart"
# The charts prepared for this deployment live outside the repository.
DEFAULT_IMAGES = Path("/home/isbest/Pictures/屏幕截图")
# Largest tolerated drift between the two auxiliary-head readings; the observed drift is 5e-5,
# so this still fails loudly if the head is applied to the wrong decoding step.
MAGNITUDE_TOLERANCE = 1e-2


def load_reference():
    """Load the released checkpoint in float32 together with its tokenizer."""
    from transformers import AutoModel, AutoTokenizer

    model = AutoModel.from_pretrained(SOURCE, trust_remote_code=True).eval().cuda()
    tokenizer = AutoTokenizer.from_pretrained(
        SOURCE, trust_remote_code=True, use_fast=False, padding_side="right"
    )
    # The release predates generic `add_bos_token` support, so transformers now prepend
    # `</s>` and the model answers as though its turn had already ended.
    tokenizer.add_bos_token = False
    return model, tokenizer


def reference_text(
    model, tokenizer, image, max_new_tokens: int
) -> tuple[str, list[float]]:
    """Run the released greedy path and return its cleaned text and auxiliary magnitudes.

    The release writes `pred_locs` from inside its forward pass, so the magnitudes must be read
    after generation. They are the head's response to the `<Number>` token and are what the
    release's reliability check compares against the values it parsed from its own text.
    """
    input_ids = torch.tensor(tokenizer([build_prompt()]).input_ids, device="cuda")
    tensor = torch.from_numpy(preprocess(image)).to(device="cuda")
    with torch.inference_mode():
        generated = model.generate(
            input_ids,
            attention_mask=torch.ones_like(input_ids),
            images=[tensor],
            do_sample=False,
            num_beams=1,
            max_new_tokens=max_new_tokens,
            use_cache=True,
        )
    text = clean_output(tokenizer, generated[0, input_ids.shape[1] :].tolist())
    return text, [float(value) for value in model.pred_locs]


def main() -> None:
    """Compare ONNX and PyTorch text for every real chart and report token counts."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--images", type=Path, default=DEFAULT_IMAGES)
    parser.add_argument("--max-new-tokens", type=int, default=MAX_NEW_TOKENS)
    parser.add_argument(
        "--write-reference",
        type=Path,
        default=None,
        help="Rewrite the pinned cases that tests/smoke.py replays over HTTP.",
    )
    args = parser.parse_args()
    paths = sorted(args.images.glob("chart_*.png"))
    if not paths:
        raise SystemExit(f"no chart_*.png found under {args.images}")
    # The two model copies plus the vision tower's 805 MiB attention buffer exceed an 8 GiB
    # card, so the reference texts are captured first and the PyTorch owner is released.
    reference_model, tokenizer = load_reference()
    expected = {}
    for path in paths:
        with Image.open(path) as handle:
            image = handle.convert("RGB")
        expected[path.name] = reference_text(
            reference_model, tokenizer, image, args.max_new_tokens
        )
        print(f"{path.name}: captured the PyTorch reference", flush=True)
    del reference_model, tokenizer
    gc.collect()
    torch.cuda.empty_cache()

    onnx_model = load_model()
    report = {}
    cases = []
    for path in paths:
        with Image.open(path) as handle:
            image = handle.convert("RGB")
        result = generate(onnx_model, [image], args.max_new_tokens)[0]
        text = result["text"]
        assert isinstance(text, str)
        expected_text, expected_magnitudes = expected[path.name]
        # Text parity is the acceptance criterion: identical text means every graph, the
        # visual splice, the cache, and the tokenizer reproduced the PyTorch pipeline.
        assert text == expected_text, f"{path.name}: ONNX diverged from PyTorch"
        # Then confirm the output is still the release's chart schema rather than free text.
        # The hardest chart carries three series, a legend, and a statistics box, and the
        # released model degenerates before closing `values`; the deployment must reproduce
        # that behaviour, so the missing keys are reported instead of asserted away.
        assert text.startswith("{"), f"{path.name}: output is not a chart dictionary"
        assert '"title"' in text, f"{path.name}: output is missing the chart title"
        schema_keys = [
            key for key in ('"values"', '"x_title"', '"y_title"') if key in text
        ]
        # The auxiliary head must read the hidden state of the `<Number>` token itself rather
        # than the token that step produced. Nothing else catches a step-alignment regression,
        # because the reliability distance stays unset whenever the text is not valid JSON.
        magnitudes = result["magnitudes"]
        assert isinstance(magnitudes, list)
        # Every real chart makes the model open with `<Number>`, so the head must have fired;
        # an empty reading on either side would make this check pass without testing anything.
        assert magnitudes, f"{path.name}: the auxiliary head never ran"
        assert expected_magnitudes, f"{path.name}: PyTorch never ran the auxiliary head"
        assert len(magnitudes) == len(expected_magnitudes), (
            f"{path.name}: head ran on {len(magnitudes)} values, PyTorch on "
            f"{len(expected_magnitudes)}"
        )
        drift = max(
            (
                abs(actual - want)
                for actual, want in zip(magnitudes, expected_magnitudes)
            ),
            default=0.0,
        )
        assert drift < MAGNITUDE_TOLERANCE, (
            f"{path.name}: head magnitudes drifted by {drift}"
        )
        data = result["data"]
        table = result["table"]
        assert isinstance(table, dict)
        rows = sum(len(entries) for entries in table.values())
        # A chart that reached the `values` schema must also yield recoverable table rows,
        # even though the released model does not always close valid JSON for it.
        if '"values"' in text:
            assert rows > 0, (
                f"{path.name}: no table rows recovered from a values section"
            )
        cases.append(
            {
                "image": path.name,
                "max_new_tokens": args.max_new_tokens,
                "text": text,
                "table": table,
                "magnitudes": magnitudes,
            }
        )
        report[path.name] = {
            "output_tokens": result["output_tokens"],
            "schema_keys": schema_keys,
            "parsed": isinstance(data, dict),
            "table_series": len(table),
            "table_rows": rows,
            "magnitudes": len(magnitudes),
            "magnitude_drift": drift,
            "reliable": result["reliable"],
            "reliable_distance": result["reliable_distance"],
            "matches_pytorch": True,
        }
        print(f"{path.name}: {report[path.name]}", flush=True)
    if args.write_reference is not None:
        args.write_reference.write_text(
            json.dumps({"cases": cases}, ensure_ascii=False, indent=2) + "\n"
        )
        print(f"wrote {args.write_reference}", flush=True)
    print(json.dumps(report, indent=2))


if __name__ == "__main__":
    main()
