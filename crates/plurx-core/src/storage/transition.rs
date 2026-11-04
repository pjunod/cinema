//! Crash-safe, backend-neutral publication of a verified native generation.
//!
//! The caller holds the data-directory lease for prepare, every rename and
//! recovery. The converter/initializer verifies data and keys first and closes
//! the incoming database. This module knows only files, identities and storage
//! generations; it cannot construct a replication client or a quorum proof.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::mode::{
    owned_path, publish_record, read_record, sync_directory, StorageManifest, StorageMode,
    MANIFEST_FILENAME, TRANSITION_FILENAME,
};
use crate::error::StoreError;

const LEGACY_DIRECTORY: &str = "hiqlite";
const LEGACY_HANDOFF: &str = "hiqlite-activated.json";
const OPERATION: &str = "convert_to_sqlite";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedArtifact {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeHandoff {
    pub version: u32,
    pub operation: String,
    pub manifest: StorageManifest,
    pub incoming_database: PathBuf,
    pub retired_directory: PathBuf,
    pub target_sha256: String,
    pub receipt_path: PathBuf,
    pub receipt_sha256: String,
    pub source_image: Option<VerifiedArtifact>,
    previous_manifest: Option<StorageManifest>,
    had_legacy_database: bool,
    had_legacy_wal: bool,
    had_legacy_shm: bool,
}

pub fn prepare_native_handoff(
    root: &Path,
    manifest: StorageManifest,
    incoming_database: PathBuf,
    retired_directory: PathBuf,
    receipt_path: PathBuf,
) -> Result<NativeHandoff, StoreError> {
    prepare(
        root,
        manifest,
        incoming_database,
        retired_directory,
        receipt_path,
        None,
    )
}

pub fn prepare_native_handoff_with_source(
    root: &Path,
    manifest: StorageManifest,
    incoming_database: PathBuf,
    retired_directory: PathBuf,
    receipt_path: PathBuf,
    source_image: VerifiedArtifact,
) -> Result<NativeHandoff, StoreError> {
    prepare(
        root,
        manifest,
        incoming_database,
        retired_directory,
        receipt_path,
        Some(source_image),
    )
}

fn prepare(
    root: &Path,
    manifest: StorageManifest,
    incoming_database: PathBuf,
    retired_directory: PathBuf,
    receipt_path: PathBuf,
    source_image: Option<VerifiedArtifact>,
) -> Result<NativeHandoff, StoreError> {
    if read_record::<serde_json::Value>(&owned_path(root, Path::new(TRANSITION_FILENAME))?)?
        .is_some()
    {
        return Err(refused(
            "a pending storage handoff must recover before another is prepared",
        ));
    }
    let previous_manifest: Option<StorageManifest> = read_record(&root.join(MANIFEST_FILENAME))?;
    if let Some(previous) = &previous_manifest {
        previous.validate(root)?;
        if previous.mode != StorageMode::Cluster
            || previous.instance_id != manifest.instance_id
            || previous.node_id != manifest.node_id
        {
            return Err(refused(
                "existing storage authority is not the conversion source",
            ));
        }
    }
    let incoming = owned_path(root, &incoming_database)?;
    let receipt = owned_path(root, &receipt_path)?;
    let canonical = owned_path(root, Path::new("plurx.db"))?;
    let legacy_wal = owned_path(root, Path::new("plurx.db-wal"))?;
    let legacy_shm = owned_path(root, Path::new("plurx.db-shm"))?;
    if path_present(&owned_path(root, Path::new("plurx.db-journal"))?)? {
        return Err(refused(
            "a legacy rollback journal must be recovered before conversion",
        ));
    }
    let handoff = NativeHandoff {
        version: 1,
        operation: OPERATION.to_owned(),
        manifest,
        incoming_database,
        retired_directory,
        target_sha256: hash_regular(&incoming)?,
        receipt_path,
        receipt_sha256: hash_regular(&receipt)?,
        source_image,
        previous_manifest,
        had_legacy_database: regular_present(&canonical)?,
        had_legacy_wal: regular_present(&legacy_wal)?,
        had_legacy_shm: regular_present(&legacy_shm)?,
    };
    handoff.validate_shape(root)?;
    handoff.validate_artifacts(root)?;
    validate_breadcrumb(root, &handoff.manifest)?;
    let source = owned_path(root, Path::new(LEGACY_DIRECTORY))?;
    if !directory_present(&source)? {
        return Err(refused("the old storage source is absent"));
    }
    let retired = owned_path(root, &handoff.retired_directory)?;
    if path_present(&retired.join(LEGACY_DIRECTORY))?
        || path_present(&retired.join("legacy-plurx.db"))?
        || path_present(&retired.join("legacy-plurx.db-wal"))?
        || path_present(&retired.join("legacy-plurx.db-shm"))?
    {
        return Err(refused("retired storage already contains another handoff"));
    }
    if handoff.had_legacy_database {
        validate_old_database(&canonical, &handoff.manifest)?;
    } else if handoff.had_legacy_wal || handoff.had_legacy_shm {
        return Err(refused(
            "orphaned legacy sidecars have no identified database",
        ));
    }
    sync_regular(&incoming)?;
    sync_regular(&receipt)?;
    if let Some(source) = &handoff.source_image {
        sync_regular(&owned_path(root, &source.path)?)?;
    }
    sync_parent_chain(root, &handoff.incoming_database)?;
    publish_record(&root.join(TRANSITION_FILENAME), &handoff)?;
    Ok(handoff)
}

/// Resume a prepared conversion. The manifest is the last authority write.
/// No backup or initial file hash is used to overwrite an already published
/// generation: it may have accepted native writes since the last crash.
pub fn finish_native_handoff(
    root: &Path,
    handoff: &NativeHandoff,
) -> Result<StorageManifest, StoreError> {
    handoff.validate_shape(root)?;
    let pending: Option<NativeHandoff> = read_record(&root.join(TRANSITION_FILENAME))?;
    if pending.as_ref().is_some_and(|pending| pending != handoff) {
        return Err(refused("supplied handoff differs from its durable intent"));
    }
    let active: Option<StorageManifest> = read_record(&root.join(MANIFEST_FILENAME))?;
    if active.as_ref() == Some(&handoff.manifest) {
        handoff.manifest.validate(root)?;
        handoff.manifest.validate_sqlite(root)?;
        remove_journal(root)?;
        return Ok(handoff.manifest.clone());
    }
    if pending.is_none() {
        return Err(refused("an unpublished handoff has no durable intent"));
    }
    if active != handoff.previous_manifest {
        return Err(refused(
            "storage authority changed after handoff preparation",
        ));
    }
    validate_breadcrumb(root, &handoff.manifest)?;
    handoff.validate_artifacts(root)?;
    let retired = owned_path(root, &handoff.retired_directory)?;
    if !directory_present(&retired)? {
        create_directory_chain(root, &handoff.retired_directory)?;
    }
    move_exactly_one(
        &owned_path(root, Path::new(LEGACY_DIRECTORY))?,
        &owned_path(root, &handoff.retired_directory.join(LEGACY_DIRECTORY))?,
        true,
    )?;
    archive_legacy_database(root, handoff)?;
    for (name, retired_name, existed) in [
        (
            "plurx.db-wal",
            "legacy-plurx.db-wal",
            handoff.had_legacy_wal,
        ),
        (
            "plurx.db-shm",
            "legacy-plurx.db-shm",
            handoff.had_legacy_shm,
        ),
    ] {
        let source = owned_path(root, Path::new(name))?;
        let destination = owned_path(root, &handoff.retired_directory.join(retired_name))?;
        if existed {
            move_exactly_one(&source, &destination, false)?;
        } else if path_present(&source)? || path_present(&destination)? {
            return Err(refused(
                "unrecorded database sidecar appeared during handoff",
            ));
        }
    }
    let incoming = owned_path(root, &handoff.incoming_database)?;
    let canonical = owned_path(root, Path::new("plurx.db"))?;
    match (regular_present(&incoming)?, regular_present(&canonical)?) {
        (true, false) => rename_synced(&incoming, &canonical)?,
        (false, true) => {}
        (true, true) => return Err(refused("both incoming and canonical native targets exist")),
        (false, false) => return Err(refused("the verified native target disappeared")),
    }
    if hash_regular(&canonical)? != handoff.target_sha256 {
        return Err(refused(
            "native target hash changed before authority publication",
        ));
    }
    handoff.manifest.validate_sqlite(root)?;
    sync_regular(&canonical)?;
    handoff.manifest.publish(root)?;
    remove_journal(root)?;
    Ok(handoff.manifest.clone())
}

pub fn recover_native_handoff(root: &Path) -> Result<Option<StorageManifest>, StoreError> {
    let pending: Option<serde_json::Value> =
        read_record(&owned_path(root, Path::new(TRANSITION_FILENAME))?)?;
    let Some(pending) = pending else {
        return Ok(None);
    };
    match pending.get("operation").and_then(serde_json::Value::as_str) {
        Some(OPERATION) => {
            let handoff: NativeHandoff = serde_json::from_value(pending).map_err(|error| refused(format!("invalid native handoff: {error}")))?;
            finish_native_handoff(root, &handoff).map(Some)
        }
        Some("create_cluster") => Err(refused("cluster creation is incomplete; stop the daemon and rerun cluster create with the same configuration to resume")),
        Some("restore_native") => Err(refused("native restore is incomplete; rerun restore with the same archive and target directory to resume")),
        _ => Err(refused("unknown storage transition operation; refusing to choose another authority")),
    }
}

impl NativeHandoff {
    fn validate_shape(&self, root: &Path) -> Result<(), StoreError> {
        if self.version != 1
            || self.operation != OPERATION
            || self.manifest.version != 1
            || self.manifest.mode != StorageMode::Sqlite
            || self.manifest.database != Path::new("plurx.db")
            || self.manifest.schema_version < 1
            || self.manifest.instance_id.trim().is_empty()
            || self.manifest.node_id.trim().is_empty()
            || self.manifest.instance_id.len() > 256
            || self.manifest.node_id.len() > 256
            || uuid::Uuid::parse_str(&self.manifest.generation).is_err()
            || !valid_hash(&self.target_sha256)
            || !valid_hash(&self.receipt_sha256)
        {
            return Err(refused(
                "unknown operation/version or invalid handoff identity/hash",
            ));
        }
        for path in [
            &self.incoming_database,
            &self.receipt_path,
            &self.retired_directory,
        ] {
            owned_path(root, path)?;
            if path.starts_with(LEGACY_DIRECTORY)
                || path == Path::new("plurx.db")
                || path == Path::new(MANIFEST_FILENAME)
                || path == Path::new(TRANSITION_FILENAME)
                || path == Path::new(LEGACY_HANDOFF)
                || path == Path::new("node.id")
            {
                return Err(refused(
                    "handoff paths overlap live authority or identity files",
                ));
            }
        }
        let incoming_parent = self
            .incoming_database
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .ok_or_else(|| refused("incoming database requires an owned generation directory"))?;
        if self.receipt_path.parent() != Some(incoming_parent)
            || self.receipt_path == self.incoming_database
            || self.incoming_database.starts_with(&self.retired_directory)
            || self.retired_directory.starts_with(incoming_parent)
            || self.retired_directory == Path::new("plurx.db-wal")
            || self.retired_directory == Path::new("plurx.db-shm")
        {
            return Err(refused(
                "incoming, receipt and retired storage paths overlap",
            ));
        }
        if let Some(source) = &self.source_image {
            owned_path(root, &source.path)?;
            if !valid_hash(&source.sha256)
                || source.path.starts_with(LEGACY_DIRECTORY)
                || source.path.starts_with(&self.retired_directory)
                || source.path == self.incoming_database
                || source.path == self.receipt_path
                || source.path.parent() != Some(incoming_parent)
            {
                return Err(refused(
                    "immutable source image has an invalid or moving path",
                ));
            }
        }
        Ok(())
    }

    fn validate_artifacts(&self, root: &Path) -> Result<(), StoreError> {
        if path_present(&owned_path(root, Path::new("plurx.db-journal"))?)? {
            return Err(refused(
                "an unrecorded rollback journal appeared during handoff",
            ));
        }
        let receipt = owned_path(root, &self.receipt_path)?;
        if hash_regular(&receipt)? != self.receipt_sha256 {
            return Err(refused("conversion receipt changed after preparation"));
        }
        if let Some(source) = &self.source_image {
            if hash_regular(&owned_path(root, &source.path)?)? != source.sha256 {
                return Err(refused("immutable conversion source hash mismatch"));
            }
        }
        let incoming = owned_path(root, &self.incoming_database)?;
        if regular_present(&incoming)? {
            closed_database(&incoming)?;
            if hash_regular(&incoming)? != self.target_sha256 {
                return Err(refused("incoming native target hash mismatch"));
            }
            let mut incoming_manifest = self.manifest.clone();
            incoming_manifest.database = self.incoming_database.clone();
            incoming_manifest.validate_sqlite(root)?;
        } else {
            let canonical = owned_path(root, Path::new("plurx.db"))?;
            closed_database(&canonical)?;
            if hash_regular(&canonical)? != self.target_sha256 {
                return Err(refused(
                    "the moved native target is missing or changed before publication",
                ));
            }
            self.manifest.validate_sqlite(root)?;
        }
        let identity = owned_path(root, Path::new("node.id"))?;
        if !regular_present(&identity)?
            || fs::metadata(&identity)
                .map_err(|error| io("reading local identity metadata", &identity, error))?
                .len()
                > 4096
        {
            return Err(refused("local node identity is not a bounded regular file"));
        }
        let node = fs::read_to_string(&identity)
            .map_err(|error| io("reading local node identity", &identity, error))?;
        if node.trim() != self.manifest.node_id {
            return Err(refused("local node identity changed during conversion"));
        }
        Ok(())
    }
}

fn archive_legacy_database(root: &Path, handoff: &NativeHandoff) -> Result<(), StoreError> {
    let canonical = owned_path(root, Path::new("plurx.db"))?;
    let retired = owned_path(root, &handoff.retired_directory.join("legacy-plurx.db"))?;
    let incoming = owned_path(root, &handoff.incoming_database)?;
    if handoff.had_legacy_database {
        if regular_present(&retired)? {
            if regular_present(&incoming)? && regular_present(&canonical)? {
                return Err(refused(
                    "a second root database appeared after legacy retirement",
                ));
            }
        } else {
            if !regular_present(&incoming)? || !regular_present(&canonical)? {
                return Err(refused(
                    "recorded legacy database disappeared before retirement",
                ));
            }
            validate_old_database(&canonical, &handoff.manifest)?;
            rename_synced(&canonical, &retired)?;
        }
    } else if path_present(&retired)? {
        return Err(refused("an unrecorded retired legacy database exists"));
    }
    Ok(())
}

fn validate_old_database(path: &Path, manifest: &StorageManifest) -> Result<(), StoreError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let instance: Option<String> = connection
        .query_row(
            "SELECT value FROM settings WHERE key='instance.id'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let binding: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='storage_identity')",
        [],
        |row| row.get(0),
    )?;
    let already_native = binding
        && connection.query_row("SELECT EXISTS(SELECT 1 FROM storage_identity)", [], |row| {
            row.get::<_, bool>(0)
        })?;
    if instance.as_deref() != Some(manifest.instance_id.as_str()) || already_native {
        return Err(refused(
            "root legacy database identity is ambiguous or already native",
        ));
    }
    Ok(())
}

fn validate_breadcrumb(root: &Path, manifest: &StorageManifest) -> Result<(), StoreError> {
    let breadcrumb: serde_json::Value = read_record(&owned_path(root, Path::new(LEGACY_HANDOFF))?)?
        .ok_or_else(|| {
            refused("conversion must retain the legacy activation refusal breadcrumb")
        })?;
    if breadcrumb
        .get("cluster_id")
        .and_then(|value| value.as_str())
        != Some(manifest.instance_id.as_str())
        || !breadcrumb
            .get("source_backup_sha256")
            .and_then(|value| value.as_str())
            .is_some_and(valid_hash)
    {
        return Err(refused(
            "legacy handoff breadcrumb does not identify this server",
        ));
    }
    Ok(())
}

fn closed_database(path: &Path) -> Result<(), StoreError> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        if path_present(Path::new(&sidecar))? {
            return Err(refused(
                "incoming database must be closed and checkpointed before handoff",
            ));
        }
    }
    Ok(())
}

fn move_exactly_one(source: &Path, destination: &Path, directory: bool) -> Result<(), StoreError> {
    let present = |path: &Path| {
        if directory {
            directory_present(path)
        } else {
            regular_present(path)
        }
    };
    match (present(source)?, present(destination)?) {
        (true, false) => rename_synced(source, destination),
        (false, true) => Ok(()),
        (true, true) => Err(refused(
            "both active and retired copies of one source exist",
        )),
        (false, false) => Err(refused("a recorded handoff source disappeared")),
    }
}

fn sync_parent_chain(root: &Path, relative_file: &Path) -> Result<(), StoreError> {
    let mut directory = owned_path(root, relative_file)?;
    directory.pop();
    loop {
        sync_directory(&directory)?;
        if directory == root {
            break;
        }
        if !directory.pop() {
            return Err(refused("artifact directory escaped its owned root"));
        }
    }
    Ok(())
}

fn create_directory_chain(root: &Path, relative: &Path) -> Result<(), StoreError> {
    let mut directory = root.to_path_buf();
    for component in relative.components() {
        let parent = directory.clone();
        directory.push(component.as_os_str());
        if !directory_present(&directory)? {
            fs::create_dir(&directory)
                .map_err(|error| io("creating retirement directory", &directory, error))?;
            sync_directory(&directory)?;
            sync_directory(&parent)?;
        }
    }
    Ok(())
}

fn rename_synced(source: &Path, destination: &Path) -> Result<(), StoreError> {
    super::durability::rename(source, destination)
        .map_err(|error| io("moving storage generation", source, error))?;
    sync_directory(
        source
            .parent()
            .ok_or_else(|| refused("source has no parent"))?,
    )?;
    sync_directory(
        destination
            .parent()
            .ok_or_else(|| refused("destination has no parent"))?,
    )
}

fn remove_journal(root: &Path) -> Result<(), StoreError> {
    match fs::remove_file(owned_path(root, Path::new(TRANSITION_FILENAME))?) {
        Ok(()) => sync_directory(root),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io("removing completed storage handoff", root, error)),
    }
}

fn path_present(path: &Path) -> Result<bool, StoreError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(refused("symbolic handoff path")),
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io("inspecting handoff path", path, error)),
    }
}

fn regular_present(path: &Path) -> Result<bool, StoreError> {
    if !path_present(path)? {
        return Ok(false);
    }
    if !fs::symlink_metadata(path)
        .map_err(|error| io("inspecting regular file", path, error))?
        .is_file()
    {
        return Err(refused("handoff expected a regular file"));
    }
    Ok(true)
}

fn directory_present(path: &Path) -> Result<bool, StoreError> {
    if !path_present(path)? {
        return Ok(false);
    }
    if !fs::symlink_metadata(path)
        .map_err(|error| io("inspecting directory", path, error))?
        .is_dir()
    {
        return Err(refused("handoff expected a regular directory"));
    }
    Ok(true)
}

fn hash_regular(path: &Path) -> Result<String, StoreError> {
    if !regular_present(path)? {
        return Err(refused("verified handoff artifact is missing"));
    }
    let mut file =
        File::open(path).map_err(|error| io("opening verified artifact", path, error))?;
    let mut hash = Sha256::new();
    let mut chunk = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut chunk)
            .map_err(|error| io("hashing verified artifact", path, error))?;
        if read == 0 {
            break;
        }
        hash.update(&chunk[..read]);
    }
    Ok(hex::encode(hash.finalize()))
}

fn sync_regular(path: &Path) -> Result<(), StoreError> {
    if !regular_present(path)? {
        return Err(refused("handoff artifact is missing"));
    }
    super::durability::sync_regular(path)
        .map_err(|error| io("syncing verified artifact", path, error))
}

fn valid_hash(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn refused(message: impl std::fmt::Display) -> StoreError {
    StoreError::Migration(format!("native handoff refused: {message}"))
}
fn io(action: &str, path: &Path, error: impl std::fmt::Display) -> StoreError {
    refused(format!("{action} {}: {error}", path.display()))
}

#[cfg(test)]
#[path = "transition_tests.rs"]
mod tests;
