//! Captured implementation provenance for strict current-frame Profile5.
//! Backend observations authorize selection; this identity never grants it.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;

use super::{DecodeBackend, DecodeFacts, Encoder, MacosProcessingIdentity, PlanError, Rational};

/// Linux implementation and physical decoder environment. All digests cover
/// complete observations, including the loaded driver closure and kernel.
/// Only the declared render-device path enters output identity. Private loader
/// paths, host names, probe time and benchmark scores do not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinuxDolbyIdentity {
    ffmpeg_sha256: String,
    ffprobe_sha256: String,
    source_patch_digest: String,
    linked_libraries_digest: String,
    driver_environment_digest: String,
    kernel_device_digest: String,
    device_path: String,
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
        device_path: String,
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
        if !device_path
            .strip_prefix("/dev/dri/renderD")
            .is_some_and(|number| {
                !number.is_empty()
                    && number.bytes().all(|byte| byte.is_ascii_digit())
                    && number
                        .parse::<u32>()
                        .is_ok_and(|number| (128..1024).contains(&number))
            })
        {
            return Err(PlanError::InvalidCapabilityIdentity(
                "Linux strict Dolby device",
            ));
        }
        Ok(Self {
            ffmpeg_sha256,
            ffprobe_sha256,
            source_patch_digest,
            linked_libraries_digest,
            driver_environment_digest,
            kernel_device_digest,
            device_path,
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

    pub fn device_path(&self) -> &str {
        &self.device_path
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

/// A followed ELF object and its lookup objects require different metadata:
/// a temporary symlink retarget must not disappear behind a restored target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinuxExecutionObjectKind {
    /// The frozen launch object, whose loader origin must not be substituted.
    ProducerExecutable,
    NativeReporterExecutable,
    SealedParserExecutable,
    FollowedFile,
    LookupObject,
    AbsentLookup,
    /// Stable sysfs/proc attributes: inode timestamps do not identify contents.
    ContentDigest,
}

#[derive(Clone, PartialEq, Eq)]
pub struct LinuxExecutionObject {
    pub path: PathBuf,
    pub version: Option<String>,
    pub kind: LinuxExecutionObjectKind,
}

impl std::fmt::Debug for LinuxExecutionObject {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinuxExecutionObject")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// Node-private launch binding, separate from semantic output identity. It is
/// retained on the immutable plan across retries, never reconstructed from a
/// newer report. Nothing here is serialized into a portable client contract.
#[derive(Clone, PartialEq, Eq)]
pub struct LinuxDolbyExecutionBinding {
    objects: Arc<[LinuxExecutionObject]>,
    child_env: Arc<[(String, String)]>,
    source_clock: Option<LinuxSourceClockAssociation>,
}

/// Validated package values only. The daemon retains the role-bearing files
/// and validates the capsule before attaching this association; core performs
/// no filesystem reads and this value grants no decoder capability.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxSourceClockAssociation {
    capsule_digest: String,
    source_digest: String,
    native_reporter_digest: String,
    sealed_parser_digest: String,
}

impl LinuxSourceClockAssociation {
    pub fn new(
        capsule_digest: String,
        source_digest: String,
        native_reporter_digest: String,
        sealed_parser_digest: String,
    ) -> Result<Self, PlanError> {
        if [
            &capsule_digest,
            &source_digest,
            &native_reporter_digest,
            &sealed_parser_digest,
        ]
        .into_iter()
        .any(|digest| {
            digest.len() != 64
                || !digest
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        }) {
            return Err(PlanError::InvalidCapabilityIdentity(
                "Linux source clock association",
            ));
        }
        Ok(Self {
            capsule_digest,
            source_digest,
            native_reporter_digest,
            sealed_parser_digest,
        })
    }
    pub fn capsule_digest(&self) -> &str {
        &self.capsule_digest
    }
    pub fn source_digest(&self) -> &str {
        &self.source_digest
    }
    pub fn native_reporter_digest(&self) -> &str {
        &self.native_reporter_digest
    }
    pub fn sealed_parser_digest(&self) -> &str {
        &self.sealed_parser_digest
    }
}

impl std::fmt::Debug for LinuxDolbyExecutionBinding {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinuxDolbyExecutionBinding")
            .field("object_count", &self.objects.len())
            .finish_non_exhaustive()
    }
}

impl LinuxDolbyExecutionBinding {
    pub fn new(
        objects: Vec<LinuxExecutionObject>,
        child_env: Vec<(String, String)>,
    ) -> Result<Self, PlanError> {
        if objects.is_empty()
            || objects.len() > 4096
            || objects
                .iter()
                .filter(|object| object.kind == LinuxExecutionObjectKind::ProducerExecutable)
                .count()
                != 1
            || objects.iter().any(|object| {
                !object.path.is_absolute()
                    || match object.kind {
                        LinuxExecutionObjectKind::AbsentLookup => object.version.is_some(),
                        LinuxExecutionObjectKind::ContentDigest => {
                            object.version.as_ref().is_none_or(|digest| {
                                digest.len() != 64
                                    || !digest.bytes().all(|byte| {
                                        byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
                                    })
                            })
                        }
                        _ => object
                            .version
                            .as_ref()
                            .is_none_or(|version| version.is_empty() || version.len() > 512),
                    }
            })
            || child_env.len() > 3
            || child_env.iter().enumerate().any(|(index, (key, _))| {
                child_env[..index].iter().any(|(earlier, _)| earlier == key)
            })
            || child_env.iter().any(|(key, value)| {
                !matches!(
                    key.as_str(),
                    "LD_LIBRARY_PATH" | "LIBVA_DRIVER_NAME_JELLYFIN" | "LIBVA_MESSAGING_LEVEL"
                ) || value.len() > 4096
                    || value.chars().any(char::is_control)
            })
        {
            return Err(PlanError::InvalidCapabilityIdentity(
                "Linux strict execution binding",
            ));
        }
        Ok(Self {
            objects: objects.into(),
            child_env: child_env.into(),
            source_clock: None,
        })
    }

    pub fn objects(&self) -> &Arc<[LinuxExecutionObject]> {
        &self.objects
    }

    pub fn child_env(&self) -> &[(String, String)] {
        &self.child_env
    }

    pub fn with_source_clock(
        mut self,
        association: LinuxSourceClockAssociation,
    ) -> Result<Self, PlanError> {
        for kind in [
            LinuxExecutionObjectKind::NativeReporterExecutable,
            LinuxExecutionObjectKind::SealedParserExecutable,
        ] {
            if self
                .objects
                .iter()
                .filter(|object| object.kind == kind)
                .count()
                != 1
            {
                return Err(PlanError::InvalidCapabilityIdentity(
                    "Linux source clock executable roles",
                ));
            }
        }
        self.source_clock = Some(association);
        Ok(self)
    }

    pub fn source_clock(&self) -> Option<&LinuxSourceClockAssociation> {
        self.source_clock.as_ref()
    }
}

/// Envelope of one successfully observed complete strict CPU-renderer graph.
/// Decoder advertisement alone cannot construct a usable route observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxDolbyObservation {
    decoder: DecodeBackend,
    encoder: Encoder,
    source_width: u32,
    source_height: u32,
    output_width: u32,
    output_height: u32,
    frame_rate: Rational,
}

impl LinuxDolbyObservation {
    pub fn new(
        decoder: DecodeBackend,
        encoder: Encoder,
        source_width: u32,
        source_height: u32,
        output_width: u32,
        output_height: u32,
        frame_rate: Rational,
    ) -> Result<Self, PlanError> {
        if !matches!(decoder, DecodeBackend::Software | DecodeBackend::Vaapi)
            || !matches!(encoder, Encoder::Software | Encoder::Qsv | Encoder::Vaapi)
            || [source_width, source_height, output_width, output_height]
                .into_iter()
                .any(|dimension| dimension < 2 || !dimension.is_multiple_of(2))
        {
            return Err(PlanError::InvalidCapabilityIdentity(
                "Linux strict Dolby observed graph",
            ));
        }
        Ok(Self {
            decoder,
            encoder,
            source_width,
            source_height,
            output_width,
            output_height,
            frame_rate,
        })
    }

    fn accepts_source(&self, facts: &DecodeFacts) -> bool {
        facts
            .width()
            .is_some_and(|width| width <= self.source_width)
            && facts
                .height()
                .is_some_and(|height| height <= self.source_height)
            && facts.frame_rate().value().is_some_and(|rate| {
                u64::from(rate.numerator()) * u64::from(self.frame_rate.denominator())
                    <= u64::from(self.frame_rate.numerator()) * u64::from(rate.denominator())
            })
    }
}

/// Runtime-owned exact-package observations captured once with decode policy.
/// Empty observations are honest unavailability, not an optional-feature gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinuxDolbyContext {
    identity: LinuxDolbyIdentity,
    observations: Vec<LinuxDolbyObservation>,
    execution_binding: Option<LinuxDolbyExecutionBinding>,
}

impl LinuxDolbyContext {
    pub fn new(identity: LinuxDolbyIdentity, observations: Vec<LinuxDolbyObservation>) -> Self {
        Self {
            identity,
            observations,
            execution_binding: None,
        }
    }

    pub fn identity(&self) -> &LinuxDolbyIdentity {
        &self.identity
    }

    #[must_use]
    pub fn with_execution_binding(mut self, binding: LinuxDolbyExecutionBinding) -> Self {
        self.execution_binding = Some(binding);
        self
    }

    pub fn execution_binding(&self) -> Option<&LinuxDolbyExecutionBinding> {
        self.execution_binding.as_ref()
    }

    pub fn permits_source(
        &self,
        decoder: DecodeBackend,
        encoder: Encoder,
        facts: &DecodeFacts,
    ) -> bool {
        self.observations.iter().any(|observation| {
            observation.decoder == decoder
                && observation.encoder == encoder
                && observation.accepts_source(facts)
        })
    }

    pub fn permits_output(
        &self,
        decoder: DecodeBackend,
        encoder: Encoder,
        facts: &DecodeFacts,
        width: u32,
        height: u32,
    ) -> bool {
        self.observations.iter().any(|observation| {
            observation.decoder == decoder
                && observation.encoder == encoder
                && observation.accepts_source(facts)
                && width <= observation.output_width
                && height <= observation.output_height
        })
    }
}

/// Retained by the existing resolver and recovery owner. Construction belongs
/// to semantic authorization, never a client request or an advisory setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictDolbyPolicy {
    implementation: StrictDolbyImplementation,
    linux_execution_binding: Option<LinuxDolbyExecutionBinding>,
    // Qualified value snapshot survives a pending admin reprobe. The daemon
    // still fences its retained executable, loader and device at launch.
    linux_observed_context: Option<LinuxDolbyContext>,
}

impl StrictDolbyPolicy {
    pub(crate) fn new(identity: MacosProcessingIdentity) -> Self {
        Self {
            implementation: StrictDolbyImplementation::Macos(identity),
            linux_execution_binding: None,
            linux_observed_context: None,
        }
    }

    pub fn implementation(&self) -> &StrictDolbyImplementation {
        &self.implementation
    }

    pub(crate) fn new_linux(context: LinuxDolbyContext) -> Self {
        Self {
            implementation: StrictDolbyImplementation::LinuxVaapi(context.identity().clone()),
            linux_execution_binding: context.execution_binding().cloned(),
            linux_observed_context: Some(context),
        }
    }

    pub fn linux_observed_context(&self) -> Option<&LinuxDolbyContext> {
        self.linux_observed_context.as_ref()
    }

    pub fn linux_execution_binding(&self) -> Option<&LinuxDolbyExecutionBinding> {
        self.linux_execution_binding.as_ref()
    }

    pub fn linux_identity(&self) -> Option<&LinuxDolbyIdentity> {
        match &self.implementation {
            StrictDolbyImplementation::LinuxVaapi(identity) => Some(identity),
            StrictDolbyImplementation::Macos(_) => None,
        }
    }

    /// Complete encoder-device projection shared by observation and launch.
    /// QSV derives from the captured VAAPI device rather than selecting an
    /// independent default GPU after the decoder tuple was qualified.
    pub fn linux_encoder_init_args(&self, encoder: Encoder) -> Option<Vec<String>> {
        Self::linux_encoder_args_for_device(self.linux_identity()?.device_path(), encoder)
    }

    /// Pure recipe projection for a bounded observer before it has earned a
    /// policy. Device syntax alone never establishes availability.
    pub fn linux_encoder_args_for_device(device: &str, encoder: Encoder) -> Option<Vec<String>> {
        if device
            .strip_prefix("/dev/dri/renderD")
            .and_then(|index| index.parse::<u32>().ok())
            .is_none_or(|index| !(128..=1023).contains(&index))
        {
            return None;
        }
        Some(match encoder {
            Encoder::Software => Vec::new(),
            Encoder::Vaapi => vec!["-vaapi_device".into(), device.into()],
            Encoder::Qsv => vec![
                "-init_hw_device".into(),
                format!("vaapi=plurx_dovi_va:{device}"),
                "-init_hw_device".into(),
                "qsv=plurx_dovi_qsv@plurx_dovi_va".into(),
                "-filter_hw_device".into(),
                "plurx_dovi_qsv".into(),
            ],
            _ => return None,
        })
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
                "/dev/dri/renderD128".to_owned(),
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
            linux_execution_binding: None,
            linux_observed_context: None,
        };
        assert!(policy.macos_identity().is_none());
        assert_eq!(policy.ffmpeg_sha256(), "a".repeat(64));
    }
}
