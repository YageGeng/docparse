# PDF regression fixtures

- `extraction_metadata.pdf`: unembedded Helvetica/Courier fonts for text, style, and metadata regression. Native/Web substitute-font differences are recorded separately.
- `multipage_layout.pdf`: three pages with unembedded fonts, covering document context, different text coverage, and multipage processing.
- `embedded_layout.pdf`: two pages with embedded Bitstream Vera, covering strict numeric parity and rotated text. Its license is in `embedded_layout.LICENSE.txt`.
- `embedded_cjk_90.pdf`: embedded Noto Sans SC with Chinese text, 90-degree page rotation, CropBox `[20,20,592,772]`, and UserUnit 2. Its license is in `embedded_cjk.LICENSE.txt`.

The generation scripts live beside the PDFs. Vera comes from reportlab's fonts directory. The Chinese font is pinned to google/fonts commit `2894aab31764f10f29c421bdfd2340d3b382d384`, file `ofl/notosanssc/NotoSansSC[wght].ttf`, SHA-256 `a3041811a78c361b1de50f953c805e0244951c21c5bd412f7232ef0d899af0da`. Only a font subset is embedded; the full font is not committed. Generating the Chinese fixture requires reportlab and pypdf.

All fixtures use deterministic generation metadata. Ordinary tests use the committed PDFs and do not download fonts or regenerate files. Chinese strings are intentional test data.

Native references include the input PDF SHA-256. Regenerate references after changing an input; browser comparisons reject mismatched input hashes.
