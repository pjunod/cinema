//! Controller commands and immutable-worker orchestration shared by both native UIs.
use crate::{
    installation::*,
    ipc::BoundDaemon,
    native, package, process,
    protocol::*,
    secure_fs, supervisor,
    transaction::{self, JournalV1, Phase, TransactionStore},
    update::{self, ApiCacheV1, PublicReleaseClient},
    Error, Result,
};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub fn require_owner(i: &InstallationV1) -> Result<()> {
    if native_uid() != i.owner_uid {
        return Err(Error::new(
            ErrorCode::AuthorizationRequired,
            "native installation authority is required; use the narrow installed polkit helper",
        ));
    }
    i.managed()
}
pub fn inspection_unavailable(e: &Error) -> bool {
    matches!(e.os_error, Some(libc::EACCES | libc::EPERM))
        || matches!(
            e.code,
            ErrorCode::AuthorizationRequired | ErrorCode::AuthorizationDenied
        )
}
pub fn status(i: &InstallationV1) -> Result<ControlResultV1> {
    let active = crate::progress::for_installation(i)?;
    let transaction_id = active
        .filter(|j| !matches!(j.phase, Phase::Committed | Phase::Cancelled))
        .map(|j| j.transaction_id);
    let mut o = match supervisor::observe(i) {
        Ok(o) => o,
        Err(e) if inspection_unavailable(&e) => return Ok(ControlResultV1::Status {
            installation_id: i.installation_id.clone(),
            service_state: ServiceState::Unknown,
            version: Some(i.current_version.clone()),
            browser_origin: i.browser_origin.clone(),
            server_startup: StartupStateV1 {
                state: StartupState::Unknown,
                detail: Some(
                    "Native process inspection requires authorization; service health is not known"
                        .into(),
                ),
            },
            tray_startup: tray_startup(i)?,
            transaction_id,
        }),
        Err(e) => return Err(e),
    };
    let mut browser = i.browser_origin.clone();
    let mut version = Some(i.current_version.clone());
    if o.process.is_some() {
        match BoundDaemon::connect(i) {
            Ok(bound) => {
                o.state = if bound.identity.ready {
                    ServiceState::Running
                } else {
                    ServiceState::Starting
                };
                browser = bound.identity.browser_origin;
                version = Some(bound.identity.version);
            }
            Err(e) if inspection_unavailable(&e) => {
                o.state = ServiceState::Unknown;
                o.startup.detail = Some(
                    "Native process inspection requires authorization; health is unknown".into(),
                );
            }
            Err(_) => o.state = ServiceState::Unhealthy,
        }
    }

    Ok(ControlResultV1::Status {
        installation_id: i.installation_id.clone(),
        service_state: o.state,
        version,
        browser_origin: browser,
        server_startup: o.startup,
        tray_startup: tray_startup(i)?,
        transaction_id,
    })
}
pub fn startup(i: &InstallationV1) -> Result<ControlResultV1> {
    Ok(ControlResultV1::Startup {
        server: supervisor::observe(i)?.startup,
        tray: tray_startup(i)?,
    })
}
pub fn tray_startup(i: &InstallationV1) -> Result<StartupStateV1> {
    if i.method == InstallMethod::MacosNative {
        return Ok(StartupStateV1 {
            state: StartupState::Unknown,
            detail: Some("Native app reports SMAppService approval/registration directly".into()),
        });
    }
    if !i.desktop_installed {
        return Ok(StartupStateV1 {
            state: StartupState::Disabled,
            detail: Some("Desktop companion is not installed; server remains headless".into()),
        });
    }
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("HOME");
    let local = tray_config_home(xdg.as_deref(), home.as_deref(), false)?
        .map(|base| base.join("autostart").join(TRAY_ENTRY));
    read_tray_startup(
        local.as_deref(),
        Path::new("/etc/xdg/autostart/tv.plurx.Server-autostart.desktop"),
    )
}
const TRAY_ENTRY: &str = "tv.plurx.Server-autostart.desktop";
fn tray_config_home(
    xdg: Option<&OsStr>,
    home: Option<&OsStr>,
    writing: bool,
) -> Result<Option<PathBuf>> {
    let absolute = |value: &OsStr| {
        value
            .to_str()
            .filter(|text| !text.contains('\0'))
            .and_then(|text| {
                let path = Path::new(text);
                path.is_absolute().then(|| path.to_path_buf())
            })
    };
    if let Some(value) = xdg {
        if let Some(path) = absolute(value) {
            return Ok(Some(path));
        }
        if writing {
            return Err(Error::new(ErrorCode::PolicyConflict,
                "set XDG_CONFIG_HOME must be an absolute valid path; no fallback preference was written"));
        }
    }
    if let Some(path) = home.and_then(absolute) {
        return Ok(Some(path.join(".config")));
    }
    if writing {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "an absolute HOME is required when XDG_CONFIG_HOME is unset",
        ));
    }
    Ok(None)
}
fn read_tray_startup(local: Option<&Path>, system: &Path) -> Result<StartupStateV1> {
    let file = match local {
        Some(path) => match std::fs::symlink_metadata(path) {
            Ok(_) => path,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => system,
            Err(error) => return Err(error.into()),
        },
        None => system,
    };
    let parent = secure_fs::open_dir(
        file.parent()
            .ok_or_else(|| Error::new(ErrorCode::Io, "autostart parent"))?,
        native_uid(),
    )?;
    let bytes = parent.read(TRAY_ENTRY, 65536)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| Error::new(ErrorCode::PolicyConflict, "autostart entry is not UTF8"))?;
    Ok(StartupStateV1::new(
        if text.lines().any(|line| line.trim() == "Hidden=true") {
            StartupState::Disabled
        } else {
            StartupState::Enabled
        },
    ))
}
fn write_tray_startup(config_home: &Path, enabled: bool) -> Result<()> {
    let dir = config_home.join("autostart");
    std::fs::create_dir_all(&dir)?;
    let target = secure_fs::open_dir(&dir, native_uid())?;
    let content = format!("[Desktop Entry]\nType=Application\nName=Plurx Server\nExec=/usr/bin/plurx-tray --autostart\nHidden={}\nX-GNOME-Autostart-enabled={}\n", !enabled, enabled);
    target.write(TRAY_ENTRY, content.as_bytes(), 0o600)
}

pub fn set_tray(i: &InstallationV1, enabled: bool) -> Result<ControlResultV1> {
    if i.method == InstallMethod::MacosNative {
        return Err(Error::new(
            ErrorCode::Unsupported,
            "use native SMAppService; controller never overrides macOS login-item approval",
        ));
    }
    if native_uid() == 0 || !i.desktop_installed {
        return Err(Error::new(
            ErrorCode::Unsupported,
            "tray login is a graphical user's preference, not a root/server policy",
        ));
    }
    let xdg = std::env::var_os("XDG_CONFIG_HOME");
    let home = std::env::var_os("HOME");
    let config_home = tray_config_home(xdg.as_deref(), home.as_deref(), true)?
        .ok_or_else(|| Error::new(ErrorCode::PolicyConflict, "tray config path unavailable"))?;
    write_tray_startup(&config_home, enabled)?;
    startup(i)
}
pub fn lifecycle(i: &InstallationV1, operation: Operation) -> Result<ControlResultV1> {
    require_owner(i)?;
    let store = TransactionStore::locked(i)?;
    if operation == Operation::Start {
        if store.active()?.is_some_and(|j| !j.terminal()) {
            return Err(Error::new(
                ErrorCode::RecoveryRequired,
                "unresolved transaction holds startup; run explicit recovery",
            ));
        }
        let after = supervisor::start(i)?;
        return Ok(ControlResultV1::Operation {
            operation,
            service_state: after.state,
        });
    }
    if !matches!(operation, Operation::Stop | Operation::Restart) {
        return Err(Error::new(
            ErrorCode::InvalidRequest,
            "invalid lifecycle operation",
        ));
    }
    let mut j = store.create(i, operation, None)?;
    let result: Result<ControlResultV1> = (|| {
        transaction::stop_clean(i, &store, &mut j)?;
        if operation == Operation::Restart {
            j.transition(Phase::Restoring)?;
            authorize_start(&store, &mut j)?;
            supervisor::start(i)?;
            wait_ready(i, &j, None)?;
        }
        j.transition(Phase::Committed)?;
        j.start_permit_expires = None;
        store.save(&j)?;
        Ok(ControlResultV1::Operation {
            operation,
            service_state: supervisor::observe(i)?.state,
        })
    })();
    if let Err(e) = &result {
        record_failure(&store, &mut j, e)?;
    }
    result
}
pub(crate) fn authorize_start(store: &TransactionStore, j: &mut JournalV1) -> Result<()> {
    let me = process::inspect(std::process::id())?;
    j.worker_pid = Some(me.pid);
    j.worker_generation = Some(me.generation);
    j.start_permit_expires = Some(crate::unix_time()?.saturating_add(90));
    store.save(j)
}
/// Identity is already authenticated by BoundDaemon; readiness additionally pins
/// source/version and the pre-stop node rather than trusting systemctl's exit.
pub(crate) fn readiness_matches(
    i: &InstallationV1,
    j: &JournalV1,
    id: &DaemonIdentityV1,
    expected_commit: Option<&str>,
) -> bool {
    id.ready
        && id.version == i.current_version
        && expected_commit.is_none_or(|c| id.build == c)
        && j.expected_node_id
            .as_ref()
            .is_none_or(|node| node == &id.node_id)
}
pub(crate) fn wait_ready(
    i: &InstallationV1,
    j: &JournalV1,
    expected_commit: Option<&str>,
) -> Result<()> {
    let until = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(bound) = BoundDaemon::connect(i) {
            let id = bound.identity;
            if readiness_matches(i, j, &id, expected_commit) {
                return Ok(());
            }
        }
        if Instant::now() >= until {
            return Err(Error::new(ErrorCode::RecoveryRequired, "candidate did not prove expected build/node/readiness; no database/Raft restore attempted"));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
pub(crate) fn record_failure(store: &TransactionStore, j: &mut JournalV1, e: &Error) -> Result<()> {
    j.detail = Some(e.message.clone());
    // Preserve the write-ahead phase so recovery can distinguish an unchanged
    // installation from a partial package or native promotion.
    if j.phase == Phase::Prepared {
        j.transition(Phase::Cancelled)?;
    }
    j.start_permit_expires = None;
    store.save(j)
}
pub fn update_status(i: &InstallationV1, id: Option<&str>) -> Result<ControlResultV1> {
    if let Some(id) = id {
        uuid::Uuid::parse_str(id)
            .map_err(|_| Error::new(ErrorCode::InvalidRequest, "invalid transaction UUID"))?;
    }
    let j = crate::progress::for_installation(i)?;
    if id.is_some_and(|id| j.as_ref().is_none_or(|j| j.transaction_id != id)) {
        return Err(Error::new(
            ErrorCode::Unavailable,
            "requested transaction is not the current public progress snapshot",
        ));
    }
    Ok(match j {
        None => update_result(UpdateState::Idle, None, None, None),
        Some(j) => update_result(
            j.state(),
            j.target_version,
            Some(j.transaction_id),
            j.failed.then(|| "Operation requires inspection; authorized recovery CLI provides private diagnostics".into()),
        ),
    })
}
fn update_result(
    state: UpdateState,
    version: Option<String>,
    transaction_id: Option<String>,
    detail: Option<String>,
) -> ControlResultV1 {
    ControlResultV1::Update {
        state,
        version,
        transaction_id,
        detail,
    }
}
pub fn check_updates(i: &InstallationV1) -> Result<ControlResultV1> {
    require_owner(i)?;
    // Fail closed before network traffic if production authentication is unprovisioned.
    if update::production_keys()?.is_empty() {
        return Err(Error::new(ErrorCode::UntrustedUpdate, "release owner must provision production update verification keys/signing workflow before in-app upgrades"));
    }
    let store = TransactionStore::locked(i)?;
    let client = PublicReleaseClient::new()?;
    let mut cache: ApiCacheV1 = match store.root.json("github-api-cache.json") {
        Ok(c) => c,
        Err(e) if e.os_error == Some(libc::ENOENT) => ApiCacheV1::default(),
        Err(e) => return Err(e),
    };
    let discovered = client.discover(&mut cache, crate::unix_time()?);
    store.root.write_json("github-api-cache.json", &cache)?;
    let ledger = store.ledger()?;
    for version in discovered? {
        if semver::Version::parse(&version).ok() <= semver::Version::parse(&i.current_version).ok()
        {
            continue;
        }
        // A release is offered ONLY after its signature, upgrade path and full cohort are verified.
        let (candidate, _, _) = client.manifest(&version, i, &ledger, crate::unix_time()?)?;
        store.accept(&candidate)?;
        return Ok(update_result(
            UpdateState::Available,
            Some(version),
            None,
            Some(candidate.manifest().release_notes_url.clone()),
        ));
    }
    Ok(update_result(
        UpdateState::Idle,
        None,
        None,
        Some("No compatible stable release".into()),
    ))
}
pub fn prepare_update(i: &InstallationV1, version: &str) -> Result<ControlResultV1> {
    require_owner(i)?;
    let store = TransactionStore::locked(i)?;
    let client = PublicReleaseClient::new()?;
    let (candidate, bytes, signature) =
        client.manifest(version, i, &store.ledger()?, crate::unix_time()?)?;
    let mut j = store.create(i, Operation::Upgrade, Some(&candidate))?;
    let result: Result<ControlResultV1> = (|| {
        let dir = store.dir(&j.transaction_id)?;
        dir.write(update::MANIFEST_NAME, &bytes, 0o400)?;
        dir.write(update::SIGNATURE_NAME, &signature, 0o400)?;
        ensure_space(
            &i.paths.state,
            update::select_cohort(&candidate, i)?
                .iter()
                .map(|a| a.size)
                .sum::<u64>()
                .saturating_mul(4)
                .saturating_add(512 * 1024 * 1024),
        )?;
        client.stage(&candidate, i, &dir)?;
        if i.method != InstallMethod::MacosNative {
            let plan = package::preflight(i, &candidate, &store, &j)?;
            j.package_inventory_before = Some(package::inventory(i, &plan.packages)?);
        } else {
            let asset = update::select_cohort(&candidate, i)?[0];
            let extracted_path = i
                .paths
                .state
                .join("transactions")
                .join(&j.transaction_id)
                .join("extracted");
            dir.child("extracted", true)?;
            update::extract_archive(
                dir.open(&asset.name)?,
                &extracted_path,
                asset.size.saturating_mul(8).min(32 * 1024 * 1024 * 1024),
            )?;
            let package_root = native::archive_package_root(&extracted_path, asset, version)?;
            native::verify_closure(&package_root, version)?;
        }
        store.accept(&candidate)?;
        store.save(&j)?;
        Ok(update_result(
            UpdateState::Prepared,
            Some(version.into()),
            Some(j.transaction_id.clone()),
            None,
        ))
    })();
    if let Err(e) = &result {
        j.transition(Phase::Cancelled)?;
        j.detail = Some(e.message.clone());
        store.save(&j)?;
    }
    result
}
fn ensure_space(path: &Path, required: u64) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let p = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::new(ErrorCode::InvalidInstallation, "NUL in storage path"))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(p.as_ptr(), &mut stat) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let available = (stat.f_bavail as u128) * (stat.f_frsize as u128);
    if available < required as u128 {
        return Err(Error::new(
            ErrorCode::Unavailable,
            "insufficient free space for complete stage, retained artifacts and recovery capsule",
        ));
    }
    Ok(())
}
#[allow(
    clippy::disallowed_methods,
    reason = "Immutable detached updater must survive UI and daemon replacement without daemon activity/store ownership"
)]
pub fn apply_update(i: &InstallationV1, id: &str) -> Result<ControlResultV1> {
    require_owner(i)?;
    let store = TransactionStore::locked(i)?;
    let mut j = store.load(id)?;
    if j.phase != Phase::Prepared || j.operation != Operation::Upgrade {
        return Err(Error::new(
            ErrorCode::InvalidRequest,
            "only a prepared upgrade can be applied",
        ));
    }
    transaction::reverify_candidate(i, &store, &j)?;
    if j.worker_pid
        .zip(j.worker_generation.as_deref())
        .is_some_and(|(pid, g)| process::inspect(pid).is_ok_and(|p| p.generation == g))
    {
        return Err(Error::new(
            ErrorCode::Busy,
            "prepared transaction already has a live worker",
        ));
    }
    let worker_source = if i.method == InstallMethod::MacosNative {
        i.paths.state.join("plurx-update-worker")
    } else {
        "/usr/lib/plurx-control/plurx-update-worker".into()
    };
    let capsule = transaction::seal_worker(i, &store, &mut j, &worker_source)?;
    // Detach from UI lifetime; ownership and publication remain in the locked journal.
    let mut command = Command::new(capsule);
    command
        .args(["--transaction", id])
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    let worker = process::inspect(child.id())?;
    j.worker_pid = Some(worker.pid);
    j.worker_generation = Some(worker.generation);
    store.save(&j)?;
    Ok(update_result(
        UpdateState::WaitingForIdle,
        j.target_version,
        Some(id.into()),
        Some("Independent immutable worker launched; poll updates status".into()),
    ))
}
pub fn cancel_update(i: &InstallationV1, id: &str) -> Result<ControlResultV1> {
    require_owner(i)?;
    let store = TransactionStore::locked(i)?;
    let mut j = store.load(id)?;
    if j.phase != Phase::Prepared {
        return Err(Error::new(
            ErrorCode::UnsafeShutdown,
            "cancellation cannot release a committed shutdown fence",
        ));
    }
    j.transition(Phase::Cancelled)?;
    store.save(&j)?;
    Ok(update_result(
        UpdateState::Cancelled,
        j.target_version,
        Some(id.into()),
        None,
    ))
}
pub fn run_worker(i: &InstallationV1, id: &str) -> Result<()> {
    require_owner(i)?;
    let until = Instant::now() + Duration::from_secs(10);
    let store = loop {
        match TransactionStore::locked(i) {
            Ok(s) => break s,
            Err(e) if e.code == ErrorCode::Busy && Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(50))
            }
            Err(e) => return Err(e),
        }
    };
    let mut j = store.load(id)?;
    if j.phase != Phase::Prepared || j.operation != Operation::Upgrade {
        return Err(Error::new(
            ErrorCode::RecoveryRequired,
            "worker requires exact prepared approved upgrade",
        ));
    }
    transaction::verify_capsule(&store, &j)?;
    let me = process::inspect(std::process::id())?;
    if j.worker_pid != Some(me.pid) || j.worker_generation.as_deref() != Some(&me.generation) {
        return Err(Error::new(
            ErrorCode::IdentityMismatch,
            "worker handoff is not bound to this exact process generation",
        ));
    }
    j.worker_pid = Some(me.pid);
    j.worker_generation = Some(me.generation);
    store.save(&j)?;
    let result = perform_update(i, &store, &mut j);
    if let Err(e) = &result {
        record_failure(&store, &mut j, e)?;
    }
    result
}
fn perform_update(i: &InstallationV1, store: &TransactionStore, j: &mut JournalV1) -> Result<()> {
    let candidate = transaction::reverify_candidate(i, store, j)?;
    let package_plan = if i.method != InstallMethod::MacosNative {
        let p = package::preflight(i, &candidate, store, j)?;
        if j.package_inventory_before.as_deref() != Some(&package::inventory(i, &p.packages)?) {
            return Err(Error::new(
                ErrorCode::PolicyConflict,
                "package inventory changed after preparation",
            ));
        }
        Some(p)
    } else {
        None
    };
    transaction::stop_clean(i, store, j)?;
    let install_lock = crate::launch_lock::acquire(i, true)?;
    if !supervisor::observe(i)?.stopped() {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "external daemon/wrapper startup raced installation; no replacement attempted",
        ));
    }
    transaction::revalidate_policy(i, j)?;
    // Independent trusted bytes recheck immediately before installation. No network beyond this fence.
    transaction::reverify_candidate(i, store, j)?;
    if package_plan.is_some() {
        if let Err(error) = crate::package_locks::check(i) {
            if error.code == ErrorCode::Busy {
                // No manager has been invoked and no bytes changed. Restore only
                // the verified old release and exactly the initial running state.
                j.transition(Phase::Restoring)?;
                store.save(j)?;
                drop(install_lock);
                if j.initial_running {
                    authorize_start(store, j)?;
                    supervisor::start(i)?;
                    wait_ready(i, j, Some(&i.current_commit))?;
                }
                j.start_permit_expires = None;
                j.transition(Phase::Cancelled)?;
                store.save(j)?;
            }
            return Err(error);
        }
    }
    j.transition(Phase::Installing)?;
    store.save(j)?;
    if let Some(plan) = package_plan {
        let exit = package::install(&plan)?;
        let after = package::inventory(i, &plan.packages)?;
        j.package_inventory_after = Some(after.clone());
        store.save(j)?;
        finalize_package_install_with(
            i,
            &plan,
            exit,
            &after,
            supervisor::reload_after_package_install,
        )?;
    } else {
        let extracted = i
            .paths
            .state
            .join("transactions")
            .join(&j.transaction_id)
            .join("extracted");
        let artifact = update::select_cohort(&candidate, i)?[0];
        let package_root =
            native::archive_package_root(&extracted, artifact, &candidate.manifest().version)?;
        native::promote(i, store, j, &package_root)?;
    }
    j.transition(Phase::CandidateInstalled)?;
    store.save(j)?;
    let mut installed = i.clone();
    installed.current_version = candidate.manifest().version.clone();
    installed.current_tag = candidate.manifest().tag.clone();
    installed.current_commit = candidate.manifest().source_commit.clone();
    if installed.method == InstallMethod::MacosNative {
        installed.architecture = Architecture::Universal;
    }
    installed.trust_key_ids = native::installed_trust_keys(&installed, candidate.key_id())?;
    installed.last_transaction_id = Some(j.transaction_id.clone());
    save_installation(&installed)?;
    transaction::revalidate_policy(&installed, j)?;
    j.transition(Phase::Restoring)?;
    store.save(j)?;
    drop(install_lock);
    if j.initial_running {
        authorize_start(store, j)?;
        supervisor::start(&installed)?;
        wait_ready(&installed, j, Some(&candidate.manifest().source_commit))?;
    } else if !supervisor::observe(&installed)?.stopped() {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "initially stopped installation was started externally; restoration held",
        ));
    }
    j.start_permit_expires = None;
    j.transition(Phase::Committed)?;
    store.save(j)
}
/// The worker owns systemd reload after its exact signed package cohort commits.
/// Keep the executor injectable for file-only regression fixtures; production
/// always supplies the fixed bounded native reload operation above.
pub fn finalize_package_install_with(
    i: &InstallationV1,
    plan: &package::PackagePlan,
    exit: i32,
    after: &str,
    reload: impl FnOnce(&InstallationV1) -> Result<()>,
) -> Result<()> {
    if exit != 0 {
        return Err(Error::new(ErrorCode::RecoveryRequired, format!("package manager exited {exit}; preserve stopped daemon and inspect exact package inventory; never automatically restore DB/Raft")));
    }
    package::validate_installed(i, plan, after)?;
    reload(i)
}

pub fn save_installation(i: &InstallationV1) -> Result<()> {
    i.validate()?;
    let path = default_record()?;
    secure_fs::open_dir(
        path.parent()
            .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "record parent"))?,
        i.owner_uid,
    )?
    .write(
        "installation.json",
        &serde_json::to_vec(i)?,
        if i.method == InstallMethod::MacosNative {
            0o600
        } else {
            0o644
        },
    )
}
/// Bounded boot reconciliation never invokes supervisors or package managers (a
/// service ordered before plurxd must not synchronously start that same unit).
/// An untouched preparation can be cancelled without GUI availability; later
/// phases retain their durable evidence and use the read-only start guard.
pub fn boot_reconcile(i: &InstallationV1) -> Result<()> {
    require_owner(i)?;
    let root = i.state_dir()?;
    let transactions = root.child("transactions", false)?;
    if transaction::read_active(&root, &transactions)?.is_some_and(|j| j.phase == Phase::Prepared) {
        let store = TransactionStore::locked(i)?;
        if let Some(mut j) = store.active()? {
            if j.installation_id != i.installation_id {
                return Err(Error::new(
                    ErrorCode::RecoveryRequired,
                    "boot journal belongs to another installation",
                ));
            }
            if j.phase == Phase::Prepared {
                j.transition(Phase::Cancelled)?;
                j.start_permit_expires = None;
                j.detail = Some("Boot reconciliation cancelled unchanged prepared transaction; no daemon or package mutation".into());
                store.save(&j)?;
            }
        }
    }
    transaction::guard(i)
}
/// Explicit recovery never fetches or starts a new release. Unknown/partial package state
/// stays stopped with a repair instruction; no nested apt/dnf from a boot/prestart hook.
pub fn recover(i: &InstallationV1, id: &str) -> Result<()> {
    require_owner(i)?;
    let store = TransactionStore::locked(i)?;
    let mut j = store.load(id)?;
    if j.installation_id != i.installation_id
        || store
            .active()?
            .as_ref()
            .map(|active| active.transaction_id.as_str())
            != Some(id)
    {
        return Err(Error::new(
            ErrorCode::RecoveryRequired,
            "recovery requires the active transaction for this installation",
        ));
    }
    if j.phase == Phase::ShutdownCommitted {
        if i.current_version != j.old_version {
            return Err(Error::new(
                ErrorCode::RecoveryRequired,
                "old binary identity changed; explicit repair required",
            ));
        }
        let retained: InstallationV1 = store.dir(id)?.json("installation-before.json")?;
        retained.validate()?;
        if retained.installation_id != i.installation_id
            || retained.current_commit != i.current_commit
            || retained.current_version != j.old_version
            || retained.owner_uid != i.owner_uid
            || retained.service_uid != i.service_uid
        {
            return Err(Error::new(
                ErrorCode::RecoveryRequired,
                "retained installation identity changed; explicit repair required",
            ));
        }
        transaction::revalidate_policy(i, &j)?;
        let receipt: ShutdownReceiptV1 = i
            .receipt_dir()?
            .json(&format!("{}.json", j.transaction_id))?;
        let expected = j.process.as_ref().ok_or_else(|| {
            Error::new(
                ErrorCode::UnsafeShutdown,
                "committed shutdown has no bound process identity",
            )
        })?;
        if expected.uid != i.service_uid {
            return Err(Error::new(
                ErrorCode::UnsafeShutdown,
                "committed process does not belong to the installed service UID",
            ));
        }
        let process_gone = !process::still_same(&process::KernelProcess {
            pid: expected.pid,
            uid: expected.uid,
            generation: expected.start_generation.clone(),
        })?;
        let observed = transaction::revalidate_policy(i, &j)?;
        j.complete_clean_shutdown(receipt, process_gone, observed.stopped())?;
        store.save(&j)?;
    }
    match j.phase {
        Phase::Committed | Phase::Cancelled => Ok(()),
        Phase::Prepared => { j.transition(Phase::Cancelled)?; store.save(&j) },
        Phase::CleanStop => {
            transaction::revalidate_policy(i, &j)?;
            if i.current_version != j.old_version { return Err(Error::new(ErrorCode::RecoveryRequired, "old binary identity changed; explicit repair required")); }
            j.transition(Phase::Restoring)?;
            if j.initial_running && j.operation != Operation::Stop { authorize_start(&store, &mut j)?; supervisor::start(i)?; wait_ready(i, &j, None)?; }
            j.transition(Phase::Committed)?; j.start_permit_expires = None; store.save(&j)
        }
        Phase::Restoring if i.method == InstallMethod::MacosNative && j.operation == Operation::Upgrade => {
            native::verify_promoted_installation(i, &store, &j)?;
            let observed = transaction::revalidate_policy(i, &j)?;
            if !j.initial_running {
                if !observed.stopped() {
                    return Err(Error::new(ErrorCode::RecoveryRequired, "candidate must preserve the originally stopped state; recovery held"));
                }
            } else {
                let bound = BoundDaemon::connect(i)?;
                if !readiness_matches(i, &j, &bound.identity, Some(&i.current_commit)) {
                    return Err(Error::new(ErrorCode::RecoveryRequired, "candidate did not prove expected build/node/readiness; recovery held"));
                }
                bound.revalidate()?;
            }
            transaction::revalidate_policy(i, &j)?;
            j.transition(Phase::Committed)?;
            j.start_permit_expires = None;
            j.detail = None;
            store.save(&j)
        }
        Phase::Installing if i.method == InstallMethod::MacosNative && j.rollback == update::Rollback::BinaryCompatible => {
            let _install_lock = crate::launch_lock::acquire(i, true)?;
            if !supervisor::observe(i)?.stopped() { return Err(Error::new(ErrorCode::RecoveryRequired, "cannot restore a runtime while a daemon or wrapper process exists")); }
            let old: InstallationV1 = store.dir(id)?.json("installation-before.json")?;
            old.validate()?;
            if old.installation_id != i.installation_id || old.current_version != j.old_version { return Err(Error::new(ErrorCode::RecoveryRequired, "retained installation identity differs")); }
            native::rollback(&old, &store, &j)?;
            save_installation(&old)?;
            j.detail = Some("Retained compatible native closure restored; no data restored. Explicit Start may follow inspection.".into());
            j.phase = Phase::Committed; store.save(&j)
        }
        _ => Err(Error::new(ErrorCode::RecoveryRequired, format!("transaction at {:?} requires explicit exact package/closure repair and identity verification; no forced stop or automatic DB/Raft restore", j.phase))),
    }
}

#[cfg(test)]
mod startup_path_tests {
    use super::*;
    use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};

    #[test]
    fn xdg_writer_and_reader_resolve_the_same_absolute_config_home() {
        let xdg = OsStr::new("/custom/config");
        let home = OsStr::new("/user/home");
        for writing in [false, true] {
            assert_eq!(
                tray_config_home(Some(xdg), Some(home), writing).expect("XDG"),
                Some(PathBuf::from("/custom/config"))
            );
            assert_eq!(
                tray_config_home(None, Some(home), writing).expect("HOME"),
                Some(PathBuf::from("/user/home/.config"))
            );
            assert_eq!(
                tray_config_home(Some(xdg), Some(OsStr::new("relative")), writing)
                    .expect("XDG wins"),
                Some(PathBuf::from("/custom/config"))
            );
        }
    }
    #[test]
    fn xdg_writer_refuses_invalid_set_paths_while_reader_ignores_them() {
        let home = OsStr::new("/user/home");
        for invalid in [
            std::ffi::OsString::from(""),
            "relative/config".into(),
            "/bad\0path".into(),
            std::ffi::OsString::from_vec(b"/bad\xff".to_vec()),
        ] {
            assert_eq!(
                tray_config_home(Some(&invalid), Some(home), true)
                    .expect_err("must refuse writing")
                    .code,
                ErrorCode::PolicyConflict
            );
            assert_eq!(
                tray_config_home(Some(&invalid), Some(home), false)
                    .expect("read ignores invalid XDG"),
                Some(PathBuf::from("/user/home/.config"))
            );
            assert_eq!(
                tray_config_home(None, Some(&invalid), true)
                    .expect_err("invalid HOME")
                    .code,
                ErrorCode::PolicyConflict
            );
            assert_eq!(
                tray_config_home(None, Some(&invalid), false).expect("no user override"),
                None
            );
        }
    }
    #[test]
    fn xdg_tray_override_write_and_checkmark_readback_share_the_selected_file() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/desktop-startup-tests")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).expect("scratch");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("private scratch");
        let root = root.canonicalize().expect("absolute scratch");
        let xdg = root.join("custom-config");
        let home = root.join("home");
        let system = root.join("system");
        std::fs::create_dir(&system).expect("system fixture");
        let system_file = system.join(TRAY_ENTRY);
        std::fs::write(&system_file, "[Desktop Entry]\nHidden=false\n").expect("system entry");
        let config = tray_config_home(Some(xdg.as_os_str()), Some(home.as_os_str()), true)
            .expect("write selection")
            .expect("user config");
        let read_config = tray_config_home(Some(xdg.as_os_str()), Some(home.as_os_str()), false)
            .expect("read selection")
            .expect("user config");
        let local = read_config.join("autostart").join(TRAY_ENTRY);
        assert_eq!(
            read_tray_startup(Some(&local), &system_file)
                .expect("system default")
                .state,
            StartupState::Enabled
        );
        for enabled in [false, true] {
            write_tray_startup(&config, enabled).expect("write user override");
            assert_eq!(
                read_tray_startup(Some(&local), &system_file)
                    .expect("read user override")
                    .state,
                if enabled {
                    StartupState::Enabled
                } else {
                    StartupState::Disabled
                }
            );
        }
        assert!(!home.join(".config/autostart").join(TRAY_ENTRY).exists());
        std::fs::remove_dir_all(root).expect("remove fixture");
    }
}
