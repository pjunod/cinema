//! Startup composition only. This does not enable finite production demands.
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
    let runtime = tokio::task::spawn_blocking(move || NodeResourceRuntime::open(options))
        .await
        .context("resource custody startup owner failed")?
        .context("opening authoritative resource custody before cleanup")?;
    require_no_unreconciled_custody(&runtime)?;
    Ok(Some(Arc::new(runtime)))
}

fn require_no_unreconciled_custody(runtime: &NodeResourceRuntime) -> anyhow::Result<()> {
    // Interim conservative startup refusal: no positive former-owner/platform
    // recovery backend is implemented yet. Do NOT clear protected paths, probe,
    // launch competitors or call legacy admission around these full holds.
    // This is not final production recovery: the existing cleanup owner still
    // needs positive adapter reconciliation before this restriction can narrow.
    anyhow::ensure!(
        !runtime.has_recovered_obligations(),
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
