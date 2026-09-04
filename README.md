# docparse PDFium crates

This workspace contains the low-level PDFium bindings and safe Rust wrapper used by docparse.

## Build

```bash
cargo check --workspace
```

On the first native build, `docparse-pdfium-sys` downloads the PDFium `chromium/8028` archive for the active target from [`run-llama/pdfium-binaries`](https://github.com/run-llama/pdfium-binaries). The archive is cached outside this repository. A local PDFium installation can be selected with `PDFIUM_LIB_PATH` and `PDFIUM_INCLUDE_PATH`.

## Source provenance

The following directories were derived from [LiteParse](https://github.com/run-llama/liteparse):

- `crates/pdfium`
- `crates/pdfium-sys`

The imported snapshot corresponds to LiteParse revision [`b2e76ec5b0c1cb4eb11d67296e916792f4fb5858`](https://github.com/run-llama/liteparse/tree/b2e76ec5b0c1cb4eb11d67296e916792f4fb5858), tagged `crates-v2.14.3`. The imported code has been modified for docparse, including crate/package renaming, workspace dependency inheritance, formatting, documentation, and license/provenance annotations.

LiteParse is licensed under the Apache License 2.0. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

## Third-party software

PDFium is not relicensed by this repository's Apache-2.0 license. Its binary distribution and bundled dependencies carry separate notices. See [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) before redistributing a PDFium shared library or WASM archive.
