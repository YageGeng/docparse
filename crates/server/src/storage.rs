use crate::{
    code::ApiCode,
    error::{ApiResult, RequestSnafu, StorageSnafu, TaskSnafu},
};
use snafu::ResultExt;
use std::{
    io::Read,
    path::{Path, PathBuf},
};
use tempfile::NamedTempFile;

/// All replicas mount this directory; published objects are immutable and atomically visible.
#[derive(Clone)]
pub struct SharedStorage {
    root: PathBuf,
}

impl SharedStorage {
    // Filesystem failures use inline status constructors; blocking-task failures retain the common internal fallback.
    /// Resolves the shared directory once without deriving paths from uploaded filenames.
    pub async fn new(root: impl AsRef<Path>) -> ApiResult<Self> {
        tokio::fs::create_dir_all(root.as_ref()).await.context(
            StorageSnafu {
                stage: "storage-create-dir",
                code: ApiCode::service_unavailable(5031003),
            },
        )?;
        let root = tokio::fs::canonicalize(root.as_ref()).await.context(
            StorageSnafu {
                stage: "storage-resolve-dir",
                code: ApiCode::service_unavailable(5031003),
            },
        )?;
        Ok(Self { root })
    }

    /// Places temporary data on the destination filesystem so publication needs no cross-device copy.
    pub async fn temporary(&self) -> ApiResult<NamedTempFile> {
        let root = self.root.clone();
        // Storage work retains both the owning span and any caller-local subscriber across the blocking hop.
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        tokio::task::spawn_blocking(move || {
            tracing::dispatcher::with_default(&dispatcher, || {
                span.in_scope(|| NamedTempFile::new_in(root))
            })
        })
        .await
        .context(TaskSnafu {
            stage: "storage-create-temp-task",
            code: ApiCode::COMMON_INTERNAL_ERROR,
        })?
        .context(StorageSnafu {
            stage: "storage-create-temp",
            code: ApiCode::service_unavailable(5031003),
        })
    }

    /// Syncs bytes and the parent directory before a database row can reference the final name.
    pub async fn publish(
        &self,
        file: NamedTempFile,
        name: &str,
    ) -> ApiResult<()> {
        let target = self.path(name)?;
        let root = self.root.clone();
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        tokio::task::spawn_blocking(move || tracing::dispatcher::with_default(&dispatcher, || span.in_scope(|| -> std::io::Result<()> {
            file.as_file().sync_all()?;
            match file.persist_noclobber(&target) {
                Ok(_) => {}
                Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                    // Do not acknowledge a corrupt/replaced object merely because its immutable name exists.
                    let mut existing = std::fs::File::open(&target)?;
                    let mut pending = std::fs::File::open(error.file.path())?;
                    if !existing.metadata()?.is_file() || existing.metadata()?.len() != pending.metadata()?.len() {
                        return Err(std::io::Error::other("existing object differs from published content"));
                    }
                    let mut left = [0_u8; 64 * 1024];
                    let mut right = [0_u8; 64 * 1024];
                    loop {
                        let count = pending.read(&mut left)?;
                        if count == 0 { break; }
                        let expected = left.get(..count).ok_or_else(|| std::io::Error::other("invalid read length"))?;
                        let actual = right.get_mut(..count).ok_or_else(|| std::io::Error::other("invalid read length"))?;
                        existing.read_exact(actual)?;
                        if expected != actual { return Err(std::io::Error::other("existing object differs from published content")); }
                    }
                }
                Err(error) => return Err(error.error),
            }
            std::fs::File::open(root)?.sync_all()
        })))
        .await
        .context(TaskSnafu {
            stage: "storage-publish-file-task",
            code: ApiCode::COMMON_INTERNAL_ERROR,
        })?
        .context(StorageSnafu {
            stage: "storage-publish-file",
            code: ApiCode::service_unavailable(5031003),
        })
    }

    /// Removes an immutable object and syncs its directory before cleanup can be acknowledged in PostgreSQL.
    pub async fn remove(&self, name: &str) -> ApiResult<()> {
        let path = self.path(name)?;
        let root = self.root.clone();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            // Also sync after NotFound: a previous interrupted attempt may have unlinked without syncing.
            std::fs::File::open(root)?.sync_all()
        })
        .await
        .context(TaskSnafu {
            stage: "storage-remove-task",
            code: ApiCode::COMMON_INTERNAL_ERROR,
        })?
        .context(StorageSnafu {
            stage: "storage-remove-file",
            code: ApiCode::service_unavailable(5031003),
        })
    }

    /// Restricts database and internal object names to a flat namespace without path traversal.
    pub fn path(&self, name: &str) -> ApiResult<PathBuf> {
        if name.is_empty()
            || name.len() > 128
            || name == "."
            || name == ".."
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        {
            return RequestSnafu {
                stage: "storage-check-name",
                code: ApiCode::COMMON_INTERNAL_ERROR,
            }
            .fail();
        }
        Ok(self.root.join(name))
    }

    /// Checks storage writability without retaining a health-check artifact.
    pub async fn ready(&self) -> ApiResult<()> {
        drop(self.temporary().await?);
        Ok(())
    }
}
