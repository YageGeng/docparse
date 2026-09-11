//! Native LiteParse-compatible outline hash database with bounded, lazy shard loading.
// SPDX-License-Identifier: Apache-2.0
// Database layout follows LiteParse 4d4a51c246ff56d382166930942898f3ff563eba.

use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

type Shard = HashMap<[u8; 16], u32>;

/// Resolves glyphs from lazily cached `[hash, unicode]` MessagePack shard records.
pub struct FontDbResolver {
    directory: PathBuf,
    shards: RwLock<HashMap<u16, Option<Arc<Shard>>>>,
}

impl FontDbResolver {
    /// Selects a database directory without performing I/O until an outline is requested.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            shards: RwLock::new(HashMap::new()),
        }
    }

    /// Loads one bounded shard; malformed records reject the entire shard instead of caching partial guesses.
    fn load_shard(&self, prefix: u16) -> io::Result<Shard> {
        let path = self.directory.join(format!("{prefix:04x}.msgpack"));
        let file = std::fs::File::open(path)?;
        if file.metadata()?.len() > 64 * 1024 * 1024 {
            return Err(io::Error::other("font database shard exceeds 64 MiB"));
        }
        let mut reader = BufReader::new(file.take(64 * 1024 * 1024 + 1));
        let mut shard = HashMap::new();
        let mut records = 0;
        while !reader.fill_buf()?.is_empty() {
            records += 1;
            if records > 1_000_000
                || rmp::decode::read_array_len(&mut reader)
                    .map_err(io::Error::other)?
                    != 2
            {
                return Err(io::Error::other(
                    "invalid or excessive font database records",
                ));
            }
            let length = rmp::decode::read_bin_len(&mut reader)
                .map_err(io::Error::other)?;
            // Producers store either the truncated 16-byte key or the full 32-byte BLAKE3 digest.
            if !(16..=32).contains(&length) {
                return Err(io::Error::other(
                    "invalid font database hash length",
                ));
            }
            let mut bytes = [0; 32];
            reader.read_exact(
                bytes
                    .get_mut(..length as usize)
                    .ok_or_else(|| io::Error::other("invalid key"))?,
            )?;
            let key: [u8; 16] = bytes
                .get(..16)
                .and_then(|bytes| bytes.try_into().ok())
                .ok_or_else(|| io::Error::other("invalid key"))?;
            let value = rmp::decode::read_int::<u32, _>(&mut reader)
                .map_err(io::Error::other)?;
            if value != 0
                && char::from_u32(value).is_some_and(|c| {
                    !c.is_control() && !matches!(c, '\u{FFFD}'..='\u{FFFF}')
                })
            {
                shard.insert(key, value);
            }
        }
        Ok(shard)
    }
}

impl crate::GlyphResolver for FontDbResolver {
    /// Looks up the first sixteen BLAKE3 bytes of little-endian `(i32, f32, f32)` outline segments.
    fn resolve(&self, segments: &[(i32, f32, f32)]) -> Option<String> {
        let mut hasher = blake3::Hasher::new();
        for &(kind, x, y) in segments {
            if !x.is_finite() || !y.is_finite() {
                return None;
            }
            hasher.update(&kind.to_le_bytes());
            hasher.update(&x.to_le_bytes());
            hasher.update(&y.to_le_bytes());
        }
        let hash = hasher.finalize();
        let key: [u8; 16] = hash.as_bytes().get(..16)?.try_into().ok()?;
        let prefix = u16::from_be_bytes(key.get(..2)?.try_into().ok()?);
        // Missing and invalid shards are memoized too, avoiding repeated disk reads for unknown glyphs.
        let cached = self
            .shards
            .read()
            .ok()?
            .get(&prefix)
            .map(|shard| shard.as_ref().map(Arc::clone));
        let shard = match cached {
            Some(shard) => shard?,
            None => {
                // ponytail: concurrent cold lookups may read twice; use per-shard OnceLock if cold-load contention matters.
                let loaded = match self.load_shard(prefix) {
                    Ok(shard) => Some(Arc::new(shard)),
                    Err(error) => {
                        tracing::debug!(
                            "font database shard {:04x} unavailable: {}",
                            prefix,
                            error
                        );
                        None
                    }
                };
                self.shards
                    .write()
                    .ok()?
                    .entry(prefix)
                    .or_insert(loaded)
                    .as_ref()
                    .map(Arc::clone)?
            }
        };
        char::from_u32(*shard.get(&key)?).map(|c| c.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::GlyphResolver;

    /// Fixed independent BLAKE3 empty-input bytes check database compatibility and negative caching.
    #[test]
    fn outline_database_validates_records_and_caches_missing_shards() {
        let directory = tempfile::tempdir().expect("directory");
        let key = [
            0xaf, 0x13, 0x49, 0xb9, 0xf5, 0xf9, 0xa1, 0xa6, 0xa0, 0x40, 0x4d,
            0xea, 0x36, 0xdc, 0xc9, 0x49,
        ];
        let mut record = vec![0x92, 0xc4, 0x10];
        record.extend(key);
        record.extend([0xcd, 0x01, 0x60]); // U+0160, encoded independently of the reader.
        let path = directory.path().join("af13.msgpack");
        std::fs::write(&path, &record).expect("shard");
        let resolver = FontDbResolver::new(directory.path());
        assert_eq!(resolver.resolve(&[]).as_deref(), Some("Š"));
        std::fs::remove_file(&path).expect("remove shard");
        assert_eq!(resolver.resolve(&[]).as_deref(), Some("Š"));
        let missing = FontDbResolver::new(directory.path());
        assert_eq!(missing.resolve(&[]), None);
        std::fs::write(&path, &record).expect("restore shard");
        assert_eq!(missing.resolve(&[]), None);
        record.push(0x92); // Truncated second record must reject the whole shard.
        std::fs::write(&path, &record).expect("broken shard");
        assert_eq!(FontDbResolver::new(directory.path()).resolve(&[]), None);
    }
}
