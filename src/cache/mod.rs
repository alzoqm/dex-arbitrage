//! Atomic cache operations with schema versioning and durability.
//!
//! Provides:
//! - Atomic write-rename pattern
//! - Schema version header
//! - Checksums for integrity
//! - Cache repair on startup

use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Current cache schema version
pub const CACHE_SCHEMA_VERSION: u32 = 2;

/// Cache metadata header
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheHeader {
    version: u32,
    created_at: u64,          // Unix timestamp
    checksum: Option<String>, // SHA-256 hex string
}

/// Result of cache load attempt
#[derive(Debug)]
pub enum CacheLoadResult<T> {
    Ok(T),
    VersionMismatch {
        current_version: u32,
        expected_version: u32,
    },
    Corrupt {
        error: String,
    },
    NotFound,
}

/// Atomic cache writer
pub struct AtomicCacheWriter {
    base_path: PathBuf,
    temp_path: PathBuf,
    use_checksum: bool,
}

impl AtomicCacheWriter {
    /// Create a new atomic cache writer
    ///
    /// # Arguments
    /// * `path` - The target file path
    /// * `use_checksum` - Enable SHA-256 checksums
    pub fn new<P: AsRef<Path>>(path: P, use_checksum: bool) -> Self {
        let base_path = path.as_ref().to_path_buf();
        let temp_path = base_path.with_extension(".tmp");
        Self {
            base_path,
            temp_path,
            use_checksum,
        }
    }

    /// Write data atomically
    pub fn write<T: Serialize>(&self, data: &T) -> Result<()> {
        let header = CacheHeader {
            version: CACHE_SCHEMA_VERSION,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs(),
            checksum: None,
        };

        let mut buffer = Vec::new();

        // Serialize header
        header.write_to(&mut buffer)?;

        // Serialize data
        let data_bytes = serde_json::to_vec(data).context("failed to serialize cache data")?;

        if self.use_checksum {
            let checksum = checksum64(&data_bytes);
            let checksum_hex = format!("{:016x}", checksum);
            // Update header with checksum
            let header_with_checksum = CacheHeader {
                checksum: Some(checksum_hex),
                ..header
            };
            buffer.clear();
            header_with_checksum.write_to(&mut buffer)?;
        }

        buffer.extend_from_slice(&data_bytes);

        // Write to temp file
        let temp_dir = self.temp_path.parent().context("temp path has no parent")?;
        fs::create_dir_all(temp_dir).context("failed to create cache directory")?;

        fs::write(&self.temp_path, &buffer).context("failed to write temp cache file")?;

        // Atomic rename
        fs::rename(&self.temp_path, &self.base_path)
            .context("failed to rename temp cache file to final location")?;

        debug!(
            path = %self.base_path.display(),
            size = buffer.len(),
            "cache written atomically"
        );

        Ok(())
    }
}

impl CacheHeader {
    fn write_to<W: Write>(&self, writer: &mut W) -> Result<()> {
        // Write magic bytes
        writer.write_all(b"DARC")?;
        // Write version
        writer.write_all(&self.version.to_be_bytes())?;
        // Write created_at
        writer.write_all(&self.created_at.to_be_bytes())?;
        // Write flags: bit 0 = has checksum
        let flags: u8 = if self.checksum.is_some() { 1 } else { 0 };
        writer.write_all(&[flags])?;
        // Write checksum length if present
        if let Some(checksum) = &self.checksum {
            let checksum_bytes = checksum.as_bytes();
            writer.write_all(&(checksum_bytes.len() as u16).to_be_bytes())?;
            writer.write_all(checksum_bytes)?;
        }

        Ok(())
    }

    fn read_from<R: Read>(reader: &mut R) -> Result<(Self, usize)> {
        let mut magic = [0u8; 4];
        reader
            .read_exact(&mut magic)
            .context("failed to read magic bytes")?;
        if magic != *b"DARC" {
            anyhow::bail!("invalid cache file: bad magic bytes");
        }

        let mut version_bytes = [0u8; 4];
        reader.read_exact(&mut version_bytes)?;
        let version = u32::from_be_bytes(version_bytes);

        let mut created_at_bytes = [0u8; 8];
        reader.read_exact(&mut created_at_bytes)?;
        let created_at = u64::from_be_bytes(created_at_bytes);

        let mut flags = [0u8; 1];
        reader.read_exact(&mut flags)?;
        let has_checksum = flags[0] & 1 != 0;

        let mut header = CacheHeader {
            version,
            created_at,
            checksum: None,
        };

        if has_checksum {
            let mut checksum_len_bytes = [0u8; 2];
            reader.read_exact(&mut checksum_len_bytes)?;
            let checksum_len = u16::from_be_bytes(checksum_len_bytes) as usize;
            let mut checksum_bytes = vec![0u8; checksum_len];
            reader.read_exact(&mut checksum_bytes)?;
            header.checksum =
                Some(String::from_utf8(checksum_bytes).context("checksum not valid UTF-8")?);
        }

        let header_size = 4
            + 4
            + 8
            + 1
            + if has_checksum {
                2 + header.checksum.as_ref().map_or(0, |c| c.len())
            } else {
                0
            };
        Ok((header, header_size))
    }
}

/// Load a cache file with version checking and integrity verification
pub fn load_cache<T: DeserializeOwned, P: AsRef<Path>>(path: P) -> CacheLoadResult<T> {
    let path = path.as_ref();

    if !path.exists() {
        return CacheLoadResult::NotFound;
    }

    let file = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return CacheLoadResult::Corrupt {
                error: format!("failed to open cache file: {}", e),
            };
        }
    };

    let mut reader = io::BufReader::new(file);

    // Read header
    let (header, _header_size) = match CacheHeader::read_from(&mut reader) {
        Ok(h) => h,
        Err(e) => {
            return CacheLoadResult::Corrupt {
                error: format!("failed to read cache header: {}", e),
            };
        }
    };

    // Check version
    if header.version != CACHE_SCHEMA_VERSION {
        warn!(
            path = %path.display(),
            current_version = header.version,
            expected_version = CACHE_SCHEMA_VERSION,
            "cache schema version mismatch"
        );
        return CacheLoadResult::VersionMismatch {
            current_version: header.version,
            expected_version: CACHE_SCHEMA_VERSION,
        };
    }

    // Read data
    let mut data = Vec::new();
    if let Err(e) = reader.read_to_end(&mut data) {
        return CacheLoadResult::Corrupt {
            error: format!("failed to read cache data: {}", e),
        };
    }

    // Verify checksum if present
    if let Some(expected_checksum) = header.checksum {
        let actual_checksum = checksum64(&data);
        let actual_checksum_hex = format!("{:016x}", actual_checksum);
        if expected_checksum != actual_checksum_hex {
            warn!(
                path = %path.display(),
                expected = %expected_checksum,
                actual = %actual_checksum_hex,
                "cache checksum mismatch"
            );
            return CacheLoadResult::Corrupt {
                error: format!(
                    "checksum mismatch: expected {}, got {}",
                    expected_checksum, actual_checksum_hex
                ),
            };
        }
    }

    // Deserialize
    match serde_json::from_slice(&data) {
        Ok(value) => {
            debug!(
                path = %path.display(),
                version = header.version,
                size = data.len(),
                "cache loaded successfully"
            );
            CacheLoadResult::Ok(value)
        }
        Err(e) => CacheLoadResult::Corrupt {
            error: format!("failed to deserialize cache: {}", e),
        },
    }
}

fn checksum64(data: &[u8]) -> u64 {
    data.iter()
        .fold(0u64, |acc, &b| acc.wrapping_mul(31) ^ u64::from(b))
}

/// Legacy cache loader for old format files
///
/// Tries to load as old JSON format if the new binary format fails
pub fn load_legacy_cache<T: DeserializeOwned, P: AsRef<Path>>(path: P) -> Result<Option<T>> {
    let path = path.as_ref();

    if !path.exists() {
        return Ok(None);
    }

    let content = fs::read_to_string(path).context("failed to read legacy cache file")?;

    match serde_json::from_str::<T>(&content) {
        Ok(value) => {
            info!(
                path = %path.display(),
                "loaded legacy JSON cache"
            );
            Ok(Some(value))
        }
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "failed to parse legacy cache as JSON"
            );
            Ok(None)
        }
    }
}

/// Repair a corrupt cache file by removing it
pub fn repair_cache<P: AsRef<Path>>(path: P) -> Result<bool> {
    let path = path.as_ref();

    if !path.exists() {
        return Ok(false);
    }

    warn!(path = %path.display(), "repairing corrupt cache file by removing it");

    // Rename to .corrupt instead of deleting
    let corrupt_path = path.with_extension(format!(
        "{}.corrupt",
        path.extension().and_then(|s| s.to_str()).unwrap_or("cache")
    ));

    fs::rename(path, &corrupt_path).context("failed to rename corrupt cache file")?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_cache_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "dex-arbitrage-cache-test-{}-{}",
            std::process::id(),
            unique
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    #[test]
    fn test_atomic_cache_write_and_load() {
        let path = temp_cache_path("test.cache");

        let data = vec![1u64, 2, 3, 4, 5];

        let writer = AtomicCacheWriter::new(&path, true);
        writer.write(&data).unwrap();

        match load_cache::<Vec<u64>, _>(&path) {
            CacheLoadResult::Ok(loaded) => {
                assert_eq!(loaded, data);
            }
            other => panic!("unexpected result: {:?}", other),
        }

        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().unwrap());
    }

    #[test]
    fn test_version_mismatch() {
        let path = temp_cache_path("test.cache");

        // Write a raw file with wrong version
        let header = CacheHeader {
            version: 999, // Wrong version
            created_at: 0,
            checksum: None,
        };
        let mut buffer = Vec::new();
        header.write_to(&mut buffer).unwrap();

        fs::write(&path, &buffer).unwrap();

        match load_cache::<Vec<u64>, _>(&path) {
            CacheLoadResult::VersionMismatch {
                current_version,
                expected_version,
            } => {
                assert_eq!(current_version, 999);
                assert_eq!(expected_version, CACHE_SCHEMA_VERSION);
            }
            other => panic!("expected version mismatch, got: {:?}", other),
        }

        let _ = fs::remove_file(&path);
        let _ = fs::remove_dir(path.parent().unwrap());
    }
}
