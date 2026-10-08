//! Captured implementation provenance for strict current-frame Profile5.
//! Backend observations authorize selection; this identity never grants it.

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{MacosProcessingIdentity, PlanError};

/// Linux implementation and physical decoder environment. All digests cover
/// complete observations, including the loaded driver closure and kernel.
/// Paths, host names, probe time and benchmark scores are not output identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinuxDolbyIdentity {
    ffmpeg_sha256: String,
    ffprobe_sha256: String,
    source_patch_digest: String,
    linked_libraries_digest: String,
    driver_environment_digest: String,
    kernel_device_digest: String,
    graph_revision: u32,
}

impl LinuxDolbyIdentity {
    pub fn new(
        ffmpeg_sha256: String,
        ffprobe_sha256: String,
        source_patch_digest: String,
        linked_libraries_digest: String,
        driver_environment_digest: String,
        kernel_device_digest: String,
    ) -> Result<Self, PlanError> {
        for value in [
            &ffmpeg_sha256,
            &ffprobe_sha256,
            &source_patch_digest,
            &linked_libraries_digest,
            &driver_environment_digest,
            &kernel_device_digest,
        ] {
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(PlanError::InvalidCapabilityIdentity(
                    "Linux strict Dolby implementation digest",
                ));
            }
        }
        Ok(Self {
            ffmpeg_sha256,
            ffprobe_sha256,
            source_patch_digest,
            linked_libraries_digest,
            driver_environment_digest,
            kernel_device_digest,
            graph_revision: 1,
        })
    }

    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(
            serde_json::to_vec(self).expect("identity serialization is infallible"),
        ))
    }

    pub fn ffmpeg_sha256(&self) -> &str {
        &self.ffmpeg_sha256
    }

    pub fn ffprobe_sha256(&self) -> &str {
        &self.ffprobe_sha256
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StrictDolbyImplementation {
    Macos(MacosProcessingIdentity),
    LinuxVaapi(LinuxDolbyIdentity),
}

/// Retained by the existing resolver and recovery owner. Construction belongs
/// to semantic authorization, never a client request or an advisory setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictDolbyPolicy {
    implementation: StrictDolbyImplementation,
}

impl StrictDolbyPolicy {
    pub(crate) fn new(identity: MacosProcessingIdentity) -> Self {
        Self {
            implementation: StrictDolbyImplementation::Macos(identity),
        }
    }

    pub fn implementation(&self) -> &StrictDolbyImplementation {
        &self.implementation
    }

    pub fn macos_identity(&self) -> Option<&MacosProcessingIdentity> {
        match &self.implementation {
            StrictDolbyImplementation::Macos(identity) => Some(identity),
            StrictDolbyImplementation::LinuxVaapi(_) => None,
        }
    }

    pub fn ffmpeg_sha256(&self) -> &str {
        match &self.implementation {
            StrictDolbyImplementation::Macos(identity) => identity.ffmpeg_sha256(),
            StrictDolbyImplementation::LinuxVaapi(identity) => identity.ffmpeg_sha256(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_strict_identity_binds_driver_and_kernel_independently() {
        let identity = |driver: &str, kernel: &str| {
            LinuxDolbyIdentity::new(
                "a".repeat(64),
                "b".repeat(64),
                "c".repeat(64),
                "d".repeat(64),
                driver.to_owned(),
                kernel.to_owned(),
            )
        };
        let original = identity(&"e".repeat(64), &"f".repeat(64))
            .expect("complete Linux implementation identity");
        let driver_changed =
            identity(&"0".repeat(64), &"f".repeat(64)).expect("changed driver identity");
        let kernel_changed =
            identity(&"e".repeat(64), &"0".repeat(64)).expect("changed kernel/device identity");
        assert_ne!(original.digest(), driver_changed.digest());
        assert_ne!(original.digest(), kernel_changed.digest());
        assert!(identity("unknown", &"f".repeat(64)).is_err());
        assert!(identity(&"e".repeat(64), "unknown").is_err());
        let policy = StrictDolbyPolicy {
            implementation: StrictDolbyImplementation::LinuxVaapi(original),
        };
        assert!(policy.macos_identity().is_none());
        assert_eq!(policy.ffmpeg_sha256(), "a".repeat(64));
    }
}
