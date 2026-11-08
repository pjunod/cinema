//! Bounded migration intent and receipts. These DTOs carry no serving authority.
//! The launcher authenticates the executable generation and owns child reap;
//! the helper independently owns the root and repeats the actual source checks.

use std::fs;
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::mode::{owned_path, publish_record, read_record, StorageManifest, StorageMode};
use super::NativeDirectoryLock;
use crate::config::Config;
use crate::error::StoreError;

#[cfg(feature = "hiqlite-store")]
mod runtime;
#[cfg(feature = "hiqlite-store")]
pub use runtime::{convert_private_selected, run};
#[cfg(feature = "hiqlite-store")]
pub(crate) use runtime::{open_actual_census_key, prepare_key_before_clock};

const ACTIVE: &str = "storage-migration.json";
const LIMIT: u64 = 65_536;
const RECEIPT_LIMIT: u64 = 16 * 1024 * 1024;
const METADATA: [&str; 5] = [
    "node.id",
    "storage-mode.json",
    "hiqlite/activation.json",
    "membership.json",
    "hiqlite-activated.json",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperBuild {
    pub commit: String,
    pub tree: String,
    pub target: String,
}

/// Expected source layout, not evidence that a particular database matches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationSourceCohort {
    pub auth_version: i64,
    /// SHA256 of the domain-separated, exact canonical production recognizer
    /// SQL, including its compiled schema definitions and refusal predicates.
    /// This is distinct from both converter source SHA and database byte SHA.
    pub canonical_layout_fingerprint: String,
}

/// Available without the SDK so the native launcher can select an authenticated
/// helper cohort. The helper must still evaluate this recognizer on its actual
/// immutable source cut; metadata and fingerprints cannot replace that check.
pub fn expected_source_cohorts() -> Vec<MigrationSourceCohort> {
    use crate::store::sharing_source_schema::{
        migration_source_layout_guard, SOURCE_SCHEMA_PREDECESSOR, SOURCE_SCHEMA_VERSION,
    };
    [SOURCE_SCHEMA_PREDECESSOR, SOURCE_SCHEMA_VERSION]
        .into_iter()
        .map(|auth_version| {
            let recognizer = migration_source_layout_guard(auth_version)
                .expect("closed supported migration cohort");
            let mut hash = Sha256::new();
            hash.update(b"plurx-private-singleton-source-layout-recognizer-v1\0");
            hash.update(auth_version.to_be_bytes());
            hash.update(recognizer.as_bytes());
            MigrationSourceCohort {
                auth_version,
                canonical_layout_fingerprint: format!("{:x}", hash.finalize()),
            }
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperDescription {
    pub version: u32,
    pub build: HelperBuild,
    pub source_cohorts: Vec<MigrationSourceCohort>,
    /// Exact converter and closed Source-schema implementation, not a semver.
    pub converter_sha256: String,
    pub native_schema_version: i64,
    pub manifest_version: u32,
    pub transition_version: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperBinding {
    pub daemon: HelperBuild,
    pub helper: HelperDescription,
    pub executable_sha256: String,
    pub bundle_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedRequest {
    pub id: String,
    pub sha256: String,
    pub binding: HelperBinding,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperResult {
    pub version: u32,
    pub request: PreparedRequest,
    pub manifest: StorageManifest,
    pub receipt_path: PathBuf,
    pub receipt_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    device: u64,
    inode: u64,
    high: u64,
}

impl Identity {
    fn from_file(value: crate::fs_secure::FileIdentity) -> Self {
        Self {
            device: value.device,
            inode: value.inode,
            #[cfg(windows)]
            high: value.inode_high,
            #[cfg(not(windows))]
            high: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileBinding {
    identity: Identity,
    sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReopenSettings {
    server_bind: SocketAddr,
    raft_bind: SocketAddr,
    api_bind: SocketAddr,
    bounded_replica_reads: bool,
    bounded_replica_max_lag_entries: u64,
    read_pool_size: usize,
    logs_until_snapshot: u64,
    snapshot_chunk_timeout_secs: u64,
    snapshot_transfer_timeout_secs: u64,
    install_snapshot_timeout_secs: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    id: String,
    binding: HelperBinding,
    root: Identity,
    metadata: Vec<Option<FileBinding>>,
    source_records: Vec<Option<serde_json::Value>>,
    key_path_sha256: String,
    key: Option<FileBinding>,
    settings: ReopenSettings,
}

pub fn describe(build: HelperBuild) -> Result<HelperDescription, StoreError> {
    validate_build(&build)?;
    let mut implementation = Sha256::new();
    implementation.update(include_bytes!("helper/runtime.rs"));
    implementation.update(include_bytes!("../cluster/migration/standalone_export.rs"));
    implementation.update(include_bytes!("../store/sharing_source_schema.rs"));
    Ok(HelperDescription {
        version: 1,
        build,
        source_cohorts: expected_source_cohorts(),
        converter_sha256: format!("{:x}", implementation.finalize()),
        native_schema_version: crate::store::SQLITE_SCHEMA_VERSION,
        manifest_version: 1,
        transition_version: 1,
    })
}

/// Freeze intent while the caller still holds the actual root lease. The
/// returned descriptor deliberately contains no lease clone.
pub fn prepare(
    config: &Config,
    lease: &NativeDirectoryLock,
    binding: HelperBinding,
) -> Result<PreparedRequest, StoreError> {
    validate_binding(&binding)?;
    let root = lease.data_dir();
    if fs::canonicalize(&config.storage.data_dir).map_err(io)? != root {
        return Err(refused("configuration and held root differ"));
    }
    if pending(root)?.is_some() {
        return Err(refused("a migration operation already owns this root"));
    }
    if read_record::<serde_json::Value>(&owned_path(root, Path::new("storage-transition.json"))?)?
        .is_some()
    {
        return Err(refused(
            "an existing storage transition must be resolved first",
        ));
    }
    if StorageManifest::read(root)?.is_some() {
        return Err(refused(
            "committed storage requires its explicit migration/decommission operation",
        ));
    }
    let settings = ReopenSettings::capture(config)?;
    let metadata = metadata(root)?;
    if metadata[0].is_none() || metadata[2].is_none() {
        return Err(refused(
            "helper requires an existing activated legacy root and node identity",
        ));
    }
    let source_records = [
        "hiqlite/activation.json",
        "membership.json",
        "hiqlite-activated.json",
    ]
    .iter()
    .map(|name| read_record(&owned_path(root, Path::new(name))?))
    .collect::<Result<Vec<_>, _>>()?;
    check_private_metadata(&source_records, &binding.helper.source_cohorts)?;
    refuse_foreign_operations(root)?;
    let id = uuid::Uuid::new_v4().to_string();
    let base = owned_path(root, Path::new("storage-migrations"))?;
    if !base.exists() {
        fs::create_dir(&base).map_err(io)?;
        super::mode::sync_directory(root)?;
    }
    crate::fs_secure::directory_identity_nofollow_blocking(&base).map_err(io)?;
    let directory = owned_path(root, &Path::new("storage-migrations").join(&id))?;
    fs::create_dir(&directory).map_err(io)?;
    super::mode::sync_directory(&base)?;
    let key_path = config.credential_key_path();
    let request = Request {
        version: 1,
        id,
        binding,
        root: root_identity(root)?,
        metadata,
        source_records,
        key_path_sha256: path_hash(&key_path)?,
        key: file_binding(&key_path)?,
        settings,
    };
    let path = directory.join("request.json");
    publish_record(&path, &request)?;
    let prepared = PreparedRequest {
        id: request.id,
        sha256: hash_regular(&path, LIMIT)?,
        binding: request.binding,
    };
    publish_record(&owned_path(root, Path::new(ACTIVE))?, &prepared)?;
    Ok(prepared)
}

/// These persisted facts can rule out private conversion before we pin an
/// operation. Positive admission still requires the helper's live SDK checks.
fn check_private_metadata(
    records: &[Option<serde_json::Value>],
    cohorts: &[MigrationSourceCohort],
) -> Result<(), StoreError> {
    let marker = records[0]
        .as_ref()
        .ok_or_else(|| refused("existing legacy activation marker missing"))?;
    if !marker["admitted_role"].is_null()
        || !cohorts
            .iter()
            .any(|cohort| marker["replicated_schema_version"].as_i64() == Some(cohort.auth_version))
    {
        return Err(refused("admitted or unsupported legacy source"));
    }
    if let Some(member) = &records[1] {
        let local = &member["local"];
        let loopback = ["raft_address", "api_address"].iter().all(|&field| {
            local[field]
                .as_str()
                .and_then(|value| value.parse::<SocketAddr>().ok())
                .is_some_and(|address| address.ip().is_loopback())
        });
        if !loopback
            || !member["join_token_digest"].is_null()
            || member.get("role").is_some_and(|role| role != "voter")
            || !member["bootstrap"]
                .as_array()
                .is_some_and(|peers| peers.len() <= 1 && peers.iter().all(|peer| peer == local))
        {
            return Err(refused(
                "explicitly peered legacy root cannot use the singleton helper",
            ));
        }
    }
    Ok(())
}

fn refuse_foreign_operations(root: &Path) -> Result<(), StoreError> {
    for name in [
        "hiqlite.incoming",
        "hiqlite-readdress.json",
        "hiqlite.readdress-incoming",
        "hiqlite.before-readdress",
        "migration/hiqlite-activation.in-progress",
    ] {
        match fs::symlink_metadata(owned_path(root, Path::new(name))?) {
            Ok(_) => {
                return Err(refused(
                    "another legacy activation/readdress operation is pending",
                ))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
        }
    }
    Ok(())
}

/// Inspect only; callers must hold the root before using this result to act.
/// Even malformed intent refuses ordinary startup rather than disappearing.
pub fn pending(root: &Path) -> Result<Option<PreparedRequest>, StoreError> {
    let prepared: Option<PreparedRequest> = read_record(&owned_path(root, Path::new(ACTIVE))?)?;
    if let Some(prepared) = &prepared {
        read_request(root, prepared)?;
    }
    Ok(prepared)
}

/// Called only after the actual child was reaped and the parent reacquired the
/// root. It checks durable publication; initial database bytes are not compared
/// after the native generation can have accepted writes.
pub fn validate_completed(
    lease: &NativeDirectoryLock,
    prepared: &PreparedRequest,
) -> Result<HelperResult, StoreError> {
    let root = lease.data_dir();
    let request = read_request(root, prepared)?;
    if root_identity(root)? != request.root {
        return Err(refused("migration root identity changed"));
    }
    let result: HelperResult = read_record(&operation_path(root, &prepared.id, "result.json")?)?
        .ok_or_else(|| refused("helper has no durable completion receipt"))?;
    if result.version != 1
        || result.request != *prepared
        || result.manifest.mode != StorageMode::Sqlite
    {
        return Err(refused("helper completion belongs to another request"));
    }
    let manifest =
        StorageManifest::read(root)?.ok_or_else(|| refused("native manifest missing"))?;
    if manifest != result.manifest {
        return Err(refused(
            "helper completion differs from the active generation",
        ));
    }
    manifest.validate_sqlite(root)?;
    if hash_regular(&owned_path(root, &result.receipt_path)?, RECEIPT_LIMIT)?
        != result.receipt_sha256
    {
        return Err(refused("conversion receipt changed"));
    }
    // Removing the active marker is the final parent-side commit. The request
    // and result stay archived, and repeat validation remains idempotent.
    if let Some(active) = pending(root)? {
        if active != *prepared {
            return Err(refused("another operation replaced the active marker"));
        }
        fs::remove_file(owned_path(root, Path::new(ACTIVE))?).map_err(io)?;
        super::mode::sync_directory(root)?;
    }
    Ok(result)
}

impl ReopenSettings {
    fn capture(config: &Config) -> Result<Self, StoreError> {
        let cluster = &config.cluster;
        if config.storage.mode == Some(StorageMode::Cluster)
            || !cluster.advertise_host.is_empty()
            || !cluster.join_url.is_empty()
            || !cluster.artwork_url.is_empty()
            || !cluster.join_token_file.as_os_str().is_empty()
        {
            return Err(refused(
                "advertised/join configuration is not a private singleton",
            ));
        }
        config
            .validate_cluster_runtime()
            .map_err(|error| refused(error.to_string()))?;
        let mut raft_bind = cluster.raft_bind;
        let mut api_bind = cluster.api_bind;
        // The existing private-singleton opener forces loopback too. Preserve
        // configured ports while explicitly excluding an externally bound helper.
        raft_bind.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
        api_bind.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
        Ok(Self {
            server_bind: config.server.bind,
            raft_bind,
            api_bind,
            bounded_replica_reads: cluster.bounded_replica_reads,
            bounded_replica_max_lag_entries: cluster.bounded_replica_max_lag_entries,
            read_pool_size: cluster.read_pool_size,
            logs_until_snapshot: cluster.logs_until_snapshot,
            snapshot_chunk_timeout_secs: cluster.snapshot_chunk_timeout_secs,
            snapshot_transfer_timeout_secs: cluster.snapshot_transfer_timeout_secs,
            install_snapshot_timeout_secs: cluster.install_snapshot_timeout_secs,
        })
    }

    #[cfg(feature = "hiqlite-store")]
    fn config(&self, root: &Path, key: &Path) -> Result<Config, StoreError> {
        if !self.raft_bind.ip().is_loopback() || !self.api_bind.ip().is_loopback() {
            return Err(refused("helper listeners must remain private loopback"));
        }
        let mut config = Config::default();
        config.storage.data_dir = root.to_path_buf();
        config.server.bind = self.server_bind;
        config.cluster.raft_bind = self.raft_bind;
        config.cluster.api_bind = self.api_bind;
        config.cluster.credential_key_file = key.to_path_buf();
        config.cluster.bounded_replica_reads = self.bounded_replica_reads;
        config.cluster.bounded_replica_max_lag_entries = self.bounded_replica_max_lag_entries;
        config.cluster.read_pool_size = self.read_pool_size;
        config.cluster.logs_until_snapshot = self.logs_until_snapshot;
        config.cluster.snapshot_chunk_timeout_secs = self.snapshot_chunk_timeout_secs;
        config.cluster.snapshot_transfer_timeout_secs = self.snapshot_transfer_timeout_secs;
        config.cluster.install_snapshot_timeout_secs = self.install_snapshot_timeout_secs;
        config
            .validate_cluster_runtime()
            .map_err(|error| refused(error.to_string()))?;
        Ok(config)
    }
}

fn read_request(root: &Path, prepared: &PreparedRequest) -> Result<Request, StoreError> {
    canonical_id(&prepared.id)?;
    hex(&prepared.sha256, 64)?;
    validate_binding(&prepared.binding)?;
    let path = operation_path(root, &prepared.id, "request.json")?;
    if hash_regular(&path, LIMIT)? != prepared.sha256 {
        return Err(refused("immutable request bytes changed"));
    }
    let request: Request = read_record(&path)?.ok_or_else(|| refused("request missing"))?;
    if request.version != 1
        || request.id != prepared.id
        || request.binding != prepared.binding
        || request.metadata.len() != METADATA.len()
        || request.source_records.len() != 3
    {
        return Err(refused("invalid or mismatched migration request"));
    }
    Ok(request)
}

fn validate_build(build: &HelperBuild) -> Result<(), StoreError> {
    hex(&build.commit, 40)?;
    hex(&build.tree, 40)?;
    if build.target.is_empty()
        || build.target.len() > 128
        || !build
            .target
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
    {
        return Err(refused("invalid exact build target"));
    }
    Ok(())
}

fn validate_binding(binding: &HelperBinding) -> Result<(), StoreError> {
    validate_build(&binding.daemon)?;
    hex(&binding.executable_sha256, 64)?;
    hex(&binding.bundle_sha256, 64)?;
    if binding.helper != describe(binding.helper.build.clone())?
        || binding.daemon.target != binding.helper.build.target
    {
        return Err(refused(
            "unsupported helper protocol, source cohort, or target",
        ));
    }
    Ok(())
}

fn canonical_id(value: &str) -> Result<(), StoreError> {
    if uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value) {
        Ok(())
    } else {
        Err(refused("noncanonical migration operation id"))
    }
}

fn hex(value: &str, length: usize) -> Result<(), StoreError> {
    if value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(refused("noncanonical build or content hash"))
    }
}

fn operation_path(root: &Path, id: &str, name: &str) -> Result<PathBuf, StoreError> {
    canonical_id(id)?;
    owned_path(root, &Path::new("storage-migrations").join(id).join(name))
}

fn root_identity(root: &Path) -> Result<Identity, StoreError> {
    crate::fs_secure::directory_identity_nofollow_blocking(root)
        .map(Identity::from_file)
        .map_err(io)
}

fn metadata(root: &Path) -> Result<Vec<Option<FileBinding>>, StoreError> {
    METADATA
        .iter()
        .map(|name| file_binding(&owned_path(root, Path::new(name))?))
        .collect()
}

fn file_binding(path: &Path) -> Result<Option<FileBinding>, StoreError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(error)),
        Ok(metadata)
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > LIMIT =>
        {
            return Err(refused("metadata is not a bounded regular file"))
        }
        Ok(_) => {}
    }
    let mut file = crate::fs_secure::open_read_nofollow_blocking(path).map_err(io)?;
    let before = crate::fs_secure::std_file_identity(&file).map_err(io)?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    let after = crate::fs_secure::std_file_identity(&file).map_err(io)?;
    if before != after || bytes.len() as u64 > LIMIT {
        return Err(refused("metadata changed while reading"));
    }
    Ok(Some(FileBinding {
        identity: Identity::from_file(before),
        sha256: format!("{:x}", Sha256::digest(bytes)),
    }))
}

fn hash_regular(path: &Path, limit: u64) -> Result<String, StoreError> {
    let metadata = fs::symlink_metadata(path).map_err(io)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(refused("artifact is not a bounded regular file"));
    }
    let mut file = crate::fs_secure::open_read_nofollow_blocking(path).map_err(io)?;
    let before = crate::fs_secure::std_file_identity(&file).map_err(io)?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 32_768];
    let mut total = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(io)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count as u64);
        if total > limit {
            return Err(refused("artifact exceeds its size bound"));
        }
        hash.update(&buffer[..count]);
    }
    if before != crate::fs_secure::std_file_identity(&file).map_err(io)? {
        return Err(refused("artifact changed while hashing"));
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn path_hash(path: &Path) -> Result<String, StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| refused("key path has no parent"))?;
    let canonical = fs::canonicalize(parent).map_err(io)?.join(
        path.file_name()
            .ok_or_else(|| refused("key path has no filename"))?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Ok(format!(
            "{:x}",
            Sha256::digest(canonical.as_os_str().as_bytes())
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let bytes: Vec<u8> = canonical
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect();
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
}

fn refused(message: impl AsRef<str>) -> StoreError {
    StoreError::Migration(format!("migration helper refused: {}", message.as_ref()))
}
fn io(error: std::io::Error) -> StoreError {
    refused(error.to_string())
}

#[cfg(test)]
mod tests;

/// Nonblocking outer operation ownership, independent of the SQLite directory
/// lease released to the child. Call while holding the root lease; never wait
/// for this lock while holding the root. Keep it through actual helper exit.
/// The managed supervisor inherits a duplicate into the exact child; direct
/// operator migration holds this descriptor until its command exits.
#[cfg(unix)]
pub fn lock_supervisor(root: &Path) -> Result<std::fs::File, StoreError> {
    let path = owned_path(root, Path::new("storage-helper-owner.lock"))?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = options
        .open(&path)
        .map_err(|error| refused(format!("open migration supervisor lease: {error}")))?;
    let metadata = file
        .metadata()
        .map_err(|error| refused(format!("inspect migration supervisor lease: {error}")))?;
    if !metadata.is_file() {
        return Err(refused("migration supervisor lease is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(refused(
                "migration supervisor lease has unsafe ownership or permissions",
            ));
        }
    }
    file.try_lock().map_err(|error| {
        refused(format!(
            "another migration supervisor still owns this root: {error}"
        ))
    })?;
    Ok(file)
}

#[cfg(not(unix))]
pub fn lock_supervisor(_root: &Path) -> Result<std::fs::File, StoreError> {
    Err(refused("supervised legacy migration is supported on macOS and Linux; this platform requires an explicitly qualified migration first"))
}
