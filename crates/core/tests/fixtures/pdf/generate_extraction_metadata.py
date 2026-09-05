"""Generate the deterministic PDFium metadata integration fixture."""

from __future__ import annotations

import argparse
from pathlib import Path

from reportlab.lib.colors import Color, black, blue, red
from reportlab.lib.pagesizes import letter
from reportlab.pdfgen import canvas


def draw_fixture(output: Path) -> None:
    """Writes one deterministic page with text, style, link, and rotation cases."""
    output.parent.mkdir(parents=True, exist_ok=True)
    pdf = canvas.Canvas(
        str(output),
        pagesize=letter,
        pageCompression=1,
        invariant=1,
    )
    pdf.setTitle("DocParse extraction metadata fixture")
    pdf.setAuthor("DocParse tests")

    pdf.setFont("Helvetica-Bold", 18)
    pdf.setFillColor(black)
    pdf.drawString(54, 738, "DocParse Metadata Fixture")

    pdf.setFont("Helvetica", 11)
    pdf.drawString(54, 690, "Left column baseline")
    pdf.drawString(330, 690, "Right column baseline")

    pdf.setFont("Helvetica-Oblique", 9)
    pdf.setFillColor(blue)
    pdf.drawString(54, 655, "Italic blue text")
    pdf.setFont("Helvetica-Bold", 14)
    pdf.setFillColor(red)
    pdf.drawString(330, 655, "Bold red text")

    pdf.setFont("Courier", 10)
    pdf.setFillColor(black)
    pdf.drawString(54, 615, "Missing")
    pdf.drawString(102, 615, "Space")
    pdf.drawString(54, 590, "Contents....................42")

    pdf.setFont("Helvetica", 11)
    pdf.setFillColor(Color(0.1, 0.35, 0.8, alpha=1.0))
    link_text = "https://example.com/docparse"
    pdf.drawString(54, 550, link_text)
    pdf.linkURL(
        "https://example.com/docparse",
        (54, 547, 235, 561),
        relative=0,
        thickness=0,
    )

    pdf.saveState()
    pdf.translate(500, 430)
    pdf.rotate(90)
    pdf.setFillColor(black)
    pdf.setFont("Helvetica", 12)
    pdf.drawString(0, 0, "Rotated ninety degrees")
    pdf.restoreState()

    # The marked-content sequence gives PDFium a stable MCID-bearing text object.
    pdf._code.append("/P <</MCID 7>> BDC")
    pdf.setFillColor(Color(0.2, 0.5, 0.2, alpha=1.0))
    text = pdf.beginText(54, 500)
    text.setFont("Helvetica", 11)
    text.setTextRenderMode(2)
    text.textOut("Marked fill and stroke")
    pdf.drawText(text)
    pdf._code.append("EMC")

    pdf.setFillColor(black)
    pdf.setFont("Helvetica", 11)
    pdf.drawString(54, 460, "Explicit line one")
    pdf.drawString(54, 442, "Explicit line two")

    pdf.showPage()
    pdf.save()


def parse_args() -> argparse.Namespace:
    """Parses the fixture output path."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    return parser.parse_args()


def main() -> int:
    """Generates the requested fixture and returns a process status."""
    arguments = parse_args()
    draw_fixture(arguments.output)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
