fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        println!("cargo:rerun-if-changed=src/pdfium_wasm_shim.c");
        println!("cargo:rustc-link-arg=--allow-multiple-definition");

        cc::Build::new()
            .file("src/pdfium_wasm_shim.c")
            .warnings(false)
            .compile("pdfium_wasm_shim");
    }
}
