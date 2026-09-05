use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

use figment::Figment;
use figment::providers::{Format, Toml};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use typed_builder::TypedBuilder;

/// Validated identity and expected metadata for one local E2E document.
#[derive(Debug, Clone, PartialEq, Eq, TypedBuilder)]
pub(crate) struct E2eDocument {
    pub(crate) logical_id: String,
    pub(crate) basename: String,
    pub(crate) sha256: String,
    pub(crate) size_bytes: u64,
    pub(crate) page_count: u32,
}

/// Stable exact corpus declaration shared by local integration tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct E2eCorpusManifest {
    pub(crate) schema_version: u32,
    pub(crate) documents: Vec<E2eDocument>,
}

/// One manifest row paired with its fully verified local path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedE2eDocument {
    pub(crate) manifest: E2eDocument,
    pub(crate) path: PathBuf,
}

/// Unvalidated TOML root used only at the serialization boundary.
#[derive(Debug, Deserialize)]
struct RawE2eCorpusManifest {
    schema_version: u32,
    documents: Vec<RawE2eDocument>,
}

/// Unvalidated TOML row converted through the typed domain builder.
#[derive(Debug, Deserialize)]
struct RawE2eDocument {
    logical_id: String,
    basename: String,
    sha256: String,
    size_bytes: u64,
    page_count: u32,
}

/// Precise manifest and local corpus validation failures.
#[derive(Debug, thiserror::Error)]
pub(crate) enum E2eManifestError {
    #[error("failed to load E2E manifest {}: {source}", path.display())]
    Load {
        path: PathBuf,
        #[source]
        source: Box<figment::Error>,
    },
    #[error("unsupported E2E manifest schema version {0}")]
    Schema(u32),
    #[error("invalid E2E document {logical_id}: {reason}")]
    InvalidDocument { logical_id: String, reason: String },
    #[error("duplicate E2E logical ID {0}")]
    DuplicateLogicalId(String),
    #[error("duplicate E2E PDF basename {0}")]
    DuplicateBasename(String),
    #[error("failed to scan PDF directory {}: {source}", path.display())]
    Directory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("PDF corpus set mismatch; missing={missing:?}, extra={extra:?}")]
    CorpusSet {
        missing: Vec<String>,
        extra: Vec<String>,
    },
    #[error("PDF metadata mismatch for {basename}: {reason}")]
    Metadata { basename: String, reason: String },
    #[error("failed to hash PDF {}: {source}", path.display())]
    Hash {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to inspect PDF {basename}: {source}")]
    Pdfium {
        basename: String,
        #[source]
        source: pdfium::PdfiumError,
    },
}

impl TryFrom<RawE2eCorpusManifest> for E2eCorpusManifest {
    type Error = E2eManifestError;

    /// Validates schema, identities, basenames, hashes, sizes, and page counts.
    fn try_from(raw: RawE2eCorpusManifest) -> Result<Self, Self::Error> {
        if raw.schema_version != 1 {
            return Err(E2eManifestError::Schema(raw.schema_version));
        }
        let mut logical_ids = BTreeSet::new();
        let mut basenames = BTreeSet::new();
        let mut documents = Vec::with_capacity(raw.documents.len());
        for raw_document in raw.documents {
            if raw_document.logical_id.trim().is_empty() {
                return Err(E2eManifestError::InvalidDocument {
                    logical_id: raw_document.logical_id,
                    reason: "logical_id cannot be empty".to_owned(),
                });
            }
            if !logical_ids.insert(raw_document.logical_id.clone()) {
                return Err(E2eManifestError::DuplicateLogicalId(
                    raw_document.logical_id,
                ));
            }
            if !valid_pdf_basename(&raw_document.basename) {
                return Err(E2eManifestError::InvalidDocument {
                    logical_id: raw_document.logical_id,
                    reason: "basename must be one top-level PDF filename"
                        .to_owned(),
                });
            }
            if !basenames.insert(raw_document.basename.clone()) {
                return Err(E2eManifestError::DuplicateBasename(
                    raw_document.basename,
                ));
            }
            if raw_document.sha256.len() != 64
                || !raw_document.sha256.bytes().all(|byte| {
                    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                })
            {
                return Err(E2eManifestError::InvalidDocument {
                    logical_id: raw_document.logical_id,
                    reason:
                        "sha256 must be 64 lowercase hexadecimal characters"
                            .to_owned(),
                });
            }
            if raw_document.size_bytes == 0 || raw_document.page_count == 0 {
                return Err(E2eManifestError::InvalidDocument {
                    logical_id: raw_document.logical_id,
                    reason: "size_bytes and page_count must be non-zero"
                        .to_owned(),
                });
            }
            documents.push(
                E2eDocument::builder()
                    .logical_id(raw_document.logical_id)
                    .basename(raw_document.basename)
                    .sha256(raw_document.sha256)
                    .size_bytes(raw_document.size_bytes)
                    .page_count(raw_document.page_count)
                    .build(),
            );
        }
        documents.sort_by(|left, right| left.logical_id.cmp(&right.logical_id));
        Ok(Self {
            schema_version: raw.schema_version,
            documents,
        })
    }
}

/// Loads and validates one tracked E2E corpus manifest.
pub(crate) fn load_manifest(
    path: &Path,
) -> Result<E2eCorpusManifest, E2eManifestError> {
    let raw = Figment::new()
        .merge(Toml::file(path))
        .extract::<RawE2eCorpusManifest>()
        .map_err(|source| E2eManifestError::Load {
            path: path.to_path_buf(),
            source: Box::new(source),
        })?;
    E2eCorpusManifest::try_from(raw)
}

/// Verifies exact top-level PDF membership, bytes, size, and PDFium page count.
pub(crate) fn verify_pdf_directory(
    manifest: &E2eCorpusManifest,
    pdf_dir: &Path,
) -> Result<Vec<VerifiedE2eDocument>, E2eManifestError> {
    let entries = std::fs::read_dir(pdf_dir).map_err(|source| {
        E2eManifestError::Directory {
            path: pdf_dir.to_path_buf(),
            source,
        }
    })?;
    let mut discovered = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|source| E2eManifestError::Directory {
            path: pdf_dir.to_path_buf(),
            source,
        })?;
        let file_type = entry.file_type().map_err(|source| {
            E2eManifestError::Directory {
                path: entry.path(),
                source,
            }
        })?;
        if !file_type.is_file() {
            continue;
        }
        let path = entry.path();
        let is_pdf = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"));
        if is_pdf {
            discovered
                .insert(entry.file_name().to_string_lossy().into_owned(), path);
        }
    }
    let expected: BTreeSet<_> = manifest
        .documents
        .iter()
        .map(|document| document.basename.clone())
        .collect();
    let actual: BTreeSet<_> = discovered.keys().cloned().collect();
    if expected != actual {
        return Err(E2eManifestError::CorpusSet {
            missing: expected.difference(&actual).cloned().collect(),
            extra: actual.difference(&expected).cloned().collect(),
        });
    }

    let library = pdfium::Library::init();
    let mut verified = Vec::with_capacity(manifest.documents.len());
    for document in &manifest.documents {
        let path = discovered.get(&document.basename).ok_or_else(|| {
            E2eManifestError::Metadata {
                basename: document.basename.clone(),
                reason: "matched corpus path disappeared".to_owned(),
            }
        })?;
        let metadata =
            path.metadata()
                .map_err(|source| E2eManifestError::Directory {
                    path: path.clone(),
                    source,
                })?;
        if metadata.len() != document.size_bytes {
            return Err(E2eManifestError::Metadata {
                basename: document.basename.clone(),
                reason: format!(
                    "size expected {}, got {}",
                    document.size_bytes,
                    metadata.len()
                ),
            });
        }
        let actual_hash = sha256_file(path)?;
        if actual_hash != document.sha256 {
            return Err(E2eManifestError::Metadata {
                basename: document.basename.clone(),
                reason: format!(
                    "sha256 expected {}, got {actual_hash}",
                    document.sha256
                ),
            });
        }
        let path_text =
            path.to_str().ok_or_else(|| E2eManifestError::Metadata {
                basename: document.basename.clone(),
                reason: "path is not valid UTF-8".to_owned(),
            })?;
        let pdf = library.load_document(path_text, None).map_err(|source| {
            E2eManifestError::Pdfium {
                basename: document.basename.clone(),
                source,
            }
        })?;
        let actual_pages = u32::try_from(pdf.page_count()).unwrap_or_default();
        if actual_pages != document.page_count {
            return Err(E2eManifestError::Metadata {
                basename: document.basename.clone(),
                reason: format!(
                    "page_count expected {}, got {actual_pages}",
                    document.page_count
                ),
            });
        }
        verified.push(VerifiedE2eDocument {
            manifest: document.clone(),
            path: path.clone(),
        });
    }
    Ok(verified)
}

/// Returns whether a name is a traversal-free top-level PDF basename.
fn valid_pdf_basename(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && path.file_name().is_some_and(|name| name == value)
        && path.components().count() == 1
        && path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
}

/// Streams one file into a lowercase SHA-256 digest.
fn sha256_file(path: &Path) -> Result<String, E2eManifestError> {
    let file = File::open(path).map_err(|source| E2eManifestError::Hash {
        path: path.to_path_buf(),
        source,
    })?;
    let mut reader = BufReader::new(file);
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(|source| {
            E2eManifestError::Hash {
                path: path.to_path_buf(),
                source,
            }
        })?;
        if count == 0 {
            break;
        }
        if let Some(chunk) = buffer.get(..count) {
            digest.update(chunk);
        }
    }
    Ok(lowercase_hex(digest.finalize().as_ref()))
}

/// Encodes digest bytes without relying on removed `LowerHex` implementations.
fn lowercase_hex(bytes: &[u8]) -> String {
    let mut encoded = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            let digit = match nibble {
                0..=9 => b'0' + nibble,
                _ => b'a' + (nibble - 10),
            };
            encoded.push(char::from(digit));
        }
    }
    encoded
}
