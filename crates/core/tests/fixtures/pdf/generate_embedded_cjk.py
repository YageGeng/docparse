"""Generate a Chinese embedded-font PDF with rotation, CropBox, and UserUnit."""
import argparse
import io
from pathlib import Path
from pypdf import PdfReader, PdfWriter
from pypdf.generic import ArrayObject, FloatObject, NameObject, NumberObject
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas


def generate(output: Path, font: Path) -> None:
    """Embed a supplied Noto Sans SC font and set explicit PDF coordinate-system facts."""
    pdfmetrics.registerFont(TTFont("EmbeddedCJK", str(font)))
    stream = io.BytesIO()
    pdf = canvas.Canvas(stream, pagesize=(612, 792), invariant=1, pageCompression=1)
    pdf.setTitle("DocParse Chinese geometry fixture")
    pdf.setFont("EmbeddedCJK", 22)
    pdf.drawString(54, 720, "中文文档解析测试")
    pdf.setFont("EmbeddedCJK", 14)
    pdf.drawString(54, 670, "保留原始字符与阅读顺序。")
    pdf.drawString(54, 640, "嵌入字体保证跨平台文字度量一致。")
    pdf.drawString(54, 610, "Native and Web share the same PDF pipeline.")
    pdf.showPage()
    pdf.save()
    writer = PdfWriter()
    writer.clone_document_from_reader(PdfReader(stream))
    page = writer.pages[0]
    page[NameObject("/Rotate")] = NumberObject(90)
    page[NameObject("/UserUnit")] = FloatObject(2)
    page[NameObject("/CropBox")] = ArrayObject([NumberObject(n) for n in (20, 20, 592, 772)])
    with output.open("wb") as target:
        writer.write(target)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--font", type=Path, required=True)
    arguments = parser.parse_args()
    generate(arguments.output, arguments.font)
