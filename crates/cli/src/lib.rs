pub mod args;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::{fs::File, io::Write};

use anyhow::{Context, bail};
use args::{
    Cli, Command, InspectModelArgs, OutputFormat, OutputView, ParseArgs,
};
use docparse_config::{ConfigLoader, ValidatedConfig};
use docparse_core::{
    DocParser, DocumentResult, JsonRenderer, MarkdownRenderer, RenderView,
    TextRenderer, write_pdf_overlays,
};
use docparse_layout::{ModelManifest, inspect_model};

/// Injectable parse boundary used by offline CLI behavior tests.
#[async_trait::async_trait]
pub trait ParserFactory: Send + Sync {
    /// Builds any required parser and parses one path into the canonical schema.
    async fn parse(
        &self,
        config: Arc<ValidatedConfig>,
        input: &Path,
    ) -> anyhow::Result<DocumentResult>;

    /// Writes model-independent page overlays for an already parsed document.
    async fn write_overlays(
        &self,
        config: &ValidatedConfig,
        input: &Path,
        document: &DocumentResult,
        output_dir: &Path,
    ) -> anyhow::Result<()>;
}

/// Production factory that constructs the configured default parser.
pub struct DefaultParserFactory;

#[async_trait::async_trait]
impl ParserFactory for DefaultParserFactory {
    /// Loads the real configured engine before parsing the requested path.
    async fn parse(
        &self,
        config: Arc<ValidatedConfig>,
        input: &Path,
    ) -> anyhow::Result<DocumentResult> {
        let parser = DocParser::builder().config(config).build().await?;
        parser.parse_path(input).await.map_err(anyhow::Error::from)
    }

    /// Reopens the source PDF and writes PNG/SVG diagnostics without rerunning ONNX.
    async fn write_overlays(
        &self,
        config: &ValidatedConfig,
        input: &Path,
        document: &DocumentResult,
        output_dir: &Path,
    ) -> anyhow::Result<()> {
        write_pdf_overlays(config, input, document, output_dir).await?;
        Ok(())
    }
}

/// Executes a parsed CLI command with the production parser factory.
pub async fn run(cli: Cli) -> anyhow::Result<Option<String>> {
    run_with_factory(cli, &DefaultParserFactory).await
}

/// Executes one command with an injectable parser and returns stdout content.
pub async fn run_with_factory(
    cli: Cli,
    factory: &dyn ParserFactory,
) -> anyhow::Result<Option<String>> {
    match cli.command {
        Command::Parse(arguments) => parse_command(arguments, factory).await,
        Command::InspectModel(arguments) => {
            inspect_model_command(arguments).map(Some)
        }
    }
}

/// Loads configuration, invokes the parser, renders, and atomically writes if requested.
async fn parse_command(
    arguments: ParseArgs,
    factory: &dyn ParserFactory,
) -> anyhow::Result<Option<String>> {
    let mut raw = load_raw_config(arguments.config, arguments.profile)?;
    if let Some(continue_on_page_error) = arguments.continue_on_page_error {
        raw.runtime.continue_on_page_error = continue_on_page_error;
    }
    let config = Arc::new(ValidatedConfig::try_from(raw)?);
    let formula_placeholder = config.output().formula_placeholder.clone();
    let document = factory
        .parse(Arc::clone(&config), &arguments.input)
        .await
        .with_context(|| {
        format!("failed to parse {}", arguments.input.display())
    })?;
    if let Some(overlay_dir) = &arguments.overlay_dir {
        factory
            .write_overlays(
                config.as_ref(),
                &arguments.input,
                &document,
                overlay_dir,
            )
            .await
            .with_context(|| {
                format!("failed to write overlays to {}", overlay_dir.display())
            })?;
    }
    let view = match arguments.view {
        OutputView::Raw => RenderView::Raw,
        OutputView::Semantic => RenderView::Semantic,
    };
    if let Some(output) = arguments.output {
        // JSON is streamed directly into the temporary file so large documents never require a
        // second complete output buffer; text formats retain their existing in-memory renderers.
        atomic_write(&output, arguments.force, |file| {
            match arguments.format {
                OutputFormat::Json => JsonRenderer::write_with_config(
                    &document,
                    config.output(),
                    &mut *file,
                )
                .map_err(anyhow::Error::from),
                OutputFormat::Text => file
                    .write_all(
                        TextRenderer::new(view, formula_placeholder)
                            .render(&document)
                            .as_bytes(),
                    )
                    .context("failed to stream text output"),
                OutputFormat::Markdown => file
                    .write_all(
                        MarkdownRenderer::new(view, formula_placeholder)
                            .render(&document)
                            .as_bytes(),
                    )
                    .context("failed to stream Markdown output"),
            }
        })?;
        return Ok(None);
    }
    let rendered = match arguments.format {
        OutputFormat::Json => {
            JsonRenderer::render_with_config(&document, config.output())?
        }
        OutputFormat::Text => {
            TextRenderer::new(view, formula_placeholder).render(&document)
        }
        OutputFormat::Markdown => {
            MarkdownRenderer::new(view, formula_placeholder).render(&document)
        }
    };
    Ok(Some(rendered))
}

/// Validates configured artifacts and prints their stable manifest and tensor schema.
fn inspect_model_command(
    arguments: InspectModelArgs,
) -> anyhow::Result<String> {
    let raw = load_raw_config(arguments.config, arguments.profile)?;
    let config = ValidatedConfig::try_from(raw)?;
    let layout = config.layout();
    let manifest = ModelManifest::load_and_verify(
        &layout.model_path,
        &layout.model_config_path,
        &layout.model_manifest_path,
    )?;
    let schema = inspect_model(&layout.model_path)?;
    schema.validate_pp_doclayout_v3()?;
    serde_json::to_string_pretty(&serde_json::json!({
        "model_path": layout.model_path,
        "execution_provider": format!("{:?}", layout.execution_provider).to_lowercase(),
        "manifest": manifest,
        "schema": schema,
    }))
    .map_err(anyhow::Error::from)
}

/// Loads one required primary config without searching any parent directory.
fn load_raw_config(
    config: Option<PathBuf>,
    profile: Option<String>,
) -> anyhow::Result<docparse_config::RawConfig> {
    let path = config.unwrap_or_else(|| PathBuf::from("./docparse.toml"));
    let loader = match profile {
        Some(profile) => ConfigLoader::new(path).with_profile(profile),
        None => ConfigLoader::new(path),
    };
    loader.load_raw().map_err(anyhow::Error::from)
}

/// Streams through a same-directory temporary file and then atomically renames it.
fn atomic_write(
    path: &Path,
    force: bool,
    write: impl FnOnce(&mut File) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if path.exists() && !force {
        bail!(
            "output {} already exists; pass --force to replace it",
            path.display()
        );
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("docparse-output");
    let temporary =
        parent.join(format!(".{file_name}.{}.tmp", std::process::id()));
    let mut file = File::create(&temporary)
        .with_context(|| format!("failed to create {}", temporary.display()))?;
    if let Err(error) = write(&mut file) {
        drop(file);
        let _ = std::fs::remove_file(&temporary);
        return Err(error).with_context(|| {
            format!("failed to write {}", temporary.display())
        });
    }
    drop(file);
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error)
            .with_context(|| format!("failed to replace {}", path.display()));
    }
    Ok(())
}
