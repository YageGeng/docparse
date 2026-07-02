use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};
use docparse_core::{
    DocumentInput, LayoutAnalyzer, LayoutBlock, LoadDocumentOptions,
    load_document_pages,
};
use docparse_ort::{OrtLayoutAnalyzer, OrtLayoutConfig};
use futures::executor::block_on;

/// Document parsing CLI.
#[derive(Debug, Parser)]
#[command(name = "docparse")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Layout analysis commands.
    Layout {
        #[command(subcommand)]
        command: LayoutCommands,
    },
}

#[derive(Debug, Subcommand)]
enum LayoutCommands {
    /// Analyze an image or PDF and emit JSON layout detections.
    Analyze(AnalyzeArgs),
}

#[derive(Debug, Parser)]
struct AnalyzeArgs {
    /// Input image or PDF path.
    #[arg(long)]
    input: PathBuf,
    /// ONNX model path.
    #[arg(long, default_value = "models/pp-structure-v3-onnx/inference.onnx")]
    model: PathBuf,
    /// Maximum number of PDF pages to analyze.
    #[arg(long = "max-page", alias = "max_page", default_value_t = 1, value_parser = parse_positive_usize)]
    max_page: usize,
    /// PDF render DPI.
    #[arg(long = "pdf-dpi", alias = "pdf_dpi", default_value_t = 144.0)]
    pdf_dpi: f32,
    /// Output JSON path. Defaults to stdout.
    #[arg(long)]
    output: Option<PathBuf>,
}

#[derive(Debug, serde::Serialize)]
struct AnalyzeOutput {
    pages: Vec<AnalyzePage>,
}

#[derive(Debug, serde::Serialize)]
struct AnalyzePage {
    page_index: usize,
    width: u32,
    height: u32,
    blocks: Vec<LayoutBlock>,
}

fn main() -> Result<()> {
    run_cli(Cli::parse())
}

fn run_cli(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Layout { command } => match command {
            LayoutCommands::Analyze(args) => run_layout_analyze(args),
        },
    }
}

fn run_layout_analyze(args: AnalyzeArgs) -> Result<()> {
    let analyzer = OrtLayoutAnalyzer::new(OrtLayoutConfig {
        model_path: args.model,
        ..OrtLayoutConfig::default()
    })?;
    let document_pages = load_document_pages(
        &DocumentInput { path: args.input },
        LoadDocumentOptions {
            max_pages: args.max_page,
            pdf_dpi: args.pdf_dpi,
        },
    )?;

    let mut pages = Vec::with_capacity(document_pages.len());
    for document_page in document_pages {
        let layout_page =
            block_on(analyzer.analyze_image(&document_page.image))?;
        pages.push(AnalyzePage {
            page_index: document_page.page_index,
            width: layout_page.width,
            height: layout_page.height,
            blocks: layout_page.blocks,
        });
    }

    let json = serde_json::to_string_pretty(&AnalyzeOutput { pages })?;
    if let Some(output) = args.output {
        std::fs::write(output, json)?;
    } else {
        println!("{json}");
    }

    Ok(())
}

fn parse_positive_usize(value: &str) -> Result<usize, String> {
    let parsed = value
        .parse::<usize>()
        .map_err(|error| format!("invalid unsigned integer: {error}"))?;
    if parsed == 0 {
        return Err("value must be greater than 0".to_owned());
    }
    Ok(parsed)
}
