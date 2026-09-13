"""Create raster-only and mixed PDFs for acceptance against the real browser OCR pipeline.

Run with: uv run --locked --group dev <this-file> <output-directory>
The inputs reuse the existing redistributable embedded-font extraction fixtures.
"""
import argparse
import subprocess
from pathlib import Path

import reportlab
from PIL import Image, ImageDraw, ImageFont
from reportlab.lib.utils import ImageReader
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas


def generate(output: Path) -> None:
    """Rasterize actual PDFs and remove their text layer to ensure OCR cannot be bypassed."""
    root = Path(__file__).resolve().parents[4]
    output.mkdir(parents=True, exist_ok=True)
    source = root / "crates/core/tests/fixtures/pdf"
    for name in ["embedded_layout", "embedded_cjk_90"]:
        subprocess.run(["rtk", "proxy", "pdftoppm", "-f", "1", "-l", "1", "-r", "144", "-singlefile", "-png",
                        str(source / f"{name}.pdf"), str(output / name)], check=True)
    printed = Image.open(output / "embedded_layout.png").convert("RGB")
    chinese = Image.open(output / "embedded_cjk_90.png").convert("RGB")
    pdf = canvas.Canvas(str(output / "scanned-rotations.pdf"), invariant=1)
    pdf.setTitle("Real PaddleOCR: upright, upside-down, sideways and Chinese scans")
    for image in [printed, printed.transpose(Image.Transpose.ROTATE_180), printed.transpose(Image.Transpose.ROTATE_90), chinese]:
        width, height = image.width / 2, image.height / 2
        pdf.setPageSize((width, height))
        pdf.drawImage(ImageReader(image), 0, 0, width, height)
        pdf.showPage()
    pdf.save()
    font = Path(reportlab.__file__).parent / "fonts/Vera.ttf"
    pdfmetrics.registerFont(TTFont("EmbeddedVera", str(font)))
    pdf = canvas.Canvas(str(output / "mixed-native-scan.pdf"), pagesize=(612, 792), invariant=1)
    pdf.setFont("EmbeddedVera", 16)
    pdf.drawString(50, 750, "Native text must remain exactly once.")
    pdf.drawImage(ImageReader(printed), 50, 50, 500, 500 * printed.height / printed.width)
    pdf.showPage()
    pdf.save()
    # Share a line between PDF text and image-only words to exercise partial native/OCR ownership.
    pdf = canvas.Canvas(str(output / "native-ocr-overlap.pdf"), pagesize=(612, 792), invariant=1)
    raster_font = ImageFont.truetype(str(font), 40)

    def raster_word(text: str, x: float, baseline: float) -> None:
        """Embed visible glyph pixels at the same baseline as the native PDF font."""
        ascent, descent = raster_font.getmetrics()
        pixels = Image.new("RGB", (int(raster_font.getlength(text)) + 4, ascent + descent), "white")
        ImageDraw.Draw(pixels).text((0, ascent), text, font=raster_font, fill="black", anchor="ls")
        pdf.drawImage(ImageReader(pixels), x, baseline - descent / 2, pixels.width / 2, pixels.height / 2)

    pdf.setFont("EmbeddedVera", 16)
    pdf.drawString(50, 750, "Mixed native labels and raster values")
    pdf.setFont("EmbeddedVera", 20)
    pdf.drawString(50, 690, "Total:")
    raster_word("100", 120, 690)
    raster_word("Pay", 50, 635)
    pdf.drawString(105, 635, "USD")
    raster_word("20", 160, 635)
    raster_word("Hello,", 50, 580)
    raster_word("world", 115, 580)
    pdf.showPage()
    pdf.save()
    print(f"Created OCR rotation, mixed-page and inline-overlap PDFs in {output}")


if __name__ == "__main__":
    arguments = argparse.ArgumentParser(description=__doc__)
    arguments.add_argument("output", type=Path)
    generate(arguments.parse_args().output)
