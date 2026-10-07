//! TensorRT shape profiles, per-configuration engine caches and provider construction for
//! native CUDA sessions; builds without the `tensorrt` feature keep the types inert.
use crate::LayoutError;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// Creates the TensorRT cache directory and proves it is writable before engines are built.
#[cfg(all(not(target_arch = "wasm32"), feature = "tensorrt"))]
fn prepare_directory(dir: &Path) -> Result<(), LayoutError> {
    // TensorRT takes the path as a string; refusing non-UTF-8 keeps the later conversion lossless.
    let created = if dir.to_str().is_some() {
        std::fs::create_dir_all(dir)
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path is not valid UTF-8",
        ))
    };
    created
        .and_then(|()| {
            let probe = dir
                .join(format!(".docparse-write-probe-{}", std::process::id()));
            std::fs::write(&probe, b"")?;
            std::fs::remove_file(probe)
        })
        .map_err(|source| {
            tracing::error!(
                "TensorRT cache directory {} is unusable: {}",
                dir.display(),
                source
            );
            LayoutError::TensorRtCache {
                path: dir.to_path_buf(),
                source,
            }
        })
}

/// One dimension after the batch axis of a TensorRT optimization profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileDim {
    /// The model always receives exactly this size.
    Fixed(usize),
    /// A data-dependent size bounded by the model's own preprocessing.
    Range { min: usize, opt: usize, max: usize },
    /// A data-dependent size whose upper bound comes from config. `opt` is the typical size,
    /// clamped to that bound: measured on the OCR models it leaves inference time unchanged but
    /// roughly halves engine build time compared with optimizing for the bound itself.
    Extent { min: usize, opt: usize },
}

/// Which end of a profile's ranges to format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
// Only TensorRT builds format shapes.
#[cfg_attr(not(feature = "tensorrt"), allow(dead_code))]
enum ProfileBound {
    Min,
    Opt,
    Max,
}

/// Batched input shapes of one model, so TensorRT builds a single engine covering every
/// request instead of rebuilding for minutes whenever a larger shape appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, typed_builder::TypedBuilder)]
// Without the `tensorrt` feature the profile is accepted but never turned into shapes.
#[cfg_attr(not(feature = "tensorrt"), allow(dead_code))]
pub struct TensorRtProfile {
    /// Each batched input by name with its dimensions after the batch axis.
    inputs: &'static [(&'static str, &'static [ProfileDim])],
    /// Largest batch the engine accepts and optimizes for; the smallest is always 1.
    max_batch: usize,
    /// Configured upper bound, and optimization target, of every `ProfileDim::Extent` axis.
    #[builder(default)]
    extent: usize,
    /// Leading 64 bits of the pinned model SHA-256, given as its hex digest. ONNX Runtime keys
    /// cached engines by graph names only, so same-topology models (wired/wireless cells) would
    /// otherwise load each other's engine.
    #[builder(setter(transform = |sha256: &str| model_key(sha256)))]
    model: u64,
}

/// Reduces a pinned SHA-256 hex digest to the 64-bit key that names its cached engines.
fn model_key(sha256: &str) -> u64 {
    debug_assert!(
        sha256.len() == 64
            && sha256.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "expected a pinned SHA-256 hex digest, got {sha256:?}"
    );
    // A non-hex pin could never pass artifact verification, so the fallback is unreachable for
    // any model that actually loads.
    sha256
        .get(..16)
        .and_then(|digits| u64::from_str_radix(digits, 16).ok())
        .unwrap_or_default()
}

#[cfg_attr(not(feature = "tensorrt"), allow(dead_code))]
impl TensorRtProfile {
    /// Rejects an `Extent` axis whose configured bound is below its minimum; TensorRT would
    /// only report the resulting max < min profile as an obscure engine-build failure.
    fn check_extents(&self) -> Result<(), LayoutError> {
        let short = self.inputs.iter().find_map(|(input, dims)| {
            dims.iter().find_map(|dim| match *dim {
                ProfileDim::Extent { min, .. } if self.extent < min => {
                    Some((*input, min))
                }
                _ => None,
            })
        });
        match short {
            Some((input, min)) => Err(LayoutError::TensorRtProfile {
                input,
                min,
                extent: self.extent,
            }),
            None => Ok(()),
        }
    }

    /// Names this model's engine and profile files so no other model, batch size, or extent
    /// configuration can reuse or overwrite them.
    fn cache_prefix(&self) -> String {
        format!("{:016x}_b{}_e{}", self.model, self.max_batch, self.extent)
    }

    /// Formats ONNX Runtime's `name:BxD1xD2,...` profile string for one bound.
    fn shapes(&self, bound: ProfileBound) -> String {
        let batch = match bound {
            ProfileBound::Min => 1,
            ProfileBound::Opt | ProfileBound::Max => self.max_batch,
        };
        self.inputs
            .iter()
            .map(|(name, dims)| {
                let dims =
                    std::iter::once(batch)
                        .chain(dims.iter().map(|dim| match (*dim, bound) {
                            (ProfileDim::Fixed(size), _) => size,
                            (
                                ProfileDim::Range { min, .. },
                                ProfileBound::Min,
                            )
                            | (
                                ProfileDim::Extent { min, .. },
                                ProfileBound::Min,
                            ) => min,
                            (
                                ProfileDim::Range { opt, .. },
                                ProfileBound::Opt,
                            ) => opt,
                            (
                                ProfileDim::Range { max, .. },
                                ProfileBound::Max,
                            ) => max,
                            (
                                ProfileDim::Extent { opt, .. },
                                ProfileBound::Opt,
                            ) => opt.min(self.extent),
                            (ProfileDim::Extent { .. }, ProfileBound::Max) => {
                                self.extent
                            }
                        }))
                        .map(|dim| dim.to_string())
                        .collect::<Vec<_>>()
                        .join("x");
                format!("{name}:{dims}")
            })
            .collect::<Vec<_>>()
            .join(",")
    }
}

/// One configured cache directory, interned for the life of the process so `OnnxBackend` stays
/// `Copy`, together with whether this process has already proven it usable.
#[derive(Debug)]
// Only TensorRT builds probe the directory.
#[cfg_attr(not(feature = "tensorrt"), allow(dead_code))]
struct CacheDir {
    path: PathBuf,
    // Sessions initialize on parallel threads; the lock runs the probe once per directory so
    // concurrent probes cannot delete each other's file.
    prepared: Mutex<bool>,
}

/// The `[runtime] tensorrt_cache_dir` of one configuration; each backend carries its own, so
/// another configuration in the same process can neither replace nor supply it. Public only
/// because the backend builder exposes the field; this module is private and never re-exports it.
#[derive(Debug, Clone, Copy)]
pub struct TensorRtCache(&'static CacheDir);

#[cfg_attr(not(feature = "tensorrt"), allow(dead_code))]
impl TensorRtCache {
    /// Interns `path`, leaking each distinct directory once for the life of the process.
    pub(crate) fn new(path: &Path) -> Self {
        static DIRECTORIES: Mutex<Vec<&'static CacheDir>> =
            Mutex::new(Vec::new());
        let mut directories =
            DIRECTORIES.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(known) = directories.iter().find(|known| known.path == path)
        {
            return Self(known);
        }
        let interned: &'static CacheDir = Box::leak(Box::new(CacheDir {
            path: path.to_path_buf(),
            prepared: Mutex::new(false),
        }));
        directories.push(interned);
        Self(interned)
    }

    /// The configured directory.
    pub(crate) fn path(self) -> &'static Path {
        &self.0.path
    }

    /// Proves the directory usable once per process before engines are built there.
    #[cfg(all(not(target_arch = "wasm32"), feature = "tensorrt"))]
    fn prepare(self) -> Result<(), LayoutError> {
        let mut prepared = self
            .0
            .prepared
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if !*prepared {
            prepare_directory(&self.0.path)?;
            *prepared = true;
        }
        Ok(())
    }
}

#[cfg(all(not(target_arch = "wasm32"), feature = "tensorrt"))]
impl TensorRtProfile {
    /// Builds the TensorRT provider for this profile, caching engines in this configuration's
    /// directory; a missing directory is refused because every start would rebuild every engine.
    pub(crate) fn execution_provider(
        self,
        cache: Option<TensorRtCache>,
    ) -> Result<ort::ep::ExecutionProviderDispatch, LayoutError> {
        self.check_extents().inspect_err(|error| {
            tracing::error!("invalid TensorRT profile: {}", error);
        })?;
        let Some(cache) = cache else {
            tracing::error!(
                "tensorrt builds require [runtime] tensorrt_cache_dir; engines would rebuild on every start"
            );
            return Err(LayoutError::TensorRtCacheUnset);
        };
        cache.prepare()?;
        // Lossless: prepare_directory refused non-UTF-8 paths.
        let path = cache.path().to_string_lossy();
        tracing::info!(
            "registering ONNX execution provider tensorrt with cache {}",
            path
        );
        Ok(ort::ep::TensorRT::default()
            .with_engine_cache(true)
            .with_engine_cache_path(&*path)
            .with_engine_cache_prefix(self.cache_prefix())
            .with_timing_cache(true)
            .with_timing_cache_path(&*path)
            .with_profile_min_shapes(self.shapes(ProfileBound::Min))
            .with_profile_opt_shapes(self.shapes(ProfileBound::Opt))
            .with_profile_max_shapes(self.shapes(ProfileBound::Max))
            .build()
            .error_on_failure())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Well-formed stand-in for a pinned model digest.
    const DIGEST: &str =
        "45bf71750b00739a41fc209f132eb104a4d6b5bb29483c9078164d8b87cf28ba";

    /// An unusable TensorRT cache location fails fast and names the path.
    #[cfg(all(not(target_arch = "wasm32"), feature = "tensorrt"))]
    #[test]
    fn tensorrt_cache_must_be_a_writable_directory() {
        let directory = tempfile::tempdir().expect("directory");
        let file = directory.path().join("not-a-directory");
        std::fs::write(&file, b"x").expect("file");
        let error =
            prepare_directory(&file).expect_err("file is not a directory");
        assert!(error.to_string().contains("not-a-directory"));
        prepare_directory(&directory.path().join("engines"))
            .expect("creatable directory");
    }

    /// TensorRT receives the cache path as a string, so a non-UTF-8 path is refused instead of
    /// being replaced lossily and silently sending engines somewhere else.
    #[cfg(all(unix, not(target_arch = "wasm32"), feature = "tensorrt"))]
    #[test]
    fn tensorrt_cache_path_must_be_utf8() {
        use std::os::unix::ffi::OsStrExt;
        let directory = tempfile::tempdir().expect("directory");
        let path = directory
            .path()
            .join(std::ffi::OsStr::from_bytes(b"engines-\xff"));
        assert!(matches!(
            prepare_directory(&path),
            Err(LayoutError::TensorRtCache { .. })
        ));
    }

    /// Profiles span batch 1 through the configured batch and each dynamic axis's full range.
    #[test]
    fn tensorrt_profile_shapes_cover_batch_and_dynamic_ranges() {
        static INPUTS: [(&str, &[ProfileDim]); 2] = [
            (
                "pixel_mask",
                &[ProfileDim::Range {
                    min: 1,
                    opt: 600,
                    max: 800,
                }],
            ),
            (
                "x",
                &[
                    ProfileDim::Fixed(3),
                    ProfileDim::Extent { min: 32, opt: 1600 },
                    // A typical size above the configured bound is clamped to it.
                    ProfileDim::Extent { min: 32, opt: 4096 },
                ],
            ),
        ];
        let profile = TensorRtProfile::builder()
            .inputs(&INPUTS)
            .max_batch(4)
            .model(DIGEST)
            .extent(2048)
            .build();
        assert_eq!(
            profile.shapes(ProfileBound::Min),
            "pixel_mask:1x1,x:1x3x32x32"
        );
        assert_eq!(
            profile.shapes(ProfileBound::Opt),
            "pixel_mask:4x600,x:4x3x1600x2048"
        );
        assert_eq!(
            profile.shapes(ProfileBound::Max),
            "pixel_mask:4x800,x:4x3x2048x2048"
        );
    }

    /// An `Extent` axis without a configured bound would give TensorRT max < min, which it only
    /// reports as an obscure engine-build failure, so the profile names the input instead.
    #[test]
    fn tensorrt_profile_rejects_extent_below_minimum() {
        static INPUTS: [(&str, &[ProfileDim]); 1] = [(
            "x",
            &[
                ProfileDim::Fixed(3),
                ProfileDim::Extent { min: 32, opt: 64 },
            ],
        )];
        let unbounded = TensorRtProfile::builder()
            .inputs(&INPUTS)
            .max_batch(1)
            .model(DIGEST)
            .build();
        assert!(matches!(
            unbounded.check_extents(),
            Err(LayoutError::TensorRtProfile {
                input: "x",
                min: 32,
                extent: 0
            })
        ));
        let bounded = TensorRtProfile::builder()
            .inputs(&INPUTS)
            .max_batch(1)
            .extent(2048)
            .model(DIGEST)
            .build();
        bounded.check_extents().expect("bounded extent");
    }

    /// A malformed digest would collapse every model onto one cache prefix, so debug builds
    /// refuse it instead of falling back silently.
    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "pinned SHA-256")]
    fn tensorrt_profile_rejects_malformed_digest() {
        static INPUTS: [(&str, &[ProfileDim]); 1] =
            [("x", &[ProfileDim::Fixed(3)])];
        let _ = TensorRtProfile::builder()
            .inputs(&INPUTS)
            .max_batch(1)
            .model("00")
            .build();
    }

    /// ONNX Runtime names cached engines by graph names only, so the prefix must separate
    /// same-topology models (wired/wireless cells) and every batch or extent bound.
    #[test]
    fn tensorrt_cache_prefix_separates_models_batches_and_extents() {
        static INPUTS: [(&str, &[ProfileDim]); 1] =
            [("x", &[ProfileDim::Fixed(3)])];
        const WIRED: &str =
            "bf5490020512a31f43813d90feadae9526a2c3474ffe807571f2c23594f5958f";
        const WIRELESS: &str =
            "47515940ec5c37156e09aa9acb20c4e7e22456cad6ce49473661f1762fb46a78";
        let wired = TensorRtProfile::builder()
            .inputs(&INPUTS)
            .max_batch(4)
            .model(WIRED)
            .build();
        assert_eq!(wired.cache_prefix(), "bf5490020512a31f_b4_e0");
        for other in [
            TensorRtProfile::builder()
                .inputs(&INPUTS)
                .max_batch(4)
                .model(WIRELESS)
                .build(),
            TensorRtProfile::builder()
                .inputs(&INPUTS)
                .max_batch(8)
                .model(WIRED)
                .build(),
            TensorRtProfile::builder()
                .inputs(&INPUTS)
                .max_batch(4)
                .model(WIRED)
                .extent(2048)
                .build(),
        ] {
            assert_ne!(wired.cache_prefix(), other.cache_prefix());
        }
    }
}
