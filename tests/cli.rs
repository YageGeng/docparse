//! Verifies the docparse command-line interface routes layout commands.

use std::process::Command;

/// Ensures `docparse layout` is routed to the CLI handler before inference.
#[test]
fn layout_command_reaches_layout_handler() {
    let output = Command::new(env!("CARGO_BIN_EXE_docparse"))
        .arg("layout")
        .output()
        .expect("docparse binary should run");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("usage: docparse layout [--profile] <image>")
    );
}
