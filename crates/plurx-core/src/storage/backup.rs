//! Portable native SQLite snapshots. This module carries no replication cut.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::StoreError;
use crate::secrets::{CredentialKey, CREDENTIAL_KEY_FILENAME};
use crate::store::SQLITE_SCHEMA_VERSION;

use super::identity::{LocalIdentity, NODE_ID_FILENAME};
use super::mode::{self, StorageMode};
use super::native::DATABASE_FILENAME;

#[path = "restore_sharing.rs"]
mod restore_sharing;

const BACKUP_BUDGET: Duration = Duration::from_secs(30);
const MANIFEST_LIMIT: u64 = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeBackupManifest {
    pub schema_version: u32,
    pub storage_backend: StorageMode,
    pub instance_id: String,
    pub node_id: String,
    pub storage_generation: String,
    pub built_at_unix_ms: i64,
    pub binary_version: String,
    pub sqlite_schema_version: i64,
    pub image_sha256: String,
    pub image_bytes: u64,
    pub integrity_check: String,
    pub table_counts: BTreeMap<String, u64>,
    pub credential_key_id: String,
}

/// Build on the blocking pool while the runtime retains its directory lease.
/// SQLite's online backup includes committed WAL pages and never copies a live
/// database file blindly. Only a complete fsynced archive becomes visible.
pub fn build_native_backup_artifact(
    source: &Path,
    destination: &Path,
    identity: &LocalIdentity,
    credential_key_source: &Path,
    expected_key_id: &str,
    binary_version: &str,
) -> Result<(PathBuf, NativeBackupManifest), StoreError> {
    if !destination.is_absolute() || !destination.is_dir() {
        return Err(invalid(
            "backup destination must be an existing absolute directory",
        ));
    }
    require_file(source)?;
    let key = CredentialKey::load(credential_key_source)
        .map_err(|error| StoreError::Identity(error.to_string()))?;
    if key.id() != expected_key_id {
        return Err(invalid(
            "native backup key differs from the running store's key",
        ));
    }
    let name = format!(
        "plurx-native-backup-{}-{}",
        chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
        uuid::Uuid::new_v4()
    );
    let staging = destination.join(format!(".{name}.incoming"));
    let published = destination.join(name);
    std::fs::create_dir(&staging)
        .map_err(|error| io("creating backup staging", &staging, error))?;
    #[cfg(unix)]
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| io("protecting backup staging", &staging, error))?;
    let result = (|| {
        let source = Connection::open_with_flags(
            source,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        source.busy_timeout(Duration::from_millis(250))?;
        let image = staging.join(DATABASE_FILENAME);
        let mut target = Connection::open(&image)?;
        target.pragma_update(None, "synchronous", "FULL")?;
        #[cfg(target_os = "macos")]
        target.pragma_update(None, "fullfsync", true)?;
        let started = Instant::now();
        {
            let backup = Backup::new(&source, &mut target)?;
            loop {
                if started.elapsed() >= BACKUP_BUDGET {
                    return Err(invalid(
                        "native online backup exceeded its 30-second budget",
                    ));
                }
                match backup.step(128)? {
                    StepResult::Done => break,
                    StepResult::More => std::thread::yield_now(),
                    StepResult::Busy | StepResult::Locked => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    _ => {
                        return Err(invalid(
                            "native online backup returned an unknown progress state",
                        ))
                    }
                }
            }
        }
        target.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        let facts = image_facts(&target)?;
        if facts.identity != *identity {
            return Err(invalid(
                "native backup snapshot has a different application identity",
            ));
        }
        drop(target);
        super::durability::sync_regular(&image)
            .map_err(|error| io("syncing native backup image", &image, error))?;
        let copied_key = staging.join(CREDENTIAL_KEY_FILENAME);
        copy_private_file(credential_key_source, &copied_key)?;
        let copied = CredentialKey::load(&copied_key)
            .map_err(|error| StoreError::Identity(error.to_string()))?;
        if copied.id() != key.id()
            || file_sha256(credential_key_source)? != file_sha256(&copied_key)?
        {
            return Err(invalid(
                "native backup credential key changed while copying",
            ));
        }
        write_private_file(
            &staging.join(NODE_ID_FILENAME),
            format!("{}\n", identity.node_id).as_bytes(),
        )?;
        let manifest = NativeBackupManifest {
            schema_version: 1,
            storage_backend: StorageMode::Sqlite,
            instance_id: identity.instance_id.clone(),
            node_id: identity.node_id.clone(),
            storage_generation: facts.generation,
            built_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_millis()).ok())
                .ok_or_else(|| invalid("native backup clock is invalid"))?,
            binary_version: binary_version.to_owned(),
            sqlite_schema_version: facts.schema_version,
            image_sha256: file_sha256(&image)?,
            image_bytes: std::fs::metadata(&image)
                .map_err(|error| io("reading backup size", &image, error))?
                .len(),
            integrity_check: "ok".to_owned(),
            table_counts: facts.table_counts,
            credential_key_id: copied.id().to_owned(),
        };
        let mut encoded = serde_json::to_vec_pretty(&manifest)
            .map_err(|error| invalid(format!("encoding native backup manifest: {error}")))?;
        encoded.push(b'\n');
        write_private_file(&staging.join("manifest.json"), &encoded)?;
        let mut sums = String::new();
        for name in [
            "manifest.json",
            DATABASE_FILENAME,
            NODE_ID_FILENAME,
            CREDENTIAL_KEY_FILENAME,
        ] {
            sums.push_str(&format!("{}  {name}\n", file_sha256(&staging.join(name))?));
        }
        write_private_file(&staging.join("SHA256SUMS"), sums.as_bytes())?;
        mode::sync_directory(&staging)?;
        super::durability::rename(&staging, &published)
            .map_err(|error| io("publishing native backup", &published, error))?;
        mode::sync_directory(destination)?;
        Ok((published.clone(), manifest))
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&staging);
    }
    result
}

/// Verify without migrating or writing the image. Native startup separately
/// performs the complete sealed-material census before it may serve a restore.
pub fn verify_native_backup_artifact(archive: &Path) -> Result<NativeBackupManifest, StoreError> {
    let path = archive.join("manifest.json");
    require_file(&path)?;
    let mut bytes = Vec::new();
    File::open(&path)
        .and_then(|file| file.take(MANIFEST_LIMIT + 1).read_to_end(&mut bytes))
        .map_err(|error| io("reading native backup manifest", &path, error))?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(invalid("native backup manifest exceeded its size limit"));
    }
    let manifest: NativeBackupManifest = serde_json::from_slice(&bytes)
        .map_err(|error| invalid(format!("decoding native backup manifest: {error}")))?;
    if manifest.schema_version != 1
        || manifest.storage_backend != StorageMode::Sqlite
        || manifest.built_at_unix_ms <= 0
        || manifest.integrity_check != "ok"
        || manifest.sqlite_schema_version > SQLITE_SCHEMA_VERSION
    {
        return Err(invalid("unknown native backup format, backend or schema"));
    }
    let sums_path = archive.join("SHA256SUMS");
    require_file(&sums_path)?;
    if std::fs::metadata(&sums_path)
        .map_err(|error| io("checking native hash inventory", &sums_path, error))?
        .len()
        > 1024
    {
        return Err(invalid(
            "native backup checksum inventory exceeded its bound",
        ));
    }
    let sums = std::fs::read_to_string(&sums_path)
        .map_err(|error| io("reading native backup hashes", &sums_path, error))?;
    let mut expected = String::new();
    for name in [
        "manifest.json",
        DATABASE_FILENAME,
        NODE_ID_FILENAME,
        CREDENTIAL_KEY_FILENAME,
    ] {
        let path = archive.join(name);
        require_file(&path)?;
        expected.push_str(&format!("{}  {name}\n", file_sha256(&path)?));
    }
    if sums != expected {
        return Err(invalid("native backup checksum inventory differs"));
    }
    let image = archive.join(DATABASE_FILENAME);
    if file_sha256(&image)? != manifest.image_sha256
        || std::fs::metadata(&image)
            .map_err(|error| io("reading backup image size", &image, error))?
            .len()
            != manifest.image_bytes
    {
        return Err(invalid("native backup image hash or size differs"));
    }
    let connection = Connection::open_with_flags(
        &image,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let facts = image_facts(&connection)?;
    if facts.schema_version != manifest.sqlite_schema_version
        || facts.generation != manifest.storage_generation
        || facts.identity.instance_id != manifest.instance_id
        || facts.identity.node_id != manifest.node_id
        || facts.table_counts != manifest.table_counts
    {
        return Err(invalid(
            "native backup manifest does not describe its image",
        ));
    }
    let node = std::fs::read_to_string(archive.join(NODE_ID_FILENAME))
        .map_err(|error| invalid(format!("reading native backup node identity: {error}")))?;
    if node.trim() != manifest.node_id {
        return Err(invalid("native backup node identity differs"));
    }
    let key = CredentialKey::load(&archive.join(CREDENTIAL_KEY_FILENAME))
        .map_err(|error| StoreError::Identity(error.to_string()))?;
    if key.id() != manifest.credential_key_id {
        return Err(invalid("native backup credential key differs"));
    }
    Ok(manifest)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRestoreReport {
    pub instance_id: String,
    pub node_id: String,
    pub source_built_at_unix_ms: i64,
    pub storage_generation: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RestorePhase {
    Staging,
    Prepared,
    DatabaseCopied,
    ManifestPublished,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RestoreJournal {
    version: u32,
    operation: String,
    phase: RestorePhase,
    source: NativeBackupManifest,
    staging: String,
    credential_key_target: PathBuf,
    key_existed_before_restore: bool,
    generation: Option<String>,
    target_fingerprint: Option<String>,
}

/// Offline fresh-root restore, or explicit resumption of that exact owned
/// restore. Unknown files, changed archives and divergent native data refuse
/// without replacing any application state.
pub async fn restore_native_backup_artifact(
    archive: &Path,
    data_dir: &Path,
    credential_key_override: Option<&Path>,
) -> Result<NativeRestoreReport, StoreError> {
    let manifest = verify_native_backup_artifact(archive)?;
    let directory_lock = super::native::NativeDirectoryLock::try_acquire(data_dir)?;
    let root = directory_lock.data_dir().to_path_buf();
    let default_key = root.join(CREDENTIAL_KEY_FILENAME);
    let requested_key = credential_key_override.unwrap_or(&default_key);
    let parent = requested_key
        .parent()
        .filter(|path| path.is_dir())
        .ok_or_else(|| invalid("restore credential-key parent must exist"))?;
    let key_target = std::fs::canonicalize(parent)
        .map_err(|error| io("resolving key parent", parent, error))?
        .join(
            requested_key
                .file_name()
                .ok_or_else(|| invalid("restore credential-key filename is absent"))?,
        );
    for name in [
        DATABASE_FILENAME,
        NODE_ID_FILENAME,
        mode::MANIFEST_FILENAME,
        mode::TRANSITION_FILENAME,
        mode::ADOPTION_FILENAME,
        "plurx.toml",
        super::native::DIRECTORY_LOCK_FILENAME,
    ] {
        if key_target == root.join(name) {
            return Err(invalid("restore credential key collides with native state"));
        }
    }
    let journal_path = root.join(mode::TRANSITION_FILENAME);
    let mut journal = match mode::read_record::<RestoreJournal>(&journal_path)? {
        Some(journal) => {
            if journal.version != 1
                || journal.operation != "restore_native"
                || journal.source != manifest
                || journal.credential_key_target != key_target
            {
                return Err(invalid(
                    "pending restore belongs to a different archive, operation or key destination",
                ));
            }
            journal
        }
        None => {
            for entry in std::fs::read_dir(&root)
                .map_err(|error| io("reading restore root", &root, error))?
            {
                let entry = entry.map_err(|error| io("reading restore entry", &root, error))?;
                if entry.file_name() != super::native::DIRECTORY_LOCK_FILENAME
                    && entry.file_name() != "plurx.toml"
                {
                    return Err(invalid(
                        "native restore requires an empty/config-only data directory",
                    ));
                }
            }
            let journal = RestoreJournal {
                version: 1,
                operation: "restore_native".into(),
                phase: RestorePhase::Staging,
                source: manifest.clone(),
                staging: format!(".native-restore-{}", uuid::Uuid::new_v4()),
                credential_key_target: key_target.clone(),
                key_existed_before_restore: key_target.exists(),
                generation: None,
                target_fingerprint: None,
            };
            mode::publish_record(&journal_path, &journal)?;
            journal
        }
    };
    if !journal
        .staging
        .strip_prefix(".native-restore-")
        .is_some_and(|suffix| uuid::Uuid::parse_str(suffix).is_ok())
    {
        return Err(invalid("pending restore has an invalid staging owner"));
    }
    let staging = mode::owned_path(&root, Path::new(&journal.staging))?;
    validate_restore_root(&root, &journal)?;
    if !staging.exists() {
        std::fs::create_dir(&staging)
            .map_err(|error| io("creating restore staging", &staging, error))?;
    }
    #[cfg(unix)]
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| io("protecting restore staging", &staging, error))?;
    validate_restore_staging(&staging)?;
    if journal.phase == RestorePhase::Staging {
        for name in [
            "manifest.json",
            "SHA256SUMS",
            DATABASE_FILENAME,
            NODE_ID_FILENAME,
            CREDENTIAL_KEY_FILENAME,
        ] {
            let source = archive.join(name);
            let target = staging.join(name);
            if target.exists() && file_sha256(&target)? != file_sha256(&source)? {
                // Only this journal's unpublished staging file is repairable.
                require_file(&target)?;
                std::fs::remove_file(&target)
                    .map_err(|error| io("replacing owned staging file", &target, error))?;
            }
            if !target.exists() {
                copy_private_file(&source, &target)?;
            }
        }
        if verify_native_backup_artifact(&staging)? != manifest {
            return Err(invalid("restore archive changed during staging"));
        }
        let generation = mode::begin_native_adoption(&root)?;
        let mut image = Connection::open(staging.join(DATABASE_FILENAME))?;
        image.pragma_update(None, "synchronous", "FULL")?;
        #[cfg(target_os = "macos")]
        image.pragma_update(None, "fullfsync", true)?;
        let key = CredentialKey::load(&staging.join(CREDENTIAL_KEY_FILENAME))
            .map_err(|error| invalid(format!("loading retained restore key: {error}")))?;
        let transaction = image.transaction()?;
        restore_sharing::fence(&transaction, &key)?;
        let changed=transaction.execute("UPDATE storage_identity SET generation=?1 WHERE singleton=1 AND instance_id=?2 AND node_id=?3 AND generation=?4",
            rusqlite::params![generation,manifest.instance_id,manifest.node_id,manifest.storage_generation])?;
        if changed != 1 {
            return Err(invalid(
                "restore source generation was not the verified generation",
            ));
        }
        transaction.commit()?;
        image.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        let fingerprint = restore_content_fingerprint(&image)?;
        drop(image);
        super::durability::sync_regular(&staging.join(DATABASE_FILENAME))
            .map_err(|error| io("syncing prepared native restore", &staging, error))?;
        mode::sync_directory(&staging)?;
        mode::sync_directory(&root)?;
        journal.generation = Some(generation);
        journal.target_fingerprint = Some(fingerprint);
        journal.phase = RestorePhase::Prepared;
        mode::publish_record(&journal_path, &journal)?;
    }
    let generation = journal
        .generation
        .as_deref()
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
        .ok_or_else(|| invalid("restore has no qualified target generation"))?
        .to_owned();
    let expected = journal
        .target_fingerprint
        .as_deref()
        .ok_or_else(|| invalid("restore has no target content binding"))?;
    let active = mode::StorageManifest::read(&root)?;
    if let Some(active) = &active {
        if active.mode != StorageMode::Sqlite
            || active.instance_id != manifest.instance_id
            || active.node_id != manifest.node_id
            || active.generation != generation
        {
            return Err(invalid(
                "another native authority was accepted during restore",
            ));
        }
        active.validate_sqlite(&root)?;
    }
    if active.is_none() {
        if !journal.key_existed_before_restore {
            copy_owned_restore_file(&staging.join(CREDENTIAL_KEY_FILENAME), &key_target)?;
        }
        if let Some(parent) = key_target.parent() {
            mode::sync_directory(parent)?;
        }
        copy_owned_restore_file(
            &staging.join(NODE_ID_FILENAME),
            &root.join(NODE_ID_FILENAME),
        )?;
        if journal.phase == RestorePhase::Prepared {
            copy_owned_restore_file(
                &staging.join(DATABASE_FILENAME),
                &root.join(DATABASE_FILENAME),
            )?;
        } else if !root.join(DATABASE_FILENAME).is_file() {
            return Err(invalid("published restore database disappeared"));
        }
        mode::sync_directory(&root)?;
    }
    let key = CredentialKey::load(&key_target)
        .map_err(|error| StoreError::Identity(error.to_string()))?;
    if key.id() != manifest.credential_key_id
        || file_sha256(&key_target)? != file_sha256(&archive.join(CREDENTIAL_KEY_FILENAME))?
    {
        return Err(invalid(
            "restore credential key differs; refusing to replace it",
        ));
    }
    let node = std::fs::read_to_string(root.join(NODE_ID_FILENAME))
        .map_err(|error| invalid(format!("reading restored node: {error}")))?;
    if node.trim() != manifest.node_id {
        return Err(invalid("restored node identity differs"));
    }
    let image = Connection::open_with_flags(
        root.join(DATABASE_FILENAME),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let facts = image_facts(&image)?;
    if facts.generation != generation
        || facts.identity.instance_id != manifest.instance_id
        || facts.identity.node_id != manifest.node_id
        || restore_content_fingerprint(&image)? != expected
    {
        return Err(invalid(
            "restored database diverged; refusing to overwrite accepted native writes",
        ));
    }
    drop(image);
    if active.is_none() {
        journal.phase = RestorePhase::DatabaseCopied;
        mode::publish_record(&journal_path, &journal)?;
        restore_test_crash(RestorePhase::DatabaseCopied)?;
    }
    let identity = LocalIdentity {
        instance_id: manifest.instance_id,
        node_id: manifest.node_id,
    };
    // Complete sealed-material census is required on both new publication and
    // an already-published manifest. Neither branch reimports accepted data.
    let parts = super::native::open_parts_at(&root, &key_target).await?;
    if parts.identity != identity {
        return Err(invalid(
            "restored identity differs after startup validation",
        ));
    }
    mode::finish_native_adoption(&root, &identity, &generation)?;
    drop(parts);
    journal.phase = RestorePhase::ManifestPublished;
    mode::publish_record(&journal_path, &journal)?;
    restore_test_crash(RestorePhase::ManifestPublished)?;
    std::fs::remove_file(&journal_path)
        .map_err(|error| io("completing restore transition", &journal_path, error))?;
    mode::sync_directory(&root)?;
    let handle =
        super::native::open_native_store_at_locked(&root, &key_target, directory_lock).await?;
    drop(handle);
    if validate_restore_staging(&staging).is_ok() {
        if let Err(error) = std::fs::remove_dir_all(&staging) {
            tracing::warn!(%error,"completed native restore staging cleanup is pending");
        }
    } else {
        tracing::warn!("completed restore staging contains an unknown file; preserving it");
    }
    mode::sync_directory(&root)?;
    Ok(NativeRestoreReport {
        instance_id: identity.instance_id,
        node_id: identity.node_id,
        source_built_at_unix_ms: manifest.built_at_unix_ms,
        storage_generation: generation,
    })
}

fn validate_restore_root(root: &Path, journal: &RestoreJournal) -> Result<(), StoreError> {
    for entry in std::fs::read_dir(root).map_err(|error| io("reading restore root", root, error))? {
        let entry = entry.map_err(|error| io("reading restore entry", root, error))?;
        let name = entry.file_name();
        let ordinary = [
            super::native::DIRECTORY_LOCK_FILENAME,
            "plurx.toml",
            mode::TRANSITION_FILENAME,
            mode::ADOPTION_FILENAME,
            mode::MANIFEST_FILENAME,
            DATABASE_FILENAME,
            "plurx.db-wal",
            "plurx.db-shm",
            NODE_ID_FILENAME,
        ];
        let key_is_owned = journal.credential_key_target.parent() == Some(root)
            && journal.credential_key_target.file_name() == Some(name.as_os_str());
        if !ordinary.iter().any(|allowed| name == *allowed)
            && name != journal.staging.as_str()
            && !key_is_owned
        {
            return Err(invalid(
                "pending restore root contains an unknown file; preserving it",
            ));
        }
        if entry
            .file_type()
            .map_err(|error| io("reading restore file type", &entry.path(), error))?
            .is_symlink()
        {
            return Err(invalid("pending restore root contains a symbolic path"));
        }
    }
    Ok(())
}

fn validate_restore_staging(staging: &Path) -> Result<(), StoreError> {
    for entry in
        std::fs::read_dir(staging).map_err(|error| io("reading restore staging", staging, error))?
    {
        let entry = entry.map_err(|error| io("reading staging entry", staging, error))?;
        if ![
            "manifest.json",
            "SHA256SUMS",
            DATABASE_FILENAME,
            NODE_ID_FILENAME,
            CREDENTIAL_KEY_FILENAME,
        ]
        .iter()
        .any(|name| entry.file_name() == *name)
            || !entry
                .file_type()
                .map_err(|error| io("reading staging type", &entry.path(), error))?
                .is_file()
        {
            return Err(invalid(
                "restore staging contains an unknown or nonregular file",
            ));
        }
    }
    Ok(())
}

/// Resume only a known complete file or an exact prefix of this restore's
/// prepared bytes. Arbitrary existing native content is never truncated.
fn copy_owned_restore_file(source: &Path, target: &Path) -> Result<(), StoreError> {
    if !target.exists() {
        return copy_private_file(source, target);
    }
    require_file(target)?;
    if file_sha256(source)? == file_sha256(target)? {
        return Ok(());
    }
    let source_len = std::fs::metadata(source)
        .map_err(|error| io("reading prepared size", source, error))?
        .len();
    let target_len = std::fs::metadata(target)
        .map_err(|error| io("reading restored size", target, error))?
        .len();
    if target_len >= source_len {
        return Err(invalid("restore target contains different accepted bytes"));
    }
    let mut original =
        File::open(source).map_err(|error| io("opening prepared image", source, error))?;
    let mut partial =
        File::open(target).map_err(|error| io("opening partial image", target, error))?;
    let mut left = target_len;
    let mut expected = [0_u8; 64 * 1024];
    let mut actual = [0_u8; 64 * 1024];
    while left > 0 {
        let count = usize::try_from(left.min(expected.len() as u64))
            .map_err(|_| invalid("partial restore bound"))?;
        original
            .read_exact(&mut expected[..count])
            .map_err(|error| io("reading prepared prefix", source, error))?;
        partial
            .read_exact(&mut actual[..count])
            .map_err(|error| io("reading partial prefix", target, error))?;
        if expected[..count] != actual[..count] {
            return Err(invalid("partial restore differs from the prepared prefix"));
        }
        left -= count as u64;
    }
    drop(partial);
    let mut output = OpenOptions::new()
        .append(true)
        .open(target)
        .map_err(|error| io("resuming partial restore", target, error))?;
    std::io::copy(&mut original, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|error| io("finishing partial restore", target, error))?;
    Ok(())
}

pub(crate) fn restore_content_fingerprint(connection: &Connection) -> Result<String, StoreError> {
    let tables=connection.prepare("SELECT name,sql FROM sqlite_master WHERE type='table' AND name!='sharing_local_census' ORDER BY name")?
        .query_map([],|row|Ok((row.get::<_,String>(0)?,row.get::<_,Option<String>>(1)?)))?.collect::<Result<Vec<_>,_>>()?;
    let mut all = Sha256::new();
    for (name, sql) in tables {
        all.update((name.len() as u64).to_be_bytes());
        all.update(name.as_bytes());
        all.update(sql.unwrap_or_default().as_bytes());
        let quoted = name.replace('"', "\"\"");
        let mut statement = connection.prepare(&format!("SELECT * FROM \"{quoted}\""))?;
        let columns = statement.column_count();
        let mut rows = statement.query([])?;
        let mut digests = Vec::new();
        while let Some(row) = rows.next()? {
            let mut digest = Sha256::new();
            for column in 0..columns {
                match row.get_ref(column)? {
                    rusqlite::types::ValueRef::Null => digest.update([0]),
                    rusqlite::types::ValueRef::Integer(value) => {
                        digest.update([1]);
                        digest.update(value.to_be_bytes());
                    }
                    rusqlite::types::ValueRef::Real(value) => {
                        digest.update([2]);
                        digest.update(value.to_bits().to_be_bytes());
                    }
                    rusqlite::types::ValueRef::Text(value) => {
                        digest.update([3]);
                        digest.update((value.len() as u64).to_be_bytes());
                        digest.update(value);
                    }
                    rusqlite::types::ValueRef::Blob(value) => {
                        digest.update([4]);
                        digest.update((value.len() as u64).to_be_bytes());
                        digest.update(value);
                    }
                }
            }
            digests.push(format!("{:x}", digest.finalize()));
        }
        digests.sort_unstable();
        all.update((digests.len() as u64).to_be_bytes());
        for digest in digests {
            all.update(digest.as_bytes());
        }
    }
    Ok(format!("{:x}", all.finalize()))
}

#[cfg(test)]
thread_local! { static RESTORE_CRASH:std::cell::Cell<Option<RestorePhase>>=const {std::cell::Cell::new(None)}; }
fn restore_test_crash(phase: RestorePhase) -> Result<(), StoreError> {
    #[cfg(test)]
    if RESTORE_CRASH.with(|point| point.get() == Some(phase)) {
        RESTORE_CRASH.with(|point| point.set(None));
        return Err(invalid("injected native restore interruption"));
    }
    let _ = phase;
    Ok(())
}

struct ImageFacts {
    schema_version: i64,
    identity: LocalIdentity,
    generation: String,
    table_counts: BTreeMap<String, u64>,
}

fn image_facts(connection: &Connection) -> Result<ImageFacts, StoreError> {
    let integrity = connection
        .prepare("PRAGMA integrity_check")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    if integrity != ["ok"] {
        return Err(invalid("native backup image failed integrity_check"));
    }
    if connection
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(invalid("native backup image has dangling foreign keys"));
    }
    let schema_version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !(1..=SQLITE_SCHEMA_VERSION).contains(&schema_version) {
        return Err(invalid("native backup image has an unknown schema"));
    }
    let replicated: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE name IN ('_metadata','cluster_meta')",
        [],
        |row| row.get(0),
    )?;
    if replicated != 0 {
        return Err(invalid(
            "replicated image cannot be declared a native backup",
        ));
    }
    let (instance_id, node_id, generation): (String, String, String) = connection.query_row(
        "SELECT instance_id,node_id,generation FROM storage_identity WHERE singleton=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let instance: Option<String> = connection
        .query_row(
            "SELECT value FROM settings WHERE key='instance.id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if instance.as_deref() != Some(instance_id.as_str())
        || uuid::Uuid::parse_str(&generation).is_err()
        || instance_id.is_empty()
        || node_id.is_empty()
    {
        return Err(invalid(
            "native backup identity/generation is not bound to its database",
        ));
    }
    let tables = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")?
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let mut table_counts = BTreeMap::new();
    for table in tables {
        let quoted = table.replace('"', "\"\"");
        let count: i64 =
            connection.query_row(&format!("SELECT count(*) FROM \"{quoted}\""), [], |row| {
                row.get(0)
            })?;
        table_counts.insert(
            table,
            u64::try_from(count).map_err(|_| invalid("negative native backup row count"))?,
        );
    }
    Ok(ImageFacts {
        schema_version,
        identity: LocalIdentity {
            instance_id,
            node_id,
        },
        generation,
        table_counts,
    })
}

fn require_file(path: &Path) -> Result<(), StoreError> {
    let metadata =
        std::fs::symlink_metadata(path).map_err(|error| io("inspecting", path, error))?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(invalid(format!(
            "{} is not a regular owned file",
            path.display()
        )));
    }
    Ok(())
}

fn copy_private_file(source: &Path, target: &Path) -> Result<(), StoreError> {
    require_file(source)?;
    let mut input = File::open(source).map_err(|error| io("opening", source, error))?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut output = options
        .open(target)
        .map_err(|error| io("creating", target, error))?;
    std::io::copy(&mut input, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|error| io("copying", target, error))?;
    Ok(())
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(path)
        .map_err(|error| io("creating", path, error))?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|error| io("writing", path, error))
}

fn file_sha256(path: &Path) -> Result<String, StoreError> {
    let mut input = File::open(path).map_err(|error| io("hashing", path, error))?;
    let mut digest = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let read = input
            .read(&mut bytes)
            .map_err(|error| io("hashing", path, error))?;
        if read == 0 {
            break;
        }
        digest.update(&bytes[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn invalid(reason: impl std::fmt::Display) -> StoreError {
    StoreError::Migration(format!("native backup refused: {reason}"))
}
fn io(action: &str, path: &Path, error: impl std::fmt::Display) -> StoreError {
    invalid(format!("{action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn archive_fixture() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, LocalIdentity) {
        let root = tempfile::tempdir().expect("native storage fixture");
        let destination = tempfile::tempdir().expect("native storage fixture");
        let key = root.path().join(CREDENTIAL_KEY_FILENAME);
        let handle = crate::storage::open_native_store_at(root.path(), &key)
            .await
            .expect("native storage fixture");
        let identity = handle.identity.clone();
        let (archive, _) = build_native_backup_artifact(
            &root.path().join(DATABASE_FILENAME),
            destination.path(),
            &identity,
            &key,
            handle.credential_key.id(),
            "fixture",
        )
        .expect("native storage fixture");
        drop(handle);
        (root, destination, archive, identity)
    }

    #[tokio::test]
    async fn native_restore_fences_pre_revocation_sharing_and_preserves_sealed_purpose_history() {
        let source = tempfile::tempdir().expect("native storage fixture");
        let backups = tempfile::tempdir().expect("native storage fixture");
        let key_path = source.path().join(CREDENTIAL_KEY_FILENAME);
        let handle = crate::storage::open_native_store_at(source.path(), &key_path)
            .await
            .expect("native storage fixture");
        let authority = handle
            .sqlite
            .prepare_native_source_before_serving(&handle.identity.node_id, &handle.credential_key)
            .await
            .expect("native storage fixture");
        authority.revoke();
        let live = Connection::open(source.path().join(DATABASE_FILENAME))
            .expect("native storage fixture");
        live.execute_batch("INSERT INTO settings(key,value,updated_at) VALUES('sharing_enabled','true',1);
            INSERT INTO sharing_invitations(id,token_hash,library_ids_json,created_at_ms,expires_at_ms,state) VALUES('restore-invitation','invitation-hash','[]',1,9223372036854775807,'open');
            INSERT INTO sharing_exports VALUES('restore-grant','restore-invitation','recipient','Recipient','old-credential-hash',1,1,1,1,'active',2,1,1);").expect("native storage fixture");
        let custody = "{\"version\":1,\"sealed\":false,\"slots\":[],\"highwater\":[]}";
        let incarnation = uuid::Uuid::new_v4().to_string();
        live.execute(
            "INSERT INTO sharing_ingress_custody VALUES('source',?1,?2,?3,1)",
            rusqlite::params![incarnation, "a".repeat(64), custody],
        )
        .expect("native storage fixture");
        let mut statement=live.prepare("SELECT revision_envelope FROM sharing_catalogue_keys UNION ALL SELECT locator_envelope FROM sharing_file_locator_keys").expect("native storage fixture");
        let envelopes = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("native storage fixture")
            .collect::<Result<Vec<_>, _>>()
            .expect("native storage fixture");
        drop(statement);
        let (archive, _) = build_native_backup_artifact(
            &source.path().join(DATABASE_FILENAME),
            backups.path(),
            &handle.identity,
            &key_path,
            handle.credential_key.id(),
            "restore-revocation-regression",
        )
        .expect("native storage fixture");
        // The backup predates a real revocation. Offline restore cannot know
        // its later revocation state and must retire all old sharing authority.
        live.execute(
            "UPDATE sharing_exports SET state='revoked' WHERE id='restore-grant'",
            [],
        )
        .expect("native storage fixture");
        drop(live);
        drop(handle);
        let restored_root = tempfile::tempdir().expect("native storage fixture");
        restore_native_backup_artifact(&archive, restored_root.path(), None)
            .await
            .expect("native storage fixture");
        let restored = Connection::open(restored_root.path().join(DATABASE_FILENAME))
            .expect("native storage fixture");
        assert_eq!(
            restored
                .query_row(
                    "SELECT value FROM settings WHERE key='sharing_enabled'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .expect("native storage fixture"),
            "false"
        );
        assert_eq!(
            restored
                .query_row(
                    "SELECT state FROM sharing_exports WHERE id='restore-grant'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .expect("native storage fixture"),
            "revoked"
        );
        assert_eq!(
            restored
                .query_row(
                    "SELECT state FROM sharing_invitations WHERE id='restore-invitation'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .expect("native storage fixture"),
            "cancelled"
        );
        assert_eq!(
            restored
                .query_row("SELECT count(*) FROM sharing_identity", [], |row| row
                    .get::<_, i64>(0))
                .expect("native storage fixture"),
            0
        );
        assert_eq!(
            restored
                .query_row(
                    "SELECT custody_json FROM sharing_ingress_custody WHERE incarnation_id=?1",
                    [incarnation],
                    |row| row.get::<_, String>(0)
                )
                .expect("native storage fixture"),
            custody,
            "restore cannot fabricate closure or discard accepted-driver custody"
        );
        let mut archived = restored
            .prepare("SELECT envelope FROM sharing_purpose_key_archive ORDER BY purpose")
            .expect("native storage fixture")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("native storage fixture")
            .collect::<Result<Vec<_>, _>>()
            .expect("native storage fixture");
        let mut envelopes = envelopes;
        archived.sort();
        envelopes.sort();
        assert_eq!(
            archived, envelopes,
            "old purpose ciphertext stays under its original authenticated identity"
        );
        let key = CredentialKey::load(&restored_root.path().join(CREDENTIAL_KEY_FILENAME))
            .expect("native storage fixture");
        crate::store::sharing_purpose_keys::connection_material(&restored)
            .expect("native storage fixture")
            .expect("native storage fixture")
            .verify(&key)
            .expect("native storage fixture");
        assert_eq!(restored.query_row("SELECT count(*) FROM sqlite_master WHERE name='sharing_purpose_census_intents'",[],|row|row.get::<_,i64>(0)).expect("native storage fixture"),0,"native restore never fabricates replicated census");
    }

    #[tokio::test]
    async fn native_restore_resumes_after_database_copy_without_replacing_data() {
        let (_source, _backups, archive, identity) = archive_fixture().await;
        let root = tempfile::tempdir().expect("native storage fixture");
        RESTORE_CRASH.with(|point| point.set(Some(RestorePhase::DatabaseCopied)));
        assert!(restore_native_backup_artifact(&archive, root.path(), None)
            .await
            .is_err());
        let journal: RestoreJournal =
            mode::read_record(&root.path().join(mode::TRANSITION_FILENAME))
                .expect("native storage fixture")
                .expect("native storage fixture");
        let generation = journal.generation.expect("native storage fixture");
        let restored = restore_native_backup_artifact(&archive, root.path(), None)
            .await
            .expect("native storage fixture");
        assert_eq!(restored.instance_id, identity.instance_id);
        assert_eq!(restored.storage_generation, generation);
        assert!(!root.path().join(mode::TRANSITION_FILENAME).exists());
    }

    #[tokio::test]
    async fn native_restore_finishes_a_published_manifest_without_reimporting() {
        let (_source, _backups, archive, _identity) = archive_fixture().await;
        let root = tempfile::tempdir().expect("native storage fixture");
        RESTORE_CRASH.with(|point| point.set(Some(RestorePhase::ManifestPublished)));
        assert!(restore_native_backup_artifact(&archive, root.path(), None)
            .await
            .is_err());
        let journal: RestoreJournal =
            mode::read_record(&root.path().join(mode::TRANSITION_FILENAME))
                .expect("native storage fixture")
                .expect("native storage fixture");
        // Once the manifest is accepted, a staging image is not authority.
        // Removing it proves resumption validates the accepted native database.
        std::fs::remove_file(root.path().join(&journal.staging).join(DATABASE_FILENAME))
            .expect("native storage fixture");
        let restored = restore_native_backup_artifact(&archive, root.path(), None)
            .await
            .expect("native storage fixture");
        assert_eq!(Some(restored.storage_generation), journal.generation);
        assert!(!root.path().join(mode::TRANSITION_FILENAME).exists());
    }

    #[tokio::test]
    async fn native_restore_refuses_divergent_accepted_writes_and_unknown_files() {
        let (_source, _backups, archive, _identity) = archive_fixture().await;
        let root = tempfile::tempdir().expect("native storage fixture");
        RESTORE_CRASH.with(|point| point.set(Some(RestorePhase::ManifestPublished)));
        assert!(restore_native_backup_artifact(&archive, root.path(), None)
            .await
            .is_err());
        let connection =
            Connection::open(root.path().join(DATABASE_FILENAME)).expect("native storage fixture");
        connection.execute("INSERT INTO settings(key,value,updated_at) VALUES('restore.accepted','preserve',0)",[]).expect("native storage fixture");
        drop(connection);
        assert!(restore_native_backup_artifact(&archive, root.path(), None)
            .await
            .is_err());
        let connection =
            Connection::open(root.path().join(DATABASE_FILENAME)).expect("native storage fixture");
        let value: String = connection
            .query_row(
                "SELECT value FROM settings WHERE key='restore.accepted'",
                [],
                |row| row.get(0),
            )
            .expect("native storage fixture");
        assert_eq!(value, "preserve");
        drop(connection);
        std::fs::write(root.path().join("operator-notes.txt"), "preserve too")
            .expect("native storage fixture");
        assert!(restore_native_backup_artifact(&archive, root.path(), None)
            .await
            .is_err());
        assert_eq!(
            std::fs::read_to_string(root.path().join("operator-notes.txt"))
                .expect("native storage fixture"),
            "preserve too"
        );
    }

    #[tokio::test]
    async fn native_restore_preserves_current_data_and_refuses_an_existing_database() {
        let root = tempfile::tempdir().expect("native root");
        let destination = tempfile::tempdir().expect("backup destination");
        let restored = tempfile::tempdir().expect("restore root");
        let key_path = root.path().join(CREDENTIAL_KEY_FILENAME);
        let handle = crate::storage::open_native_store_at(root.path(), &key_path)
            .await
            .expect("native boot");
        let (archive, _) = build_native_backup_artifact(
            &root.path().join(DATABASE_FILENAME),
            destination.path(),
            &handle.identity,
            &key_path,
            handle.credential_key.id(),
            "fixture",
        )
        .expect("backup");
        let report = restore_native_backup_artifact(&archive, restored.path(), None)
            .await
            .expect("native restore");
        assert_eq!(report.instance_id, handle.identity.instance_id);
        assert_eq!(report.node_id, handle.identity.node_id);
        assert!(mode::StorageManifest::read(restored.path())
            .expect("native manifest")
            .is_some());
        assert!(!restored.path().join("hiqlite").exists());
        assert!(
            restore_native_backup_artifact(&archive, restored.path(), None)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn native_backup_captures_committed_wal_rows_and_rejects_tampering() {
        let root = tempfile::tempdir().expect("native root");
        let destination = tempfile::tempdir().expect("backup destination");
        let key_path = root.path().join(CREDENTIAL_KEY_FILENAME);
        let handle = crate::storage::open_native_store_at(root.path(), &key_path)
            .await
            .expect("native boot");
        let source = root.path().join(DATABASE_FILENAME);
        let connection = Connection::open(&source).expect("writer");
        connection
            .execute(
                "INSERT INTO settings(key,value,updated_at) VALUES('backup.fixture','kept',0)",
                [],
            )
            .expect("WAL write");
        let (archive, manifest) = build_native_backup_artifact(
            &source,
            destination.path(),
            &handle.identity,
            &key_path,
            handle.credential_key.id(),
            "fixture",
        )
        .expect("backup");
        assert_eq!(
            verify_native_backup_artifact(&archive).expect("verify"),
            manifest
        );
        let snapshot = Connection::open(archive.join(DATABASE_FILENAME)).expect("snapshot");
        let value: String = snapshot
            .query_row(
                "SELECT value FROM settings WHERE key='backup.fixture'",
                [],
                |row| row.get(0),
            )
            .expect("committed WAL row");
        assert_eq!(value, "kept");
        drop(snapshot);
        std::fs::write(archive.join(NODE_ID_FILENAME), "wrong\n").expect("tamper");
        assert!(verify_native_backup_artifact(&archive).is_err());
    }
}
