"""Derive a broken-map font fixture from the existing licensed Bitstream Vera PDF."""

from pathlib import Path

from pypdf import PdfReader, PdfWriter
from pypdf.generic import DecodedStreamObject, NameObject


def main() -> None:
    """Keep real embedded glyph outlines while making character maps unusable for recovery tests."""
    directory = Path(__file__).parent
    reader = PdfReader(directory / "embedded_layout.pdf")
    writer = PdfWriter()
    writer.append(reader)
    for page in writer.pages:
        for reference in page["/Resources"]["/Font"].values():
            font = reference.get_object()
            if font.get("/Subtype") != "/TrueType":
                continue
            # The known buggy-subset name makes the declared base encoding untrusted.
            font[NameObject("/BaseFont")] = NameObject("/ABCDEF+TTRecovery")
            cmap = DecodedStreamObject()
            pairs = "\n".join(f"<{code:02X}> <0000>" for code in range(256))
            cmap.set_data((
                "/CIDInit /ProcSet findresource begin 12 dict begin begincmap "
                "/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def "
                "/CMapName /Broken def /CMapType 2 def "
                "1 begincodespacerange <00> <FF> endcodespacerange "
                f"256 beginbfchar\n{pairs}\nendbfchar "
                "endcmap CMapName currentdict /CMap defineresource pop end end"
            ).encode("ascii"))
            font[NameObject("/ToUnicode")] = writer._add_object(cmap)
    writer.write(directory / "glyph_recovery.pdf")


if __name__ == "__main__":
    main()
