# OCR regression image

`printed.png` is page one of `crates/core/tests/fixtures/pdf/embedded_layout.pdf`
rendered at 144 DPI with Poppler. It contains ordinary horizontal text and one
rotated line. The original fixture and its Bitstream Vera notice remain in the
core fixture directory. This raster is test input for real model inference;
no OCR results are embedded in it.

Regenerate from the workspace root:

```sh
rtk proxy pdftoppm -f 1 -l 1 -r 144 -singlefile -png crates/core/tests/fixtures/pdf/embedded_layout.pdf crates/ocr/tests/fixtures/printed
```

Generate image-only English/Chinese rotation pages and a mixed native/scan page:

```sh
rtk proxy uv run --with reportlab --with pillow crates/ocr/tests/fixtures/generate_browser_corpus.py packages/web/test-results/paddle-ocr/inputs
```

Upload these PDFs through the production example. Every scan must report OCR
items and expected text; the mixed page must preserve its native title once.

`native-ocr-overlap.pdf` places native `Total:` / `USD` labels on the same lines
as raster-only values. It verifies partial overlap without duplicating the native
labels. The raster English/Chinese pages also exercise corrected line direction.
