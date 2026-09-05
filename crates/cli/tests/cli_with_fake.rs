use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use clap::Parser;
use docparse_cli::args::Cli;
use docparse_cli::{ParserFactory, run_with_factory};
use docparse_config::ValidatedConfig;
use docparse_core::{
    DocumentContext, DocumentRelations, DocumentResult, PageResult,
    SchemaVersion,
};

/// Fake parser records overlay requests while returning one stable empty page.
struct FakeFactory {
    overlay_calls: AtomicUsize,
}

#[async_trait::async_trait]
impl ParserFactory for FakeFactory {
    /// Returns one deterministic canonical document without touching the input path.
    async fn parse(
        &self,
        _config: Arc<ValidatedConfig>,
        _input: &Path,
    ) -> anyhow::Result<DocumentResult> {
        Ok(DocumentResult::builder()
            .schema_version(SchemaVersion::V2_0)
            .context(DocumentContext::builder().page_count(1).build())
            .pages(vec![
                PageResult::builder()
                    .page_number(1)
                    .width(100.0)
                    .height(100.0)
                    .rotation(0)
                    .blocks(Vec::new())
                    .build(),
            ])
            .relations(DocumentRelations::default())
            .build())
    }

    /// Records the call and writes one marker proving the explicit directory was used.
    async fn write_overlays(
        &self,
        _config: &ValidatedConfig,
        _input: &Path,
        _document: &DocumentResult,
        output_dir: &Path,
    ) -> anyhow::Result<()> {
        self.overlay_calls.fetch_add(1, Ordering::SeqCst);
        std::fs::create_dir_all(output_dir)?;
        std::fs::write(output_dir.join("fake-overlay.svg"), "<svg/>")?;
        Ok(())
    }
}

/// Writes a complete config whose relative artifacts need not exist for the fake.
fn write_config(path: &Path) {
    std::fs::write(path, include_str!("../../../docparse.toml"))
        .expect("test config must write");
}

/// Verifies JSON stdout, atomic file output, and explicit overlay dispatch.
#[tokio::test]
async fn injected_cli_factory_covers_output_and_overlay_paths() {
    let directory =
        tempfile::tempdir().expect("temporary directory must create");
    let config = directory.path().join("docparse.toml");
    let input = directory.path().join("input with spaces.pdf");
    let output = directory.path().join("result.json");
    let overlays = directory.path().join("overlays");
    write_config(&config);
    let factory = FakeFactory {
        overlay_calls: AtomicUsize::new(0),
    };
    let cli = Cli::try_parse_from([
        "docparse",
        "parse",
        input.to_str().expect("input path must be UTF-8"),
        "--config",
        config.to_str().expect("config path must be UTF-8"),
        "--format",
        "json",
        "--output",
        output.to_str().expect("output path must be UTF-8"),
        "--overlay-dir",
        overlays.to_str().expect("overlay path must be UTF-8"),
    ])
    .expect("CLI arguments must parse");

    let stdout = run_with_factory(cli, &factory)
        .await
        .expect("fake CLI run must succeed");

    assert!(stdout.is_none());
    assert_eq!(factory.overlay_calls.load(Ordering::SeqCst), 1);
    assert!(output.is_file());
    assert!(overlays.join("fake-overlay.svg").is_file());
    let document: DocumentResult = serde_json::from_slice(
        &std::fs::read(output).expect("result file must read"),
    )
    .expect("result JSON must decode");
    assert_eq!(document.pages.len(), 1);
}
