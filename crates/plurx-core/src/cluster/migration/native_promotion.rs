//! Explicit, offline native-to-cluster authority transfer.
//!
//! The immutable native archive remains authoritative while a real isolated
//! voter is populated. A qualified target is never imported again, including
//! after a crash before its final mode manifest is published.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::cluster::membership::{ClusterPeer, ClusterRole, LocalMembership};
use crate::config::Config;
use crate::error::StoreError;
use crate::secrets::CredentialKey;
use crate::storage::backup::{
    build_native_backup_artifact, restore_content_fingerprint, verify_native_backup_artifact,
    NativeBackupManifest,
};
use crate::storage::mode::{
    self, owned_path, publish_record, read_record, sync_directory, StorageManifest, StorageMode,
    MANIFEST_FILENAME, TRANSITION_FILENAME,
};
use crate::storage::telemetry::{transfer_local_telemetry, LocalTelemetryTransferReceipt};
use crate::storage::{open_native_store_locked, NativeDirectoryLock};
use crate::store::{HiqliteAuthStore, AUTH_SCHEMA_BASELINE_VERSION, AUTH_SCHEMA_VERSION};

use super::native_import::{import_verified_native, NativeImportReceipt};
use super::{ActivationMarker, SelectedStore, StartupClockObserver};

const OPERATION: &str = "create_cluster";
const RECEIPT_FILENAME: &str = "native-promotion.json";
const MAX_RECEIPT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativePromotionReceipt {
    pub source: StorageManifest,
    pub target: StorageManifest,
    pub archive: PathBuf,
    pub import: NativeImportReceipt,
    pub local_telemetry: LocalTelemetryTransferReceipt,
    /// Native boot authorities are retained for diagnosis, never replicated.
    pub retired_native_database: PathBuf,
    pub retired_cluster_metadata: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Phase {
    Prepared,
    MetadataRetired,
    Importing,
    Imported,
    Activated,
    NativeRetired,
    Published,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetiredMetadata {
    filename: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    operation: String,
    phase: Phase,
    source: StorageManifest,
    target: StorageManifest,
    work: PathBuf,
    archive: PathBuf,
    backup: NativeBackupManifest,
    source_fingerprint: String,
    credential_key: PathBuf,
    retired_metadata: Vec<RetiredMetadata>,
    receipt_sha256: Option<String>,
}

fn refused(reason: impl std::fmt::Display) -> StoreError {
    StoreError::Migration(format!("managed cluster creation refused: {reason}"))
}

fn io(action: &str, path: &Path, error: impl std::fmt::Display) -> StoreError {
    refused(format!("{action} {}: {error}", path.display()))
}

/// Blocking work continues after its awaiting future is cancelled. Its own
/// lease must outlive every SQLite connection and file mutation in the worker.
fn spawn_blocking_with_lock<T, F>(
    lock: &NativeDirectoryLock,
    worker: F,
) -> tokio::task::JoinHandle<Result<T, StoreError>>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoreError> + Send + 'static,
{
    let worker_lock = lock.clone();
    tokio::task::spawn_blocking(move || {
        let result = worker();
        drop(worker_lock);
        result
    })
}

/// The caller already owns the root lease and has started no application
/// workers. Listener observation is the ordinary real cluster startup path.
pub async fn promote_native_storage(
    config: &Config,
    observer: Option<&dyn StartupClockObserver>,
    lock: NativeDirectoryLock,
) -> Result<(SelectedStore, NativePromotionReceipt), StoreError> {
    config
        .validate_cluster_runtime()
        .map_err(|error| refused(error.to_string()))?;
    let root = fs::canonicalize(&config.storage.data_dir)
        .map_err(|error| io("resolving root", &config.storage.data_dir, error))?;
    if root != lock.data_dir() {
        return Err(refused("a different directory owns the supplied lease"));
    }
    if crate::storage::helper::pending(&root)?.is_some() {
        return Err(refused(
            "complete the pending standalone migration before cluster creation",
        ));
    }
    if !config.cluster.join_token_file.as_os_str().is_empty() {
        return Err(refused("cluster create cannot also join another cluster"));
    }
    let configured_key = config.credential_key_path();
    let resolved_key = fs::canonicalize(&configured_key)
        .map_err(|error| io("resolving native master key", &configured_key, error))?;
    for reserved in [
        super::LOCAL_MEMBERSHIP_FILENAME,
        super::RAFT_SECRET_FILENAME,
        super::API_SECRET_FILENAME,
        super::ACTIVITY_SIGNING_KEY_FILENAME,
    ] {
        if resolved_key == root.join(reserved) {
            return Err(refused("the native master key path conflicts with cluster runtime metadata; relocate it explicitly before creating a cluster"));
        }
    }
    let journal_path = root.join(TRANSITION_FILENAME);
    let existing: Option<Journal> = read_record(&journal_path)?;
    let (mut journal, key, lock) = match existing {
        Some(journal) => {
            validate_journal(&root, config, &journal)?;
            let key = Arc::new(
                CredentialKey::load(&journal.credential_key)
                    .map_err(|error| refused(format!("reading retained master key: {error}")))?,
            );
            if key.id() != journal.backup.credential_key_id {
                return Err(refused(
                    "the master key differs from the pinned native archive",
                ));
            }
            (journal, key, lock)
        }
        None => prepare(config, &root, lock).await?,
    };
    let work = owned_path(&root, &journal.work)?;
    let archive = owned_path(&root, &journal.archive)?;
    if verify_native_backup_artifact(&archive)? != journal.backup {
        return Err(refused("the immutable native archive changed"));
    }
    let current: StorageManifest = read_record(&root.join(MANIFEST_FILENAME))?
        .ok_or_else(|| refused("storage authority manifest disappeared"))?;
    if current != journal.source && current != journal.target {
        return Err(refused(
            "storage authority differs from this promotion intent",
        ));
    }
    if current == journal.target {
        current.validate(&root)?;
        // A completed manifest may have accepted subsequent cluster writes.
        // Reopen it; never rebuild it from the retained native archive.
        let receipt = read_receipt(&root, &journal)?;
        validate_active_marker(&root, &journal, &receipt)?;
        clear_journal(&root)?;
        let selected = open_target(config, observer, lock, key).await?;
        return Ok((selected, receipt));
    }

    if matches!(
        journal.phase,
        Phase::Prepared | Phase::MetadataRetired | Phase::Importing
    ) {
        journal.source.validate_sqlite(&root)?;
        require_source_unchanged(&root.join("plurx.db"), &journal)?;
        if journal.phase == Phase::Prepared {
            retire_metadata(&root, &work, &journal.retired_metadata)?;
            journal.phase = Phase::MetadataRetired;
            persist(&root, &journal)?;
        } else {
            for entry in &journal.retired_metadata {
                let retained = owned_path(
                    &work,
                    &PathBuf::from("retired-cluster-metadata").join(&entry.filename),
                )?;
                if file_hash(&retained)? != entry.sha256 {
                    return Err(refused("retained former cluster metadata changed"));
                }
            }
        }
        build_target(config, &root, &work, &archive, &mut journal, &lock).await?;
    }

    let receipt = read_receipt(&root, &journal)?;
    if journal.phase == Phase::Imported {
        activate_target(&root, &work, &journal, &receipt)?;
        journal.phase = Phase::Activated;
        persist(&root, &journal)?;
        promotion_test_crash(Phase::Activated)?;
    }
    validate_active_marker(&root, &journal, &receipt)?;
    let telemetry_source = archive.join("plurx.db");
    let telemetry_target = root.join("hiqlite/telemetry.db");
    let local_telemetry = spawn_blocking_with_lock(&lock, move || {
        transfer_local_telemetry(&telemetry_source, &telemetry_target)
    })
    .await
    .map_err(|error| refused(format!("local continuity verification failed: {error}")))??;
    if local_telemetry != receipt.local_telemetry {
        return Err(refused("qualified node-local continuity receipt changed"));
    }
    // Genuine clock/membership admission and full key checks run before the
    // native authority is retired. No serving application exists yet.
    let selected =
        super::PendingSelectedStore::new(open_target(config, observer, lock, key).await?);
    let commit = finish_authority(&root, &work, &mut journal);
    if let Err(error) = commit {
        return Err(selected.refuse(error).await);
    }
    Ok((selected.into_selected(), receipt))
}

async fn prepare(
    config: &Config,
    root: &Path,
    lock: NativeDirectoryLock,
) -> Result<(Journal, Arc<CredentialKey>, NativeDirectoryLock), StoreError> {
    let source = StorageManifest::read(root)?
        .ok_or_else(|| refused("cluster create requires an adopted native installation"))?;
    source.validate_sqlite(root)?;
    for filename in [
        "hiqlite",
        "hiqlite.incoming",
        "hiqlite-readdress.json",
        mode::ADOPTION_FILENAME,
    ] {
        if owned_path(root, Path::new(filename))?.exists() {
            return Err(refused(format!("unresolved or conflicting {filename}")));
        }
    }
    let native = open_native_store_locked(config, lock).await?;
    // Use the same local schema/key preparation as normal native startup.
    // This also completes a first boot that stopped before Source prepared;
    // its local capability is revoked before any archive or voter exists.
    let source_authority = native
        .sqlite
        .prepare_native_source_before_serving(&native.identity.node_id, &native.credential_key)
        .await?;
    source_authority.revoke();
    drop(source_authority);
    let identity = native.identity.clone();
    let key = native.credential_key.clone();
    let lock = native.directory_lock.clone();
    drop(native);
    mode::checkpoint_native_image(&root.join("plurx.db"))?;
    let source =
        StorageManifest::read(root)?.ok_or_else(|| refused("native manifest disappeared"))?;
    let generation = uuid::Uuid::new_v4().to_string();
    let relative = PathBuf::from("migration").join(format!("native-promotion-{generation}"));
    let work = owned_path(root, &relative)?;
    fs::create_dir_all(work.parent().expect("migration parent"))
        .map_err(|error| io("creating migration directory", root, error))?;
    fs::create_dir(&work).map_err(|error| io("creating promotion workspace", &work, error))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&work, fs::Permissions::from_mode(0o700))
            .map_err(|error| io("protecting promotion workspace", &work, error))?;
    }
    sync_directory(work.parent().expect("migration parent"))?;
    let source_path = root.join("plurx.db");
    let key_path = config.credential_key_path();
    let backup_key = key_path.clone();
    let destination = work.clone();
    let key_id = key.id().to_owned();
    let (archive, backup) = spawn_blocking_with_lock(&lock, move || {
        build_native_backup_artifact(
            &source_path,
            &destination,
            &identity,
            &backup_key,
            &key_id,
            env!("CARGO_PKG_VERSION"),
        )
    })
    .await
    .map_err(|error| refused(format!("native archive worker failed: {error}")))??;
    let source_fingerprint = fingerprint(&root.join("plurx.db"))?;
    if source_fingerprint != fingerprint(&archive.join("plurx.db"))? {
        return Err(refused(
            "native data changed while its archive was prepared",
        ));
    }
    let mut retired_metadata = Vec::new();
    for filename in [
        super::LOCAL_MEMBERSHIP_FILENAME,
        super::RAFT_SECRET_FILENAME,
        super::API_SECRET_FILENAME,
        super::ACTIVITY_SIGNING_KEY_FILENAME,
    ] {
        let path = owned_path(root, Path::new(filename))?;
        if path
            .try_exists()
            .map_err(|error| io("inspecting old cluster metadata", &path, error))?
        {
            retired_metadata.push(RetiredMetadata {
                filename: filename.to_owned(),
                sha256: file_hash(&path)?,
            });
        }
    }
    let journal = Journal {
        version: 1,
        operation: OPERATION.to_owned(),
        phase: Phase::Prepared,
        target: StorageManifest {
            version: 1,
            mode: StorageMode::Cluster,
            database: "hiqlite".into(),
            instance_id: source.instance_id.clone(),
            node_id: source.node_id.clone(),
            generation,
            schema_version: AUTH_SCHEMA_VERSION,
        },
        source,
        work: relative,
        archive: archive
            .strip_prefix(root)
            .map_err(|_| refused("archive escaped root"))?
            .to_path_buf(),
        backup,
        source_fingerprint,
        credential_key: key_path,
        retired_metadata,
        receipt_sha256: None,
    };
    persist(root, &journal)?;
    promotion_test_crash(Phase::Prepared)?;
    Ok((journal, key, lock))
}

fn validate_journal(root: &Path, config: &Config, journal: &Journal) -> Result<(), StoreError> {
    if journal.version != 1
        || journal.operation != OPERATION
        || journal.source.mode != StorageMode::Sqlite
        || journal.target.mode != StorageMode::Cluster
        || journal.source.instance_id != journal.target.instance_id
        || journal.source.node_id != journal.target.node_id
        || ![AUTH_SCHEMA_BASELINE_VERSION, AUTH_SCHEMA_VERSION]
            .contains(&journal.target.schema_version)
        || journal.source.database != Path::new("plurx.db")
        || journal.target.database != Path::new("hiqlite")
        || uuid::Uuid::parse_str(&journal.target.generation).is_err()
        || journal.work
            != PathBuf::from("migration")
                .join(format!("native-promotion-{}", journal.target.generation))
        || journal.archive.parent() != Some(journal.work.as_path())
        || journal.credential_key != config.credential_key_path()
        || journal.backup.instance_id != journal.source.instance_id
        || journal.backup.node_id != journal.source.node_id
        || journal.backup.storage_generation != journal.source.generation
        || journal.backup.sqlite_schema_version != journal.source.schema_version
    {
        return Err(refused(
            "incompatible promotion journal or command arguments",
        ));
    }
    owned_path(root, &journal.work)?;
    owned_path(root, &journal.archive)?;
    let mut seen = std::collections::BTreeSet::new();
    for entry in &journal.retired_metadata {
        if ![
            super::LOCAL_MEMBERSHIP_FILENAME,
            super::RAFT_SECRET_FILENAME,
            super::API_SECRET_FILENAME,
            super::ACTIVITY_SIGNING_KEY_FILENAME,
        ]
        .contains(&entry.filename.as_str())
            || !seen.insert(&entry.filename)
            || !is_hash(&entry.sha256)
        {
            return Err(refused("unknown retired metadata receipt"));
        }
    }
    if !is_hash(&journal.source_fingerprint)
        || journal
            .receipt_sha256
            .as_ref()
            .is_some_and(|hash| !is_hash(hash))
    {
        return Err(refused("invalid promotion content fingerprint"));
    }
    Ok(())
}

fn is_hash(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn persist(root: &Path, journal: &Journal) -> Result<(), StoreError> {
    publish_record(&root.join(TRANSITION_FILENAME), journal)
}

fn retire_metadata(
    root: &Path,
    work: &Path,
    entries: &[RetiredMetadata],
) -> Result<(), StoreError> {
    let retired = owned_path(work, Path::new("retired-cluster-metadata"))?;
    fs::create_dir_all(&retired)
        .map_err(|error| io("creating metadata retention", &retired, error))?;
    for entry in entries {
        let source = owned_path(root, Path::new(&entry.filename))?;
        let target = owned_path(
            work,
            &PathBuf::from("retired-cluster-metadata").join(&entry.filename),
        )?;
        match (source.exists(), target.exists()) {
            (true, false) if file_hash(&source)? == entry.sha256 => {
                crate::storage::durability::rename(&source, &target)
                    .map_err(|error| io("retiring old cluster metadata", &source, error))?;
            }
            (false, true) if file_hash(&target)? == entry.sha256 => {}
            _ => {
                return Err(refused(format!(
                    "old cluster metadata {} changed or is ambiguous",
                    entry.filename
                )))
            }
        }
    }
    // Files that were absent at the intent cannot appear and silently become
    // the new cluster's credentials or old membership.
    for filename in [
        super::LOCAL_MEMBERSHIP_FILENAME,
        super::RAFT_SECRET_FILENAME,
        super::API_SECRET_FILENAME,
        super::ACTIVITY_SIGNING_KEY_FILENAME,
    ] {
        if !entries.iter().any(|entry| entry.filename == filename) && root.join(filename).exists() {
            return Err(refused(format!("unexpected cluster metadata {filename}")));
        }
    }
    sync_directory(&retired)?;
    sync_directory(work)?;
    sync_directory(root)
}

async fn build_target(
    config: &Config,
    root: &Path,
    work: &Path,
    archive: &Path,
    journal: &mut Journal,
    lock: &NativeDirectoryLock,
) -> Result<(), StoreError> {
    let incoming = owned_path(work, Path::new("hiqlite.incoming"))?;
    if incoming.exists() {
        let retained = work.join(format!("interrupted-voter-{}", uuid::Uuid::new_v4()));
        crate::storage::durability::rename(&incoming, &retained)
            .map_err(|error| io("retaining interrupted staging voter", &incoming, error))?;
        sync_directory(work)?;
    }
    if root.join("hiqlite").exists() {
        return Err(refused(
            "an active target appeared before staging completed",
        ));
    }
    journal.phase = Phase::Importing;
    persist(root, journal)?;
    let secrets = super::load_or_create_secrets(root)?;
    let identity = super::super::ClusterIdentity {
        cluster_id: journal.source.instance_id.clone(),
        node_id: journal.source.node_id.clone(),
        raft_id: super::super::SINGLE_VOTER_RAFT_ID,
    };
    let (client, local, mut pending) = super::start_voter(
        config,
        &incoming,
        &secrets,
        &identity,
        None,
        false,
        Some(lock.duplicate_locked_file()?),
        true,
        false,
    )
    .await?;
    let result = async {
        let store = HiqliteAuthStore::bootstrap(
            client.clone(),
            &journal.source.instance_id,
            &incoming.join("telemetry.db"),
        )
        .await?;
        let import = import_verified_native(
            &client,
            &archive.join("plurx.db"),
            &journal.backup.image_sha256,
            &journal.source,
        )
        .await?;
        journal.target.schema_version = import.replicated_schema_version;
        drop(store);
        let telemetry_source = archive.join("plurx.db");
        let telemetry_target = incoming.join("telemetry.db");
        let local_telemetry = spawn_blocking_with_lock(lock, move || {
            transfer_local_telemetry(&telemetry_source, &telemetry_target)
        })
        .await
        .map_err(|error| refused(format!("local continuity worker failed: {error}")))??;
        // Record explicit creation from the actually committed staged voter.
        // The role-aware marker must never precede this real membership file.
        crate::cluster::membership::require_native_import_singleton(&client, identity.raft_id)
            .await
            .map_err(|error| refused(error.to_string()))?;
        let membership_metrics = client
            .local_db_raft_metrics()
            .map_err(|error| refused(error.to_string()))?;
        let observed = membership_metrics.membership_snapshot();
        let metrics = client
            .metrics_db()
            .await
            .map_err(|error| refused(error.to_string()))?;
        let bootstrap = metrics
            .membership_config
            .nodes()
            .map(|(_, node)| ClusterPeer::from(node))
            .collect::<Vec<_>>();
        if bootstrap.as_slice() != std::slice::from_ref(&local)
            || membership_metrics.membership_snapshot() != observed
        {
            return Err(refused(
                "actual promotion singleton changed before local admission publication",
            ));
        }
        let membership = LocalMembership {
            version: super::local_membership_version(ClusterRole::Voter),
            cluster_id: identity.cluster_id.clone(),
            node_id: identity.node_id.clone(),
            raft_id: observed.node_id,
            local: local.clone(),
            bootstrap,
            join_token_digest: None,
            role: ClusterRole::Voter,
        };
        super::write_local_membership(root, &membership)?;
        if membership_metrics.membership_snapshot() != observed {
            return Err(refused(
                "actual promotion singleton changed during local admission publication",
            ));
        }
        let marker = ActivationMarker {
            marker_version: super::ACTIVATION_MARKER_VERSION,
            cluster_id: journal.source.instance_id.clone(),
            source_backup_sha256: import.source_sha256.clone(),
            source_schema_version: import.source_schema_version,
            replicated_schema_version: import.replicated_schema_version,
            imported_rows: import.imported_rows,
            table_hashes: import.tables.clone(),
            admitted_role: Some(ClusterRole::Voter),
        };
        let receipt = NativePromotionReceipt {
            source: journal.source.clone(),
            target: journal.target.clone(),
            archive: journal.archive.clone(),
            import,
            local_telemetry,
            retired_native_database: journal.work.join("retired-native/plurx.db"),
            retired_cluster_metadata: journal
                .retired_metadata
                .iter()
                .map(|entry| {
                    journal
                        .work
                        .join("retired-cluster-metadata")
                        .join(&entry.filename)
                })
                .collect(),
        };
        let receipt_path = work.join(RECEIPT_FILENAME);
        publish_record(&receipt_path, &receipt)?;
        journal.receipt_sha256 = Some(file_hash(&receipt_path)?);
        super::write_activation_marker(&incoming, &marker)?;
        super::shutdown_voter(&client, false).await?;
        Ok::<(), StoreError>(())
    }
    .await;
    if let Err(error) = result {
        if let Some(pending) = pending.as_mut() {
            pending.drain_now().await;
        }
        return Err(error);
    }
    if let Some(pending) = pending.as_mut() {
        pending.handoff();
    }
    drop(client);
    sync_directory(&incoming)?;
    journal.phase = Phase::Imported;
    persist(root, journal)?;
    promotion_test_crash(Phase::Imported)
}

fn read_receipt(root: &Path, journal: &Journal) -> Result<NativePromotionReceipt, StoreError> {
    let path = owned_path(root, &journal.work.join(RECEIPT_FILENAME))?;
    let hash = journal
        .receipt_sha256
        .as_ref()
        .ok_or_else(|| refused("qualified import receipt is missing"))?;
    if file_hash(&path)? != *hash {
        return Err(refused("qualified import receipt changed"));
    }
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| io("reading receipt metadata", &path, error))?;
    if metadata.len() > MAX_RECEIPT_BYTES {
        return Err(refused("import receipt exceeds its size bound"));
    }
    let mut bytes = Vec::new();
    File::open(&path)
        .and_then(|file| file.take(MAX_RECEIPT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| io("reading receipt", &path, error))?;
    let receipt: NativePromotionReceipt = serde_json::from_slice(&bytes)
        .map_err(|error| refused(format!("invalid import receipt: {error}")))?;
    if receipt.source != journal.source
        || receipt.target != journal.target
        || receipt.archive != journal.archive
        || receipt.import.source_sha256 != journal.backup.image_sha256
        || receipt.import.source_schema_version != journal.source.schema_version
        || receipt.import.version != 1
        || receipt.import.source_storage_generation != journal.source.generation
        || receipt.import.replicated_schema_version != journal.target.schema_version
        || receipt.retired_native_database != journal.work.join("retired-native/plurx.db")
    {
        return Err(refused(
            "import receipt does not bind this authority transition",
        ));
    }
    Ok(receipt)
}

fn validate_marker(
    path: &Path,
    journal: &Journal,
    receipt: &NativePromotionReceipt,
) -> Result<(), StoreError> {
    let marker = super::read_activation_marker(path)?;
    if marker.cluster_id != journal.source.instance_id
        || marker.source_backup_sha256 != journal.backup.image_sha256
        || marker.source_schema_version != journal.source.schema_version
        || marker.replicated_schema_version != journal.target.schema_version
        || marker.imported_rows != receipt.import.imported_rows
        || marker.table_hashes != receipt.import.tables
        || marker.admitted_role != Some(ClusterRole::Voter)
    {
        return Err(refused(
            "target activation marker differs from the verified full import",
        ));
    }
    Ok(())
}

fn validate_active_marker(
    root: &Path,
    journal: &Journal,
    receipt: &NativePromotionReceipt,
) -> Result<(), StoreError> {
    validate_marker(&owned_path(root, Path::new("hiqlite"))?, journal, receipt)
}

fn activate_target(
    root: &Path,
    work: &Path,
    journal: &Journal,
    receipt: &NativePromotionReceipt,
) -> Result<(), StoreError> {
    let incoming = owned_path(work, Path::new("hiqlite.incoming"))?;
    let active = owned_path(root, Path::new("hiqlite"))?;
    match (incoming.exists(), active.exists()) {
        (true, false) => {
            validate_marker(&incoming, journal, receipt)?;
            crate::storage::durability::rename(&incoming, &active)
                .map_err(|error| io("activating imported voter", &active, error))?;
            sync_directory(work)?;
            sync_directory(root)?;
        }
        (false, true) => validate_marker(&active, journal, receipt)?,
        _ => return Err(refused("staged and active target ownership is ambiguous")),
    }
    Ok(())
}

async fn open_target(
    config: &Config,
    observer: Option<&dyn StartupClockObserver>,
    lock: NativeDirectoryLock,
    key: Arc<CredentialKey>,
) -> Result<SelectedStore, StoreError> {
    super::readdress_single_voter_if_needed(config)?;
    let selected = super::PendingSelectedStore::new(
        super::open_active_store_with_key(config, lock.into_locked_file()?, Some(key), observer)
            .await?,
    );
    if let Err(error) = super::finish_readdress(&config.storage.data_dir) {
        return Err(selected.refuse(error).await);
    }
    Ok(selected.into_selected())
}

fn finish_authority(root: &Path, work: &Path, journal: &mut Journal) -> Result<(), StoreError> {
    let source = owned_path(root, Path::new("plurx.db"))?;
    let retained = owned_path(work, Path::new("retired-native/plurx.db"))?;
    match (source.exists(), retained.exists()) {
        (true, false) => {
            require_source_unchanged(&source, journal)?;
            mode::checkpoint_native_image(&source)?;
            require_source_unchanged(&source, journal)?;
            for sidecar in ["plurx.db-wal", "plurx.db-shm", "plurx.db-journal"] {
                if root.join(sidecar).exists() {
                    return Err(refused(
                        "native source still has a live or unresolved journal",
                    ));
                }
            }
            fs::create_dir_all(retained.parent().expect("retention parent"))
                .map_err(|error| io("creating native retention", work, error))?;
            crate::storage::durability::rename(&source, &retained)
                .map_err(|error| io("retiring native authority", &source, error))?;
            sync_directory(retained.parent().expect("retention parent"))?;
            sync_directory(work)?;
            sync_directory(root)?;
        }
        (false, true) => require_source_unchanged(&retained, journal)?,
        _ => return Err(refused("native authority retirement is ambiguous")),
    }
    journal.phase = Phase::NativeRetired;
    persist(root, journal)?;
    promotion_test_crash(Phase::NativeRetired)?;
    journal.target.publish(root)?;
    journal.phase = Phase::Published;
    persist(root, journal)?;
    promotion_test_crash(Phase::Published)?;
    clear_journal(root)
}

fn clear_journal(root: &Path) -> Result<(), StoreError> {
    fs::remove_file(root.join(TRANSITION_FILENAME))
        .map_err(|error| io("clearing completed promotion intent", root, error))?;
    sync_directory(root)
}

fn require_source_unchanged(path: &Path, journal: &Journal) -> Result<(), StoreError> {
    if fingerprint(path)? != journal.source_fingerprint {
        return Err(refused(
            "native source contains writes outside the pinned import",
        ));
    }
    Ok(())
}

fn fingerprint(path: &Path) -> Result<String, StoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    restore_content_fingerprint(&connection)
}

fn file_hash(path: &Path) -> Result<String, StoreError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|error| io("inspecting file", path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(refused("expected an owned regular file"));
    }
    let mut file = File::open(path).map_err(|error| io("opening file", path, error))?;
    let mut hash = Sha256::new();
    let mut bytes = [0u8; 64 * 1024];
    loop {
        let count = file
            .read(&mut bytes)
            .map_err(|error| io("hashing file", path, error))?;
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

#[cfg(test)]
thread_local! {static PROMOTION_CRASH:std::cell::Cell<Option<Phase>>=const{std::cell::Cell::new(None)};}
fn promotion_test_crash(phase: Phase) -> Result<(), StoreError> {
    #[cfg(test)]
    if PROMOTION_CRASH.with(|point| point.get() == Some(phase)) {
        PROMOTION_CRASH.with(|point| point.set(None));
        return Err(refused("injected managed cluster creation interruption"));
    }
    let _ = phase;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn cancelled_promotion_waiter_retains_root_until_blocking_writer_exits() {
        let directory = tempfile::tempdir().expect("native cancellation fixture");
        let root = directory.path();
        let native = crate::storage::open_native_store_at(root, &root.join("credentials.key"))
            .await
            .expect("actual native owner");
        drop(native);
        let lock = NativeDirectoryLock::acquire(root)
            .await
            .expect("original native startup writers have released their lease");
        let database = root.join("plurx.db");
        let (started, observed_start) = tokio::sync::oneshot::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        let (finished, observed_finish) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            spawn_blocking_with_lock(&lock, move || {
                let mut connection = Connection::open(database)?;
                connection.pragma_update(None, "synchronous", "FULL")?;
                let transaction = connection.transaction()?;
                transaction.execute(
                    "INSERT INTO settings(key,value) VALUES('promotion.blocking-cancel','committed after waiter cancellation')",
                    [],
                )?;
                let _ = started.send(());
                paused
                    .recv_timeout(Duration::from_secs(30))
                    .map_err(|error| refused(format!("paused writer was not resumed: {error}")))?;
                transaction.commit()?;
                drop(connection);
                let _ = finished.send(());
                Ok(())
            })
            .await
            .expect("blocking worker task")
        });
        tokio::time::timeout(Duration::from_secs(10), observed_start)
            .await
            .expect("blocking writer started")
            .expect("actual SQLite transaction is paused");
        waiter.abort();
        assert!(waiter
            .await
            .expect_err("promotion waiter cancelled")
            .is_cancelled());
        let remained_busy = matches!(
            NativeDirectoryLock::try_acquire(root),
            Err(StoreError::Identity(message)) if message.contains("busy")
        );
        // Complete the actual writer before asserting the lease invariant so
        // a failing regression still releases the test's owned resources.
        resume.send(()).expect("resume actual blocking writer");
        tokio::time::timeout(Duration::from_secs(10), observed_finish)
            .await
            .expect("blocking writer finished")
            .expect("actual SQLite transaction committed");
        let released = NativeDirectoryLock::acquire(root)
            .await
            .expect("worker completion releases the root lease");
        let connection =
            Connection::open_with_flags(root.join("plurx.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("read completed native write");
        let value: String = connection
            .query_row(
                "SELECT value FROM settings WHERE key='promotion.blocking-cancel'",
                [],
                |row| row.get(0),
            )
            .expect("actual post-cancellation commit");
        drop(connection);
        drop(released);
        assert!(
            remained_busy,
            "cancelling the async waiter must not release the blocking writer's root lease"
        );
        assert_eq!(value, "committed after waiter cancellation");
    }

    struct AppliedSingletonObserver;

    impl StartupClockObserver for AppliedSingletonObserver {
        fn start(
            &self,
            manager: crate::cluster::membership::MembershipManager,
            _node: String,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), StoreError>> + Send + '_>>
        {
            Box::pin(async move {
                let roster = manager
                    .clock_peers()
                    .await
                    .map_err(|error| refused(error.to_string()))?;
                let applied = roster
                    .membership
                    .as_ref()
                    .ok_or_else(|| refused("test observer has no applied membership"))?;
                let singleton = std::collections::BTreeSet::from([applied.local_node]);
                if applied.members != singleton
                    || applied.voters != singleton
                    || !roster.peers.is_empty()
                {
                    return Err(refused("singleton observer requires a genuine committed local voter; remote members require authenticated transport"));
                }
                let guard = manager.clock_guard();
                let generation = guard
                    .roster_for_peer_directory(&roster)
                    .map_err(|error| refused(error.to_string()))?;
                // There are no remote peers in this actually applied roster.
                // This publishes no synthetic votes, peer timing or readiness.
                if !guard.publish(generation, std::collections::BTreeMap::new()) {
                    return Err(refused("applied singleton changed during observation"));
                }
                Ok(())
            })
        }
    }

    #[test]
    fn real_native_promotion_preserves_source_master_local_data_and_accepted_writes() {
        let worker = std::thread::Builder::new()
            .name("real-native-promotion".into())
            .stack_size(8 * 1024 * 1024)
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(4)
                    .thread_stack_size(8 * 1024 * 1024)
                    .enable_all()
                    .build()
                    .expect("owned promotion test runtime");
                runtime.block_on(Box::pin(async {
                    let root = tempfile::tempdir().expect("owned promotion fixture").keep();
                    eprintln!("real native promotion evidence root: {}", root.display());
                    let result = tokio::time::timeout(
                        Duration::from_secs(240),
                        real_promotion_fixture(&root),
                    )
                    .await;
                    match result {
                        Ok(Ok(())) => {
                            fs::remove_dir_all(&root).expect("remove completed owned fixture")
                        }
                        Ok(Err(error)) => panic!(
                            "real native promotion failed; retained evidence {}: {error}",
                            root.display()
                        ),
                        Err(error) => panic!(
                            "real native promotion timed out; retained evidence {}: {error}",
                            root.display()
                        ),
                    }
                }));
            })
            .expect("owned promotion worker");
        if let Err(panic) = worker.join() {
            std::panic::resume_unwind(panic);
        }
    }

    async fn real_promotion_fixture(root: &Path) -> Result<(), StoreError> {
        let listeners = (0..3)
            .map(|_| std::net::TcpListener::bind("127.0.0.1:0"))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| refused(error.to_string()))?;
        let addresses = listeners
            .iter()
            .map(std::net::TcpListener::local_addr)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| refused(error.to_string()))?;
        let mut config = Config::default();
        config.storage.data_dir = root.to_path_buf();
        config.server.bind = addresses[0];
        config.cluster.raft_bind = addresses[1];
        config.cluster.api_bind = addresses[2];
        drop(listeners);
        let native = crate::storage::open_native_store(&config).await?;
        let authority = native
            .sqlite
            .prepare_native_source_before_serving(&native.identity.node_id, &native.credential_key)
            .await?;
        authority.revoke();
        drop(authority);
        native
            .store
            .put_setting("promotion.core-native", "retained application bytes")
            .await?;
        let local = Connection::open(root.join("plurx.db"))?;
        local.execute("INSERT INTO playback_events(id,at_unix_ms,event) VALUES(77,123,'core-promotion-retained')",[])?;
        local.execute("INSERT INTO rendition_plans VALUES('core-normative-plan',4,1024,55,'argv',1,90000,6,1,?1,123)",[&[0u8,255,16,0][..]])?;
        drop(local);
        let identity = native.identity.clone();
        let master = native.credential_key.id().to_owned();
        let lock = native.directory_lock.clone();
        drop(native);
        let (selected, receipt) =
            promote_native_storage(&config, Some(&AppliedSingletonObserver), lock).await?;
        let selected = super::super::PendingSelectedStore::new(selected);
        let checks = async {
            if selected.identity.cluster_id != identity.instance_id || selected.identity.node_id != identity.node_id || selected.credential_key.id() != master {
                return Err(refused("managed promotion changed native identity or master key"));
            }
            if StorageManifest::read(root)? != Some(receipt.target.clone()) || receipt.target.mode != StorageMode::Cluster || root.join("plurx.db").exists() {
                return Err(refused("managed promotion did not exclusively publish cluster authority"));
            }
            for table in ["sharing_catalogue_keys","sharing_file_locator_keys","sharing_source_schema_installation"] {
                if !receipt.import.tables.iter().any(|entry|entry.table==table && entry.row_count>0) {
                    return Err(refused(format!("missing preserved Source/purpose parity: {table}")));
                }
            }
            if selected.store.get_setting("promotion.core-native").await? != Some("retained application bytes".into()) {
                return Err(refused("native application setting was not imported"));
            }
            let local = Connection::open(root.join("hiqlite/telemetry.db"))?;
            let event:String=local.query_row("SELECT event FROM playback_events WHERE id=77",[],|row|row.get(0))?;
            let packed:Vec<u8>=local.query_row("SELECT entries_packed FROM rendition_plans WHERE rendition_key='core-normative-plan'",[],|row|row.get(0))?;
            if event!="core-promotion-retained" || packed!=vec![0,255,16,0] {
                return Err(refused("live local observations or normative plan bytes were lost"));
            }
            drop(local);
            selected.store.put_setting("promotion.core-cluster", "accepted after real handoff").await?;
            if selected.store.get_setting("promotion.core-cluster").await? != Some("accepted after real handoff".into()) {
                return Err(refused("target did not retain a genuine post-handoff write"));
            }
            Ok(())
        }.await;
        selected.shutdown().await?;
        drop(selected);
        let released = NativeDirectoryLock::try_acquire(root)?;
        drop(released);
        checks
    }

    async fn fixture() -> (tempfile::TempDir, Journal) {
        let directory = tempfile::tempdir().expect("fixture root");
        let root = directory.path();
        let key_path = root.join("credentials.key");
        let native = crate::storage::open_native_store_at(root, &key_path)
            .await
            .expect("native store");
        let identity = native.identity.clone();
        let key_id = native.credential_key.id().to_owned();
        drop(native);
        mode::checkpoint_native_image(&root.join("plurx.db")).expect("closed native image");
        let source = StorageManifest::read(root)
            .expect("manifest")
            .expect("adopted native");
        let generation = uuid::Uuid::new_v4().to_string();
        let relative = PathBuf::from("migration").join(format!("native-promotion-{generation}"));
        let work = root.join(&relative);
        fs::create_dir_all(&work).expect("work");
        let (archive, backup) = build_native_backup_artifact(
            &root.join("plurx.db"),
            &work,
            &identity,
            &key_path,
            &key_id,
            "test",
        )
        .expect("pinned archive");
        fs::create_dir(root.join("hiqlite")).expect("qualified target fixture");
        let journal = Journal {
            version: 1,
            operation: OPERATION.to_owned(),
            phase: Phase::Activated,
            target: StorageManifest {
                version: 1,
                mode: StorageMode::Cluster,
                database: "hiqlite".into(),
                instance_id: source.instance_id.clone(),
                node_id: source.node_id.clone(),
                generation,
                schema_version: AUTH_SCHEMA_VERSION,
            },
            source,
            work: relative,
            archive: archive
                .strip_prefix(root)
                .expect("archive owned")
                .to_path_buf(),
            backup,
            source_fingerprint: fingerprint(&root.join("plurx.db")).expect("source digest"),
            credential_key: key_path,
            retired_metadata: Vec::new(),
            receipt_sha256: None,
        };
        persist(root, &journal).expect("intent");
        (directory, journal)
    }

    // These tests exercise filesystem authority cuts after the real importer
    // has qualified its target. They do not claim a simulated directory is a
    // live Raft voter; that requires the real-client import regression.
    #[tokio::test]
    async fn native_promotion_resumes_after_source_retirement_without_reimport() {
        let (directory, mut journal) = fixture().await;
        let root = directory.path();
        let work = root.join(&journal.work);
        PROMOTION_CRASH.with(|point| point.set(Some(Phase::NativeRetired)));
        assert!(finish_authority(root, &work, &mut journal).is_err());
        assert!(!root.join("plurx.db").exists());
        assert!(work.join("retired-native/plurx.db").is_file());
        assert_eq!(
            read_record::<StorageManifest>(&root.join(MANIFEST_FILENAME)).expect("old manifest"),
            Some(journal.source.clone())
        );
        let mut resumed: Journal = read_record(&root.join(TRANSITION_FILENAME))
            .expect("resumable intent")
            .expect("intent retained");
        finish_authority(root, &work, &mut resumed).expect("finish same generation");
        assert_eq!(
            StorageManifest::read(root).expect("cluster manifest"),
            Some(journal.target)
        );
        assert!(!root.join(TRANSITION_FILENAME).exists());
        assert_eq!(
            fingerprint(&work.join("retired-native/plurx.db")).expect("retained data"),
            journal.source_fingerprint
        );
    }

    #[tokio::test]
    async fn native_promotion_resumes_after_manifest_without_replacing_target_writes() {
        let (directory, mut journal) = fixture().await;
        let root = directory.path();
        let work = root.join(&journal.work);
        PROMOTION_CRASH.with(|point| point.set(Some(Phase::Published)));
        assert!(finish_authority(root, &work, &mut journal).is_err());
        let accepted = root.join("hiqlite/accepted-after-publication");
        fs::write(&accepted, b"later cluster bytes").expect("accepted target fixture");
        let mut resumed: Journal = read_record(&root.join(TRANSITION_FILENAME))
            .expect("intent")
            .expect("retained");
        assert_eq!(
            StorageManifest::read(root).expect("committed target"),
            Some(journal.target.clone())
        );
        finish_authority(root, &work, &mut resumed).expect("idempotent publication");
        assert_eq!(
            fs::read(accepted).expect("target retained"),
            b"later cluster bytes"
        );
        assert!(!root.join("plurx.db").exists());
    }

    #[tokio::test]
    async fn native_promotion_refuses_source_drift_before_authority_cutover() {
        let (directory, mut journal) = fixture().await;
        let root = directory.path();
        let source = Connection::open(root.join("plurx.db")).expect("native source");
        source.execute("INSERT INTO settings(key,value) VALUES('promotion.external-write','accepted elsewhere')",[]).expect("unowned write");
        drop(source);
        assert!(finish_authority(root, &root.join(&journal.work), &mut journal).is_err());
        assert!(root.join("plurx.db").is_file());
        assert!(!root
            .join(&journal.work)
            .join("retired-native/plurx.db")
            .exists());
        assert_eq!(
            StorageManifest::read(root).expect("native retained"),
            Some(journal.source)
        );
    }

    #[test]
    fn native_promotion_metadata_retirement_resumes_only_exact_recorded_files() {
        let directory = tempfile::tempdir().expect("fixture root");
        let root = directory.path();
        let work = root.join("owned-work");
        fs::create_dir(&work).expect("work");
        let source = root.join(super::super::LOCAL_MEMBERSHIP_FILENAME);
        fs::write(&source, b"retained old membership bytes").expect("old membership");
        let entries = vec![RetiredMetadata {
            filename: super::super::LOCAL_MEMBERSHIP_FILENAME.to_owned(),
            sha256: file_hash(&source).expect("receipt"),
        }];
        retire_metadata(root, &work, &entries).expect("first move");
        retire_metadata(root, &work, &entries).expect("same intent resumes");
        fs::write(&source, b"different unowned membership").expect("conflicting root bytes");
        assert!(retire_metadata(root, &work, &entries).is_err());
        assert_eq!(
            fs::read(&source).expect("unowned bytes preserved"),
            b"different unowned membership"
        );
    }
}
