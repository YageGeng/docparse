# Third-party notices

This document records the origin and redistribution requirements of third-party software used by the PDFium crates. It is informational and does not replace the applicable license texts.

## LiteParse

The Rust source under `crates/pdfium` and `crates/pdfium-sys` is derived from [LiteParse](https://github.com/run-llama/liteparse), revision [`b2e76ec5b0c1cb4eb11d67296e916792f4fb5858`](https://github.com/run-llama/liteparse/tree/b2e76ec5b0c1cb4eb11d67296e916792f4fb5858), tag `crates-v2.14.3`.

License: Apache License 2.0. The complete license text is in [LICENSE](LICENSE). Modified imported files carry an SPDX identifier and a prominent source/change notice.

## PDFium binary distribution

The build script downloads target-specific PDFium release `chromium/8028` from [run-llama/pdfium-binaries](https://github.com/run-llama/pdfium-binaries). This source repository does not contain a PDFium binary.

The downloaded archive contains:

- a root `LICENSE` for the binary distribution;
- `licenses/pdfium.txt` for PDFium;
- separate license files for Abseil, Anti-Grain Geometry, fast_float, FreeType, ICU, Little CMS, libjpeg-turbo, OpenJPEG, libpng, LLVM libc, simdutf, and zlib.

If a docparse package, installer, container, application bundle, wheel, npm package, or other artifact redistributes `libpdfium`, `pdfium.dll`, `libpdfium.dylib`, or the PDFium WASM archive, it must also redistribute the downloaded archive's root `LICENSE` and complete `licenses/` directory, or equivalent readable copies of every applicable notice. Keeping only the shared library is not sufficient.

### run-llama/pdfium-binaries license

Copyright 2014-2025 Benoit Blanchon

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.

### PDFium BSD notice

Copyright 2014 The PDFium Authors

Redistribution and use in source and binary forms, with or without modification, are permitted provided that the following conditions are met:

- Redistributions of source code must retain the above copyright notice, this list of conditions and the following disclaimer.
- Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following disclaimer in the documentation and/or other materials provided with the distribution.
- Neither the name of Google Inc. nor the names of its contributors may be used to endorse or promote products derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

## PP-DocLayoutV3

The optional runtime model is downloaded from [PaddlePaddle/PP-DocLayoutV3_onnx](https://huggingface.co/PaddlePaddle/PP-DocLayoutV3_onnx), fixed at revision `46bbdf188bb0a772c08aed74882ce7e51a8f1ea6`.

- Model SHA-256: `45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba`
- Inference configuration SHA-256: `506fcfac13b3b546ae40d7886b44126420f392adb694e3f8bb6a6286a1f90fdc`
- License: Apache License 2.0

The model files are not committed or packaged. Redistributors who bundle them must preserve the model repository's license and notices.

## ort and ONNX Runtime

DocParse uses Rust crate `ort` version `2.0.0-rc.13`, which dynamically loads the separately downloaded ONNX Runtime binary selected by `ort-sys`.

- ort source: [pykeio/ort](https://github.com/pykeio/ort)
- ort license: MIT OR Apache-2.0
- ONNX Runtime source: [microsoft/onnxruntime](https://github.com/microsoft/onnxruntime)
- ONNX Runtime license: MIT

CUDA, CoreML, and OpenVINO execution providers may load additional vendor runtimes. Their installation and redistribution terms remain the distributor's responsibility.

## geo

Geometry intersection and validation use Rust crate `geo` version `0.33.1`.

- Source: [georust/geo](https://github.com/georust/geo)
- License: MIT OR Apache-2.0

## Python oracle and E2E dependencies

PaddleOCR 3.6.0, PaddleX, OpenCV, NumPy, Python ONNX Runtime, psutil, and pypdf are used only by locked development/test scripts. They are not linked into or distributed with the Rust runtime crates. Their own package licenses apply to anyone redistributing those development environments.
