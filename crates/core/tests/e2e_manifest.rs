mod common;

use std::path::{Path, PathBuf};

use common::e2e_manifest::{load_manifest, verify_pdf_directory};

/// Resolves the tracked root corpus manifest from the core crate directory.
fn manifest_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/e2e-corpus.toml")
}

/// Verifies the tracked manifest fixes the expected five-document, 110-page corpus.
#[test]
fn tracked_manifest_has_exact_expected_identity() {
    let manifest =
        load_manifest(&manifest_path()).expect("manifest must validate");

    assert_eq!(manifest.schema_version, 1);
    assert_eq!(manifest.documents.len(), 5);
    assert_eq!(
        manifest
            .documents
            .iter()
            .map(|document| document.page_count)
            .sum::<u32>(),
        110
    );
}

/// Verifies both missing and extra top-level PDFs fail before metadata inspection.
#[test]
fn directory_membership_requires_exact_pdf_set() {
    let manifest =
        load_manifest(&manifest_path()).expect("manifest must validate");
    let directory = tempfile::tempdir().expect("temporary corpus must create");

    let missing = verify_pdf_directory(&manifest, directory.path())
        .expect_err("missing PDFs must fail");
    assert!(missing.to_string().contains("missing="));

    std::fs::write(directory.path().join("unexpected.PDF"), b"not a PDF")
        .expect("extra placeholder must write");
    let extra = verify_pdf_directory(&manifest, directory.path())
        .expect_err("extra PDF must fail");
    assert!(extra.to_string().contains("unexpected.PDF"));
}
