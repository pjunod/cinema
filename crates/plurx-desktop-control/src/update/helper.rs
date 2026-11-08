//! Authenticated, separately selected migration artifacts. These DTOs are
//! untrusted until the enclosing exact manifest bytes pass update verification.
use super::{
    asset_name, hash, untrusted, ArtifactV1, Format, Platform, Rollback, UpdateManifestV1,
    VerifiedManifest, MAX_ARTIFACT,
};
use crate::{
    installation::{Architecture, InstallMethod, InstallationV1},
    Result,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateManifestV2 {
    pub schema_version: u32,
    pub repository: String,
    pub channel: String,
    pub tag: String,
    pub version: String,
    pub sequence: u64,
    pub source_commit: String,
    pub source_tree: String,
    pub packaging_commit: String,
    pub created_unix: u64,
    pub expires_unix: u64,
    pub upgrade_from: Vec<String>,
    pub data_compatibility: String,
    pub cluster_compatibility: String,
    pub minimum_controller: String,
    pub rollback: Rollback,
    pub release_notes_url: String,
    pub artifacts: Vec<ArtifactV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration_helpers: Option<Vec<MigrationHelperV1>>,
}

impl From<UpdateManifestV2> for UpdateManifestV1 {
    fn from(m: UpdateManifestV2) -> Self {
        Self {
            schema_version: m.schema_version,
            repository: m.repository,
            channel: m.channel,
            tag: m.tag,
            version: m.version,
            sequence: m.sequence,
            source_commit: m.source_commit,
            source_tree: m.source_tree,
            packaging_commit: m.packaging_commit,
            created_unix: m.created_unix,
            expires_unix: m.expires_unix,
            upgrade_from: m.upgrade_from,
            data_compatibility: m.data_compatibility,
            cluster_compatibility: m.cluster_compatibility,
            minimum_controller: m.minimum_controller,
            rollback: m.rollback,
            release_notes_url: m.release_notes_url,
            artifacts: m.artifacts,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationBuildV1 {
    pub commit: String,
    pub tree: String,
    pub target: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationSourceCohortV1 {
    pub auth_version: i64,
    pub canonical_layout_fingerprint: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationOutputV1 {
    pub native_schema_version: i64,
    pub manifest_format: u32,
    pub transition_format: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationHelperProfile {
    StorageMigration,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationHelperArtifactV1 {
    pub name: String,
    pub platform: Platform,
    pub architecture: Architecture,
    pub format: Format,
    pub size: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationHelperV1 {
    pub artifact: MigrationHelperArtifactV1,
    pub daemon_build: MigrationBuildV1,
    pub helper_build: MigrationBuildV1,
    pub helper_version: String,
    pub profile: MigrationHelperProfile,
    pub executable_name: String,
    pub executable_sha256: String,
    pub bundle_sha256: String,
    pub helper_protocol: u32,
    pub request_format: u32,
    pub receipt_format: u32,
    pub source_cohorts: Vec<MigrationSourceCohortV1>,
    pub output: MigrationOutputV1,
    pub converter_sha256: String,
}

/// Deliberate selection only; ordinary update staging never requests a helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationHelperPurpose {
    LegacyDataUpgrade,
    ExplicitTransition,
}

/// Authentication of release metadata, not permission to convert a data root.
/// The launcher must still verify downloaded bytes and the actual helper's
/// description, and the helper must acquire/revalidate the real root itself.
#[derive(Debug, Clone)]
pub struct VerifiedMigrationHelper<'a> {
    release: &'a VerifiedManifest,
    helper: &'a MigrationHelperV1,
    cohort: &'a MigrationSourceCohortV1,
    purpose: MigrationHelperPurpose,
}
impl<'a> VerifiedMigrationHelper<'a> {
    pub fn artifact(&self) -> &'a MigrationHelperArtifactV1 {
        &self.helper.artifact
    }
    pub fn binding(&self) -> &'a MigrationHelperV1 {
        self.helper
    }
    pub fn source_cohort(&self) -> &'a MigrationSourceCohortV1 {
        self.cohort
    }
    pub fn purpose(&self) -> MigrationHelperPurpose {
        self.purpose
    }
    pub fn manifest_digest(&self) -> &'a str {
        self.release.digest()
    }
    pub fn key_id(&self) -> &'a str {
        self.release.key_id()
    }
    pub fn release_tag(&self) -> &'a str {
        &self.release.manifest().tag
    }
    pub fn daemon_build(&self) -> &'a MigrationBuildV1 {
        &self.helper.daemon_build
    }
    pub fn helper_build(&self) -> &'a MigrationBuildV1 {
        &self.helper.helper_build
    }
    pub fn executable_sha256(&self) -> &'a str {
        &self.helper.executable_sha256
    }
    pub fn bundle_sha256(&self) -> &'a str {
        &self.helper.bundle_sha256
    }
    pub fn converter_sha256(&self) -> &'a str {
        &self.helper.converter_sha256
    }
    pub fn output(&self) -> &'a MigrationOutputV1 {
        &self.helper.output
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SignedManifest {
    V1(UpdateManifestV1),
    V2(UpdateManifestV2),
}

pub(super) fn parse_signed_manifest(
    bytes: &[u8],
) -> Result<(UpdateManifestV1, Vec<MigrationHelperV1>)> {
    match serde_json::from_slice::<SignedManifest>(bytes)
        .map_err(|_| untrusted("malformed/unsupported signed manifest"))?
    {
        SignedManifest::V1(m) if matches!(m.schema_version, 1 | 2) => Ok((m, Vec::new())),
        SignedManifest::V2(mut m) if m.schema_version == 2 => {
            let helpers = match m.migration_helpers.take() {
                Some(helpers) if helpers.is_empty() => {
                    return Err(untrusted("empty advertised migration helper cohort"))
                }
                helpers => helpers.unwrap_or_default(),
            };
            Ok((m.into(), helpers))
        }
        _ => Err(untrusted("unsupported signed manifest schema")),
    }
}

fn build_target(build: &MigrationBuildV1) -> Result<(Platform, Architecture)> {
    if !hash(&build.commit, 40) || !hash(&build.tree, 40) {
        return Err(untrusted("invalid migration build provenance"));
    }
    match build.target.as_str() {
        "x86_64-unknown-linux-gnu" => Ok((Platform::Linux, Architecture::Amd64)),
        "aarch64-unknown-linux-gnu" => Ok((Platform::Linux, Architecture::Arm64)),
        "x86_64-apple-darwin" => Ok((Platform::Macos, Architecture::Amd64)),
        "aarch64-apple-darwin" => Ok((Platform::Macos, Architecture::Arm64)),
        _ => Err(untrusted("unsupported migration helper target")),
    }
}

pub(super) fn validate_helpers(m: &UpdateManifestV1, helpers: &[MigrationHelperV1]) -> Result<()> {
    if helpers.len() > 4 || (m.schema_version == 1 && !helpers.is_empty()) {
        return Err(untrusted("unsupported migration helper cohort"));
    }
    let mut names: BTreeSet<&str> = m.artifacts.iter().map(|a| a.name.as_str()).collect();
    let mut targets = BTreeSet::new();
    for h in helpers {
        let (platform, architecture) = build_target(&h.helper_build)?;
        let a = &h.artifact;
        if h.daemon_build != h.helper_build
            || h.helper_build.commit != m.source_commit
            || h.helper_build.tree != m.source_tree
            || h.helper_version != m.version
            || h.profile != MigrationHelperProfile::StorageMigration
            || !targets.insert(h.helper_build.target.as_str())
            || a.platform != platform
            || a.architecture != architecture
            || a.format != Format::TarGz
            || !a.name.ends_with(".tar.gz")
            || !asset_name(&a.name)
            || !names.insert(a.name.as_str())
            || a.size == 0
            || a.size > MAX_ARTIFACT
            || !hash(&a.sha256, 64)
            || !hash(&h.executable_sha256, 64)
            || !hash(&h.bundle_sha256, 64)
            || h.bundle_sha256 != a.sha256
            || !hash(&h.converter_sha256, 64)
            || h.executable_name != "plurx-storage-migrate"
            || h.helper_protocol != 1
            || h.request_format != 1
            || h.receipt_format != 1
            || h.output.native_schema_version <= 0
            || h.output.manifest_format != 1
            || h.output.transition_format != 1
            || h.source_cohorts.len() != 2
            || h.source_cohorts.iter().map(|c| c.auth_version).ne([81, 82])
            || h.source_cohorts
                .iter()
                .any(|c| !hash(&c.canonical_layout_fingerprint, 64))
        {
            return Err(untrusted(
                "migration helper artifact/build/protocol/profile/cohort mismatch",
            ));
        }
    }
    Ok(())
}

pub(super) fn validate_installation_target(
    installed: &InstallationV1,
    build: &MigrationBuildV1,
) -> Result<()> {
    let (platform, architecture) = build_target(build)?;
    let installation_platform = match installed.method {
        InstallMethod::LinuxDeb | InstallMethod::LinuxRpm => Platform::Linux,
        InstallMethod::MacosNative => Platform::Macos,
        _ => {
            return Err(untrusted(
                "migration helper requires a managed installation",
            ))
        }
    };
    if platform != installation_platform
        || (installed.architecture != architecture
            && !(platform == Platform::Macos && installed.architecture == Architecture::Universal))
    {
        return Err(untrusted(
            "migration helper target does not match the installation",
        ));
    }
    Ok(())
}

pub fn select_migration_helper<'a>(
    verified: &'a VerifiedManifest,
    installed: &InstallationV1,
    purpose: MigrationHelperPurpose,
    expected_daemon: &MigrationBuildV1,
    source_cohort: &MigrationSourceCohortV1,
) -> Result<VerifiedMigrationHelper<'a>> {
    validate_installation_target(installed, expected_daemon)?;
    if expected_daemon.commit != verified.manifest.source_commit
        || expected_daemon.tree != verified.manifest.source_tree
        || installed.repository != verified.manifest.repository
        || !installed
            .trust_key_ids
            .iter()
            .any(|id| id == verified.key_id())
        || !matches!(source_cohort.auth_version, 81 | 82)
        || !hash(&source_cohort.canonical_layout_fingerprint, 64)
    {
        return Err(untrusted(
            "migration helper selection does not match daemon/installation/source",
        ));
    }
    let mut matches = verified
        .migration_helpers
        .iter()
        .filter(|h| h.daemon_build == *expected_daemon);
    let helper = matches
        .next()
        .ok_or_else(|| untrusted("required authenticated migration helper is unavailable"))?;
    if matches.next().is_some() {
        return Err(untrusted("ambiguous migration helper selection"));
    }
    let cohort = helper
        .source_cohorts
        .iter()
        .find(|c| *c == source_cohort)
        .ok_or_else(|| untrusted("migration helper does not accept the exact source cohort"))?;
    Ok(VerifiedMigrationHelper {
        release: verified,
        helper,
        cohort,
        purpose,
    })
}

#[cfg(test)]
pub(super) mod tests;
