# PDF regression fixtures

- `extraction_metadata.pdf`: unembedded Helvetica/Courier fonts for text, style, and metadata regression. Native/Web substitute-font differences are recorded separately.
- `multipage_layout.pdf`: three pages with unembedded fonts, covering document context, different text coverage, and multipage processing.
- `embedded_layout.pdf`: two pages with embedded Bitstream Vera, covering strict numeric parity and rotated text. Its license is in `embedded_layout.LICENSE.txt`.
- `embedded_cjk_90.pdf`: embedded Noto Sans SC with Chinese text, 90-degree page rotation, CropBox `[20,20,592,772]`, and UserUnit 2. Its license is in `embedded_cjk.LICENSE.txt`.
- `symbol_glyph_names.pdf`: an original Type3 font with named circle glyphs deliberately mapped to SPACE, `#`, `G`, U+0000, U+FFFF, and U+FFFD. It checks invalid-Unicode and visible-space recovery, both semicircle/outline paint orders, adjacent independent marks, ordinary-text controls, and source-code provenance. Its generator needs only pypdf and embeds no external font.

The generation scripts live beside the PDFs. Vera comes from reportlab's fonts directory. The Chinese font is pinned to google/fonts commit `2894aab31764f10f29c421bdfd2340d3b382d384`, file `ofl/notosanssc/NotoSansSC[wght].ttf`, SHA-256 `a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da`. Only a font subset is embedded; the full font is not committed. Generating the Chinese fixture requires reportlab and pypdf.

All fixtures use deterministic generation metadata. Ordinary tests use the committed PDFs and do not download fonts or regenerate files. Chinese strings are intentional test data.

Native references include the input PDF SHA-256. Regenerate references after changing an input; browser comparisons reject mismatched input hashes.

## Table structure fixture

`table_layout.pdf` contains three embedded-font pages: a ruled grid, a borderless table, and a tagged table. It exercises grouped/multiline headers, row/column spans, an empty interior cell, decimal punctuation, and unrelated surrounding text. `generate_table_layout.py` regenerates it with ReportLab and pypdf. The Vera fonts use the same redistribution notice as `embedded_layout.LICENSE.txt`.

The real-model native reference test and browser acceptance compare all three pages, including structured cells and their source ranges. Expected dimensions are 8x4 (ruled), 7x4 (text alignment), and 8x4 (tagged PDF). The tagged page deliberately includes an empty TD, which PDFium may omit from its page-filtered tree; the remaining cells must keep their physical columns.
