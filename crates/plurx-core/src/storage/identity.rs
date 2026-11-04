//! Stable application and node identities, independent of replication.

use crate::error::StoreError;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub const NODE_ID_FILENAME: &str = "node.id";

/// Durable application identity. A standalone server has no Raft identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalIdentity {
    pub instance_id: String,
    pub node_id: String,
}

/// Read or atomically create this process's local identity.
///
/// The initial value is the logical id on purpose: all cache locations and
/// offline packages written before M0 use that exact string as their owner.
/// Seeding rather than minting preserves those rows and their bytes without a
/// data migration.
pub fn initialize_local_identity(
    data_dir: &Path,
    instance_id: &str,
) -> Result<LocalIdentity, StoreError> {
    std::fs::create_dir_all(data_dir).map_err(|error| {
        StoreError::Identity(format!(
            "creating data directory {}: {error}",
            data_dir.display()
        ))
    })?;

    let path = data_dir.join(NODE_ID_FILENAME);
    let raw_node_id = match read_node_id(&path) {
        Ok(id) => id,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            create_node_id_noclobber(data_dir, &path, instance_id)?;
            read_node_id(&path).map_err(|error| identity_io("reading", &path, error))?
        }
        Err(error) => return Err(identity_io("reading", &path, error)),
    };
    let node_id = normalize_node_id(instance_id, &raw_node_id)?;

    Ok(LocalIdentity {
        instance_id: instance_id.to_owned(),
        node_id,
    })
}

/// A newly admitted machine has no legacy local ownership to preserve.
/// The cluster adapter supplies its separate membership identity.
#[cfg(feature = "hiqlite-store")]
pub(crate) fn initialize_join_node_identity(
    data_dir: &Path,
    instance_id: &str,
) -> Result<LocalIdentity, StoreError> {
    std::fs::create_dir_all(data_dir).map_err(|error| identity_io("creating", data_dir, error))?;
    let path = data_dir.join(NODE_ID_FILENAME);
    if !path.exists() {
        create_node_id_noclobber(data_dir, &path, &uuid::Uuid::new_v4().to_string())?;
    }
    let raw = read_node_id(&path).map_err(|error| identity_io("reading", &path, error))?;
    let node_id = normalize_node_id(&raw, &raw)?;
    Ok(LocalIdentity {
        instance_id: instance_id.to_owned(),
        node_id,
    })
}

pub(crate) fn read_node_id(path: &Path) -> std::io::Result<String> {
    let mut raw = String::new();
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = std::fs::canonicalize(parent)?;
    let filename = path
        .file_name()
        .ok_or_else(|| std::io::Error::new(ErrorKind::InvalidInput, "identity has no filename"))?;
    crate::fs_secure::open_read_nofollow_blocking(&parent.join(filename))?
        .take(4097)
        .read_to_string(&mut raw)?;
    if raw.len() > 4096 {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "node identity exceeds its size bound",
        ));
    }
    let id = raw.trim();
    if id.is_empty() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidData,
            "the file is empty",
        ));
    }
    Ok(id.to_owned())
}

/// Publish a complete, fsynced file without ever replacing an identity another
/// process won the race to create.
fn create_node_id_noclobber(
    data_dir: &Path,
    destination: &Path,
    seed: &str,
) -> Result<(), StoreError> {
    let temporary = temporary_path(data_dir);
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    let result = (|| -> Result<(), StoreError> {
        let mut file = options
            .open(&temporary)
            .map_err(|error| identity_io("creating", &temporary, error))?;
        file.write_all(seed.as_bytes())
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_all())
            .map_err(|error| identity_io("writing", &temporary, error))?;

        #[cfg(unix)]
        {
            let mode = file
                .metadata()
                .map_err(|error| identity_io("reading metadata for", &temporary, error))?
                .permissions()
                .mode()
                & 0o777;
            if !mode_is_owner_only(mode) {
                tracing::warn!(
                    path = %temporary.display(),
                    mode = format_args!("{mode:o}"),
                    "cluster identity filesystem exposed group or other permissions despite requesting mode 600"
                );
            }
        }
        drop(file);

        #[cfg(windows)]
        {
            match super::durability::publish_new_file(&temporary, destination) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(identity_io("publishing", destination, error)),
            }
        }
        #[cfg(unix)]
        match std::fs::hard_link(&temporary, destination) {
            Ok(()) => sync_directory(data_dir),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
            Err(error) if hard_links_unavailable(&error) => {
                publish_with_rename_fallback(data_dir, &temporary, destination)
            }
            Err(error) => Err(identity_io("publishing", destination, error)),
        }
    })();

    let cleanup = std::fs::remove_file(&temporary);
    if let Err(error) = cleanup {
        if error.kind() != ErrorKind::NotFound && result.is_ok() {
            return Err(identity_io("removing temporary", &temporary, error));
        }
    }
    result
}

#[cfg(unix)]
fn hard_links_unavailable(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::PermissionDenied | ErrorKind::Unsupported
    )
}

#[cfg(unix)]
pub(crate) fn mode_is_owner_only(mode: u32) -> bool {
    mode & 0o077 == 0
}

/// Best-effort no-clobber fallback for filesystems that cannot create hard
/// links. A create-new destination probe elects one publisher; only that
/// process may replace its own empty placeholder with the already-fsynced
/// temporary file.
#[cfg(any(unix, test))]
pub(crate) fn publish_with_rename_fallback(
    data_dir: &Path,
    temporary: &Path,
    destination: &Path,
) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);

    match options.open(destination) {
        Ok(placeholder) => {
            drop(placeholder);
            super::durability::rename(temporary, destination)
                .map_err(|error| identity_io("publishing", destination, error))?;
            sync_directory(data_dir)
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(identity_io("reserving", destination, error)),
    }
}

fn temporary_path(data_dir: &Path) -> PathBuf {
    data_dir.join(format!(".{NODE_ID_FILENAME}.{}.tmp", uuid::Uuid::new_v4()))
}

#[cfg(any(unix, test))]
fn sync_directory(path: &Path) -> Result<(), StoreError> {
    super::durability::directory_barrier(path)
        .map_err(|error| identity_io("syncing directory", path, error))
}

fn validate_uuid(label: &str, value: &str) -> Result<uuid::Uuid, StoreError> {
    let id = uuid::Uuid::parse_str(value)
        .map_err(|error| StoreError::Identity(format!("{label} is not a UUID: {error}")))?;
    if id.is_nil() {
        return Err(StoreError::Identity(format!("{label} must not be nil")));
    }
    Ok(id)
}

fn normalize_node_id(cluster_id: &str, node_id: &str) -> Result<String, StoreError> {
    if node_id == cluster_id {
        return Ok(cluster_id.to_owned());
    }

    let parsed = validate_uuid("node.id", node_id)?;
    if uuid::Uuid::parse_str(cluster_id).is_ok_and(|cluster| cluster == parsed) {
        // Preserve the exact legacy ownership key stored in instance.id even
        // if an operator wrote an equivalent UUID spelling to node.id.
        Ok(cluster_id.to_owned())
    } else {
        Ok(parsed.to_string())
    }
}

fn identity_io(action: &str, path: &Path, error: std::io::Error) -> StoreError {
    StoreError::Identity(format!("{action} {}: {error}", path.display()))
}
