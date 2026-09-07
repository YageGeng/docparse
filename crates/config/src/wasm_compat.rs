//! Native configuration loading and browser-specific validation limits.
#[cfg(all(target_arch = "wasm32", not(feature = "wasm")))]
compile_error!("docparse Web builds require the wasm feature");
#[cfg(all(target_arch = "wasm32", not(target_os = "unknown")))]
compile_error!("docparse supports wasm32-unknown-unknown browser builds only");

#[cfg(not(all(feature = "wasm", target_arch = "wasm32")))]
mod platform {
    use std::env;
    use std::path::{Path, PathBuf};

    use figment::Figment;
    use figment::providers::{Env, Format, Serialized, Toml};
    use figment::value::Dict;
    use typed_builder::TypedBuilder;

    use crate::{ConfigError, RawConfig};

    /// Loads a strict configuration from deterministic, explicitly ordered sources.
    #[derive(TypedBuilder)]
    pub struct ConfigLoader {
        #[builder(setter(into))]
        config_path: PathBuf,
        #[builder(default, setter(strip_option, into))]
        profile: Option<String>,
        #[builder(default, setter(strip_option))]
        env_provider: Option<Figment>,
        #[builder(default, setter(strip_option))]
        explicit_overrides: Option<Dict>,
    }

    impl ConfigLoader {
        /// Creates a loader for one required primary configuration file.
        pub fn new(path: impl Into<PathBuf>) -> Self {
            Self::builder().config_path(path).build()
        }

        /// Selects a profile explicitly, ahead of `DOCPARSE_PROFILE`.
        pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
            self.profile = Some(profile.into());
            self
        }

        /// Replaces process environment discovery with an isolated Figment provider.
        pub fn with_env_provider(mut self, provider: Figment) -> Self {
            self.env_provider = Some(provider);
            self
        }

        /// Adds the highest-priority explicit override dictionary.
        pub fn with_overrides(mut self, values: Dict) -> Self {
            self.explicit_overrides = Some(values);
            self
        }

        /// Merges defaults, files, environment, and explicit overrides into `RawConfig`.
        pub fn load_raw(self) -> Result<RawConfig, ConfigError> {
            if !self.config_path.is_file() {
                return Err(ConfigError::ConfigFileNotFound {
                    path: self.config_path,
                });
            }

            let (environment_profile, environment_values) =
                self.environment_values()?;
            let ConfigLoader {
                config_path: requested_path,
                profile,
                env_provider: _,
                explicit_overrides,
            } = self;
            let config_path =
                requested_path.canonicalize().map_err(|source| {
                    ConfigError::ConfigPath {
                        path: requested_path,
                        source,
                    }
                })?;
            let base_directory = config_path
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| ConfigError::ConfigPathHasNoParent {
                    path: config_path.clone(),
                })?;
            let selected_profile = profile.or(environment_profile);

            let mut figment =
                Figment::from(Serialized::defaults(RawConfig::default()))
                    .merge(Toml::file(&config_path));

            if let Some(profile) = selected_profile {
                Self::validate_profile_name(&profile)?;
                let profile_path =
                    base_directory.join(format!("docparse.{profile}.toml"));
                if !profile_path.is_file() {
                    return Err(ConfigError::ProfileFileNotFound {
                        profile,
                        path: profile_path,
                    });
                }

                figment = figment.merge(Toml::file(profile_path));
            }

            figment = figment.merge(Serialized::defaults(environment_values));
            if let Some(overrides) = explicit_overrides {
                figment = figment.merge(Serialized::defaults(overrides));
            }

            let mut config: RawConfig =
                figment.extract().map_err(|source| ConfigError::Load {
                    path: config_path,
                    source: Box::new(source),
                })?;
            config.resolve_paths(&base_directory);
            Ok(config)
        }

        /// Reads configuration values and a profile from either the injected or process environment.
        fn environment_values(
            &self,
        ) -> Result<(Option<String>, Dict), ConfigError> {
            if let Some(provider) = &self.env_provider {
                let mut values: Dict =
                    provider.extract().map_err(|source| {
                        ConfigError::Environment {
                            source: Box::new(source),
                        }
                    })?;
                let profile = match values.remove("profile") {
                    Some(value) => {
                        Some(value.deserialize::<String>().map_err(
                            |source| ConfigError::Environment {
                                source: Box::new(source),
                            },
                        )?)
                    }
                    None => None,
                };
                return Ok((profile, values));
            }

            let profile = match env::var_os("DOCPARSE_PROFILE") {
                Some(value) => Some(value.into_string().map_err(|value| {
                    ConfigError::InvalidProfileEnvironment { value }
                })?),
                None => None,
            };
            // E2E orchestration variables share the product prefix but are not config fields.
            let values = Figment::from(
                Env::prefixed("DOCPARSE_")
                    .ignore(&[
                        "profile",
                        "e2e_pdf_dir",
                        "e2e_manifest",
                        "e2e_output_dir",
                        "e2e_config",
                        "e2e_only",
                        "e2e_write_overlays",
                    ])
                    .split("__"),
            )
            .extract()
            .map_err(|source| ConfigError::Environment {
                source: Box::new(source),
            })?;
            Ok((profile, values))
        }

        /// Rejects profile names that do not map to one fixed sibling filename.
        fn validate_profile_name(profile: &str) -> Result<(), ConfigError> {
            let valid = !profile.is_empty()
                && profile.bytes().all(|byte| {
                    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
                });
            if valid {
                Ok(())
            } else {
                Err(ConfigError::InvalidProfileName {
                    name: profile.to_owned(),
                })
            }
        }
    }

    impl RawConfig {
        /// Resolves all relative model artifact paths against the primary configuration directory.
        fn resolve_paths(&mut self, base_directory: &Path) {
            if self.layout.model_path.is_relative() {
                self.layout.model_path =
                    base_directory.join(&self.layout.model_path);
            }
            if self.layout.model_config_path.is_relative() {
                self.layout.model_config_path =
                    base_directory.join(&self.layout.model_config_path);
            }
            if self.layout.model_manifest_path.is_relative() {
                self.layout.model_manifest_path =
                    base_directory.join(&self.layout.model_manifest_path);
            }
        }
    }

    impl crate::ValidatedConfig {
        /// Leaves native concurrency limits governed by the shared numeric validation.
        pub(crate) fn validate_platform(
            _config: &crate::RawConfig,
        ) -> Result<(), crate::ConfigError> {
            Ok(())
        }
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
mod platform {
    impl crate::ValidatedConfig {
        /// Rejects browser concurrency that the single-Worker implementation cannot provide.
        pub(crate) fn validate_platform(
            config: &crate::RawConfig,
        ) -> Result<(), crate::ConfigError> {
            for (field, value) in [
                ("layout.session_pool_size", config.layout.session_pool_size),
                ("runtime.page_concurrency", config.runtime.page_concurrency),
                (
                    "runtime.render_queue_capacity",
                    config.runtime.render_queue_capacity,
                ),
                (
                    "runtime.blocking_task_limit",
                    config.runtime.blocking_task_limit,
                ),
            ] {
                if value != 1 {
                    return Err(
                        crate::ConfigError::UnsupportedWebConcurrency {
                            field,
                            value,
                        },
                    );
                }
            }
            Ok(())
        }
    }
}

#[allow(unused_imports)]
pub use platform::*;
