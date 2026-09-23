//! Small derived files accelerate immutable results without changing their canonical JSON bytes.
use crate::{
    code::ApiCode,
    error::{ApiResult, SerializeSnafu, StorageSnafu, TaskSnafu},
    storage::SharedStorage,
};
use docparse_core::{DocumentResult, PageError};
use serde::{Deserialize, Serialize};
use snafu::ResultExt;
use std::{
    collections::BTreeMap,
    io::{BufReader, BufWriter, Write},
    ops::Range,
    path::PathBuf,
};

/// Byte ranges refer directly to the original JSON; only document metadata is duplicated.
#[derive(Serialize, Deserialize)]
pub struct ResultIndex {
    pub page_count: u32,
    pub errors: Vec<PageError>,
    pub pages: BTreeMap<u32, Range<u64>>,
}

impl TryFrom<&[u8]> for ResultIndex {
    type Error = serde_json::Error;

    /// Borrows raw pages to index legacy and current envelopes without constructing the full document graph.
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        /// Only page count is needed from document context.
        #[derive(Deserialize)]
        struct Context {
            page_count: u32,
        }
        /// Raw pages borrow their exact source bytes, including whitespace and escaped Unicode.
        #[derive(Deserialize)]
        struct Document<'a> {
            context: Context,
            #[serde(borrow)]
            pages: Vec<&'a serde_json::value::RawValue>,
            errors: Vec<PageError>,
        }
        /// The persisted success envelope retains ownership of the source buffer.
        #[derive(Deserialize)]
        struct Envelope<'a> {
            #[serde(borrow)]
            data: Document<'a>,
        }
        /// Page identity is read independently of order or missing parser pages.
        #[derive(Deserialize)]
        struct Page {
            page_number: u32,
        }
        let envelope: Envelope<'_> = serde_json::from_slice(bytes)?;
        let mut pages = BTreeMap::new();
        for raw in envelope.data.pages {
            let page: Page = serde_json::from_str(raw.get())?;
            let start =
                (raw.get().as_ptr() as usize - bytes.as_ptr() as usize) as u64;
            if page.page_number == 0
                || page.page_number > envelope.data.context.page_count
                || pages
                    .insert(
                        page.page_number,
                        start..start + raw.get().len() as u64,
                    )
                    .is_some()
            {
                return Err(<serde_json::Error as serde::de::Error>::custom(
                    "invalid or duplicate result page number",
                ));
            }
        }
        Ok(Self {
            page_count: envelope.data.context.page_count,
            errors: envelope.data.errors,
            pages,
        })
    }
}

impl SharedStorage {
    /// Builds a missing index or Markdown projection once, sharing a separate coordination lock with deletion.
    pub async fn result_artifact(
        &self,
        name: &str,
        markdown: Option<&str>,
    ) -> ApiResult<PathBuf> {
        let source = self.path(name)?;
        // Version the derived format and Markdown policy; changing configuration cannot reuse stale output.
        let suffix = match markdown {
            Some(placeholder) => format!(
                "{}.md",
                blake3::hash(placeholder.as_bytes())
                    .to_hex()
                    .chars()
                    .take(32)
                    .collect::<String>()
            ),
            None => "index.json".to_owned(),
        };
        // Algorithm fences change Markdown bytes; regenerate earlier cached presentations.
        let version = if markdown.is_some() { "v3" } else { "v1" };
        let cache = self.path(&format!("{name}.{version}.{suffix}"))?;
        let placeholder = markdown.map(str::to_owned);
        let storage = self.clone();
        let name = name.to_owned();
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        tokio::task::spawn_blocking(move || {
            tracing::dispatcher::with_default(&dispatcher, || {
                span.in_scope(|| -> ApiResult<PathBuf> {
                    if cache.try_exists().context(StorageSnafu {
                        stage: "result-check-cache",
                        code: ApiCode::service_unavailable(5031003),
                    })? {
                        return Ok(cache);
                    }
                    // A writable coordination file supports NFS locking without locking payload reads on SMB.
                    let _lock =
                        storage.lock_result(&name).context(StorageSnafu {
                            stage: "result-lock-artifacts",
                            code: ApiCode::service_unavailable(5031003),
                        })?;
                    // Reopen only after locking: a deletion that won the race must prevent cache recreation.
                    let file =
                        std::fs::File::open(&source).context(StorageSnafu {
                            stage: "result-open-source",
                            code: ApiCode::service_unavailable(5031003),
                        })?;
                    if cache.try_exists().context(StorageSnafu {
                        stage: "result-check-cache",
                        code: ApiCode::service_unavailable(5031003),
                    })? {
                        return Ok(cache);
                    }
                    let started = std::time::Instant::now();
                    tracing::info!(
                        "building result artifact {}",
                        cache.display()
                    );
                    let root = source
                        .parent()
                        .unwrap_or_else(|| std::path::Path::new("."));
                    let mut temporary = tempfile::NamedTempFile::new_in(root)
                        .context(StorageSnafu {
                        stage: "result-create-cache",
                        code: ApiCode::service_unavailable(5031003),
                    })?;
                    {
                        let mut writer = BufWriter::with_capacity(
                            256 * 1024,
                            temporary.as_file_mut(),
                        );
                        if let Some(placeholder) = placeholder {
                            /// Markdown uses the same canonical stored document as the original download handler.
                            #[derive(Deserialize)]
                            struct StoredDocument {
                                data: DocumentResult,
                            }
                            let document: StoredDocument =
                                serde_json::from_reader(
                                    BufReader::with_capacity(256 * 1024, &file),
                                )
                                .context(
                                    SerializeSnafu {
                                        stage: "result-decode-json",
                                        code: ApiCode::COMMON_INTERNAL_ERROR,
                                    },
                                )?;
                            let markdown =
                                docparse_core::MarkdownRenderer::new(
                                    docparse_core::RenderView::Semantic,
                                    placeholder,
                                )
                                .render(&document.data);
                            writer.write_all(markdown.as_bytes()).context(
                                StorageSnafu {
                                    stage: "result-write-markdown",
                                    code: ApiCode::service_unavailable(5031003),
                                },
                            )?;
                        } else {
                            let bytes = std::fs::read(&source).context(
                                StorageSnafu {
                                    stage: "result-read-index-source",
                                    code: ApiCode::service_unavailable(5031003),
                                },
                            )?;
                            let index = ResultIndex::try_from(bytes.as_slice())
                                .context(SerializeSnafu {
                                    stage: "result-build-index",
                                    code: ApiCode::COMMON_INTERNAL_ERROR,
                                })?;
                            serde_json::to_writer(&mut writer, &index)
                                .context(SerializeSnafu {
                                    stage: "result-write-index",
                                    code: ApiCode::COMMON_INTERNAL_ERROR,
                                })?;
                        }
                        writer.flush().context(StorageSnafu {
                            stage: "result-flush-cache",
                            code: ApiCode::service_unavailable(5031003),
                        })?;
                    }
                    temporary.as_file().sync_all().context(StorageSnafu {
                        stage: "result-sync-cache",
                        code: ApiCode::service_unavailable(5031003),
                    })?;
                    temporary
                        .persist(&cache)
                        .map_err(|error| error.error)
                        .context(StorageSnafu {
                            stage: "result-publish-cache",
                            code: ApiCode::service_unavailable(5031003),
                        })?;
                    std::fs::File::open(root)
                        .and_then(|directory| directory.sync_all())
                        .context(StorageSnafu {
                            stage: "result-sync-directory",
                            code: ApiCode::service_unavailable(5031003),
                        })?;
                    tracing::info!(
                        "built result artifact {} in {} ms",
                        cache.display(),
                        started.elapsed().as_millis()
                    );
                    Ok(cache)
                })
            })
        })
        .await
        .context(TaskSnafu {
            stage: "result-cache-task",
            code: ApiCode::COMMON_INTERNAL_ERROR,
        })?
    }
}
