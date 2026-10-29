//! Durable transaction state machine. All mutations require a held native coordination lock.
use crate::{
    installation::InstallationV1,
    ipc::BoundDaemon,
    process,
    protocol::*,
    secure_fs::{Lock, SecureDir},
    supervisor,
    update::{self, AcceptedReleasesV1, Rollback, VerifiedManifest},
    Error, Result,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io::Read, time::Duration};

pub const JOURNAL_SCHEMA: u32 = 1;
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Prepared,
    ShutdownCommitted,
    CleanStop,
    Installing,
    CandidateInstalled,
    Restoring,
    Committed,
    Cancelled,
    RecoveryRequired,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalV1 {
    pub schema_version: u32,
    pub transaction_id: String,
    pub installation_id: String,
    pub operation: Operation,
    pub old_version: String,
    pub target_version: Option<String>,
    pub manifest_digest: Option<String>,
    pub phase: Phase,
    pub initial_running: bool,
    pub initial_startup: StartupStateV1,
    pub policy_revision: String,
    pub config_digest: String,
    pub process: Option<ProcessIdentityV1>,
    pub shutdown_generation: Option<String>,
    pub clean_receipt: Option<ShutdownReceiptV1>,
    pub rollback: Rollback,
    pub worker_pid: Option<u32>,
    pub worker_generation: Option<String>,
    pub capsule_sha256: Option<String>,
    pub package_inventory_before: Option<String>,
    pub package_inventory_after: Option<String>,
    pub detail: Option<String>,
    pub expected_node_id: Option<String>,
    pub start_permit_expires: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActiveV1 {
    schema_version: u32,
    transaction_id: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    AllowUnchanged,
    AllowCommitted,
    RestoreOldIfPolicyUnchanged,
    HoldUncertainShutdown,
    HoldPackageRepair,
    HoldCandidateHealth,
    HoldUnknown,
}
impl JournalV1 {
    pub fn recovery_action(&self) -> RecoveryAction {
        if self.schema_version != 1 || self.validate_identity().is_err() {
            return RecoveryAction::HoldUnknown;
        }
        match self.phase {
            Phase::Prepared | Phase::Cancelled => RecoveryAction::AllowUnchanged,
            Phase::Committed => RecoveryAction::AllowCommitted,
            Phase::ShutdownCommitted => RecoveryAction::HoldUncertainShutdown,
            Phase::CleanStop => RecoveryAction::RestoreOldIfPolicyUnchanged,
            Phase::Installing => RecoveryAction::HoldPackageRepair,
            Phase::CandidateInstalled | Phase::Restoring => RecoveryAction::HoldCandidateHealth,
            Phase::RecoveryRequired => RecoveryAction::HoldUnknown,
        }
    }
    /// Reconcile a shutdown committed before the controller recorded its receipt.
    /// Absence of the bound process alone never proves a clean shutdown.
    pub fn complete_clean_shutdown(
        &mut self,
        receipt: ShutdownReceiptV1,
        process_gone: bool,
        supervisor_stopped: bool,
    ) -> Result<()> {
        self.validate_identity()?;
        if self.phase != Phase::ShutdownCommitted
            || !clean_receipt_matches(self, &receipt)
            || !process_gone
            || !supervisor_stopped
        {
            return Err(Error::new(ErrorCode::UnsafeShutdown, "committed shutdown lacks matching clean receipt and stopped process/supervisor evidence"));
        }
        self.transition(Phase::CleanStop)?;
        self.clean_receipt = Some(receipt);
        Ok(())
    }
    pub fn validate_identity(&self) -> Result<()> {
        if self.schema_version != 1
            || uuid::Uuid::parse_str(&self.transaction_id).is_err()
            || uuid::Uuid::parse_str(&self.installation_id).is_err()
        {
            return Err(Error::new(
                ErrorCode::RecoveryRequired,
                "unknown or corrupt transaction identity/schema",
            ));
        }
        Ok(())
    }
    pub fn terminal(&self) -> bool {
        matches!(self.phase, Phase::Committed | Phase::Cancelled)
    }
    /// The guard separately verifies the live owner of this finite version-bound permit.
    pub fn permits_start(&self, version: &str, now: u64) -> bool {
        self.phase == Phase::Restoring
            && self
                .start_permit_expires
                .is_some_and(|expires| now < expires)
            && (self.target_version.as_deref() == Some(version) || self.old_version == version)
    }
    pub fn transition(&mut self, next: Phase) -> Result<()> {
        use Phase::*;
        let allowed = matches!(
            (self.phase, next),
            (Prepared, ShutdownCommitted | CleanStop | Cancelled)
                | (ShutdownCommitted, CleanStop)
                | (CleanStop, Installing | Restoring | Committed)
                | (Installing, CandidateInstalled)
                | (CandidateInstalled, Restoring)
                | (Restoring, Committed | Cancelled)
        ) || (!self.terminal() && next == RecoveryRequired);
        if !allowed {
            return Err(Error::new(
                ErrorCode::RecoveryRequired,
                "invalid durable transaction transition",
            ));
        }
        self.phase = next;
        Ok(())
    }
}
/// Shared read-only refusal for preflight and transaction creation.
pub fn ensure_resolved(active: Option<&JournalV1>) -> Result<()> {
    if let Some(j) = active.filter(|j| !j.terminal()) {
        return Err(Error::new(
            ErrorCode::Busy,
            format!(
                "installation has unresolved transaction {} at {:?}; run plurx-recovery --transaction {}",
                j.transaction_id, j.phase, j.transaction_id
            ),
        ));
    }
    Ok(())
}
pub struct TransactionStore {
    pub root: SecureDir,
    pub transactions: SecureDir,
    _lock: Lock,
}
impl TransactionStore {
    pub fn locked(i: &InstallationV1) -> Result<Self> {
        let root = i.state_dir()?;
        let lock = root.lock()?;
        let transactions = root.child("transactions", true)?;
        Ok(Self {
            root,
            transactions,
            _lock: lock,
        })
    }
    pub fn active(&self) -> Result<Option<JournalV1>> {
        read_active(&self.root, &self.transactions)
    }
    pub fn dir(&self, id: &str) -> Result<SecureDir> {
        uuid::Uuid::parse_str(id)
            .map_err(|_| Error::new(ErrorCode::InvalidRequest, "transaction must be UUID"))?;
        self.transactions.child(id, false)
    }
    pub fn load(&self, id: &str) -> Result<JournalV1> {
        let j: JournalV1 = self.dir(id)?.json("journal.json")?;
        j.validate_identity()?;
        if j.transaction_id != id {
            return Err(Error::new(
                ErrorCode::RecoveryRequired,
                "journal directory identity differs",
            ));
        }
        Ok(j)
    }
    pub fn save(&self, journal: &JournalV1) -> Result<()> {
        journal.validate_identity()?;
        self.dir(&journal.transaction_id)?
            .write_json("journal.json", journal)?;
        crate::progress::publish(&self.root, journal)
    }
    pub fn create(
        &self,
        i: &InstallationV1,
        operation: Operation,
        candidate: Option<&VerifiedManifest>,
    ) -> Result<JournalV1> {
        ensure_resolved(self.active()?.as_ref())?;
        let observed = supervisor::observe(i)?;
        if !observed.stopped() && observed.process.is_none() {
            return Err(Error::new(
                ErrorCode::PolicyConflict,
                "unknown process state cannot start a transaction",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let dir = self.transactions.child(&id, true)?;
        let j = JournalV1 {
            schema_version: 1,
            transaction_id: id.clone(),
            installation_id: i.installation_id.clone(),
            operation,
            old_version: i.current_version.clone(),
            target_version: candidate.map(|m| m.manifest().version.clone()),
            manifest_digest: candidate.map(|m| m.digest().to_string()),
            phase: Phase::Prepared,
            initial_running: observed.process.is_some(),
            initial_startup: observed.startup,
            policy_revision: observed.policy_revision,
            config_digest: config_digest(i)?,
            process: None,
            shutdown_generation: None,
            clean_receipt: None,
            rollback: candidate
                .map(|m| m.manifest().rollback)
                .unwrap_or(Rollback::BinaryCompatible),
            worker_pid: None,
            worker_generation: None,
            capsule_sha256: None,
            package_inventory_before: None,
            package_inventory_after: None,
            detail: None,
            expected_node_id: None,
            start_permit_expires: None,
        };
        dir.write_json("journal.json", &j)?;
        dir.write_json("installation-before.json", i)?;
        self.root.write_json(
            "active.json",
            &ActiveV1 {
                schema_version: 1,
                transaction_id: id,
            },
        )?;
        crate::progress::publish(&self.root, &j)?;
        Ok(j)
    }
    pub fn ledger(&self) -> Result<AcceptedReleasesV1> {
        match self.root.json("accepted-releases.json") {
            Ok(ledger) => Ok(ledger),
            Err(e) if is_missing(&e) => Ok(AcceptedReleasesV1::default()),
            Err(e) => Err(e),
        }
    }
    pub fn accept(&self, candidate: &VerifiedManifest) -> Result<()> {
        let mut ledger = self.ledger()?;
        candidate.accept(&mut ledger);
        self.root.write_json("accepted-releases.json", &ledger)
    }
}
// Missing is checked using the original OS error text only at optional leafs; malformed or unreadable files fail closed.
fn is_missing(error: &Error) -> bool {
    error.os_error == Some(libc::ENOENT)
}
fn optional_json<T: serde::de::DeserializeOwned>(dir: &SecureDir, name: &str) -> Result<Option<T>> {
    match dir.open(name) {
        Ok(file) => {
            let mut b = Vec::new();
            file.take(crate::secure_fs::MAX_JSON + 1)
                .read_to_end(&mut b)?;
            if b.len() as u64 > crate::secure_fs::MAX_JSON {
                return Err(Error::new(
                    ErrorCode::RecoveryRequired,
                    "journal exceeds bound",
                ));
            }
            Ok(Some(serde_json::from_slice(&b)?))
        }
        Err(e) if is_missing(&e) => Ok(None),
        Err(e) => Err(e),
    }
}
pub fn read_active(root: &SecureDir, transactions: &SecureDir) -> Result<Option<JournalV1>> {
    let Some(active): Option<ActiveV1> = optional_json(root, "active.json")? else {
        return Ok(None);
    };
    if active.schema_version != 1 || uuid::Uuid::parse_str(&active.transaction_id).is_err() {
        return Err(Error::new(
            ErrorCode::RecoveryRequired,
            "unknown active transaction schema",
        ));
    }
    let j: JournalV1 = transactions
        .child(&active.transaction_id, false)?
        .json("journal.json")?;
    j.validate_identity()?;
    if j.transaction_id != active.transaction_id {
        return Err(Error::new(
            ErrorCode::RecoveryRequired,
            "active journal mismatch",
        ));
    }
    Ok(Some(j))
}
pub fn config_digest(i: &InstallationV1) -> Result<String> {
    let parent = i.paths.config.parent().ok_or_else(|| {
        Error::new(
            ErrorCode::InvalidInstallation,
            "configuration parent missing",
        )
    })?;
    let name = i
        .paths
        .config
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "configuration name invalid"))?;
    let read = crate::secure_fs::open_dir(parent, i.owner_uid)
        .and_then(|directory| directory.read(name, 1024 * 1024));
    let mut digest = Sha256::new();
    digest.update(i.paths.config.as_os_str().as_encoded_bytes());
    match read {
        Ok(bytes) => {
            digest.update(b"present:");
            digest.update(bytes);
        }
        Err(e) if e.os_error == Some(libc::ENOENT) => {
            digest.update(b"absent:");
        }
        Err(e) => return Err(e),
    }
    Ok(hex::encode(digest.finalize()))
}
pub fn revalidate_policy(i: &InstallationV1, j: &JournalV1) -> Result<supervisor::Observation> {
    let o = supervisor::observe(i)?;
    if i.installation_id != j.installation_id
        || o.startup != j.initial_startup
        || o.policy_revision != j.policy_revision
        || config_digest(i)? != j.config_digest
    {
        return Err(Error::new(ErrorCode::PolicyConflict, "external startup/configuration policy changed; preserving current values, restoration held"));
    }
    Ok(o)
}
pub fn clean_receipt_matches(j: &JournalV1, r: &ShutdownReceiptV1) -> bool {
    r.schema_version == 1
        && r.operation_id == j.transaction_id
        && j.process.as_ref() == Some(&r.process)
        && j.shutdown_generation.as_ref() == Some(&r.generation)
        && r.http_drained
        && r.store_closed
        && r.exit_class == ExitClass::Clean
}
pub fn stop_clean(i: &InstallationV1, store: &TransactionStore, j: &mut JournalV1) -> Result<()> {
    let observed = revalidate_policy(i, j)?;
    if !j.initial_running {
        if !observed.stopped() {
            return Err(Error::new(
                ErrorCode::PolicyConflict,
                "previously stopped service changed externally",
            ));
        }
        j.transition(Phase::CleanStop)?;
        return store.save(j);
    }
    let mut daemon = BoundDaemon::connect(i)?;
    let prepared = daemon.prepare(&j.transaction_id, j.operation, j.target_version.clone())?;
    j.expected_node_id = Some(daemon.identity.node_id.clone());
    j.process = Some(daemon.identity.process.clone());
    j.shutdown_generation = Some(prepared.generation.clone());
    // Write-ahead uncertain commitment: connection loss/controller death cannot make a committed fence look harmless.
    j.transition(Phase::ShutdownCommitted)?;
    store.save(j)?;
    daemon.commit(prepared)?;
    process::wait_gone(daemon.peer(), Duration::from_secs(60))?;
    let receipt: ShutdownReceiptV1 = i
        .receipt_dir()?
        .json(&format!("{}.json", j.transaction_id))?;
    if !clean_receipt_matches(j, &receipt) {
        return Err(Error::new(ErrorCode::UnsafeShutdown, "missing/mismatched/unclean drain and store-close receipt; executable replacement refused"));
    }
    if !supervisor::observe(i)?.stopped() {
        return Err(Error::new(
            ErrorCode::UnsafeShutdown,
            "supervisor unexpectedly restarted or changed the daemon",
        ));
    }
    j.clean_receipt = Some(receipt);
    j.transition(Phase::CleanStop)?;
    store.save(j)
}
/// A guard never invokes package managers, fetches a release, or repairs an unknown journal.
pub fn guard(i: &InstallationV1) -> Result<()> {
    let root = i.state_dir()?;
    let transactions = root.child("transactions", false)?;
    let Some(j) = read_active(&root, &transactions)? else {
        return Ok(());
    };
    if j.installation_id != i.installation_id {
        return Err(Error::new(
            ErrorCode::RecoveryRequired,
            "journal is for another installation",
        ));
    }
    match j.recovery_action() {
        RecoveryAction::HoldCandidateHealth
            if crate::unix_time().is_ok_and(|now| j.permits_start(&i.current_version, now)) =>
        {
            let worker = j
                .worker_pid
                .zip(j.worker_generation.as_ref())
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::RecoveryRequired,
                        "missing restoration worker identity",
                    )
                })?;
            let live = process::inspect(worker.0)?;
            if live.uid != i.owner_uid || live.generation != *worker.1 {
                return Err(Error::new(
                    ErrorCode::RecoveryRequired,
                    "restoration worker is no longer the verified process",
                ));
            }
            Ok(())
        }
        RecoveryAction::AllowUnchanged | RecoveryAction::AllowCommitted => Ok(()),
        RecoveryAction::RestoreOldIfPolicyUnchanged => {
            let _lock = root.lock()?;
            if j.operation == Operation::Upgrade && i.current_version != j.old_version {
                return Err(Error::new(
                    ErrorCode::RecoveryRequired,
                    "old installation identity changed before recovery",
                ));
            }
            revalidate_policy(i, &j)?;
            if j.initial_running && j.operation != Operation::Stop {
                Ok(())
            } else {
                Err(Error::new(
                    ErrorCode::RecoveryRequired,
                    "transaction preserves deliberately stopped state; run explicit recovery/start",
                ))
            }
        }
        _ => Err(Error::new(
            ErrorCode::RecoveryRequired,
            format!(
                "transaction {} at {:?} holds daemon startup; run plurx-recovery --transaction {}",
                j.transaction_id, j.phase, j.transaction_id
            ),
        )),
    }
}
/// Retain the active worker independently of any package/control paths being replaced.
pub fn seal_worker(
    i: &InstallationV1,
    store: &TransactionStore,
    j: &mut JournalV1,
    source: &std::path::Path,
) -> Result<std::path::PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let parent = source
        .parent()
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "worker parent"))?;
    let name = source
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "worker filename"))?;
    let source_dir = crate::secure_fs::open_dir(parent, i.owner_uid)?;
    let file = source_dir.open(name)?;
    if file.metadata()?.uid() != i.owner_uid || file.metadata()?.mode() & 0o111 == 0 {
        return Err(Error::new(
            ErrorCode::InvalidInstallation,
            "worker must be a trusted executable owned by native authority",
        ));
    }
    let bytes = source_dir.read(name, 256 * 1024 * 1024)?;
    let capsule = store.dir(&j.transaction_id)?.child("capsule", true)?;
    capsule.write("plurx-update-worker", &bytes, 0o500)?;
    j.capsule_sha256 = Some(hex::encode(Sha256::digest(bytes)));
    store.save(j)?;
    Ok(i.paths
        .state
        .join("transactions")
        .join(&j.transaction_id)
        .join("capsule/plurx-update-worker"))
}
pub fn verify_capsule(store: &TransactionStore, j: &JournalV1) -> Result<()> {
    let bytes = store
        .dir(&j.transaction_id)?
        .child("capsule", false)?
        .read("plurx-update-worker", 256 * 1024 * 1024)?;
    if j.capsule_sha256.as_deref() != Some(hex::encode(Sha256::digest(bytes)).as_str()) {
        return Err(Error::new(
            ErrorCode::RecoveryRequired,
            "immutable worker capsule verification failed",
        ));
    }
    Ok(())
}
pub fn reverify_candidate(
    i: &InstallationV1,
    store: &TransactionStore,
    j: &JournalV1,
) -> Result<VerifiedManifest> {
    let dir = store.dir(&j.transaction_id)?;
    let verified = update::verify_manifest(
        &dir.read(update::MANIFEST_NAME, update::MAX_MANIFEST as u64)?,
        &dir.read(update::SIGNATURE_NAME, 64)?,
        &update::production_keys()?,
        i,
        &store.ledger()?,
        crate::unix_time()?,
    )?;
    if j.manifest_digest.as_deref() != Some(verified.digest())
        || j.target_version.as_deref() != Some(&verified.manifest().version)
    {
        return Err(Error::new(
            ErrorCode::UntrustedUpdate,
            "journal candidate differs from independently verified signed metadata",
        ));
    }
    for asset in update::select_cohort(&verified, i)? {
        update::verify_staged(asset, &dir)?;
    }
    Ok(verified)
}
