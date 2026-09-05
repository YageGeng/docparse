use assert_cmd::Command;
use predicates::prelude::*;

/// Verifies the binary exposes parse and inspect-model commands in help output.
#[test]
fn binary_help_lists_supported_commands() {
    let mut command =
        Command::cargo_bin("docparse").expect("binary must build");

    command
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("parse"))
        .stdout(predicate::str::contains("inspect-model"));
}

/// Verifies unknown commands fail through clap without a Rust backtrace.
#[test]
fn binary_rejects_unknown_command() {
    let mut command =
        Command::cargo_bin("docparse").expect("binary must build");

    command
        .arg("unknown")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"))
        .stderr(predicate::str::contains("stack backtrace").not());
}
