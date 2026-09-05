use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use typed_builder::TypedBuilder;

/// Complete command-line surface for parsing and model inspection.
#[derive(Debug, Parser)]
#[command(
    name = "docparse",
    version,
    about = "Parse PDF documents with native text and layout fusion"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Supported top-level commands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Parse a PDF into canonical JSON, plain text, or Markdown.
    Parse(ParseArgs),
    /// Validate and print the configured PP-DocLayoutV3 artifact contract.
    InspectModel(InspectModelArgs),
}

/// Arguments controlling one complete PDF parse.
#[derive(Debug, Args, TypedBuilder)]
pub struct ParseArgs {
    /// Input PDF path.
    pub input: PathBuf,
    /// Primary config path; defaults to exactly ./docparse.toml.
    #[arg(long)]
    #[builder(default)]
    pub config: Option<PathBuf>,
    /// Optional sibling profile name.
    #[arg(long)]
    #[builder(default)]
    pub profile: Option<String>,
    /// Output serialization format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
    #[builder(default)]
    pub format: OutputFormat,
    /// Literal or relation-aware presentation view.
    #[arg(long, value_enum, default_value_t = OutputView::Raw)]
    #[builder(default)]
    pub view: OutputView,
    /// Destination file; stdout is used when omitted.
    #[arg(long)]
    #[builder(default)]
    pub output: Option<PathBuf>,
    /// Optional override for PDFium page-stage continuation.
    #[arg(long)]
    #[builder(default)]
    pub continue_on_page_error: Option<bool>,
    /// Optional directory for per-page PNG/SVG diagnostic overlays.
    #[arg(long)]
    #[builder(default)]
    pub overlay_dir: Option<PathBuf>,
    /// Allow replacing an existing output file.
    #[arg(long)]
    #[builder(default)]
    pub force: bool,
}

/// Arguments controlling model artifact inspection.
#[derive(Debug, Args, TypedBuilder)]
pub struct InspectModelArgs {
    /// Primary config path; defaults to exactly ./docparse.toml.
    #[arg(long)]
    #[builder(default)]
    pub config: Option<PathBuf>,
    /// Optional sibling profile name.
    #[arg(long)]
    #[builder(default)]
    pub profile: Option<String>,
}

/// Supported parser output formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Json,
    Text,
    Markdown,
}

/// Supported presentation projections for text-like formats.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum OutputView {
    #[default]
    Raw,
    Semantic,
}
