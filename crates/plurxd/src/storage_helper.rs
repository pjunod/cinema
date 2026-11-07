//! Optional legacy conversion is a separately authenticated process. The normal
//! SQLite composition never links or initializes the cluster SDK.
use plurx_core::{
    config::Config,
    error::StoreError,
    storage::{helper, mode::StorageManifest, NativeDirectoryLock},
};
use std::path::Path;
#[cfg(unix)]
use std::{sync::Arc, time::Duration};

#[cfg(unix)]
mod process;

/// This runs before generic handoff recovery and before opening either Store.
/// The detached owner spans preparation, release, child reap, independent root
/// reacquisition and receipt validation even if startup's waiter is cancelled.
pub(crate) async fn settle(
    config: &Config,
    lease: NativeDirectoryLock,
) -> Result<NativeDirectoryLock, StoreError> {
    let pending = helper::pending(lease.data_dir())?;
    // The cluster distribution retains its already checked in-process legacy
    // converter. Both distributions must settle any existing helper intent.
    let legacy = !cfg!(feature = "cluster")
        && StorageManifest::read(lease.data_dir())?.is_none()
        && plurx_core::storage::mode::owned_path(
            lease.data_dir(),
            Path::new("hiqlite/activation.json"),
        )?
        .exists();
    if pending.is_none() && !legacy {
        return Ok(lease);
    }
    let owner = OperationOwner::acquire(lease.data_dir())?;
    if let Some(request) = &pending {
        let result = plurx_core::storage::mode::owned_path(
            lease.data_dir(),
            &Path::new("storage-migrations")
                .join(&request.id)
                .join("result.json"),
        )?;
        match std::fs::symlink_metadata(result) {
            Ok(_) => {
                helper::validate_completed(&lease, request)?;
                return Ok(lease);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(refused(format!("inspect migration completion: {error}"))),
        }
    }
    if config.storage.mode == Some(plurx_core::storage::mode::StorageMode::Cluster)
        || !config.cluster.join_token_file.as_os_str().is_empty()
        || !config.cluster.advertise_host.trim().is_empty()
    {
        return Err(refused(
            "a pending standalone migration cannot admit an explicit cluster",
        ));
    }
    let config = config.clone();
    tokio::spawn(async move { settle_owned(config, lease, pending, owner).await })
        .await
        .map_err(|error| refused(format!("migration owner failed: {error}")))?
}

#[cfg(unix)]
async fn settle_owned(
    config: Config,
    lease: NativeDirectoryLock,
    pending: Option<helper::PreparedRequest>,
    owner: OperationOwner,
) -> Result<NativeDirectoryLock, StoreError> {
    use plurx_desktop_control::{
        migration_helper,
        update::{MigrationBuildV1, MigrationSourceCohortV1},
    };
    let daemon = helper::HelperBuild {
        commit: env!("PLURX_SOURCE_COMMIT").into(),
        tree: env!("PLURX_SOURCE_TREE").into(),
        target: env!("PLURX_SOURCE_TARGET").into(),
    };
    // describe validates build provenance and the exact compiled recognizer,
    // without opening any source database or constructing cluster authority.
    let expected = helper::describe(daemon.clone())?;
    let build = MigrationBuildV1 {
        commit: daemon.commit.clone(),
        tree: daemon.tree.clone(),
        target: daemon.target.clone(),
    };
    let cohorts = expected
        .source_cohorts
        .iter()
        .map(|cohort| MigrationSourceCohortV1 {
            auth_version: cohort.auth_version,
            canonical_layout_fingerprint: cohort.canonical_layout_fingerprint.clone(),
        })
        .collect::<Vec<_>>();
    let generation = Arc::new(
        migration_helper::load_qualified(&build, &cohorts)
            .map_err(|error| refused(format!("qualified migration helper unavailable: {error}")))?,
    );
    let metadata = generation.metadata();
    let binding = helper::HelperBinding {
        daemon,
        helper: helper::HelperDescription {
            version: metadata.helper_protocol,
            build: helper::HelperBuild {
                commit: metadata.helper_build.commit.clone(),
                tree: metadata.helper_build.tree.clone(),
                target: metadata.helper_build.target.clone(),
            },
            source_cohorts: metadata
                .source_cohorts
                .iter()
                .map(|cohort| helper::MigrationSourceCohort {
                    auth_version: cohort.auth_version,
                    canonical_layout_fingerprint: cohort.canonical_layout_fingerprint.clone(),
                })
                .collect(),
            converter_sha256: metadata.converter_sha256.clone(),
            native_schema_version: metadata.output.native_schema_version,
            manifest_version: metadata.output.manifest_format,
            transition_version: metadata.output.transition_format,
        },
        executable_sha256: metadata.executable_sha256.clone(),
        bundle_sha256: metadata.bundle_sha256.clone(),
    };
    if binding.helper != expected {
        return Err(refused(
            "signed migration helper does not match the running daemon",
        ));
    }
    generation
        .revalidate()
        .map_err(|error| refused(format!("migration generation changed: {error}")))?;
    let (_, description) = process::run_owned_inheriting(
        Arc::clone(&generation),
        generation.executable().to_owned(),
        vec!["describe".into()],
        Duration::from_secs(30),
        inherited_leases(generation.cleanup_lease_fd(), &owner)?,
    )
    .await?;
    tracing::debug!(
        stderr_bytes = description.stderr.len(),
        "migration helper described"
    );
    let actual: helper::HelperDescription = serde_json::from_slice(&description.stdout)
        .map_err(|error| refused(format!("invalid helper description: {error}")))?;
    if actual != binding.helper {
        return Err(refused(
            "executed helper description differs from its signed binding",
        ));
    }
    let request = match pending {
        Some(request) if request.binding == binding => request,
        Some(_) => {
            return Err(refused(
                "pending migration requires its original authenticated helper generation",
            ))
        }
        None => helper::prepare(&config, &lease, binding)?,
    };
    let root = lease.data_dir().to_owned();
    let args = vec![
        "run".into(),
        "--root".into(),
        root.as_os_str().to_owned(),
        "--key".into(),
        config.credential_key_path().into_os_string(),
        "--request".into(),
        request.id.clone().into(),
        "--request-sha256".into(),
        request.sha256.clone().into(),
    ];
    generation
        .revalidate()
        .map_err(|error| refused(format!("migration generation changed: {error}")))?;
    let inherited = inherited_leases(generation.cleanup_lease_fd(), &owner)?;
    drop(lease);
    let outcome = process::run_owned_inheriting(
        Arc::clone(&generation),
        generation.executable().to_owned(),
        args,
        Duration::from_secs(600),
        inherited,
    )
    .await;
    // An owned child has been reaped before its result is returned. Reacquire
    // independently; neither a successful exit nor stdout grants a root lease.
    let lease = NativeDirectoryLock::acquire(&root).await?;
    let (_, output) = outcome?;
    tracing::debug!(
        stderr_bytes = output.stderr.len(),
        "migration helper completed"
    );
    let actual: helper::HelperResult = serde_json::from_slice(&output.stdout)
        .map_err(|error| refused(format!("invalid helper completion: {error}")))?;
    let path = plurx_core::storage::mode::owned_path(
        &root,
        &Path::new("storage-migrations")
            .join(&request.id)
            .join("result.json"),
    )?;
    let recorded: helper::HelperResult = plurx_core::storage::mode::read_record(&path)?
        .ok_or_else(|| refused("helper has no durable completion"))?;
    if actual != recorded {
        return Err(refused("helper stdout differs from durable completion"));
    }
    helper::validate_completed(&lease, &request)?;
    // Keep the authenticated generation and its cleanup lease until after the
    // child has terminated and the root and durable result are both checked.
    drop(generation);
    Ok(lease)
}

#[cfg(not(unix))]
async fn settle_owned(
    _config: Config,
    _lease: NativeDirectoryLock,
    _pending: Option<helper::PreparedRequest>,
    _owner: OperationOwner,
) -> Result<NativeDirectoryLock, StoreError> {
    Err(refused(
        "this platform requires an explicitly verified legacy migration before standalone startup",
    ))
}

fn refused(message: impl Into<String>) -> StoreError {
    StoreError::Migration(message.into())
}

/// Separate from the data-root lease: it spans the interval in which only the
/// helper owns the data root. The child inherits this exact descriptor, so a
/// restarted parent cannot commit its result while the old helper is alive.
struct OperationOwner(std::fs::File);
impl OperationOwner {
    fn acquire(root: &Path) -> Result<Self, StoreError> {
        helper::lock_supervisor(root).map(Self)
    }
}

#[cfg(unix)]
fn inherited_leases(
    generation_fd: std::os::fd::RawFd,
    owner: &OperationOwner,
) -> Result<Vec<std::fs::File>, StoreError> {
    use std::os::fd::BorrowedFd;
    // SAFETY: the qualified generation owns this descriptor throughout this
    // call and the returned duplicate has its own owned lifetime.
    let generation = unsafe { BorrowedFd::borrow_raw(generation_fd) }
        .try_clone_to_owned()
        .map(std::fs::File::from)
        .map_err(|error| refused(format!("retain helper generation lease: {error}")))?;
    let operation = owner
        .0
        .try_clone()
        .map_err(|error| refused(format!("retain migration supervisor lease: {error}")))?;
    Ok(vec![generation, operation])
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn supervisor_ownership_is_independent_of_the_released_data_root() {
        let directory = tempfile::tempdir().expect("data root");
        let root = NativeDirectoryLock::try_acquire(directory.path()).expect("root owner");
        let supervisor = OperationOwner::acquire(root.data_dir()).expect("supervisor owner");
        drop(root);
        let root = NativeDirectoryLock::try_acquire(directory.path())
            .expect("independent root acquisition");
        assert!(
            OperationOwner::acquire(root.data_dir()).is_err(),
            "another parent must refuse without waiting while holding the root"
        );
        drop(supervisor);
        assert!(OperationOwner::acquire(root.data_dir()).is_ok());
    }

    #[tokio::test]
    async fn malformed_pending_operation_cannot_reach_native_open_or_generic_recovery() {
        let directory = tempfile::tempdir().expect("data root");
        std::fs::write(
            directory.path().join("storage-migration.json"),
            b"{\"unknown\":true}",
        )
        .expect("interrupted intent");
        let mut config = Config::default();
        config.storage.data_dir = directory.path().to_owned();
        let lease = NativeDirectoryLock::try_acquire(directory.path()).expect("root owner");
        assert!(settle(&config, lease).await.is_err());
        assert!(!directory.path().join("plurx.db").exists());
        assert!(!directory.path().join("storage-mode.json").exists());
        assert_eq!(
            std::fs::read(directory.path().join("storage-migration.json"))
                .expect("retained intent"),
            b"{\"unknown\":true}"
        );
    }

    #[tokio::test]
    async fn only_the_selected_helper_inherits_the_owned_supervisor_lease() {
        let directory = tempfile::tempdir().expect("data root");
        let owner = OperationOwner::acquire(directory.path()).expect("supervisor owner");
        let inherited = owner.0.try_clone().expect("child lease duplicate");
        let fd = inherited.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        assert_ne!(flags & libc::FD_CLOEXEC, 0);
        let script = format!("test -r /dev/fd/{fd} && printf held");
        let (_, output) = process::run_owned_inheriting(
            (),
            "/bin/sh".into(),
            vec!["-c".into(), script.into()],
            Duration::from_secs(5),
            vec![inherited],
        )
        .await
        .expect("owned child and inherited lease");
        assert_eq!(output.stdout, b"held");
        assert!(OperationOwner::acquire(directory.path()).is_err());
        let parent_flags = unsafe { libc::fcntl(owner.0.as_raw_fd(), libc::F_GETFD) };
        assert_ne!(
            parent_flags & libc::FD_CLOEXEC,
            0,
            "parent descriptor remains close-on-exec for unrelated children"
        );
        drop(owner);
        assert!(OperationOwner::acquire(directory.path()).is_ok());
    }
}
