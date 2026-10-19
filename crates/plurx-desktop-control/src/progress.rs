//! Sanitized public progress; private transaction authority remains inaccessible to desktop users.
use crate::{
    installation::InstallationV1,
    protocol::{ErrorCode, UpdateState},
    secure_fs::SecureDir,
    transaction::{JournalV1, Phase},
    Error, Result,
};
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicProgressV1 {
    pub schema_version: u32,
    pub installation_id: String,
    pub transaction_id: String,
    pub target_version: Option<String>,
    pub phase: Phase,
    pub failed: bool,
}
impl PublicProgressV1 {
    pub fn validate(&self, installation_id: &str) -> Result<()> {
        if self.schema_version != 1
            || self.installation_id != installation_id
            || uuid::Uuid::parse_str(&self.installation_id).is_err()
            || uuid::Uuid::parse_str(&self.transaction_id).is_err()
            || self
                .target_version
                .as_ref()
                .is_some_and(|v| v.len() > 128 || semver::Version::parse(v).is_err())
        {
            return Err(Error::new(
                ErrorCode::InvalidInstallation,
                "public progress identity/schema/version mismatch",
            ));
        }
        Ok(())
    }
    pub fn state(&self) -> UpdateState {
        if self.failed && !matches!(self.phase, Phase::Committed | Phase::Cancelled) {
            return UpdateState::RecoveryRequired;
        }
        match self.phase {
            Phase::Prepared => UpdateState::Prepared,
            Phase::ShutdownCommitted => UpdateState::ShutdownCommitted,
            Phase::CleanStop => UpdateState::WaitingForIdle,
            Phase::Installing | Phase::CandidateInstalled => UpdateState::Installing,
            Phase::Restoring => UpdateState::Restarting,
            Phase::Committed => UpdateState::Complete,
            Phase::Cancelled => UpdateState::Cancelled,
            Phase::RecoveryRequired => UpdateState::RecoveryRequired,
        }
    }
}
pub fn publish(root: &SecureDir, j: &JournalV1) -> Result<()> {
    j.validate_identity()?;
    let public = PublicProgressV1 {
        schema_version: 1,
        installation_id: j.installation_id.clone(),
        transaction_id: j.transaction_id.clone(),
        target_version: j.target_version.clone(),
        phase: j.phase,
        failed: j.detail.is_some(),
    };
    public.validate(&j.installation_id)?;
    root.write(
        "progress.json",
        &serde_json::to_vec(&public)?,
        if root.owner() == 0 { 0o644 } else { 0o600 },
    )
}
pub fn read(root: &SecureDir, installation_id: &str) -> Result<Option<PublicProgressV1>> {
    let public: PublicProgressV1 = match root.json("progress.json") {
        Ok(p) => p,
        Err(e) if e.os_error == Some(libc::ENOENT) => return Ok(None),
        Err(e) => return Err(e),
    };
    public.validate(installation_id)?;
    Ok(Some(public))
}
pub fn for_installation(i: &InstallationV1) -> Result<Option<PublicProgressV1>> {
    read(&i.state_dir()?, &i.installation_id)
}
