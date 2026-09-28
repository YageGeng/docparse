//! Serves the built workbench (WebUI) under the API prefix, at `{api_prefix}/webui`.
//!
//! The rules this module enforces, in order:
//!
//! - The workbench lives at `{api_prefix}/webui`, so nothing outside that mount can
//!   reach the application shell; every unmatched API path keeps the typed JSON 404
//!   envelope by construction rather than by a boundary check.
//! - Requests inside the mount are file lookups. Only `GET`/`HEAD` for the mount root
//!   or for an extension-less path outside the hashed asset namespace falls back to the
//!   entry document, which the same file service resolves as `/index.html`; a missing
//!   bundle stays a 404 and a broken build stays visible.
//! - Successful content-hashed assets are immutable, while the entry document and
//!   everything else, including 404s, are revalidated.
//! - The router redirects `/` to the mount so the UI is discoverable from the origin.

use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use docparse_config::{ConfigError, ServerConfig, WebUi};
use std::path::Path;
use tower::ServiceExt;
use tower_http::services::ServeDir;

/// Mount segment appended to the API prefix: the workbench is served at `{api_prefix}/webui`.
const MOUNT_SEGMENT: &str = "webui";
/// Namespace Vite writes content-hashed bundles into, without the trailing separator.
const ASSET_NAMESPACE: &str = "/assets";
/// Directory form of the hashed bundle namespace.
const HASHED_ASSETS: &str = "/assets/";
/// Entry document served for client-side routes.
const ENTRY_DOCUMENT: &str = "index.html";
/// Request path used to resolve the entry document through the file service.
const ENTRY_PATH: &str = "/index.html";
/// A successful content-hashed asset may be cached until its file name changes.
const IMMUTABLE_CACHE: &str = "public, max-age=31536000, immutable";
/// Unhashed files must be revalidated so a deploy replaces the previous shell.
const REVALIDATE_CACHE: &str = "no-cache";

/// One resolved workbench: where it is mounted, where its files come from, and how to name that source.
pub(crate) struct Workbench {
    /// Mount path without a trailing slash, such as `/api/v1/docparse/webui`.
    mount: String,
    /// Static files, either from the configured directory or compiled into the binary.
    assets: Assets,
    /// Source description used in diagnostics, such as the served directory.
    source: String,
}

impl Workbench {
    /// Resolves the configured workbench source, or `None` when the server only answers the API.
    pub(crate) fn resolve(
        config: &ServerConfig,
    ) -> Result<Option<Self>, ConfigError> {
        let Some(source) = config.webui.as_ref() else {
            return Ok(None);
        };
        let mount = mount_path(&config.api_prefix);
        match source {
            WebUi::Disk(root) => Self::from_directory(&mount, root).map(Some),
            WebUi::Embedded => Self::from_embedded(&mount).map(Some),
        }
    }

    /// Mount path without a trailing slash, for the router to nest this service under.
    pub(crate) fn mount(&self) -> &str {
        &self.mount
    }

    /// Answers one request inside the mount, keeping API paths and missing assets out of the shell.
    ///
    /// Axum strips the mount prefix before calling a nested service, so the request
    /// path is already relative to the mount.
    pub(crate) async fn respond(&self, request: Request<Body>) -> Response {
        let path = request.uri().path().to_owned();
        let inner = if path.is_empty() { "/" } else { path.as_str() };
        // The file service consumes the request, so the validators it needs are kept
        // for the entry-document fallback below.
        let method = request.method().clone();
        let headers = request.headers().clone();
        // The mount root and extension-less navigations may receive the entry document;
        // the asset namespace is always a file lookup, even without an extension.
        let navigation = matches!(method, Method::GET | Method::HEAD)
            && (inner == "/"
                || (Path::new(inner).extension().is_none()
                    && !inner.starts_with(ASSET_NAMESPACE)));
        let mut response = self.assets.call(request).await;
        if response.status() == StatusCode::NOT_FOUND && navigation {
            response = self.entry_response(&method, &headers).await;
        }
        let policy = cache_policy(inner, response.status());
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static(policy));
        // The routine-response marker is applied by the middleware that wraps this
        // mount, so the classification lives next to the trace layer it serves.
        response
    }

    /// Serves the entry document by resolving it through the file service.
    ///
    /// Routing the fallback through the same backend keeps conditional requests,
    /// ranges, precompressed siblings and `HEAD` identical in both sources, and gives
    /// one build one validator instead of one per URL shape. A document that is still
    /// missing here was present at startup, so the deployment lost it: report 500.
    async fn entry_response(
        &self,
        method: &Method,
        headers: &HeaderMap,
    ) -> Response {
        let mut request = Request::new(Body::empty());
        *request.method_mut() = method.clone();
        *request.uri_mut() = Uri::from_static(ENTRY_PATH);
        *request.headers_mut() = headers.clone();
        let response = self.assets.call(request).await;
        if response.status() == StatusCode::NOT_FOUND {
            tracing::error!(
                "workbench entry document {ENTRY_DOCUMENT} is missing from {}",
                self.source
            );
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        response
    }

    /// Serves the workbench from a directory after proving its entry document is readable.
    fn from_directory(mount: &str, root: &Path) -> Result<Self, ConfigError> {
        let entry = root.join(ENTRY_DOCUMENT);
        // A file check rejects a directory named index.html, and opening proves the
        // document can actually be read; requests still read it per navigation.
        let entry_error = || ConfigError::InvalidValue {
            field: "server.webui",
            reason: "must name a directory that contains a readable index.html",
        };
        if !entry.is_file() {
            tracing::warn!(
                "workbench entry document {} is not a file",
                entry.display()
            );
            return Err(entry_error());
        }
        if let Err(error) = std::fs::File::open(&entry) {
            tracing::warn!(
                "workbench entry document {} is unreadable: {}",
                entry.display(),
                error
            );
            return Err(entry_error());
        }
        tracing::info!(
            "serving the workbench from {} at {}",
            root.display(),
            mount
        );
        Ok(Self {
            mount: mount.to_owned(),
            assets: Assets::Directory(
                ServeDir::new(root)
                    .precompressed_gzip()
                    // Axum strips the mount before the service runs, so tower-http
                    // needs the mount to build a reachable redirect target.
                    .redirect_path_prefix(mount),
            ),
            source: root.display().to_string(),
        })
    }
}

/// Derives the mount path from the API prefix: `{api_prefix}/webui`.
///
/// An empty or root prefix mounts the workbench at `/webui`, which keeps the
/// workbench reachable without claiming the whole root namespace.
fn mount_path(api_prefix: &str) -> String {
    let prefix = api_prefix.trim_end_matches('/');
    format!("{prefix}/{MOUNT_SEGMENT}")
}

/// Selects the cache policy; only a served or revalidated content-hashed asset is immutable.
///
/// A 404 keeps the revalidation policy so a missing bundle is never cached as a
/// permanent answer.
fn cache_policy(path: &str, status: StatusCode) -> &'static str {
    let served = status.is_success() || status == StatusCode::NOT_MODIFIED;
    if served && path.starts_with(HASHED_ASSETS) {
        IMMUTABLE_CACHE
    } else {
        REVALIDATE_CACHE
    }
}

/// Static files, either read from disk or compiled into the binary.
#[derive(Clone)]
enum Assets {
    /// Read from the configured directory on every request.
    Directory(ServeDir),
    /// Read from the build embedded by the `embed-web` feature.
    #[cfg(feature = "embed-web")]
    Embedded(
        ServeDir<
            tower_http::services::fs::DefaultServeDirFallback,
            embedded::EmbeddedBackend,
        >,
    ),
}

impl Assets {
    /// Forwards one request to the concrete file service so range, conditional,
    /// and precompressed handling stay in tower-http for both sources.
    ///
    /// The `Service` implementation turns filesystem failures into responses;
    /// its `try_call` counterpart would return the raw error to this module.
    async fn call(&self, request: Request<Body>) -> Response<Body> {
        let response = match self {
            Self::Directory(service) => service.clone().oneshot(request).await,
            #[cfg(feature = "embed-web")]
            Self::Embedded(service) => service.clone().oneshot(request).await,
        };
        response.expect("file service is infallible").map(Body::new)
    }
}

#[cfg(feature = "embed-web")]
mod embedded {
    //! Reads the workbench build compiled into this binary.
    //!
    //! Embedded assets plus the file backend tower-http needs to serve them.
    //! This source is only selected by the `webui = "embedded"` configuration,
    //! so a plain build never starts serving the UI on its own.

    use rust_embed::RustEmbed;
    use std::{
        borrow::Cow,
        future::Future,
        io::{self, Cursor, SeekFrom},
        path::{Component, Path, PathBuf},
        pin::Pin,
        task::{Context, Poll},
        time::{Duration, SystemTime, UNIX_EPOCH},
    };
    use tokio::io::{AsyncRead, AsyncSeek, ReadBuf};
    use tower_http::services::fs::{Backend, File, Metadata};

    /// The workbench build produced by `npm run build --prefix packages/web`.
    ///
    /// `allow_missing` keeps a build usable on a checkout without web tooling;
    /// `Workbench::from_embedded` then rejects the empty set at startup instead
    /// of serving a blank page.
    #[derive(RustEmbed)]
    #[folder = "../../packages/web/dist"]
    #[allow_missing = true]
    pub(super) struct WebAssets;

    /// Reads one embedded file for callers outside this module.
    pub(super) fn entry(key: &str) -> Option<Cow<'static, [u8]>> {
        WebAssets::get(key).map(|file| file.data)
    }

    /// One embedded file opened for reading.
    pub(super) struct EmbeddedFile {
        /// Borrowed bytes in release builds, filesystem bytes in debug builds.
        data: Cursor<Cow<'static, [u8]>>,
        /// Build-time modification time in seconds since the UNIX epoch.
        modified: Option<u64>,
    }

    impl AsyncRead for EmbeddedFile {
        /// Delegates reads to the in-memory cursor.
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.data).poll_read(cx, buf)
        }
    }

    impl AsyncSeek for EmbeddedFile {
        /// Delegates the seek start to the in-memory cursor.
        fn start_seek(
            mut self: Pin<&mut Self>,
            position: SeekFrom,
        ) -> io::Result<()> {
            Pin::new(&mut self.data).start_seek(position)
        }

        /// Reports the completed seek from the in-memory cursor.
        fn poll_complete(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<io::Result<u64>> {
            Pin::new(&mut self.data).poll_complete(cx)
        }
    }

    impl File for EmbeddedFile {
        type Metadata = EmbeddedMetadata;
        type MetadataFuture<'a> = Pin<
            Box<dyn Future<Output = io::Result<EmbeddedMetadata>> + Send + 'a>,
        >;

        /// Reports the opened file's size and timestamp without touching the disk.
        fn metadata(&self) -> Self::MetadataFuture<'_> {
            Box::pin(async move {
                Ok(EmbeddedMetadata {
                    len: self.data.get_ref().len() as u64,
                    modified: self.modified,
                    is_dir: false,
                })
            })
        }
    }

    /// Size and build-time timestamp of one embedded file or directory prefix.
    pub(super) struct EmbeddedMetadata {
        len: u64,
        modified: Option<u64>,
        /// Whether the key is a directory prefix; rust-embed records files only.
        is_dir: bool,
    }

    impl Metadata for EmbeddedMetadata {
        /// Reports whether the key is a directory; tower-http uses it for the 307
        /// trailing-slash redirect.
        fn is_dir(&self) -> bool {
            self.is_dir
        }

        /// Reports the build-time modification time so an unchanged rebuild keeps
        /// its ETag while changed bytes still revalidate.
        fn modified(&self) -> io::Result<SystemTime> {
            Ok(self.modified.map_or(UNIX_EPOCH, |seconds| {
                UNIX_EPOCH + Duration::from_secs(seconds)
            }))
        }

        /// Reports the uncompressed file size used for lengths and ranges.
        fn len(&self) -> u64 {
            self.len
        }
    }

    /// File backend that serves the embedded workbench to tower-http.
    #[derive(Clone, Default)]
    pub(super) struct EmbeddedBackend;

    impl Backend for EmbeddedBackend {
        type File = EmbeddedFile;
        type Metadata = EmbeddedMetadata;
        type OpenFuture =
            Pin<Box<dyn Future<Output = io::Result<EmbeddedFile>> + Send>>;
        type MetadataFuture =
            Pin<Box<dyn Future<Output = io::Result<EmbeddedMetadata>> + Send>>;

        /// Opens one embedded file, mapping a missing entry to `NotFound` so the
        /// caller can fall back to the entry document.
        fn open(&self, path: PathBuf) -> Self::OpenFuture {
            Box::pin(async move {
                let file = embedded_file(&path)?;
                Ok(EmbeddedFile {
                    data: Cursor::new(file.data),
                    modified: file.metadata.last_modified(),
                })
            })
        }

        /// Reports one embedded file's size and timestamp without opening it, which
        /// also resolves the `.gz` siblings used by precompressed serving.
        ///
        /// A key that is missing but names a directory prefix reports a directory, so
        /// the embedded source redirects with 307 exactly like the directory source.
        fn metadata(&self, path: PathBuf) -> Self::MetadataFuture {
            Box::pin(async move {
                let key = embedded_key(&path)?;
                match WebAssets::get(&key) {
                    Some(file) => Ok(EmbeddedMetadata {
                        len: file.data.len() as u64,
                        modified: file.metadata.last_modified(),
                        is_dir: false,
                    }),
                    None if is_directory(&key) => Ok(EmbeddedMetadata {
                        len: 0,
                        modified: None,
                        is_dir: true,
                    }),
                    None => Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!("embedded workbench file is missing: {key}"),
                    )),
                }
            })
        }
    }

    /// Reports whether a key is a directory prefix; rust-embed records files only.
    fn is_directory(key: &str) -> bool {
        let separator = key.len();
        WebAssets::iter().any(|candidate| {
            candidate.len() > separator
                && candidate.starts_with(key)
                && candidate.as_bytes().get(separator) == Some(&b'/')
        })
    }

    /// Reads one embedded entry, reporting a missing key as `NotFound`.
    fn embedded_file(path: &Path) -> io::Result<rust_embed::EmbeddedFile> {
        let key = embedded_key(path)?;
        WebAssets::get(&key).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("embedded workbench file is missing: {key}"),
            )
        })
    }

    /// Converts one backend path into the forward-slash key rust-embed indexes.
    ///
    /// tower-http resolves requests against the configured base path, which yields
    /// `./assets/…` for a base of `""`. Debug builds read the filesystem and
    /// tolerated that prefix, but release builds match the embedded file list
    /// exactly, so the key is rebuilt from normal components only. Anything that
    /// could escape the embedded root is rejected here as well.
    pub(super) fn embedded_key(path: &Path) -> io::Result<String> {
        let mut key = String::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => {
                    let part =
                        part.to_str().ok_or_else(|| unsupported_path(path))?;
                    if !key.is_empty() {
                        key.push('/');
                    }
                    key.push_str(part);
                }
                Component::CurDir => {}
                Component::Prefix(_)
                | Component::RootDir
                | Component::ParentDir => return Err(unsupported_path(path)),
            }
        }
        if key.is_empty() {
            return Err(unsupported_path(path));
        }
        Ok(key)
    }

    /// Reports one backend path that cannot name an embedded file.
    fn unsupported_path(path: &Path) -> io::Error {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("unsupported embedded workbench path: {}", path.display()),
        )
    }
}

/// Rejects the embedded source when this binary cannot provide it.
#[cfg(not(feature = "embed-web"))]
impl Workbench {
    /// Reports that `webui = "embedded"` needs a differently built server.
    fn from_embedded(_mount: &str) -> Result<Self, ConfigError> {
        Err(ConfigError::InvalidValue {
            field: "server.webui",
            reason: "requires building docparse-server with the embed-web feature",
        })
    }
}

/// Serves the workbench compiled into this binary.
#[cfg(feature = "embed-web")]
impl Workbench {
    /// Uses the embedded build after checking that the binary carries an entry document.
    fn from_embedded(mount: &str) -> Result<Self, ConfigError> {
        if embedded::entry(ENTRY_DOCUMENT).is_none() {
            return Err(ConfigError::InvalidValue {
                field: "server.webui",
                reason: "embed-web found no workbench build with an index.html in this binary",
            });
        }
        tracing::info!(
            "serving the workbench embedded in this binary at {}",
            mount
        );
        Ok(Self {
            mount: mount.to_owned(),
            assets: Assets::Embedded(
                ServeDir::with_backend("", embedded::EmbeddedBackend)
                    .precompressed_gzip()
                    .redirect_path_prefix(mount),
            ),
            source: "the embedded build".to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    //! Unit checks for the embedded backend's path rules and the mount derivation.

    /// Backend paths arrive as `./…`; only the exact embedded key may reach rust-embed.
    #[cfg(feature = "embed-web")]
    #[test]
    fn embedded_keys_are_normalized_and_bounded() {
        use std::path::Path;

        for (path, expected) in [
            ("./assets/index-DEADBEEF.js", "assets/index-DEADBEEF.js"),
            ("assets/index-DEADBEEF.js", "assets/index-DEADBEEF.js"),
            ("index.html", "index.html"),
            ("./pdfjs/wasm/openjpeg.wasm", "pdfjs/wasm/openjpeg.wasm"),
        ] {
            assert_eq!(
                super::embedded::embedded_key(Path::new(path))
                    .expect("normalized key")
                    .as_str(),
                expected,
                "{path}"
            );
        }
        for path in ["", ".", "..", "../secret", "/etc/passwd", "./../escape"] {
            assert!(
                super::embedded::embedded_key(Path::new(path)).is_err(),
                "{path} must not name an embedded file"
            );
        }
    }

    /// The mount always extends the API prefix, including an empty or root prefix.
    #[test]
    fn mount_path_follows_the_api_prefix() {
        for (prefix, expected) in [
            ("/api/v1/docparse", "/api/v1/docparse/webui"),
            ("/api", "/api/webui"),
            ("/", "/webui"),
            ("", "/webui"),
        ] {
            assert_eq!(super::mount_path(prefix), expected, "{prefix}");
        }
    }
}
