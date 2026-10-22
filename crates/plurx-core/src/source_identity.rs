//! Shared source object identity and scanner metadata checks.

use std::fs::Metadata;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Stat-backed identity used by the existing `s1` source attestation namespace.
/// This is metadata, not evidence that a particular descriptor was retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceIdentity {
    pub dev: u64,
    pub ino: u64,
    pub size: u64,
    pub mtime: i64,
    pub mtime_nsec: i64,
    pub ctime: i64,
    pub ctime_nsec: i64,
}

impl SourceIdentity {
    pub const fn from_fields(
        dev: u64,
        ino: u64,
        size: u64,
        mtime: i64,
        mtime_nsec: i64,
        ctime: i64,
        ctime_nsec: i64,
    ) -> Self {
        Self {
            dev,
            ino,
            size,
            mtime,
            mtime_nsec,
            ctime,
            ctime_nsec,
        }
    }

    #[cfg(unix)]
    pub fn from_metadata(metadata: &Metadata) -> Self {
        Self::from_fields(
            metadata.dev(),
            metadata.ino(),
            metadata.size(),
            metadata.mtime(),
            metadata.mtime_nsec(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    }

    /// Preserve the existing source-attestation field order and `s1` prefix.
    pub fn render_s1(self) -> String {
        format!(
            "s1:{}:{}:{}:{}:{}:{}:{}",
            self.dev, self.ino, self.size, self.mtime, self.mtime_nsec, self.ctime, self.ctime_nsec
        )
    }

    /// Match the scanner's size and whole-second modified-time semantics.
    pub fn scanner_matches(metadata: &Metadata, expected_size: i64, expected_mtime: i64) -> bool {
        let modified = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
            .unwrap_or(0);
        metadata.len() == expected_size.max(0) as u64 && modified == expected_mtime
    }
}
