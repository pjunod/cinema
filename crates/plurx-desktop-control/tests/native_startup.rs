#![cfg(unix)]
use plurx_desktop_control::{
    installation::*,
    native::{self, BootstrapDisposition, ClosureFileV1, RuntimeClosureV1},
    protocol::{ErrorCode, SupervisorKind},
    secure_fs, supervisor, Error,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/native-startup-tests")
            .join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&path).expect("owned native fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
            .expect("owned native fixture");
        Self(path.canonicalize().expect("owned native fixture"))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn installation(home: &Path, method: InstallMethod) -> InstallationV1 {
    let runtime = home.join("Library/Application Support/plurx/runtime");
    InstallationV1 {
        schema_version: 1,
        installation_id: uuid::Uuid::new_v4().to_string(),
        method,
        owner_uid: native_uid(),
        service_uid: native_uid(),
        architecture: Architecture::Universal,
        desktop_installed: true,
        repository: REPOSITORY.into(),
        current_version: "0.7.0".into(),
        current_tag: "v0.7.0".into(),
        current_commit: "a".repeat(40),
        controller_protocol: 1,
        trust_key_ids: vec!["fixture-only".into()],
        last_transaction_id: None,
        paths: ManagedPathsV1 {
            state: runtime.join("control"),
            socket: runtime.join("control/desktop-control.sock"),
            runtime,
            config: home.join("config.toml"),
            data: home.join("data"),
            log: home.join("daemon.log"),
            canonical_plist: Some(
                home.join(
                    "Library/Application Support/plurx/runtime/control/com.plurx.plurxd.plist",
                ),
            ),
            login_plist: Some(home.join("Library/LaunchAgents/com.plurx.plurxd.plist")),
        },
        service: ServiceV1 {
            kind: SupervisorKind::Launchd,
            name: LAUNCH_LABEL.into(),
            domain: format!("gui/{}", native_uid()),
        },
        browser_origin: "http://127.0.0.1:32400".into(),
    }
}
fn canonical_fixture(home: &Path) -> InstallationV1 {
    let i = installation(home, InstallMethod::MacosNative);
    fs::create_dir_all(&i.paths.state).expect("owned native fixture");
    secure_fs::open_dir(&i.paths.state, native_uid())
        .expect("owned native fixture")
        .write("com.plurx.plurxd.plist", b"owned canonical fixture", 0o600)
        .expect("owned native fixture");
    i
}
fn package(root: &Path) {
    fs::create_dir_all(root).expect("owned native fixture");
    let mut files = Vec::new();
    let names = [
        "runtime/plurxd",
        "runtime/ffmpeg",
        "runtime/ffprobe",
        "Plurx Server.app/Contents/Info.plist",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(
        native::CONTROL_BINARIES
            .iter()
            .map(|name| format!("control/{name}")),
    );
    for name in names {
        let path = root.join(&name);
        fs::create_dir_all(path.parent().expect("owned native fixture"))
            .expect("owned native fixture");
        fs::write(&path, b"fixture").expect("owned native fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500))
            .expect("owned native fixture");
        files.push(ClosureFileV1 {
            path: name,
            size: 7,
            sha256: hex::encode(Sha256::digest(b"fixture")),
            executable: true,
        });
    }
    secure_fs::open_dir(root, native_uid())
        .expect("owned native fixture")
        .write_json(
            native::CLOSURE_NAME,
            &RuntimeClosureV1 {
                schema_version: 1,
                version: "0.7.0".into(),
                files,
            },
        )
        .expect("owned native fixture");
}

#[test]
fn managed_package_launcher_selects_real_daemon_under_shipped_symlink_layout() {
    let scratch = Scratch::new();
    let i = installation(&scratch.0, InstallMethod::LinuxDeb);
    let selected = native::daemon_path(&i).expect("owned native fixture");
    assert_eq!(selected, Path::new("/usr/lib/plurx/plurxd"));
    let real = scratch
        .0
        .join(selected.strip_prefix("/").expect("owned native fixture"));
    fs::create_dir_all(real.parent().expect("owned native fixture")).expect("owned native fixture");
    fs::create_dir_all(scratch.0.join("usr/bin")).expect("owned native fixture");
    fs::write(&real, b"packaged daemon").expect("owned native fixture");
    symlink("/usr/lib/plurx/plurxd", scratch.0.join("usr/bin/plurxd"))
        .expect("owned native fixture");
    assert_eq!(
        fs::read_link(scratch.0.join("usr/bin/plurxd")).expect("package symlink"),
        selected
    );
    let alias = secure_fs::open_dir(&scratch.0.join("usr/bin"), native_uid())
        .expect("owned native fixture");
    assert!(
        alias.open("plurxd").is_err(),
        "the package alias remains no-follow"
    );
    let directory = secure_fs::open_dir(real.parent().expect("owned native fixture"), native_uid())
        .expect("owned native fixture");
    assert!(
        directory.open("plurxd").is_ok(),
        "same managed-launch leaf validation succeeds"
    );
    fs::remove_file(&real).expect("owned native fixture");
    symlink("../../bin/plurxd", &real).expect("owned native fixture");
    assert!(
        directory.open("plurxd").is_err(),
        "do not weaken the real leaf's no-follow rule"
    );
}

#[test]
fn fresh_startup_on_creates_private_launchagents_off_and_readback_do_not() {
    let scratch = Scratch::new();
    let i = canonical_fixture(&scratch.0);
    let agents = scratch.0.join("Library/LaunchAgents");
    assert!(!supervisor::launchd_login_registered(&i).expect("owned native fixture"));
    supervisor::set_launchd_login_registration(&i, false).expect("owned native fixture");
    assert!(
        !agents.exists(),
        "readback and Off never create a registration directory"
    );
    supervisor::set_launchd_login_registration(&i, true).expect("owned native fixture");
    assert!(supervisor::launchd_login_registered(&i).expect("owned native fixture"));
    assert_eq!(
        fs::metadata(&agents).expect("owned native fixture").uid(),
        native_uid()
    );
    assert_eq!(
        fs::metadata(&agents).expect("owned native fixture").mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(i.paths.login_plist.as_ref().expect("owned native fixture"))
            .expect("owned native fixture")
            .mode()
            & 0o777,
        0o600
    );
    supervisor::set_launchd_login_registration(&i, false).expect("owned native fixture");
    assert!(!supervisor::launchd_login_registered(&i).expect("owned native fixture"));
    assert!(agents.exists());
}

#[test]
fn startup_refuses_linked_or_writable_directories_and_foreign_registration() {
    let scratch = Scratch::new();
    let i = canonical_fixture(&scratch.0);
    let agents = scratch.0.join("Library/LaunchAgents");
    let unrelated = scratch.0.join("unrelated");
    fs::create_dir(&unrelated).expect("owned native fixture");
    symlink(&unrelated, &agents).expect("owned native fixture");
    assert!(supervisor::set_launchd_login_registration(&i, true).is_err());
    assert!(supervisor::set_launchd_login_registration(&i, false).is_err());
    assert!(supervisor::launchd_login_registered(&i).is_err());
    assert!(fs::read_dir(&unrelated)
        .expect("owned native fixture")
        .next()
        .is_none());
    fs::remove_file(&agents).expect("owned native fixture");
    fs::create_dir(&agents).expect("owned native fixture");
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o777)).expect("owned native fixture");
    assert!(supervisor::set_launchd_login_registration(&i, true).is_err());
    assert!(supervisor::launchd_login_registered(&i).is_err());
    fs::set_permissions(&agents, fs::Permissions::from_mode(0o700)).expect("owned native fixture");
    let login = i.paths.login_plist.as_ref().expect("owned native fixture");
    fs::write(login, b"unrelated registration").expect("owned native fixture");
    assert_eq!(
        supervisor::set_launchd_login_registration(&i, false)
            .expect_err("unsafe native fixture must fail")
            .code,
        ErrorCode::PolicyConflict
    );
    assert_eq!(
        fs::read(login).expect("owned native fixture"),
        b"unrelated registration"
    );
    fs::remove_file(login).expect("owned native fixture");
    symlink(
        i.paths
            .canonical_plist
            .as_ref()
            .expect("owned native fixture"),
        login,
    )
    .expect("owned native fixture");
    assert!(supervisor::set_launchd_login_registration(&i, true).is_err());
    assert!(supervisor::set_launchd_login_registration(&i, false).is_err());
}

#[test]
fn startup_rejects_writable_or_linked_canonical_plist_before_creating_agents() {
    let scratch = Scratch::new();
    let i = canonical_fixture(&scratch.0);
    let canonical = i
        .paths
        .canonical_plist
        .as_ref()
        .expect("owned native fixture");
    fs::set_permissions(canonical, fs::Permissions::from_mode(0o666))
        .expect("owned native fixture");
    assert!(supervisor::set_launchd_login_registration(&i, true).is_err());
    assert!(!scratch.0.join("Library/LaunchAgents").exists());
    fs::remove_file(canonical).expect("owned native fixture");
    let outside = scratch.0.join("outside");
    fs::write(&outside, b"canonical").expect("owned native fixture");
    symlink(outside, canonical).expect("owned native fixture");
    assert!(supervisor::set_launchd_login_registration(&i, true).is_err());
    assert!(!scratch.0.join("Library/LaunchAgents").exists());
}

fn staged_bootstrap(
    legacy_loaded: bool,
    login_enabled: bool,
) -> (
    Scratch,
    InstallationV1,
    BootstrapDisposition,
    Vec<Vec<String>>,
) {
    let scratch = Scratch::new();
    let home = scratch.0.join("home");
    fs::create_dir(&home).expect("owned native fixture");
    let mut i = installation(&home, InstallMethod::MacosNative);
    if login_enabled {
        let login = i.paths.login_plist.as_ref().expect("owned native fixture");
        fs::create_dir_all(login.parent().expect("owned native fixture"))
            .expect("owned native fixture");
        let old = serde_json::json!({"Label": LAUNCH_LABEL, "RunAtLoad": true, "KeepAlive":{"SuccessfulExit":false},
            "EnvironmentVariables":{"PLURX_CONFIG": home.join("legacy.toml"), "PLURX_DATA_DIR": home.join("legacy-data")},
            "StandardOutPath":home.join("legacy.log")});
        fs::write(
            login,
            serde_json::to_vec(&old).expect("owned native fixture"),
        )
        .expect("owned native fixture");
    }
    let root = scratch.0.join("package");
    package(&root);
    let mut calls = Vec::new();
    let mut loaded = legacy_loaded;
    let disposition =
        native::bootstrap_macos_with(&mut i, &root, &home, |program, args, timeout| {
            assert!(timeout <= Duration::from_secs(5));
            calls.push([vec![program.into()], args.to_vec()].concat());
            if program == supervisor::LAUNCHCTL {
                match args[0].as_str() {
                    "print" if args[1].starts_with("system/") => Ok((113, String::new())),
                    "print" => Ok((if loaded { 0 } else { 113 }, "state = waiting\n".into())),
                    "bootout" => {
                        loaded = false;
                        Ok((0, String::new()))
                    }
                    _ => panic!("unexpected live-service mutation in staged bootstrap: {args:?}"),
                }
            } else {
                assert_eq!(program, "/usr/bin/plutil");
                if args[1] == "json" {
                    Ok((
                        0,
                        fs::read_to_string(args.last().expect("owned native fixture"))
                            .expect("owned native fixture"),
                    ))
                } else {
                    assert_eq!(args[1], "xml1");
                    // File-format bridge only; real native plutil runs against the
                    // same held fixture paths in production, never a test service.
                    fs::write(&args[3], fs::read(&args[4]).expect("owned native fixture"))
                        .expect("owned native fixture");
                    Ok((0, String::new()))
                }
            }
        })
        .expect("owned native fixture");
    assert!(!loaded, "bootstrap never starts an exited legacy service");
    assert!(home.join("Applications/Plurx Server.app").is_dir());
    assert!(i.paths.runtime.join("current/runtime/plurxd").is_file());
    assert!(i.paths.state.join("plurx-service-launch").is_file());
    (scratch, i, disposition, calls)
}

#[test]
fn stopped_enabled_legacy_first_adoption_preserves_state_and_login_policy() {
    let (_scratch, i, disposition, calls) = staged_bootstrap(true, true);
    assert_eq!(disposition, BootstrapDisposition::AdoptedStopped);
    assert!(
        !disposition.starts_initial_daemon(false),
        "default adoption cannot surprise-start the daemon"
    );
    assert!(
        supervisor::launchd_login_registered(&i).expect("owned native fixture"),
        "retain enabled login registration"
    );
    assert!(i.paths.config.ends_with("legacy.toml") && i.paths.data.ends_with("legacy-data"));
    let plist: serde_json::Value = serde_json::from_slice(
        &fs::read(
            i.paths
                .canonical_plist
                .as_ref()
                .expect("owned native fixture"),
        )
        .expect("owned native fixture"),
    )
    .expect("owned native fixture");
    assert_eq!(plist["RunAtLoad"], true);
    assert_eq!(
        plist["ProgramArguments"][0],
        i.paths
            .state
            .join("plurx-service-launch")
            .to_str()
            .expect("owned native fixture")
    );
    assert_eq!(
        calls
            .iter()
            .filter(|args| args.get(1).is_some_and(|s| s == "bootout"))
            .count(),
        1
    );
    assert!(calls.iter().all(|args| !args
        .iter()
        .any(|s| matches!(s.as_str(), "kickstart" | "bootstrap" | "enable" | "disable"))));
}

#[test]
fn unloaded_legacy_plist_and_loaded_legacy_without_login_are_not_fresh_installs() {
    for (loaded, enabled) in [(false, true), (true, false)] {
        let (_scratch, i, disposition, _) = staged_bootstrap(loaded, enabled);
        assert_eq!(disposition, BootstrapDisposition::AdoptedStopped);
        assert!(!disposition.starts_initial_daemon(false));
        assert_eq!(
            supervisor::launchd_login_registered(&i).expect("owned native fixture"),
            enabled
        );
    }
}

#[test]
fn fresh_archive_keeps_default_start_choice_without_enabling_login() {
    let (_scratch, i, disposition, _) = staged_bootstrap(false, false);
    assert_eq!(disposition, BootstrapDisposition::Fresh);
    assert!(disposition.starts_initial_daemon(false));
    assert!(!disposition.starts_initial_daemon(true));
    assert!(!BootstrapDisposition::ExistingRecord.starts_initial_daemon(false));
    assert!(!supervisor::launchd_login_registered(&i).expect("owned native fixture"));
    assert!(!i
        .paths
        .login_plist
        .as_ref()
        .expect("owned native fixture")
        .parent()
        .expect("owned native fixture")
        .exists());
}

#[test]
fn worker_reload_has_fixed_bounded_command_failure_and_macos_noop() {
    let scratch = Scratch::new();
    let mut i = installation(&scratch.0, InstallMethod::LinuxDeb);
    i.service = ServiceV1 {
        kind: SupervisorKind::Systemd,
        name: SYSTEM_UNIT.into(),
        domain: "system".into(),
    };
    let mut called = false;
    supervisor::reload_after_package_install_with(&i, |program, args, timeout| {
        called = true;
        assert_eq!(program, "/usr/bin/systemctl");
        assert_eq!(args, ["--no-ask-password", "daemon-reload"]);
        assert_eq!(timeout, Duration::from_secs(10));
        Ok((0, String::new()))
    })
    .expect("owned native fixture");
    assert!(called);
    assert_eq!(
        supervisor::reload_after_package_install_with(&i, |_, _, _| Ok((1, String::new())))
            .expect_err("unsafe native fixture must fail")
            .code,
        ErrorCode::AuthorizationRequired
    );
    assert_eq!(
        supervisor::reload_after_package_install_with(&i, |_, _, _| Err(Error::new(
            ErrorCode::Unavailable,
            "fixture timeout"
        )))
        .expect_err("unsafe native fixture must fail")
        .code,
        ErrorCode::Unavailable
    );
    i.method = InstallMethod::MacosNative;
    supervisor::reload_after_package_install_with(&i, |_, _, _| {
        panic!("macOS reload must run zero commands")
    })
    .expect("owned native fixture");
}

#[test]
fn reload_policy_revision_preserves_dropin_names_bytes_order_and_startup_choice() {
    let dropins = vec![(
        "/etc/systemd/system/plurxd.service.d/local.conf".into(),
        b"[Service]\nLimitNOFILE=8192\n".to_vec(),
    )];
    let revision = |startup, entries: &[(String, Vec<u8>)]| {
        supervisor::systemd_policy_revision(
            startup,
            "/usr/lib/systemd/system/plurxd.service",
            "no",
            entries,
        )
    };
    let before = revision("enabled", &dropins);
    assert_eq!(
        before,
        revision("enabled", &dropins),
        "a main-fragment replacement does not change outside policy"
    );
    assert_ne!(before, revision("disabled", &dropins));
    assert_ne!(before, revision("enabled", &[]));
    let changed = vec![(
        dropins[0].0.clone(),
        b"[Service]\nLimitNOFILE=16384\n".to_vec(),
    )];
    assert_ne!(before, revision("enabled", &changed));
    let renamed = vec![(
        "/run/systemd/system/plurxd.service.d/local.conf".into(),
        dropins[0].1.clone(),
    )];
    assert_ne!(before, revision("enabled", &renamed));
    let ordered = vec![dropins[0].clone(), renamed[0].clone()];
    let reversed = vec![renamed[0].clone(), dropins[0].clone()];
    assert_ne!(
        revision("enabled", &ordered),
        revision("enabled", &reversed)
    );
}
