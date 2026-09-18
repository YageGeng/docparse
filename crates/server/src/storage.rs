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

/// A publication retains optional timing through the final blocking filesystem operation.
pub struct Publication {
    file: NamedTempFile,
    timing: Option<docparse_common::telemetry::Timer>,
}
impl From<NamedTempFile> for Publication {
    /// Uploads and ordinary storage users keep their existing untimed publication API.
    fn from(file: NamedTempFile) -> Self {
        Self { file, timing: None }
    }
}
impl From<(NamedTempFile, docparse_common::telemetry::Timer)> for Publication {
    /// Result serialization transfers its timer together with the bytes awaiting durable publication.
    fn from(
        (file, timing): (NamedTempFile, docparse_common::telemetry::Timer),
    ) -> Self {
        Self {
            file,
            timing: Some(timing),
        }
    }
}

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
        file: impl Into<Publication>,
        name: &str,
    ) -> ApiResult<()> {
        let Publication { file, timing } = file.into();
        let target = self.path(name)?;
        let root = self.root.clone();
        let span = tracing::Span::current();
        let dispatcher = tracing::dispatcher::get_default(Clone::clone);
        tokio::task::spawn_blocking(move || tracing::dispatcher::with_default(&dispatcher, || span.in_scope(|| -> std::io::Result<()> {
            // The real fsync/persist owner retains timing after cancellation of the async caller.
            let _timing = timing;
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
        let storage = self.clone();
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || -> std::io::Result<()> {
            // Coordination never locks the data inode, so mandatory SMB locks cannot disrupt result readers.
            let _lock = storage.lock_result(&name)?;
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                let prefix = format!(
                    "{}.",
                    path.file_name().unwrap_or_default().to_string_lossy()
                );
                for entry in std::fs::read_dir(&root)? {
                    let entry = entry?;
                    if entry.file_name().to_string_lossy().starts_with(&prefix)
                    {
                        match std::fs::remove_file(entry.path()) {
                            Ok(()) => {}
                            Err(error)
                                if error.kind()
                                    == std::io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
            }
            match std::fs::remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            // Also sync after NotFound: a previous interrupted attempt may have unlinked without syncing.
            std::fs::File::open(root)?.sync_all()?;
            Ok(())
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

    /// Acquires a writable, stable coordination inode from blocking filesystem tasks on every replica.
    pub(crate) fn lock_result(
        &self,
        name: &str,
    ) -> std::io::Result<std::fs::File> {
        // Validate the object identity before deriving a path in the reserved lock namespace.
        self.path(name).map_err(std::io::Error::other)?;
        let directory = self.root.join(".locks");
        std::fs::create_dir_all(&directory)?;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(directory.join(name))?;
        // ponytail: retain lock inodes after deletion to prevent split locks; reclaim only with all replicas stopped.
        file.lock()?;
        Ok(file)
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
