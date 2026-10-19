//! Fixed native supervisor operations. Never invokes a shell or a forced Stop/Restart.
use crate::{
    installation::{InstallMethod, InstallationV1, SYSTEM_UNIT},
    process,
    protocol::{ErrorCode, ServiceState, StartupState, StartupStateV1, SupervisorIdentityV1},
    Error, Result,
};
use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const SYSTEMCTL: &str = "/usr/bin/systemctl";
pub const LAUNCHCTL: &str = "/bin/launchctl";
#[derive(Debug, Clone)]
pub struct Observation {
    pub state: ServiceState,
    pub startup: StartupStateV1,
    pub process: Option<process::KernelProcess>,
    pub supervisor: SupervisorIdentityV1,
    pub policy_revision: String,
}
/// Bounded subprocess output and lifetime. Not used for a package manager that has started installing.
#[allow(
    clippy::disallowed_methods,
    reason = "Native supervisor controller is independent of daemon store/activity machinery; child is bounded and explicitly reaped here"
)]
pub fn output(program: &str, args: &[String], timeout: Duration) -> Result<(i32, String)> {
    let mut child = Command::new(program)
        .args(args)
        .env_remove("LD_PRELOAD")
        .env_remove("DYLD_INSERT_LIBRARIES")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::new(ErrorCode::Io, "child stdout unavailable"))?;
    let reader = std::thread::spawn(move || {
        let mut b = Vec::new();
        stdout.take(1024 * 1024 + 1).read_to_end(&mut b).map(|_| b)
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break s;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::new(
                ErrorCode::Unavailable,
                "native supervisor query timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let bytes = reader
        .join()
        .map_err(|_| Error::new(ErrorCode::Io, "native query reader failed"))??;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::new(
            ErrorCode::Unavailable,
            "native output exceeds limit",
        ));
    }
    Ok((
        status.code().unwrap_or(-1),
        String::from_utf8_lossy(&bytes).into_owned(),
    ))
}
fn fixed(program: &str, args: &[String]) -> Result<String> {
    let (status, text) = output(program, args, Duration::from_secs(10))?;
    if status != 0 {
        return Err(Error::new(
            ErrorCode::AuthorizationRequired,
            format!(
                "native operation failed (exit {status}); check authorization and external policy"
            ),
        ));
    }
    Ok(text)
}
fn s(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_string()).collect()
}
/// The authenticated package worker owns this reload after exact-cohort
/// installation and before reading back policy. It never changes startup.
pub fn reload_after_package_install(i: &InstallationV1) -> Result<()> {
    i.managed()?;
    reload_after_package_install_with(i, output)
}

/// Executor seam for staging package-owned unit changes without a live service.
/// The production entry point checks native platform authority first.
pub fn reload_after_package_install_with<F>(i: &InstallationV1, mut execute: F) -> Result<()>
where
    F: FnMut(&str, &[String], Duration) -> Result<(i32, String)>,
{
    match i.method {
        InstallMethod::MacosNative => return Ok(()),
        InstallMethod::LinuxDeb | InstallMethod::LinuxRpm
            if i.service.kind == crate::protocol::SupervisorKind::Systemd
                && i.service.name == SYSTEM_UNIT
                && i.service.domain == "system" => {}
        _ => {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "package reload requires a managed native installation",
            ))
        }
    }
    let (code, _) = execute(
        SYSTEMCTL,
        &s(&["--no-ask-password", "daemon-reload"]),
        Duration::from_secs(10),
    )?;
    if code != 0 {
        return Err(Error::new(
            ErrorCode::AuthorizationRequired,
            format!("systemd daemon-reload failed (exit {code})"),
        ));
    }
    Ok(())
}

/// Include drop-in names, order and bytes so a package-triggered reload cannot
/// conceal an outside drop-in edit. The packaged main fragment may change.
pub fn systemd_policy_revision(
    unit_file_state: &str,
    fragment_path: &str,
    need_reload: &str,
    drop_ins: &[(String, Vec<u8>)],
) -> String {
    use sha2::Digest;
    let mut hash = sha2::Sha256::new();
    for (path, bytes) in drop_ins {
        hash.update((path.len() as u64).to_be_bytes());
        hash.update(path.as_bytes());
        hash.update((bytes.len() as u64).to_be_bytes());
        hash.update(bytes);
    }
    format!(
        "{unit_file_state}|{fragment_path}|{need_reload}|{}",
        hex::encode(hash.finalize())
    )
}

fn systemd_drop_ins(paths: &str) -> Result<Vec<(String, Vec<u8>)>> {
    let mut files = Vec::new();
    let mut total = 0;
    for name in paths.split_whitespace() {
        if files.len() >= 32 {
            return Err(Error::new(
                ErrorCode::PolicyConflict,
                "systemd drop-in count exceeds limit",
            ));
        }
        let path = Path::new(name);
        let parent = path
            .parent()
            .ok_or_else(|| Error::new(ErrorCode::PolicyConflict, "invalid systemd drop-in path"))?;
        let leaf = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::new(ErrorCode::PolicyConflict, "invalid systemd drop-in name"))?;
        let bytes = crate::secure_fs::open_dir(parent, 0)?.read(leaf, 256 * 1024)?;
        total += bytes.len();
        if total > 1024 * 1024 {
            return Err(Error::new(
                ErrorCode::PolicyConflict,
                "systemd drop-in bytes exceed limit",
            ));
        }
        files.push((name.to_owned(), bytes));
    }
    Ok(files)
}

impl Observation {
    pub fn stopped(&self) -> bool {
        self.state == ServiceState::Stopped && self.process.is_none()
    }
}
pub fn observe(i: &InstallationV1) -> Result<Observation> {
    i.managed()?;
    if i.method == InstallMethod::MacosNative {
        observe_launchd(i)
    } else {
        observe_systemd(i)
    }
}
fn observe_systemd(i: &InstallationV1) -> Result<Observation> {
    let (code, text) = output(SYSTEMCTL, &s(&["show", SYSTEM_UNIT, "--no-pager", "--property=LoadState,ActiveState,SubState,MainPID,InvocationID,UnitFileState,FragmentPath,NeedDaemonReload,Job,DropInPaths"]), Duration::from_secs(2))?;
    if code != 0 {
        return Err(Error::new(
            ErrorCode::Unavailable,
            "systemd unit status unavailable",
        ));
    }
    let props: BTreeMap<_, _> = text.lines().filter_map(|l| l.split_once('=')).collect();
    let get = |key| props.get(key).copied().unwrap_or("");
    if get("LoadState") != "loaded" {
        return Err(Error::new(
            ErrorCode::Unsupported,
            "plurxd unit is missing, masked or not loaded",
        ));
    }
    let state = match get("ActiveState") {
        "active" => ServiceState::Running,
        "activating" => ServiceState::Starting,
        "inactive" => ServiceState::Stopped,
        "failed" => ServiceState::Unhealthy,
        _ => ServiceState::Unknown,
    };
    let startup = StartupStateV1::new(match get("UnitFileState") {
        "enabled" | "enabled-runtime" => StartupState::Enabled,
        "disabled" => StartupState::Disabled,
        "masked" | "masked-runtime" => StartupState::Masked,
        "static" | "indirect" | "generated" => StartupState::Static,
        _ => StartupState::Unknown,
    });
    let pid: u32 = get("MainPID")
        .parse()
        .map_err(|_| Error::new(ErrorCode::IdentityMismatch, "invalid MainPID"))?;
    let process = if pid == 0 {
        None
    } else {
        let p = process::inspect(pid)?;
        if p.uid != i.service_uid {
            return Err(Error::new(
                ErrorCode::IdentityMismatch,
                "systemd MainPID has wrong service UID",
            ));
        }
        Some(p)
    };
    let invocation_id = get("InvocationID").to_string();
    if process.is_some() && invocation_id.is_empty() {
        return Err(Error::new(
            ErrorCode::IdentityMismatch,
            "missing systemd invocation identity",
        ));
    }
    let drop_in_paths = props.get("DropInPaths").ok_or_else(|| {
        Error::new(
            ErrorCode::PolicyConflict,
            "systemd drop-in policy cannot be established",
        )
    })?;
    let revision = systemd_policy_revision(
        get("UnitFileState"),
        get("FragmentPath"),
        get("NeedDaemonReload"),
        &systemd_drop_ins(drop_in_paths)?,
    );
    if get("NeedDaemonReload") == "yes" || (get("Job") != "0" && !get("Job").is_empty()) {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "external systemd job or un-reloaded unit change",
        ));
    }
    Ok(Observation {
        state,
        startup,
        process,
        supervisor: SupervisorIdentityV1 {
            kind: i.service.kind.clone(),
            name: SYSTEM_UNIT.into(),
            invocation_id,
        },
        policy_revision: revision,
    })
}
fn canonical_launchd_bytes(i: &InstallationV1) -> Result<Vec<u8>> {
    let path = i
        .paths
        .canonical_plist
        .as_ref()
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "canonical plist absent"))?;
    crate::secure_fs::open_dir(
        path.parent()
            .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "canonical parent"))?,
        i.owner_uid,
    )?
    .read("com.plurx.plurxd.plist", 1024 * 1024)
}

fn launchd_login_parent(
    i: &InstallationV1,
    create: bool,
) -> Result<Option<crate::secure_fs::SecureDir>> {
    let login = i
        .paths
        .login_plist
        .as_ref()
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "login plist absent"))?;
    let agents = login
        .parent()
        .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "login parent"))?;
    if login.file_name().and_then(|n| n.to_str()) != Some("com.plurx.plurxd.plist")
        || agents.file_name().and_then(|n| n.to_str()) != Some("LaunchAgents")
    {
        return Err(Error::new(
            ErrorCode::InvalidInstallation,
            "noncanonical login registration",
        ));
    }
    let library = crate::secure_fs::open_dir(
        agents
            .parent()
            .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "Library parent"))?,
        i.owner_uid,
    )?;
    match library.child("LaunchAgents", create) {
        Ok(parent) => Ok(Some(parent)),
        Err(error) if !create && error.os_error == Some(libc::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Readback never creates LaunchAgents and never follows a registration link.
pub fn launchd_login_registered(i: &InstallationV1) -> Result<bool> {
    let canonical = canonical_launchd_bytes(i)?;
    let Some(parent) = launchd_login_parent(i, false)? else {
        return Ok(false);
    };
    let bytes = match parent.read("com.plurx.plurxd.plist", 1024 * 1024) {
        Ok(bytes) => bytes,
        Err(error) if error.os_error == Some(libc::ENOENT) => return Ok(false),
        Err(error) => return Err(error),
    };
    if bytes != canonical {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "login registration differs from managed canonical plist; explicit migration required",
        ));
    }
    Ok(true)
}

/// Changes only the owner's future login registration. The canonical record is
/// validated by the caller; all traversed directories/files remain no-follow.
pub fn set_launchd_login_registration(i: &InstallationV1, enabled: bool) -> Result<()> {
    let registered = launchd_login_registered(i)?;
    if enabled {
        let source = canonical_launchd_bytes(i)?;
        let parent = launchd_login_parent(i, true)?
            .ok_or_else(|| Error::new(ErrorCode::InvalidInstallation, "login parent absent"))?;
        parent.write("com.plurx.plurxd.plist", &source, 0o600)?;
    } else if registered {
        let parent = launchd_login_parent(i, false)?.ok_or_else(|| {
            Error::new(ErrorCode::PolicyConflict, "login registration disappeared")
        })?;
        parent.remove("com.plurx.plurxd.plist")?;
    }
    Ok(())
}

fn observe_launchd(i: &InstallationV1) -> Result<Observation> {
    let target = format!("{}/{}", i.service.domain, i.service.name);
    let (code, text) = output(LAUNCHCTL, &s(&["print", &target]), Duration::from_secs(2))?;
    let (_, disabled) = output(
        LAUNCHCTL,
        &s(&["print-disabled", &i.service.domain]),
        Duration::from_secs(2),
    )?;
    let disabled_key = format!("\"{}\" => true", i.service.name);
    let external_disabled = disabled.contains(&disabled_key);
    let canonical_bytes = canonical_launchd_bytes(i)?;
    let registered = launchd_login_registered(i)?;
    let startup = StartupStateV1::new(if external_disabled {
        StartupState::PolicyManaged
    } else if registered {
        StartupState::Enabled
    } else {
        StartupState::Disabled
    });
    let pid = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("pid = "))
        .find_map(|s| s.parse::<u32>().ok());
    let process = pid.map(process::inspect).transpose()?;
    if process.as_ref().is_some_and(|p| p.uid != i.service_uid) {
        return Err(Error::new(
            ErrorCode::IdentityMismatch,
            "launchd PID belongs to wrong UID",
        ));
    }
    // launchctl returns 113 when the exact service does not exist. Other errors are unknown, not stopped.
    let state = if process.is_some() {
        ServiceState::Running
    } else if code == 113 || code == 0 {
        ServiceState::Stopped
    } else {
        ServiceState::Unknown
    };
    let generation = process
        .as_ref()
        .map(|p| p.generation.clone())
        .unwrap_or_default();
    use sha2::Digest;
    let revision = format!(
        "{}|{}|{}",
        external_disabled,
        registered,
        hex::encode(sha2::Sha256::digest(&canonical_bytes))
    );
    Ok(Observation {
        state,
        startup,
        process,
        supervisor: SupervisorIdentityV1 {
            kind: i.service.kind.clone(),
            name: i.service.name.clone(),
            invocation_id: generation,
        },
        policy_revision: revision,
    })
}
pub fn start(i: &InstallationV1) -> Result<Observation> {
    let before = observe(i)?;
    if before.process.is_some() {
        return Ok(before);
    }
    if !before.stopped() {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "service is not positively stopped",
        ));
    }
    if matches!(
        before.startup.state,
        StartupState::Masked | StartupState::PolicyManaged | StartupState::Unknown
    ) {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "external service policy requires explicit enable/repair",
        ));
    }
    if i.method == InstallMethod::MacosNative {
        let canonical =
            i.paths.canonical_plist.as_ref().ok_or_else(|| {
                Error::new(ErrorCode::InvalidInstallation, "canonical plist missing")
            })?;
        let target = format!("{}/{}", i.service.domain, i.service.name);
        let (code, _) = output(LAUNCHCTL, &s(&["print", &target]), Duration::from_secs(2))?;
        if code == 113 {
            fixed(
                LAUNCHCTL,
                &s(&[
                    "bootstrap",
                    &i.service.domain,
                    canonical.to_str().ok_or_else(|| {
                        Error::new(ErrorCode::InvalidInstallation, "plist encoding")
                    })?,
                ]),
            )?;
        } else if code != 0 {
            return Err(Error::new(
                ErrorCode::Unavailable,
                "cannot determine launchd registration",
            ));
        }
        // No -k: never kills a live process. No enable: respects outside disable.
        fixed(LAUNCHCTL, &s(&["kickstart", &target]))?;
    } else {
        fixed(SYSTEMCTL, &s(&["start", SYSTEM_UNIT]))?;
    }
    observe(i)
}
pub fn set_startup(i: &InstallationV1, enabled: bool) -> Result<Observation> {
    let before = observe(i)?;
    if !matches!(
        before.startup.state,
        StartupState::Enabled | StartupState::Disabled
    ) {
        return Err(Error::new(ErrorCode::PolicyConflict, "startup is externally disabled, masked, static or unknown; explicit administrator repair required"));
    }
    if i.method == InstallMethod::MacosNative {
        set_launchd_login_registration(i, enabled)?;
        // Do not bootout, enable or disable the loaded job. This changes only future login registration.
    } else {
        fixed(
            SYSTEMCTL,
            &s(&[if enabled { "enable" } else { "disable" }, SYSTEM_UNIT]),
        )?;
    }
    let after = observe(i)?;
    if (after.startup.state == StartupState::Enabled) != enabled {
        return Err(Error::new(
            ErrorCode::PolicyConflict,
            "effective startup policy did not match requested setting",
        ));
    }
    Ok(after)
}
pub fn logs(i: &InstallationV1, lines: usize) -> Result<(Vec<String>, bool)> {
    if !(1..=1000).contains(&lines) {
        return Err(Error::new(
            ErrorCode::InvalidRequest,
            "logs lines must be 1..1000",
        ));
    }
    if i.method == InstallMethod::MacosNative {
        crate::secure_fs::log_tail(&i.paths.log, i.owner_uid, lines)
    } else {
        let (code, text) = output(
            "/usr/bin/journalctl",
            &s(&[
                "--unit=plurxd.service",
                "--no-pager",
                "--output=short-iso",
                &format!("--lines={lines}"),
            ]),
            Duration::from_secs(2),
        )?;
        if code != 0 {
            return Err(Error::new(
                ErrorCode::AuthorizationRequired,
                "cannot read plurxd journal; check journal permissions",
            ));
        }
        Ok((text.lines().map(str::to_string).collect(), true))
    }
}
