//! Installation witness, retained file identities and real Unix durability.
//! Windows deliberately refuses rather than using no-op directory sync / ACLs.
use super::{codec, schema, CustodyError, ErrorKind, InstallationContext, OpenOptions};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions as FsOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[cfg(test)]
mod tests;

/// Local installation-location guard, NOT host continuity or extinction proof.
/// It rejects ordinary copies/moves; coherent VM/filesystem clones/rollback can
/// reproduce these values and require independently established native fencing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StorageIdentity {
    version: u32,
    platform: String,
    canonical_directory: String,
    device: u64,
    inode: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Witness {
    pub version: u32,
    pub state: String,
    pub installation_id: Uuid,
    pub journal_id: Uuid,
    pub node_id: String,
    pub cluster_id: String,
    storage_identity: StorageIdentity,
}

pub(super) struct Storage {
    pub connection: Connection,
    pub witness: Witness,
    #[cfg(test)]
    pub fail_after_commit_once: bool,
    #[cfg(unix)]
    guard: UnixGuard,
}

#[cfg(unix)]
struct UnixGuard {
    path: PathBuf,
    directory: File,
    database: File,
    witness: File,
    ancestors: Vec<(PathBuf, crate::fs_secure::FileIdentity)>,
}

#[cfg(unix)]
fn sync(file: &File) -> Result<(), CustodyError> {
    file.sync_all()?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        // F_FULLFSYNC is required for hardware-cache flush on macOS files.
        // Directories use fsync; F_FULLFSYNC is not defined for directories.
        if file.metadata()?.is_file()
            && unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } != 0
        {
            return Err(CustodyError::new(
                ErrorKind::UnsupportedStorage,
                "custody fullfsync unavailable",
            ));
        }
    }
    Ok(())
}

#[cfg(unix)]
fn secure_file(path: &Path, create: bool) -> Result<File, CustodyError> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = FsOptions::new()
        .read(true)
        .write(create)
        .create_new(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let m = file.metadata()?;
    if !m.is_file()
        || m.nlink() != 1
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o777 != 0o600
    {
        return Err(CustodyError::new(
            ErrorKind::UnsupportedStorage,
            "custody file is not private single-link authority",
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn same_file(path: &Path, retained: &File) -> Result<(), CustodyError> {
    let current = secure_file(path, false)?;
    if !crate::fs_secure::std_file_identity(&current)?
        .same_inode(crate::fs_secure::std_file_identity(retained)?)
    {
        return Err(CustodyError::new(
            ErrorKind::IdentityMismatch,
            "custody authority file replaced",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn publish(
    directory: &Path,
    parent: &File,
    witness: &Witness,
    initial: bool,
) -> Result<(), CustodyError> {
    let pending = directory.join(format!("installation-{}.pending", Uuid::new_v4()));
    let mut file = secure_file(&pending, true)?;
    file.write_all(codec::encode(witness)?.as_bytes())?;
    sync(&file)?;
    let destination = directory.join("installation.json");
    if initial {
        // Atomic no-clobber publication. Incomplete crash leftovers are preserved
        // on reopen; they never authorize an empty replacement journal.
        std::fs::hard_link(&pending, &destination)?;
        std::fs::remove_file(&pending)?;
    } else {
        std::fs::rename(&pending, &destination)?;
    }
    sync(parent)
}

#[cfg(unix)]
fn configure(connection: &Connection) -> Result<(), CustodyError> {
    connection.busy_timeout(std::time::Duration::ZERO)?;
    connection.execute_batch("PRAGMA journal_mode=DELETE; PRAGMA synchronous=EXTRA; PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")?;
    #[cfg(target_os = "macos")]
    connection.execute_batch("PRAGMA fullfsync=ON; PRAGMA checkpoint_fullfsync=ON;")?;
    let mode: String = connection.query_row("PRAGMA journal_mode", [], |r| r.get(0))?;
    let sync_level: i64 = connection.query_row("PRAGMA synchronous", [], |r| r.get(0))?;
    let fk: i64 = connection.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    let trusted: i64 = connection.query_row("PRAGMA trusted_schema", [], |r| r.get(0))?;
    if mode != "delete" || sync_level != 3 || fk != 1 || trusted != 0 {
        return Err(CustodyError::new(
            ErrorKind::UnsupportedStorage,
            "custody SQLite durability unavailable",
        ));
    }
    #[cfg(target_os = "macos")]
    if connection.query_row("PRAGMA fullfsync", [], |r| r.get::<_, i64>(0))? != 1 {
        return Err(CustodyError::new(
            ErrorKind::UnsupportedStorage,
            "custody SQLite fullfsync unavailable",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn validate_filesystem(directory: &File) -> Result<(), CustodyError> {
    use std::os::fd::AsRawFd;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    if unsafe { libc::fstatfs(directory.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(CustodyError::new(
            ErrorKind::UnsupportedStorage,
            "custody filesystem identity unavailable",
        ));
    }
    let stat = unsafe { stat.assume_init() };
    #[cfg(target_os = "macos")]
    let supported = {
        let name = unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) };
        matches!(name.to_bytes(), b"apfs" | b"hfs")
    };
    #[cfg(target_os = "linux")]
    let supported = matches!(
        stat.f_type as u64,
        0xef53 | 0x58465342 | 0x9123683e | 0x2fc12fc1
    );
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let supported = false;
    if !supported {
        return Err(CustodyError::new(
            ErrorKind::UnsupportedStorage,
            "custody requires supported local durable filesystem",
        ));
    }
    Ok(())
}

impl Storage {
    #[cfg(not(unix))]
    pub(super) fn open(_options: &OpenOptions) -> Result<Self, CustodyError> {
        Err(CustodyError::new(
            ErrorKind::UnsupportedStorage,
            "custody secure durable installation unsupported on this platform",
        ))
    }

    #[cfg(unix)]
    pub(super) fn open(options: &OpenOptions) -> Result<Self, CustodyError> {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt};
        // Every component is no-follow. Retain ancestry identities and refuse
        // aliases to configured scratch/shared roots before creating anything.
        let data = crate::fs_secure::open_directory_nofollow_blocking(&options.data_dir)?;
        if std::fs::canonicalize(&options.data_dir)? != options.data_dir {
            return Err(CustodyError::new(
                ErrorKind::UnsupportedStorage,
                "custody data directory must be canonical",
            ));
        }
        let path = options.data_dir.join("resource-custody");
        for excluded in &options.excluded_roots {
            if !excluded.is_absolute() || path.starts_with(excluded) || excluded.starts_with(&path)
            {
                return Err(CustodyError::new(
                    ErrorKind::UnsupportedStorage,
                    "custody directory aliases excluded storage",
                ));
            }
        }
        if !path.try_exists()? {
            if options.installation != InstallationContext::FirstInstallation {
                return Err(CustodyError::new(
                    ErrorKind::UnreadableAuthority,
                    "known custody installation missing",
                ));
            }
            std::fs::DirBuilder::new().mode(0o700).create(&path)?;
            sync(&data)?;
        }
        let directory = crate::fs_secure::open_directory_nofollow_blocking(&path)?;
        for excluded in &options.excluded_roots {
            if crate::fs_secure::directory_source_ancestry_alias_blocking(&path, excluded)? {
                return Err(CustodyError::new(
                    ErrorKind::UnsupportedStorage,
                    "custody directory aliases excluded storage",
                ));
            }
        }
        validate_filesystem(&directory)?;
        let dm = directory.metadata()?;
        if dm.uid() != unsafe { libc::geteuid() } || dm.mode() & 0o777 != 0o700 {
            return Err(CustodyError::new(
                ErrorKind::UnsupportedStorage,
                "custody directory is not private",
            ));
        }
        let canonical_directory = path.to_str().ok_or_else(|| {
            CustodyError::new(
                ErrorKind::UnsupportedStorage,
                "custody path has no v1 identity codec",
            )
        })?;
        codec::long_text(canonical_directory)?;
        let storage_identity = StorageIdentity {
            version: 1,
            platform: std::env::consts::OS.into(),
            canonical_directory: canonical_directory.into(),
            device: dm.dev(),
            inode: dm.ino(),
        };
        // Nonprivileged other users must not be able to replace an ancestor.
        // Sticky shared ancestors are allowed only above the trusted data root;
        // same-UID/privileged hostile replacement is not an OS-enforced boundary.
        let mut ancestors = Vec::new();
        for ancestor in path.ancestors() {
            let (identity, mode) =
                crate::fs_secure::directory_ancestor_metadata_nofollow_blocking(ancestor)?;
            if mode & 0o022 != 0 && mode & 0o1000 == 0 {
                return Err(CustodyError::new(
                    ErrorKind::UnsupportedStorage,
                    "custody ancestry is replaceable",
                ));
            }
            ancestors.push((ancestor.to_path_buf(), identity));
        }
        let db_path = path.join("obligations.sqlite3");
        let witness_path = path.join("installation.json");
        let names = std::fs::read_dir(&path)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<Result<Vec<_>, _>>()?;
        for name in &names {
            if ![
                "obligations.sqlite3",
                "installation.json",
                "obligations.sqlite3-journal",
            ]
            .iter()
            .any(|expected| name == *expected)
            {
                return Err(CustodyError::new(
                    ErrorKind::UnreadableAuthority,
                    "unexpected custody installation contents",
                ));
            }
            secure_file(&path.join(name), false)?;
        }
        let has_db = db_path.try_exists()?;
        let has_witness = witness_path.try_exists()?;
        let mut witness: Witness;
        let mut installing = false;
        if !has_witness {
            if has_db
                || !names.is_empty()
                || options.installation != InstallationContext::FirstInstallation
            {
                return Err(CustodyError::new(
                    ErrorKind::UnreadableAuthority,
                    "custody witness missing with prior authority",
                ));
            }
            witness = Witness {
                version: 1,
                state: "installing".into(),
                installation_id: Uuid::new_v4(),
                journal_id: Uuid::new_v4(),
                node_id: options.owner.node_id.clone(),
                cluster_id: options.owner.cluster_id.clone(),
                storage_identity: storage_identity.clone(),
            };
            publish(&path, &directory, &witness, true)?;
            installing = true;
        } else {
            let mut bytes = String::new();
            secure_file(&witness_path, false)?
                .take((codec::JSON_LIMIT + 1) as u64)
                .read_to_string(&mut bytes)?;
            witness = codec::decode(&bytes)?;
        }
        if witness.version != 1 || !["installing", "ready"].contains(&witness.state.as_str()) {
            return Err(codec::invalid());
        }
        codec::nonnil(witness.journal_id)?;
        codec::nonnil(witness.installation_id)?;
        if witness.node_id != options.owner.node_id
            || witness.cluster_id != options.owner.cluster_id
            || witness.storage_identity != storage_identity
        {
            return Err(CustodyError::new(
                ErrorKind::IdentityMismatch,
                "custody installation binding differs",
            ));
        }
        installing |= witness.state == "installing";
        if !has_db && !installing {
            return Err(CustodyError::new(
                ErrorKind::UnreadableAuthority,
                "ready custody database missing",
            ));
        }
        let database = secure_file(&db_path, !has_db)?;
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW;
        let mut connection = Connection::open_with_flags(&db_path, flags)?;
        // Recognize BEFORE policy changes. SQLite may recover a legitimate hot
        // rollback journal on read; never remove that journal ourselves.
        let objects: i64 =
            connection.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if objects == 0 && version == 0 && installing && database.metadata()?.len() == 0 {
            configure(&connection)?;
            let tx = connection.transaction()?;
            tx.execute_batch(schema::DDL)?;
            tx.execute(
                "INSERT INTO journal_meta VALUES(1,6,?1,?2,?3,?4,0,NULL)",
                rusqlite::params![
                    witness.journal_id.as_bytes(),
                    witness.installation_id.as_bytes(),
                    witness.node_id,
                    witness.cluster_id
                ],
            )?;
            tx.commit()?;
            sync(&database)?;
            sync(&directory)?;
        } else {
            schema::validate(&connection)?;
            configure(&connection)?;
        }
        schema::validate(&connection)?;
        let meta: (Vec<u8>, Vec<u8>, String, String) = connection.query_row("SELECT journal_id,installation_id,node_id,cluster_id FROM journal_meta WHERE singleton=1", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        if codec::uuid_from_bytes(&meta.0)? != witness.journal_id
            || codec::uuid_from_bytes(&meta.1)? != witness.installation_id
            || meta.2 != witness.node_id
            || meta.3 != witness.cluster_id
        {
            return Err(codec::invalid());
        }
        if installing {
            let rows: i64 = connection.query_row("SELECT (SELECT count(*) FROM obligations)+(SELECT count(*) FROM owners)+(SELECT count(*) FROM operations)+(SELECT count(*) FROM members)+(SELECT count(*) FROM family_roles)", [], |r| r.get(0))?;
            let (seq, owner): (i64, Option<Vec<u8>>) = connection.query_row(
                "SELECT next_sequence,current_owner_id FROM journal_meta",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            if rows != 0 || seq != 0 || owner.is_some() {
                return Err(codec::invalid());
            }
            witness.state = "ready".into();
            publish(&path, &directory, &witness, false)?;
        }
        let guard = UnixGuard {
            path,
            directory,
            database,
            witness: secure_file(&witness_path, false)?,
            ancestors,
        };
        let mut storage = Self {
            connection,
            witness,
            #[cfg(test)]
            fail_after_commit_once: false,
            guard,
        };
        storage.verify()?;
        // Finish a never-ready v1 installation above before upgrading. Preserve
        // the ready witness, database inode, location guard and installation IDs.
        schema::upgrade(&mut storage.connection, &storage.witness)?;
        storage.verify().map_err(|mut error| {
            error.certainty = super::CommitCertainty::Unknown;
            error
        })?;
        Ok(storage)
    }

    pub(super) fn verify(&self) -> Result<(), CustodyError> {
        #[cfg(unix)]
        {
            for (path, identity) in &self.guard.ancestors {
                if !crate::fs_secure::directory_ancestor_metadata_nofollow_blocking(path)?
                    .0
                    .same_inode(*identity)
                {
                    return Err(CustodyError::new(
                        ErrorKind::IdentityMismatch,
                        "custody ancestor replaced",
                    ));
                }
            }
            if !crate::fs_secure::directory_identity_nofollow_blocking(&self.guard.path)?
                .same_inode(crate::fs_secure::std_file_identity(&self.guard.directory)?)
            {
                return Err(codec::invalid());
            }
            same_file(
                &self.guard.path.join("obligations.sqlite3"),
                &self.guard.database,
            )?;
            same_file(
                &self.guard.path.join("installation.json"),
                &self.guard.witness,
            )?;
            for sidecar in [
                "obligations.sqlite3-journal",
                "obligations.sqlite3-wal",
                "obligations.sqlite3-shm",
            ] {
                let path = self.guard.path.join(sidecar);
                if std::fs::symlink_metadata(&path).is_ok() {
                    if sidecar != "obligations.sqlite3-journal" {
                        return Err(codec::invalid());
                    }
                    secure_file(&path, false)?;
                }
            }
            Ok(())
        }
        #[cfg(not(unix))]
        {
            Err(CustodyError::new(
                ErrorKind::UnsupportedStorage,
                "custody durability unsupported",
            ))
        }
    }
}
