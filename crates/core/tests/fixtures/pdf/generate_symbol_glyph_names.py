"""Generate an original Type3 symbol font with deliberately incorrect Unicode mappings."""

from pathlib import Path

from pypdf import PdfWriter
from pypdf.generic import ArrayObject, DecodedStreamObject, DictionaryObject, FloatObject, NameObject, NumberObject


def main():
    """Draw named symbols, both overlay orders, and ordinary-text controls without external fonts."""
    writer = PdfWriter()
    page = writer.add_blank_page(width=300, height=160)
    procs = DictionaryObject()
    circle = b"300 40 m 444 40 560 156 560 300 c 560 444 444 560 300 560 c 156 560 40 444 40 300 c 40 156 156 40 300 40 c h "
    for name, commands in {
        "CIRCLE": b"600 0 0 0 600 600 d1 " + circle + b"f",
        "Circle": b"600 0 0 0 600 600 d1 20 w " + circle + b"S",
        "LEFTCIRCLE": b"600 0 0 0 300 600 d1 300 40 m 156 40 40 156 40 300 c 40 444 156 560 300 560 c h f",
    }.items():
        stream = DecodedStreamObject()
        stream.set_data(commands)
        procs[NameObject("/" + name)] = writer._add_object(stream)
    cmap = DecodedStreamObject()
    cmap.set_data(b"""/CIDInit /ProcSet findresource begin 12 dict begin begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /BrokenSymbols def /CMapType 2 def
1 begincodespacerange <00> <FF> endcodespacerange
3 beginbfchar <20> <0020> <23> <0023> <47> <0047> endbfchar
endcmap CMapName currentdict /CMap defineresource pop end end""")
    font = DictionaryObject({
        NameObject("/Type"): NameObject("/Font"),
        NameObject("/Subtype"): NameObject("/Type3"),
        NameObject("/Name"): NameObject("/RegressionSymbols"),
        NameObject("/FontBBox"): ArrayObject([NumberObject(n) for n in [0, 0, 600, 600]]),
        NameObject("/FontMatrix"): ArrayObject([FloatObject(n) for n in [.001, 0, 0, .001, 0, 0]]),
        NameObject("/FirstChar"): NumberObject(32),
        NameObject("/LastChar"): NumberObject(71),
        NameObject("/Widths"): ArrayObject([NumberObject(600)] * 40),
        NameObject("/CharProcs"): procs,
        NameObject("/Encoding"): DictionaryObject({NameObject("/Differences"): ArrayObject([
            NumberObject(32), NameObject("/CIRCLE"), NumberObject(35), NameObject("/Circle"), NumberObject(71), NameObject("/LEFTCIRCLE"),
        ])}),
        NameObject("/ToUnicode"): writer._add_object(cmap),
        NameObject("/Resources"): DictionaryObject(),
    })
    valid_cmap = DecodedStreamObject()
    valid_cmap.set_data(cmap.get_data().replace(b"<20> <0020>", b"<20> <2022>"))
    valid_font = DictionaryObject(font)
    valid_font[NameObject("/ToUnicode")] = writer._add_object(valid_cmap)
    # The same painted symbols also cover invalid scalars and replacement mappings.
    invalid_cmap = DecodedStreamObject()
    invalid_cmap.set_data(cmap.get_data().replace(b"<20> <0020>", b"<20> <0000>").replace(b"<23> <0023>", b"<23> <FFFF>").replace(b"<47> <0047>", b"<47> <FFFD>"))
    invalid_font = DictionaryObject(font)
    invalid_font[NameObject("/ToUnicode")] = writer._add_object(invalid_cmap)
    regular = DictionaryObject({
        NameObject("/Type"): NameObject("/Font"),
        NameObject("/Subtype"): NameObject("/Type1"),
        NameObject("/BaseFont"): NameObject("/Helvetica"),
    })
    page[NameObject("/Resources")] = DictionaryObject({
        NameObject("/Font"): DictionaryObject({
            NameObject("/F1"): writer._add_object(font),
            NameObject("/F2"): writer._add_object(regular),
            NameObject("/F3"): writer._add_object(valid_font),
            NameObject("/F4"): writer._add_object(invalid_font),
        })
    })
    content = DecodedStreamObject()
    content.set_data(b"""
BT /F1 20 Tf 1 0 0 1 30 120 Tm <20> Tj ET
BT /F1 20 Tf 1 0 0 1 90 120 Tm <23> Tj ET
BT /F1 20 Tf 1 0 0 1 150 120 Tm <47> Tj 1 0 0 1 150 120 Tm <23> Tj ET
BT /F1 20 Tf 1 0 0 1 210 120 Tm <23> Tj 1 0 0 1 210 120 Tm <47> Tj ET
BT /F2 12 Tf 1 0 0 1 30 70 Tm (G # A B) Tj ET
BT /F1 20 Tf 1 0 0 1 30 30 Tm <47> Tj 1 0 0 1 60 30 Tm <23> Tj ET
BT /F3 20 Tf 1 0 0 1 120 30 Tm <20> Tj ET
BT /F4 20 Tf 1 0 0 1 180 30 Tm <20> Tj ET
BT /F4 20 Tf 1 0 0 1 210 30 Tm <23> Tj ET
BT /F4 20 Tf 1 0 0 1 240 30 Tm <47> Tj ET
""")
    page[NameObject("/Contents")] = writer._add_object(content)
    writer.add_metadata({"/Title": "DocParse symbol glyph-name regression"})
    writer.write(Path(__file__).with_name("symbol_glyph_names.pdf"))


if __name__ == "__main__":
    main()
