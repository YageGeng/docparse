/// Rejects unsupported targets and configures PDFium's final browser artifact.
fn main() {
    println!("cargo:rerun-if-env-changed=TARGET");
    if std::env::var("TARGET").as_deref() != Ok("wasm32-unknown-unknown") {
        eprintln!(
            "docparse-web only supports wasm32-unknown-unknown; use cargo build -p docparse-web --target wasm32-unknown-unknown, or omit --workspace for the default native crates"
        );
        std::process::exit(1);
    }
    // The pinned PDFium archive supplies the real longjmp implementation.
    println!("cargo:rustc-link-arg=--allow-multiple-definition");
}
