"""Generate an embedded-font fixture without relying on host font substitution."""
from pathlib import Path
import argparse
import reportlab
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas


def generate(path: Path) -> None:
    """Embed the freely redistributable Vera font on two pages, including rotated text."""
    font = Path(reportlab.__file__).parent / "fonts/Vera.ttf"
    pdfmetrics.registerFont(TTFont("EmbeddedVera", str(font)))
    pdf = canvas.Canvas(str(path), pagesize=(612, 792), invariant=1, pageCompression=1)
    pdf.setTitle("DocParse embedded font parity")
    pdf.setAuthor("DocParse tests")
    for page in range(1, 3):
        pdf.setFont("EmbeddedVera", 20)
        pdf.drawString(54, 730, f"Embedded font document - page {page}")
        pdf.setFont("EmbeddedVera", 11)
        for index in range(10):
            pdf.drawString(54, 680 - 22 * index, f"Line {index + 1}: identical font bytes preserve text geometry.")
        pdf.saveState()
        pdf.translate(550, 430)
        pdf.rotate(90)
        pdf.drawString(0, 0, "Rotated embedded text")
        pdf.restoreState()
        pdf.drawString(54, 64, "Native and Web execute the same parser and model.")
        pdf.showPage()
    pdf.save()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    generate(parser.parse_args().output)
