"""Generate deterministic embedded-font table fixtures with explicit and inferred structure."""
from io import BytesIO
from pathlib import Path
import argparse
import reportlab
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas
from pypdf import PdfReader, PdfWriter
from pypdf.generic import ArrayObject, BooleanObject, DictionaryObject, NameObject, NumberObject


def generate(path: Path) -> None:
    """Draw ruled, borderless, and tagged tables using the same embedded font bytes."""
    fonts = Path(reportlab.__file__).parent / "fonts"
    pdfmetrics.registerFont(TTFont("TableVera", str(fonts / "Vera.ttf")))
    pdfmetrics.registerFont(TTFont("TableVeraBold", str(fonts / "VeraBd.ttf")))
    buffer = BytesIO()
    pdf = canvas.Canvas(buffer, pagesize=(612, 792), invariant=1, pageCompression=1)
    pdf.setTitle("DocParse table structure regression")
    pdf.setAuthor("DocParse tests")
    tagged_cells = []
    for page in range(3):
        pdf.setFont("TableVeraBold", 18)
        pdf.drawString(54, 738, ["Ruled benchmark table", "Borderless benchmark table", "Tagged benchmark table"][page])
        pdf.setFont("TableVera", 10)
        pdf.drawString(54, 714, "Cell structure preserves rows, empty values, and wrapped source text.")
        xs = [54, 214, 328, 442, 558]
        ys = [675, 638, 604, 568, 532, 496, 460, 424, 388]
        cells = [(0, 0, 2, 1, "System", True), (0, 1, 1, 3, "Performance measurements", True)]
        cells += [(1, col, 1, 1, text, True) for col, text in enumerate(["", "Requests", "Mean\nlatency", "Accuracy"]) if col > 0]
        rows = [("Baseline", "120", "32.5", "0.81"), ("Variant A", "240", "28.1", "0.86"), ("Variant B\n(two-stage)", "", "25.4", "0.90"), ("Variant C", "360", "21.7", "0.92"), ("Variant D", "480", "18.6", "0.95"), ("Total", "1200", "23.3", "0.89")]
        cells += [(row + 2, col, 1, 1, text, False) for row, values in enumerate(rows) for col, text in enumerate(values)]
        if page == 1:
            # Unmerged, borderless rows exercise text alignment without tag or rule hints.
            cells = [(0, col, 1, 1, text, True) for col, text in enumerate(["System", "Requests", "Latency", "Accuracy"])]
            cells += [(row + 1, col, 1, 1, text, False) for row, values in enumerate(rows) for col, text in enumerate(values)]
        if page == 0:
            pdf.setLineWidth(0.6)
            for y in ys:
                pdf.line(xs[1] if y == ys[1] else xs[0], y, xs[-1], y)
            for column, x in enumerate(xs):
                pdf.line(x, ys[1] if column in (2, 3) else ys[0], x, ys[-1])
        mcid = 0
        for row, col, row_span, col_span, text, header in cells:
            pdf.setFont("TableVeraBold" if header else "TableVera", 10)
            x = xs[col] + 9
            if col_span > 1:
                x = (xs[col] + xs[col + col_span] - pdfmetrics.stringWidth(text, "TableVeraBold", 10)) / 2
            ids = []
            for offset, line in enumerate(text.split("\n")):
                if not line:
                    continue
                if page == 2:
                    ids.append(mcid)
                    pdf._code.append(f"/Span <</MCID {mcid}>> BDC")
                    mcid += 1
                pdf.drawString(x, ys[row] - 20 - offset * 12, line)
                if page == 2:
                    pdf._code.append("EMC")
            if page == 2:
                tagged_cells.append((row, col, row_span, col_span, header, ids))
        pdf.setFont("TableVera", 10)
        pdf.drawString(54, 350, "This sentence is outside the table and must remain independent.")
        pdf.showPage()
    pdf.save()
    reader = PdfReader(buffer)
    writer = PdfWriter()
    writer.clone_document_from_reader(reader)
    page_ref = writer.pages[2].indirect_reference
    tree = DictionaryObject({NameObject("/Type"): NameObject("/StructTreeRoot")})
    tree_ref = writer._add_object(tree)
    table = DictionaryObject({NameObject("/Type"): NameObject("/StructElem"), NameObject("/S"): NameObject("/Table"), NameObject("/P"): tree_ref, NameObject("/Pg"): page_ref})
    table_ref = writer._add_object(table)
    row_refs = []
    parents = [None] * mcid
    for row in range(8):
        tr = DictionaryObject({NameObject("/Type"): NameObject("/StructElem"), NameObject("/S"): NameObject("/TR"), NameObject("/P"): table_ref, NameObject("/Pg"): page_ref})
        tr_ref = writer._add_object(tr)
        children = []
        for r, col, row_span, col_span, header, ids in tagged_cells:
            if r != row:
                continue
            attrs = DictionaryObject({NameObject("/O"): NameObject("/Table"), NameObject("/RowSpan"): NumberObject(row_span), NameObject("/ColSpan"): NumberObject(col_span)})
            cell = DictionaryObject({NameObject("/Type"): NameObject("/StructElem"), NameObject("/S"): NameObject("/TH" if header else "/TD"), NameObject("/P"): tr_ref, NameObject("/Pg"): page_ref, NameObject("/A"): attrs, NameObject("/K"): ArrayObject([NumberObject(value) for value in ids])})
            cell_ref = writer._add_object(cell)
            children.append(cell_ref)
            for value in ids:
                parents[value] = cell_ref
        tr[NameObject("/K")] = ArrayObject(children)
        row_refs.append(tr_ref)
    table[NameObject("/K")] = ArrayObject(row_refs)
    tree[NameObject("/K")] = ArrayObject([table_ref])
    tree[NameObject("/ParentTree")] = writer._add_object(DictionaryObject({NameObject("/Nums"): ArrayObject([NumberObject(2), ArrayObject(parents)])}))
    tree[NameObject("/ParentTreeNextKey")] = NumberObject(3)
    writer.pages[2][NameObject("/StructParents")] = NumberObject(2)
    writer._root_object[NameObject("/StructTreeRoot")] = tree_ref
    writer._root_object[NameObject("/MarkInfo")] = DictionaryObject({NameObject("/Marked"): BooleanObject(True)})
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("wb") as output:
        writer.write(output)


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    generate(parser.parse_args().output)
