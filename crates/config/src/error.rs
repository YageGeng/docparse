use std::ffi::OsString;
use std::path::PathBuf;

/// Errors produced while locating, merging, or decoding configuration sources.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// Browser parsing intentionally bounds every concurrency setting to one.
    #[error("unsupported Web concurrency for {field}: expected 1, got {value}")]
    UnsupportedWebConcurrency { field: &'static str, value: usize },
    /// The required primary configuration file does not exist.
    #[error("configuration file not found: {path}")]
    ConfigFileNotFound { path: PathBuf },

    /// A selected profile does not have a matching configuration file.
    #[error("configuration profile '{profile}' not found: {path}")]
    ProfileFileNotFound { profile: String, path: PathBuf },

    /// A profile name could escape the fixed profile filename convention.
    #[error("invalid configuration profile name: {name}")]
    InvalidProfileName { name: String },

    /// The primary configuration path could not be made absolute.
    #[error("failed to resolve configuration path {path}: {source}")]
    ConfigPath {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// A resolved configuration file did not have a usable parent directory.
    #[error("resolved configuration path has no parent directory: {path}")]
    ConfigPathHasNoParent { path: PathBuf },

    /// Environment-backed configuration could not be decoded.
    #[error("failed to decode DOCPARSE_ environment configuration: {source}")]
    Environment {
        #[source]
        source: Box<figment::Error>,
    },

    /// A selected profile environment variable is not valid Unicode.
    #[error("DOCPARSE_PROFILE is not valid Unicode: {value:?}")]
    InvalidProfileEnvironment { value: OsString },

    /// Merged configuration could not be decoded into the strict schema.
    #[error("failed to load configuration from {path}: {source}")]
    Load {
        path: PathBuf,
        #[source]
        source: Box<figment::Error>,
    },

    /// A decoded value violates the validated configuration contract.
    #[error("invalid configuration value at {field}: {reason}")]
    InvalidValue {
        field: &'static str,
        reason: &'static str,
    },
}
