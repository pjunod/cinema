//! Startup custody composition precedes generic cleanup and ordinary serving.
use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use plurx_core::adaptive_resources::runtime::NodeResourceRuntime;
use plurx_core::cluster::migration::SelectedStore;
use plurx_core::config::Config;
use plurx_core::resource_custody::InstallationContext;

/// Inspect the namespace without following a symlink. Once any custody namespace
/// exists, a missing/malformed witness or database can NEVER become a new install.
fn installation_context(data_dir: &Path) -> anyhow::Result<InstallationContext> {
    match std::fs::symlink_metadata(data_dir.join("resource-custody")) {
        Ok(_) => Ok(InstallationContext::Existing),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // No physical backend has a separate persisted namespace in v1.
            // Future backends must add their namespace check here before launch.
            Ok(InstallationContext::FirstInstallation)
        }
        Err(_) => anyhow::bail!("resource custody namespace inspection unavailable"),
    }
}

pub(crate) async fn open_before_cleanup(
    selected: &mut SelectedStore,
    config: &Config,
) -> anyhow::Result<Option<Arc<NodeResourceRuntime>>> {
    let installation = installation_context(&config.storage.data_dir)?;
    // Secure durable installation is not implemented on Windows. A genuinely
    // fresh unsupported platform keeps ordinary legacy behavior, NOT an empty
    // physical ledger. Any pre-existing authority must refuse, not be bypassed.
    if !cfg!(unix) {
        anyhow::ensure!(
            installation == InstallationContext::FirstInstallation,
            "existing resource custody cannot be bypassed on an unsupported platform"
        );
        return Ok(None);
    }
    let dirs = crate::configured_dirs(&config.storage);
    let rendition_root = dirs.renditions.clone();
    let mut excluded = vec![
        dirs.transcode,
        dirs.artwork,
        dirs.cache,
        dirs.subs,
        dirs.runtime_cache,
        dirs.renditions,
    ];
    excluded.extend(
        crate::existing_shared_cache_roots(config)
            .into_iter()
            .map(|(_, path)| path.to_path_buf()),
    );
    let options = selected
        .resource_custody_options(config.storage.data_dir.clone(), excluded, installation)
        .context("retaining actual daemon lock for resource custody")?;
    // Enqueued blocking startup is not raced against a cancellation timeout.
    // Its real owner drains and retains the lock even if its async waiter goes.
    let runtime = tokio::task::spawn_blocking(move || {
        NodeResourceRuntime::open_with_rendition_root(options, rendition_root)
    })
    .await
    .context("resource custody startup owner failed")?
    .context("opening authoritative resource custody before cleanup")?;
    runtime
        .reconcile_startup_recovery()
        .await
        .context("reconciling exact recovered resource custody")?;
    require_no_unreconciled_custody(&runtime)?;
    Ok(Some(Arc::new(runtime)))
}

fn require_no_unreconciled_custody(runtime: &NodeResourceRuntime) -> anyhow::Result<()> {
    // Only exact positively imported/committed dormant S can re-enter serving.
    // Unknown producers, partial effects and unsupported old domains still
    // retain every charge and cannot be bypassed through legacy startup.
    anyhow::ensure!(
        !runtime.has_unreconciled_custody(),
        "unreconciled physical resource custody prevents cleanup and competing startup work"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_existing_namespace_is_existing_not_a_first_installation_retry() {
        let root = crate::test_tempdir().expect("namespace fixture");
        assert_eq!(
            installation_context(root.path()).expect("new namespace"),
            InstallationContext::FirstInstallation
        );
        let namespace = root.path().join("resource-custody");
        std::fs::create_dir(&namespace).expect("empty but existing namespace");
        assert_eq!(
            installation_context(root.path()).expect("existing namespace"),
            InstallationContext::Existing
        );
        std::fs::remove_dir(&namespace).expect("remove empty fixture only");
        std::fs::write(&namespace, b"not a directory").expect("malformed namespace fixture");
        assert_eq!(
            installation_context(root.path()).expect("malformed exists"),
            InstallationContext::Existing
        );
    }

    #[test]
    fn recovered_obligations_preserve_both_managed_scratch_layouts() {
        for explicit in [false, true] {
            let root = crate::test_tempdir().expect("scratch fixture");
            let mut storage = plurx_core::config::StorageConfig {
                data_dir: root.path().join("data"),
                ..Default::default()
            };
            if explicit {
                storage.transcode_dir = root.path().join("scratch");
            }
            let dirs = crate::create_dirs_for_storage_with_protected_policy(
                &storage,
                &[],
                &[],
                crate::StartupScratchCleanup::Clear,
            )
            .expect("initial clean scratch claim");
            let retained = dirs.transcode.join("retained-native-entry");
            std::fs::write(&retained, b"original native bytes").expect("retained fixture");
            crate::create_dirs_for_storage_with_protected_policy(
                &storage,
                &[],
                &[],
                crate::StartupScratchCleanup::PreserveRecovered,
            )
            .expect("preserve scratch with old obligations");
            assert_eq!(
                std::fs::read(&retained).expect("original entry remains"),
                b"original native bytes"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn dangling_namespace_symlink_is_not_absent_authority() {
        let root = crate::test_tempdir().expect("namespace fixture");
        std::os::unix::fs::symlink("absent-target", root.path().join("resource-custody"))
            .expect("dangling namespace fixture");
        assert_eq!(
            installation_context(root.path()).expect("symlink exists without following"),
            InstallationContext::Existing
        );
    }
}
