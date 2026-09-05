"""Generate a deterministic three-page parser integration fixture."""

from __future__ import annotations

import argparse
from pathlib import Path

from reportlab.lib.pagesizes import letter
from reportlab.pdfgen import canvas


def draw_lines(pdf: canvas.Canvas, x: float, y: float, prefix: str) -> None:
    """Draw four deterministic prose-like lines in one local column."""
    pdf.setFont("Helvetica", 10)
    for index in range(4):
        pdf.drawString(x, y - index * 15, f"{prefix} line {index + 1}")


def generate(output: Path) -> None:
    """Write title/columns, partial-coverage, and fallback-only pages."""
    output.parent.mkdir(parents=True, exist_ok=True)
    pdf = canvas.Canvas(
        str(output), pagesize=letter, pageCompression=1, invariant=1
    )
    pdf.setTitle("DocParse multipage parser fixture")
    for page_number in range(1, 4):
        pdf.setFont("Helvetica", 8)
        pdf.drawString(54, 765, "DocParse Running Header")
        pdf.setFont("Helvetica-Bold", 18)
        pdf.drawString(54, 720, f"Fixture Page {page_number}")
        if page_number == 1:
            draw_lines(pdf, 54, 660, "Left column")
            draw_lines(pdf, 330, 660, "Right column")
        elif page_number == 2:
            draw_lines(pdf, 54, 660, "Covered section")
            draw_lines(pdf, 330, 520, "Residual section")
        else:
            draw_lines(pdf, 54, 660, "Geometry fallback")
            draw_lines(pdf, 330, 660, "Second fallback column")
        pdf.setFont("Helvetica", 8)
        pdf.drawCentredString(306, 30, str(page_number))
        pdf.showPage()
    pdf.save()


def parse_args() -> argparse.Namespace:
    """Parse the deterministic fixture output path."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    return parser.parse_args()


def main() -> None:
    """Generate the requested three-page PDF."""
    arguments = parse_args()
    generate(arguments.output)


if __name__ == "__main__":
    main()
