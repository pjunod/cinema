//! Cached recipe metadata only. The original prearmed SourceCell must open,
//! hash and retain the actual executable before this plan can be launched.
use crate::source_identity::SourceIdentity;
use std::{
    io,
    path::{Path, PathBuf},
};

pub struct SoftwareAvcExecutable {
    path: PathBuf,
    identity: SourceIdentity,
    sha256: [u8; 32],
}
impl SoftwareAvcExecutable {
    pub fn new(path: PathBuf, identity: SourceIdentity, sha256_hex: &str) -> io::Result<Self> {
        if !path.is_absolute()
            || path.as_os_str().len() > 4096
            || path.as_os_str().as_encoded_bytes().contains(&0)
            || identity.size == 0
            || identity.size > 512 * 1024 * 1024
            || identity.mtime < 0
            || identity.ctime < 0
            || !(0..1_000_000_000).contains(&identity.mtime_nsec)
            || !(0..1_000_000_000).contains(&identity.ctime_nsec)
            || sha256_hex.len() != 64
            || !sha256_hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid cached software executable plan",
            ));
        }
        let mut sha256 = [0; 32];
        hex::decode_to_slice(sha256_hex, &mut sha256).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "invalid executable digest")
        })?;
        Ok(Self {
            path,
            identity,
            sha256,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn source_identity(&self) -> SourceIdentity {
        self.identity
    }
    pub fn sha256(&self) -> [u8; 32] {
        self.sha256
    }
    pub(in crate::adaptive_resources::runtime) fn resident_memory_bytes(
        &self,
    ) -> io::Result<usize> {
        std::mem::size_of::<Self>()
            .checked_add(self.path.capacity())
            .ok_or_else(|| io::Error::other("executable plan memory overflow"))
    }
}
