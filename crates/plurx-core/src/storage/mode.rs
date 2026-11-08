//! Node-local storage authority. This module has no replication dependency.
//!
//! A preference selects a new installation; a manifest identifies the database
//! that already owns an installation. Neither a missing peer nor a failed open
//! is permission to replace that authority with another database.

use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::error::StoreError;

pub const MANIFEST_FILENAME: &str = "storage-mode.json";
pub const TRANSITION_FILENAME: &str = "storage-transition.json";
pub(crate) const ADOPTION_FILENAME: &str = "native-adoption.json";
const MAX_RECORD_BYTES: u64 = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageMode {
    #[serde(alias = "standalone")]
    Sqlite,
    Cluster,
}

/// Deliberately separate from any clustered node identity or vote.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageManifest {
    pub version: u32,
    pub mode: StorageMode,
    pub database: PathBuf,
    pub instance_id: String,
    pub node_id: String,
    pub generation: String,
    pub schema_version: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NativeAdoption {
    version: u32,
    generation: String,
    had_database: bool,
}

/// Start or resume native adoption while the caller owns the root lease.
/// The intent must precede both schema upgrades and the database binding.
pub fn begin_native_adoption(root: &Path) -> Result<String, StoreError> {
    let adoption: Option<NativeAdoption> = read_record(&root.join(ADOPTION_FILENAME))?;
    if let Some(adoption) = &adoption {
        if adoption.version != 1 || uuid::Uuid::parse_str(&adoption.generation).is_err() {
            return Err(invalid("invalid native adoption intent"));
        }
    }
    if let Some(manifest) = StorageManifest::read(root)? {
        manifest.validate_sqlite(root)?;
        if adoption.is_some_and(|intent| intent.generation != manifest.generation) {
            return Err(invalid(
                "native adoption generation conflicts with active authority",
            ));
        }
        return Ok(manifest.generation);
    }
    let database = owned_path(root, Path::new("plurx.db"))?;
    if let Some(adoption) = adoption {
        if adoption.had_database && !database.is_file() {
            return Err(invalid("native adoption lost its existing database"));
        }
        return Ok(adoption.generation);
    }
    if database.is_file() {
        let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let schema: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if schema >= crate::store::SQLITE_SCHEMA_VERSION {
            return Err(invalid(
                "native mode schema has lost its manifest and adoption intent",
            ));
        }
        let bound: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='storage_identity')",
            [], |row| row.get(0),
        )?;
        if bound
            && connection.query_row("SELECT EXISTS(SELECT 1 FROM storage_identity)", [], |row| {
                row.get::<_, bool>(0)
            })?
        {
            return Err(invalid(
                "native storage manifest is missing for an already bound database",
            ));
        }
    }
    let generation = uuid::Uuid::new_v4().to_string();
    publish_record(
        &root.join(ADOPTION_FILENAME),
        &NativeAdoption {
            version: 1,
            generation: generation.clone(),
            had_database: database.is_file(),
        },
    )?;
    Ok(generation)
}

/// An explicit cluster preference cannot bypass a lost native manifest or an
/// unfinished native adoption. Inspect this before constructing either backend.
pub fn has_native_selection_evidence(root: &Path) -> Result<bool, StoreError> {
    match fs::symlink_metadata(root.join(ADOPTION_FILENAME)) {
        Ok(_) => return Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io("inspecting native adoption", root, error)),
    }
    let database = owned_path(root, Path::new("plurx.db"))?;
    if !database.exists() {
        return Ok(false);
    }
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let identity: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='storage_identity')",
        [],
        |row| row.get(0),
    )?;
    if !identity {
        return Ok(false);
    }
    connection
        .query_row("SELECT EXISTS(SELECT 1 FROM storage_identity)", [], |row| {
            row.get(0)
        })
        .map_err(Into::into)
}

pub fn finish_native_adoption(
    root: &Path,
    identity: &super::identity::LocalIdentity,
    generation: &str,
) -> Result<StorageManifest, StoreError> {
    let manifest = bind_sqlite(root, &identity.instance_id, &identity.node_id, generation)?;
    manifest.publish(root)?;
    match fs::remove_file(root.join(ADOPTION_FILENAME)) {
        Ok(()) => sync_directory(root)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io("removing native adoption intent", root, error)),
    }
    Ok(manifest)
}

fn invalid(reason: impl std::fmt::Display) -> StoreError {
    StoreError::Migration(format!("storage authority refused: {reason}"))
}

fn io(action: &str, path: &Path, error: impl std::fmt::Display) -> StoreError {
    invalid(format!("{action} {}: {error}", path.display()))
}

/// Reject symlinks at every component, even when they happen to point back
/// into the root today. The same rule protects subsequent rename/reopen.
pub fn owned_path(root: &Path, relative: &Path) -> Result<PathBuf, StoreError> {
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(invalid(
            "database path must be a nonempty relative owned path",
        ));
    }
    let mut path = root.to_path_buf();
    for part in relative.components() {
        path.push(part.as_os_str());
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(invalid(format!("symbolic storage path {}", path.display())));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io("inspecting", &path, error)),
        }
    }
    Ok(path)
}

pub fn read_record<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>, StoreError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io("inspecting", path, error)),
    };
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_RECORD_BYTES {
        return Err(invalid(format!(
            "{} is not a bounded regular record",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent =
        fs::canonicalize(parent).map_err(|error| io("resolving record parent", path, error))?;
    let filename = path
        .file_name()
        .ok_or_else(|| invalid("record has no filename"))?;
    crate::fs_secure::open_read_nofollow_blocking(&parent.join(filename))
        .and_then(|file| file.take(MAX_RECORD_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|error| io("reading", path, error))?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(invalid("storage record exceeded its size bound"));
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| io("decoding", path, error))
}

pub fn sync_directory(path: &Path) -> Result<(), StoreError> {
    let canonical =
        fs::canonicalize(path).map_err(|error| io("resolving directory", path, error))?;
    super::durability::directory_barrier(&canonical)
        .map_err(|error| io("syncing directory", path, error))
}

/// Close the final native image's WAL before moving/hash-binding it. Callers
/// must first drop every Store connection to this unpublished generation.
pub fn checkpoint_native_image(path: &Path) -> Result<(), StoreError> {
    let meta =
        fs::symlink_metadata(path).map_err(|error| io("inspecting native image", path, error))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err(invalid(
            "native checkpoint target is not an owned regular image",
        ));
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    #[cfg(target_os = "macos")]
    connection.pragma_update(None, "fullfsync", true)?;
    let busy: i64 =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
    if busy != 0 {
        return Err(invalid(
            "native conversion image still has a live reader/writer",
        ));
    }
    let journal: String =
        connection.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    if !journal.eq_ignore_ascii_case("delete") {
        return Err(invalid("native conversion journal did not settle"));
    }
    drop(connection);
    super::durability::sync_regular(path).map_err(|error| io("syncing native image", path, error))
}

/// The caller holds the exclusive data-directory lock across this operation.
pub fn publish_record<T: Serialize>(path: &Path, value: &T) -> Result<(), StoreError> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("record has no parent"))?;
    fs::create_dir_all(parent).map_err(|error| io("creating", parent, error))?;
    if fs::symlink_metadata(path).is_ok_and(|meta| !meta.is_file() || meta.file_type().is_symlink())
    {
        return Err(invalid("refusing to replace a nonregular authority record"));
    }
    let temporary = parent.join(format!(".storage-record-{}", uuid::Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options
            .open(&temporary)
            .map_err(|error| io("creating", &temporary, error))?;
        serde_json::to_writer(&mut file, value)
            .map_err(|error| io("encoding", &temporary, error))?;
        file.write_all(b"\n")
            .map_err(|error| io("writing", &temporary, error))?;
        file.sync_all()
            .map_err(|error| io("syncing", &temporary, error))?;
        drop(file);
        super::durability::rename(&temporary, path)
            .map_err(|error| io("publishing", path, error))?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

impl StorageManifest {
    pub fn read(root: &Path) -> Result<Option<Self>, StoreError> {
        let record: Option<Self> = read_record(&root.join(MANIFEST_FILENAME))?;
        if let Some(record) = &record {
            record.validate(root)?;
        }
        Ok(record)
    }

    pub fn validate(&self, root: &Path) -> Result<(), StoreError> {
        if self.version != 1
            || self.schema_version < 1
            || self.instance_id.trim().is_empty()
            || self.node_id.trim().is_empty()
            || self.instance_id.len() > 256
            || self.node_id.len() > 256
            || uuid::Uuid::parse_str(&self.generation).is_err()
        {
            return Err(invalid(
                "unknown version or invalid storage identity/generation",
            ));
        }
        let path = owned_path(root, &self.database)?;
        let meta = fs::symlink_metadata(&path)
            .map_err(|error| io("inspecting active database", &path, error))?;
        match self.mode {
            StorageMode::Sqlite if self.database != Path::new("plurx.db") || !meta.is_file() => {
                Err(invalid(
                    "native authority must be the canonical regular plurx.db",
                ))
            }
            StorageMode::Cluster if self.database != Path::new("hiqlite") || !meta.is_dir() => Err(
                invalid("cluster authority must name the existing hiqlite directory"),
            ),
            _ => Ok(()),
        }
    }

    pub fn publish(&self, root: &Path) -> Result<(), StoreError> {
        self.validate(root)?;
        if self.mode == StorageMode::Sqlite {
            self.validate_sqlite(root)?;
        }
        publish_record(&root.join(MANIFEST_FILENAME), self)
    }

    /// Read-only validation happens before the native writer or migrations
    /// are opened. A new database must be explicitly adopted under the lock.
    pub fn validate_sqlite(&self, root: &Path) -> Result<(), StoreError> {
        if self.mode != StorageMode::Sqlite {
            return Err(invalid("native validation requested for another backend"));
        }
        let path = owned_path(root, &self.database)?;
        let connection = Connection::open_with_flags(
            &path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        let identity: Option<(String, String, String)> = connection
            .query_row(
                "SELECT instance_id,node_id,generation FROM storage_identity WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let instance: Option<String> = connection
            .query_row(
                "SELECT value FROM settings WHERE key='instance.id'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        if version < self.schema_version
            || identity
                != Some((
                    self.instance_id.clone(),
                    self.node_id.clone(),
                    self.generation.clone(),
                ))
            || instance.as_deref() != Some(self.instance_id.as_str())
        {
            return Err(invalid(
                "manifest/database identity, generation or schema mismatch",
            ));
        }
        Ok(())
    }
}

/// Provision a new/adopted native generation only while the root lock is held.
/// This never rewrites an existing different identity or generation.
pub fn bind_sqlite(
    root: &Path,
    instance_id: &str,
    node_id: &str,
    generation: &str,
) -> Result<StorageManifest, StoreError> {
    if uuid::Uuid::parse_str(generation).is_err() {
        return Err(invalid("invalid native generation"));
    }
    let path = owned_path(root, Path::new("plurx.db"))?;
    let mut connection = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    connection.pragma_update(None, "synchronous", "FULL")?;
    #[cfg(target_os = "macos")]
    connection.pragma_update(None, "fullfsync", true)?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO storage_identity(singleton,instance_id,node_id,generation) VALUES(1,?1,?2,?3) ON CONFLICT(singleton) DO NOTHING",
        params![instance_id,node_id,generation],
    )?;
    transaction.commit()?;
    let busy: i64 = connection.query_row("PRAGMA wal_checkpoint(FULL)", [], |row| row.get(0))?;
    if busy != 0 {
        return Err(invalid("native schema/binding checkpoint is still busy"));
    }
    let schema_version = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    drop(connection);
    let manifest = StorageManifest {
        version: 1,
        mode: StorageMode::Sqlite,
        database: "plurx.db".into(),
        instance_id: instance_id.to_owned(),
        node_id: node_id.to_owned(),
        generation: generation.to_owned(),
        schema_version,
    };
    manifest.validate_sqlite(root)?;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("storage mode fixture");
        let connection =
            Connection::open(root.path().join("plurx.db")).expect("storage mode fixture");
        connection.execute_batch("CREATE TABLE settings(key TEXT PRIMARY KEY,value TEXT); INSERT INTO settings VALUES('instance.id','server'); CREATE TABLE storage_identity(singleton INTEGER PRIMARY KEY,instance_id TEXT,node_id TEXT,generation TEXT);").expect("storage mode fixture");
        connection
            .pragma_update(None, "user_version", crate::store::SQLITE_SCHEMA_VERSION)
            .expect("current native fixture version");
        root
    }

    #[test]
    fn existing_authority_rejects_wrong_identity_and_never_repairs_it() {
        let root = fixture();
        let generation = uuid::Uuid::new_v4().to_string();
        let manifest =
            bind_sqlite(root.path(), "server", "node", &generation).expect("storage mode fixture");
        manifest.publish(root.path()).expect("storage mode fixture");
        assert_eq!(
            StorageManifest::read(root.path()).expect("storage mode fixture"),
            Some(manifest.clone())
        );
        assert!(bind_sqlite(root.path(), "server", "other", &generation).is_err());
        let connection =
            Connection::open(root.path().join("plurx.db")).expect("storage mode fixture");
        connection
            .execute("UPDATE storage_identity SET generation='foreign'", [])
            .expect("storage mode fixture");
        assert!(manifest.validate_sqlite(root.path()).is_err());
    }

    #[test]
    fn unbound_native_mode_schema_requires_its_original_adoption_intent() {
        let root = fixture();
        assert!(begin_native_adoption(root.path()).is_err());
        assert!(!root.path().join(ADOPTION_FILENAME).exists());
        let root = tempfile::tempdir().expect("storage mode fixture");
        let generation = begin_native_adoption(root.path()).expect("storage mode fixture");
        let database =
            Connection::open(root.path().join("plurx.db")).expect("storage mode fixture");
        database
            .pragma_update(None, "user_version", crate::store::SQLITE_SCHEMA_VERSION)
            .expect("storage mode fixture");
        drop(database);
        assert_eq!(
            begin_native_adoption(root.path()).expect("storage mode fixture"),
            generation
        );
    }

    #[test]
    fn malformed_unknown_and_missing_active_records_fail_closed() {
        let root = fixture();
        fs::write(root.path().join(MANIFEST_FILENAME), b"{}").expect("storage mode fixture");
        assert!(StorageManifest::read(root.path()).is_err());
        let mut manifest = bind_sqlite(
            root.path(),
            "server",
            "node",
            &uuid::Uuid::new_v4().to_string(),
        )
        .expect("storage mode fixture");
        manifest.version = 2;
        publish_record(&root.path().join(MANIFEST_FILENAME), &manifest)
            .expect("storage mode fixture");
        assert!(StorageManifest::read(root.path()).is_err());
        manifest.version = 1;
        manifest.database = "../plurx.db".into();
        assert!(manifest.validate(root.path()).is_err());
        manifest.database = "plurx.db".into();
        fs::remove_file(root.path().join("plurx.db")).expect("storage mode fixture");
        assert!(manifest.validate(root.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn owned_paths_reject_symlink_escape() {
        let root = tempfile::tempdir().expect("storage mode fixture");
        let other = tempfile::tempdir().expect("storage mode fixture");
        std::os::unix::fs::symlink(other.path(), root.path().join("outside"))
            .expect("storage mode fixture");
        assert!(owned_path(root.path(), Path::new("outside/plurx.db")).is_err());
        assert!(owned_path(root.path(), Path::new("/tmp/plurx.db")).is_err());
    }
}
