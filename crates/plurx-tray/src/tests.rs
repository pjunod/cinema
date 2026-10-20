//! Portable regressions: protocol contract examples, argv shapes, the real
//! child-process runner, menu/window model, the effect core (quit vs stop,
//! authorization preservation, upgrade naming/cancel), session eligibility
//! refusals and the XDG autostart contract. No GTK, D-Bus or real controller.
#![allow(clippy::unwrap_used, reason = "test assertions")]

use crate::autostart::{self, Preference, ENTRY_NAME};
use crate::cli::{self, Mode, Parsed};
use crate::command::{is_transaction, is_version, Choice, Command};
use crate::core::{management_for, Core, Effect, Pending, Purpose};
use crate::model::{self, find, Action, Check, Management, Snapshot, TrayHost};
use crate::protocol::*;
use crate::receipt::{self, SmokeAction};
use crate::runner::{controller_environment, Runner};
use crate::session::{evaluate, is_local_x11, LogindSession, SessionFacts};
use std::path::PathBuf;

const STATUS: &str = r#"{"schema_version":1,"ok":true,"result":{"kind":"status","installation_id":"00000000-0000-4000-8000-000000000001","service_state":"stopped","version":"0.7.0","browser_origin":"http://127.0.0.1:8080","server_startup":{"state":"disabled","detail":null},"tray_startup":{"state":"unknown","detail":null},"transaction_id":null},"error":null}"#;
const TXN: &str = "00000000-0000-4000-8000-000000000002";

fn status(state: ServiceState, server: StartupState, tray: StartupState) -> StatusV1 {
    StatusV1 {
        installation_id: "00000000-0000-4000-8000-000000000001".into(),
        service_state: state,
        version: Some("0.7.0".into()),
        browser_origin: "http://127.0.0.1:32400".into(),
        server_startup: StartupStateV1 {
            state: server,
            detail: None,
        },
        tray_startup: StartupStateV1 {
            state: tray,
            detail: None,
        },
        transaction_id: None,
    }
}

fn snap(state: ServiceState) -> Snapshot {
    let mut s = Snapshot::new("0.7.0", Management::Supported);
    s.status = Some(status(state, StartupState::Enabled, StartupState::Enabled));
    s
}

fn update(state: UpdateState, version: &str) -> UpdateV1 {
    UpdateV1 {
        state,
        version: Some(version.into()),
        transaction_id: Some(TXN.into()),
        detail: None,
    }
}

// ---- protocol ---------------------------------------------------------------

#[test]
fn contract_examples_decode() {
    let ControlResult::Status(s) = decode_response(STATUS.as_bytes()).unwrap() else {
        panic!()
    };
    assert_eq!(s.service_state, ServiceState::Stopped);
    assert_eq!(s.server_startup.state, StartupState::Disabled);
    assert_eq!(s.transaction_id, None);
    let examples = [
        r#"{"schema_version":1,"ok":true,"result":{"kind":"operation","operation":"start","service_state":"starting"},"error":null}"#,
        r#"{"schema_version":1,"ok":true,"result":{"kind":"startup","server":{"state":"enabled","detail":null},"tray":{"state":"disabled","detail":null}},"error":null}"#,
        r#"{"schema_version":1,"ok":true,"result":{"kind":"update","state":"prepared","version":"0.7.1","transaction_id":"00000000-0000-4000-8000-000000000002","detail":null},"error":null}"#,
        r#"{"schema_version":1,"ok":true,"result":{"kind":"logs","lines":["example"],"truncated":false},"error":null}"#,
    ];
    let kinds: Vec<_> = examples
        .iter()
        .map(|e| decode_response(e.as_bytes()).unwrap().kind())
        .collect();
    assert_eq!(kinds, ["operation", "startup", "update", "logs"]);
}

#[test]
fn every_contract_state_and_error_string_round_trips() {
    let wire = |v: serde_json::Value| v.as_str().unwrap().to_string();
    let services: Vec<_> = ServiceState::ALL
        .iter()
        .map(|s| wire(serde_json::to_value(s).unwrap()))
        .collect();
    assert_eq!(
        services,
        ["running", "starting", "stopped", "unhealthy", "unknown"]
    );
    let startup: Vec<_> = StartupState::ALL
        .iter()
        .map(|s| wire(serde_json::to_value(s).unwrap()))
        .collect();
    assert_eq!(
        startup,
        [
            "enabled",
            "disabled",
            "masked",
            "static",
            "approval_required",
            "policy_managed",
            "unknown"
        ]
    );
    let updates: Vec<_> = UpdateState::ALL
        .iter()
        .map(|s| wire(serde_json::to_value(s).unwrap()))
        .collect();
    assert_eq!(
        updates,
        [
            "idle",
            "checking",
            "available",
            "prepared",
            "waiting_for_idle",
            "shutdown_committed",
            "installing",
            "restarting",
            "complete",
            "cancelled",
            "failed",
            "recovery_required"
        ]
    );
    for code in [
        "invalid_installation",
        "unsupported",
        "authorization_required",
        "authorization_denied",
        "busy",
        "identity_mismatch",
        "unsafe_shutdown",
        "policy_conflict",
        "unavailable",
        "rate_limited",
        "untrusted_update",
        "recovery_required",
        "invalid_request",
        "io",
    ] {
        let text = format!(
            r#"{{"schema_version":1,"ok":false,"result":null,"error":{{"code":"{code}","message":"m","retryable":false}}}}"#
        );
        assert!(
            matches!(
                decode_response(text.as_bytes()),
                Err(ControlFailure::Controller(_))
            ),
            "{code}"
        );
    }
}

#[test]
fn unknown_state_field_kind_or_schema_is_a_protocol_failure() {
    let cases = [
        STATUS.replace(
            r#""service_state":"stopped""#,
            r#""service_state":"halted""#,
        ),
        STATUS.replace(
            r#""transaction_id":null}"#,
            r#""transaction_id":null,"pid":1}"#,
        ),
        STATUS.replace(r#","transaction_id":null"#, ""),
        STATUS.replace(r#"{"schema_version":1"#, r#"{"schema_version":2"#),
        STATUS.replace(r#","error":null}"#, r#","error":null,"x":1}"#),
        r#"{"schema_version":1,"ok":true,"result":{"kind":"reboot"},"error":null}"#.into(),
        r#"{"schema_version":1,"ok":true,"result":null,"error":null}"#.into(),
        r#"{"schema_version":1,"ok":false,"result":null,"error":null}"#.into(),
    ];
    for case in cases {
        assert!(
            matches!(
                decode_response(case.as_bytes()),
                Err(ControlFailure::Protocol(_))
            ),
            "{case}"
        );
    }
    assert_eq!(
        decode_response(&vec![b' '; MAX_RESPONSE_BYTES + 1]),
        Err(ControlFailure::OutputTooLarge)
    );
}

#[test]
fn browser_and_release_urls_are_constrained() {
    assert_eq!(
        browser_url("http://127.0.0.1:32400/base"),
        Some("http://127.0.0.1:32400/base")
    );
    for bad in [
        "file:///etc/passwd",
        "javascript:alert(1)",
        "http://u:p@h/",
        "/rel",
        "http:// h",
        "http://",
    ] {
        assert_eq!(browser_url(bad), None, "{bad}");
    }
    assert_eq!(
        release_notes_url("pjunod/cinema", "0.7.1").unwrap(),
        "https://github.com/pjunod/cinema/releases/tag/v0.7.1"
    );
    assert_eq!(release_notes_url("a/b/c", "0.7.1"), None);
    assert_eq!(release_notes_url("pjunod/cinema", "0.7.1/../x"), None);
}

// ---- commands and runner --------------------------------------------------

#[test]
fn contract_argument_vectors() {
    let args = |c: Command| c.args().unwrap().join(" ");
    assert_eq!(args(Command::Status), "status");
    assert_eq!(args(Command::Stop), "stop --confirm-interruption");
    assert_eq!(args(Command::Restart), "restart --confirm-interruption");
    assert_eq!(
        args(Command::StartupSetServer(Choice::Off)),
        "startup set server off"
    );
    assert_eq!(
        args(Command::StartupSetTray(Choice::On)),
        "startup set tray on"
    );
    assert_eq!(
        args(Command::UpdatesPrepare("0.7.1".into())),
        "updates prepare --version 0.7.1"
    );
    assert_eq!(
        args(Command::UpdatesApply(TXN.into())),
        format!("updates apply --transaction {TXN} --confirm-interruption")
    );
    assert_eq!(args(Command::UpdatesStatus(None)), "updates status");
    assert_eq!(
        args(Command::UpdatesCancel(TXN.into())),
        format!("updates cancel --transaction {TXN}")
    );
    assert_eq!(args(Command::Logs(1000)), "logs --lines 1000");
    let runner = Runner::new(
        "/usr/bin/plurx-control",
        Some(PathBuf::from("/rec/installation.json")),
    );
    assert_eq!(
        runner.argv(&Command::Status).unwrap(),
        ["--installation", "/rec/installation.json", "status"]
    );
}

#[test]
fn values_that_could_become_options_or_paths_are_refused() {
    for v in [
        "--installation=/x",
        "v0.7.1",
        "0.7",
        "01.2.3",
        "0.7.1 ; rm",
        "",
    ] {
        assert!(!is_version(v), "{v}");
        assert!(Command::UpdatesPrepare(v.into()).args().is_err());
    }
    assert!(is_version("1.2.3-rc.1+build.5"));
    assert!(!is_transaction("../../etc/passwd"));
    assert!(!is_transaction("00000000-0000-4000-8000-00000000000Z"));
    assert!(Command::UpdatesApply("x".into()).args().is_err());
    assert!(Command::Logs(0).args().is_err());
    assert!(Command::Logs(1001).args().is_err());
}

#[test]
fn controller_environment_drops_proxies_and_injected_libraries() {
    let parent = [
        ("HOME", "/home/a"),
        ("PATH", "/evil"),
        ("https_proxy", "http://p"),
        ("LD_PRELOAD", "/x.so"),
        ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/user/1000/bus"),
        ("XDG_RUNTIME_DIR", "/run/user/1000"),
    ]
    .map(|(k, v)| (k.to_string(), v.to_string()));
    let env = controller_environment(parent.into_iter());
    let keys: Vec<_> = env.iter().map(|(k, _)| k.to_str().unwrap()).collect();
    assert_eq!(
        keys,
        [
            "DBUS_SESSION_BUS_ADDRESS",
            "HOME",
            "PATH",
            "XDG_RUNTIME_DIR"
        ]
    );
    assert!(env.contains(&("PATH".into(), "/usr/bin:/bin:/usr/sbin:/sbin".into())));
}

#[cfg(unix)]
mod process {
    use super::*;
    use crate::runner::{execute, Control};
    use std::os::unix::fs::PermissionsExt;
    use std::time::{Duration, Instant};

    // A concurrent fork can briefly inherit another fixture's writable script
    // descriptor before exec closes it, making Linux exec return ETXTBSY.
    // Serialize only these shell fixtures; production runner calls are unchanged.
    static PROCESS_FIXTURES: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn stub(body: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "plurx-tray-stub-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plurx-control");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        (dir, path)
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    // Run the real Runner in a subprocess so no test mutates the test suite's
    // shared environment. The controller child then crosses env_clear/allowlist.
    #[test]
    #[ignore = "subprocess entry used by the XDG runner regressions"]
    fn xdg_runner_child() {
        let controller = std::env::var_os("PLURX_TRAY_TEST_CONTROLLER").unwrap();
        let installation = std::env::var_os("PLURX_TRAY_TEST_INSTALLATION").unwrap();
        let command = if std::env::var_os("PLURX_TRAY_HELPER_ROUTE").is_some() {
            Command::Start
        } else {
            Command::StartupSetTray(Choice::Off)
        };
        let result = Runner::new(controller, Some(installation.into())).run(&command);
        if std::env::var_os("PLURX_TRAY_EXPECT_INVALID").is_some() {
            assert!(matches!(result, Err(ControlFailure::Controller(error))
                if error.code == ErrorCode::PolicyConflict));
        } else if std::env::var_os("PLURX_TRAY_REAL_CONTROLLER").is_none() {
            assert!(matches!(result.unwrap(), ControlResult::Status(_)));
        }
    }

    #[allow(
        clippy::disallowed_methods,
        reason = "test-only subprocess isolates the real Runner environment from parallel tests"
    )]
    fn run_xdg_child(
        controller: &std::path::Path,
        installation: &std::path::Path,
        value: &std::ffi::OsStr,
        home: &std::path::Path,
        real: bool,
        invalid: bool,
    ) {
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child
            .args([
                "--ignored",
                "--exact",
                "tests::process::xdg_runner_child",
                "--nocapture",
            ])
            .env("PLURX_TRAY_TEST_CONTROLLER", controller)
            .env("PLURX_TRAY_TEST_INSTALLATION", installation)
            .env("XDG_CONFIG_HOME", value)
            .env("HOME", home)
            .env_remove("PLURX_TRAY_REAL_CONTROLLER")
            .env_remove("PLURX_TRAY_EXPECT_INVALID");
        if real {
            child.env("PLURX_TRAY_REAL_CONTROLLER", "1");
        }
        if invalid {
            child.env("PLURX_TRAY_EXPECT_INVALID", "1");
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[allow(
        clippy::disallowed_methods,
        reason = "test-only child verifies privilege-route environment exclusion"
    )]
    fn runner_preserves_absolute_relative_empty_and_non_utf8_xdg_values() {
        use std::os::unix::ffi::OsStrExt;
        let _fixtures = PROCESS_FIXTURES.lock().unwrap();
        let (dir, controller) = stub(&format!(
            "printf '%s' \"$XDG_CONFIG_HOME\" > \"$2\"; printf '%s' '{}'",
            STATUS
        ));
        let captured = dir.join("received-xdg");
        for value in [
            std::ffi::OsStr::new("/alternate/config"),
            std::ffi::OsStr::new("relative-config"),
            std::ffi::OsStr::new(""),
            std::ffi::OsStr::from_bytes(b"bad-\xff"),
        ] {
            run_xdg_child(&controller, &captured, value, &dir, false, false);
            assert_eq!(std::fs::read(&captured).unwrap(), value.as_bytes());
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "tests::process::xdg_runner_child"])
            .env("PLURX_TRAY_TEST_CONTROLLER", &controller)
            .env("PLURX_TRAY_TEST_INSTALLATION", &captured)
            .env("PLURX_TRAY_HELPER_ROUTE", "1")
            .env("XDG_CONFIG_HOME", "/must-not-reach-helper")
            .status()
            .unwrap();
        assert!(output.success());
        assert!(std::fs::read(&captured).unwrap().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires an explicitly supplied real controller and root-owned disposable installation record"]
    fn real_controller_runner_writes_effective_xdg_and_refuses_invalid_values() {
        use std::os::unix::ffi::OsStrExt;
        assert_ne!(
            unsafe { libc::geteuid() },
            0,
            "tray preferences are unprivileged"
        );
        let _fixtures = PROCESS_FIXTURES.lock().unwrap();
        let controller = PathBuf::from(std::env::var_os("PLURX_TRAY_TEST_CONTROLLER").unwrap());
        let installation = PathBuf::from(std::env::var_os("PLURX_TRAY_TEST_INSTALLATION").unwrap());
        let dir = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "xdg-controller-{}-{}",
                std::process::id(),
                rand_suffix()
            ));
        let home = dir.join("home");
        let xdg = dir.join("effective-config");
        std::fs::create_dir_all(&home).unwrap();
        run_xdg_child(
            &controller,
            &installation,
            xdg.as_os_str(),
            &home,
            true,
            false,
        );
        let entry = xdg.join("autostart").join(ENTRY_NAME);
        assert!(std::fs::read_to_string(&entry)
            .unwrap()
            .contains("Hidden=true"));
        let original = std::fs::read(&entry).unwrap();
        for invalid in [
            std::ffi::OsStr::new("relative-config"),
            std::ffi::OsStr::new(""),
            std::ffi::OsStr::from_bytes(b"bad-\xff"),
        ] {
            run_xdg_child(&controller, &installation, invalid, &home, true, true);
            assert_eq!(std::fs::read(&entry).unwrap(), original);
            assert!(!home.join(".config/autostart").join(ENTRY_NAME).exists());
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn argv_arrives_as_one_vector_and_stderr_stays_diagnostic() {
        let _fixtures = PROCESS_FIXTURES.lock().unwrap();
        let (dir, path) = stub(
            r#"printf '{"schema_version":1,"ok":true,"result":{"kind":"update","state":"cancelled","version":null,"transaction_id":"%s","detail":"argc=%s"},"error":null}' "$4" "$#"; echo diag >&2"#,
        );
        let got = Runner::new(&path, None)
            .run(&Command::UpdatesCancel(TXN.into()))
            .unwrap();
        assert_eq!(
            got,
            ControlResult::Update(UpdateV1 {
                state: UpdateState::Cancelled,
                version: None,
                transaction_id: Some(TXN.into()),
                detail: Some("argc=4".into())
            })
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn structured_error_with_nonzero_exit_keeps_its_code() {
        let _fixtures = PROCESS_FIXTURES.lock().unwrap();
        let (dir, path) = stub(
            r#"printf '{"schema_version":1,"ok":false,"result":null,"error":{"code":"authorization_denied","message":"cancelled","retryable":false}}'; exit 3"#,
        );
        let got = Runner::new(&path, None).run(&Command::Start).unwrap_err();
        assert!(got.is_authorization());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn success_body_with_failing_exit_and_crashes_are_not_trusted() {
        let _fixtures = PROCESS_FIXTURES.lock().unwrap();
        let (dir, path) = stub(
            r#"printf '{"schema_version":1,"ok":true,"result":{"kind":"operation","operation":"start","service_state":"starting"},"error":null}'; exit 1"#,
        );
        assert!(matches!(
            Runner::new(&path, None).run(&Command::Start),
            Err(ControlFailure::Exited { status: 1, .. })
        ));
        std::fs::remove_dir_all(dir).unwrap();
        let (dir, path) = stub("echo 'panicked at main.rs' >&2; exit 101");
        let failure = Runner::new(&path, None).run(&Command::Status).unwrap_err();
        assert!(
            failure.message().ends_with("panicked at main.rs"),
            "{}",
            failure.message()
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn oversized_output_missing_binary_and_hung_read_are_bounded() {
        let _fixtures = PROCESS_FIXTURES.lock().unwrap();
        let (dir, path) = stub("head -c 1100000 /dev/zero | tr '\\0' a");
        assert_eq!(
            Runner::new(&path, None).run(&Command::Status),
            Err(ControlFailure::OutputTooLarge)
        );
        std::fs::remove_dir_all(dir).unwrap();
        assert!(matches!(
            Runner::new("/nonexistent/plurx-control", None).run(&Command::Status),
            Err(ControlFailure::Launch(_))
        ));
        let (dir, path) = stub("exec sleep 30");
        let started = Instant::now();
        assert_eq!(
            execute(&path, &["status".into()], Duration::from_secs(1), true),
            Err(ControlFailure::TimedOut(1))
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        std::fs::remove_dir_all(dir).unwrap();
    }
}

// ---- model ----------------------------------------------------------------

#[test]
fn start_only_for_observed_stopped_and_unknown_is_never_stopped() {
    for state in ServiceState::ALL {
        let entries = model::build(&snap(state));
        assert_eq!(entries[0].title, format!("plurx — {}", state.title()));
        assert_eq!(
            find(&entries, &Action::Start).unwrap().enabled,
            state == ServiceState::Stopped,
            "{state:?}"
        );
        let stoppable = matches!(state, ServiceState::Running | ServiceState::Unhealthy);
        assert_eq!(find(&entries, &Action::Stop).unwrap().enabled, stoppable);
        assert_eq!(find(&entries, &Action::Restart).unwrap().enabled, stoppable);
    }
    let mut s = snap(ServiceState::Stopped);
    s.status_failure = Some(ControlFailure::TimedOut(15));
    assert_eq!(s.service_state(), ServiceState::Unknown);
    assert!(!find(&model::build(&s), &Action::Start).unwrap().enabled);
    let (unknown_ok, start_ok) = receipt::menu_invariants(&s);
    assert!(unknown_ok && start_ok);
}

#[test]
fn startup_checks_reflect_only_observed_state_and_are_independent() {
    for state in StartupState::ALL {
        let mut s = snap(ServiceState::Running);
        s.status.as_mut().unwrap().server_startup.state = state;
        let entries = model::build(&s);
        let server = find(&entries, &Action::ToggleServerStartup).unwrap();
        assert_eq!(server.check, Some(model::check_for(state)));
        assert_eq!(
            server.enabled,
            matches!(state, StartupState::Enabled | StartupState::Disabled),
            "{state:?}"
        );
        // The tray login preference is unaffected by the server's boot state.
        assert_eq!(
            find(&entries, &Action::ToggleTrayLogin).unwrap().check,
            Some(Check::On)
        );
        if let Some(q) = state.qualifier() {
            assert!(server.title.contains(q));
        }
    }
}

#[test]
fn non_systemd_hosts_get_explicit_unsupported_management() {
    let mut s = snap(ServiceState::Running);
    s.management = management_for(false);
    let entries = model::build(&s);
    assert!(entries
        .iter()
        .any(|e| e.title.contains("Automatic management is unavailable")));
    for action in [
        Action::Start,
        Action::Stop,
        Action::Restart,
        Action::ToggleServerStartup,
        Action::CheckForUpdates,
    ] {
        assert!(!find(&entries, &action).unwrap().enabled, "{action:?}");
    }
    assert!(find(&entries, &Action::OpenBrowser).unwrap().enabled);
    assert!(find(&entries, &Action::Quit).unwrap().enabled);
}

#[test]
fn upgrade_offers_named_version_and_in_flight_replaces_it() {
    let mut s = snap(ServiceState::Running);
    s.update = Some(UpdateV1 {
        transaction_id: None,
        ..update(UpdateState::Available, "0.7.1")
    });
    let entries = model::build(&s);
    assert_eq!(
        find(&entries, &Action::Upgrade("0.7.1".into()))
            .unwrap()
            .title,
        "Upgrade to 0.7.1…"
    );
    assert!(find(&entries, &Action::OpenReleaseNotes("0.7.1".into())).is_some());
    s.update = Some(update(UpdateState::WaitingForIdle, "0.7.1"));
    let entries = model::build(&s);
    assert!(find(&entries, &Action::CancelUpgrade).is_some());
    assert!(!find(&entries, &Action::Stop).unwrap().enabled);
    assert!(find(&entries, &Action::CheckForUpdates).is_none());
    s.update = Some(update(UpdateState::Installing, "0.7.1"));
    assert!(
        find(&model::build(&s), &Action::CancelUpgrade).is_none(),
        "no cancel after shutdown commitment"
    );
    assert_eq!(model::poll_interval(&s, false), Some(2));
}

#[test]
fn poll_intervals_and_no_polling_daemon_without_host_or_window() {
    let mut s = snap(ServiceState::Running);
    assert_eq!(model::poll_interval(&s, true), Some(5));
    assert_eq!(model::poll_interval(&s, false), Some(30));
    s.tray_host = TrayHost::Absent;
    assert_eq!(model::poll_interval(&s, false), None);
    assert_eq!(
        model::poll_interval(&snap(ServiceState::Starting), false),
        Some(2)
    );
}

#[test]
fn upgrade_progress_and_recovery_explanation() {
    let fractions: Vec<f64> = UpdateState::ALL
        .iter()
        .filter_map(|s| model::upgrade_progress(*s))
        .collect();
    assert!(fractions.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(model::upgrade_progress(UpdateState::Failed), None);
    let text = model::upgrade_explanation(Some(&UpdateV1 {
        detail: Some("phase installing".into()),
        ..update(UpdateState::RecoveryRequired, "0.7.1")
    }));
    assert!(text.contains("needs recovery") && text.ends_with("phase installing"));
    assert_eq!(
        model::filter_logs(&["INFO a".into(), "warn b".into()], " info "),
        ["INFO a"]
    );
}

// ---- core -------------------------------------------------------------------

fn core_with(state: ServiceState) -> Core {
    let mut core = Core::new("0.7.0", Management::Supported, "pjunod/cinema");
    let effects = core.poll();
    assert!(matches!(
        &effects[..],
        [Effect::Run {
            purpose: Purpose::Poll,
            command: Command::Status
        }]
    ));
    core.completed(
        Purpose::Poll,
        Ok(ControlResult::Status(status(
            state,
            StartupState::Enabled,
            StartupState::Enabled,
        ))),
    );
    core
}

fn runs(effects: &[Effect]) -> Vec<Command> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Run { command, .. } => Some(command.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn quit_never_sends_a_server_command() {
    let mut core = core_with(ServiceState::Running);
    assert_eq!(core.act(Action::Quit), vec![Effect::Quit]);
}

#[test]
fn stop_confirms_then_runs_once_rereads_and_keeps_ui() {
    let mut core = core_with(ServiceState::Running);
    let effects = core.act(Action::Stop);
    let [Effect::Confirm { pending, .. }] = &effects[..] else {
        panic!("{effects:?}")
    };
    assert!(
        runs(&core.confirmed(pending.clone(), false)).is_empty(),
        "declined stop does nothing"
    );
    let effects = core.confirmed(Pending::Stop, true);
    assert_eq!(runs(&effects), [Command::Stop]);
    assert!(model::build(&core.snapshot)
        .iter()
        .any(|e| e.title.starts_with("Stopping the server")));
    // A second Stop while busy does nothing.
    assert!(core.confirmed(Pending::Stop, true).is_empty());
    let effects = core.completed(
        Purpose::Lifecycle(OperationKind::Stop),
        Ok(ControlResult::Operation(OperationV1 {
            operation: OperationKind::Stop,
            service_state: ServiceState::Stopped,
        })),
    );
    assert_eq!(runs(&effects), [Command::Status]);
    assert!(!effects.contains(&Effect::Quit));
    assert!(find(&core.entries(), &Action::Start).unwrap().enabled);
}

#[test]
fn authorization_denial_preserves_checkbox_and_says_nothing_changed() {
    let mut core = core_with(ServiceState::Running);
    assert_eq!(
        runs(&core.act(Action::ToggleServerStartup)),
        [Command::StartupSetServer(Choice::Off)]
    );
    let denied = ControlFailure::Controller(ControlErrorV1 {
        code: ErrorCode::AuthorizationDenied,
        message: "cancelled".into(),
        retryable: false,
    });
    let effects = core.completed(Purpose::ServerStartup, Err(denied));
    assert!(effects.iter().any(|e| matches!(e, Effect::ShowError(_))));
    assert_eq!(
        find(&core.entries(), &Action::ToggleServerStartup)
            .unwrap()
            .check,
        Some(Check::On)
    );
    assert!(core
        .snapshot
        .last_error
        .as_deref()
        .unwrap()
        .contains("Nothing was changed"));
    assert!(
        find(&core.entries(), &Action::Stop).unwrap().enabled,
        "controls stay available"
    );
}

#[test]
fn startup_result_applies_observed_not_requested_state() {
    let mut core = core_with(ServiceState::Running);
    core.act(Action::ToggleTrayLogin);
    core.completed(
        Purpose::TrayStartup,
        Ok(ControlResult::Startup(StartupV1 {
            server: StartupStateV1 {
                state: StartupState::Masked,
                detail: None,
            },
            tray: StartupStateV1 {
                state: StartupState::Enabled,
                detail: Some("XDG_CONFIG_DIRS entry wins".into()),
            },
        })),
    );
    let entries = core.entries();
    assert_eq!(
        find(&entries, &Action::ToggleTrayLogin).unwrap().check,
        Some(Check::On)
    );
    assert!(
        !find(&entries, &Action::ToggleServerStartup)
            .unwrap()
            .enabled
    );
}

#[test]
fn upgrade_prepares_names_version_and_declining_cancels() {
    let mut core = core_with(ServiceState::Running);
    core.completed(
        Purpose::Check,
        Ok(ControlResult::Update(UpdateV1 {
            transaction_id: None,
            ..update(UpdateState::Available, "0.7.1")
        })),
    );
    assert_eq!(
        runs(&core.act(Action::Upgrade("0.7.1".into()))),
        [Command::UpdatesPrepare("0.7.1".into())]
    );
    let effects = core.completed(
        Purpose::Prepare("0.7.1".into()),
        Ok(ControlResult::Update(update(
            UpdateState::Prepared,
            "0.7.1",
        ))),
    );
    let confirm = effects
        .iter()
        .find_map(|e| match e {
            Effect::Confirm {
                pending,
                title,
                message,
                proceed,
            } => Some((
                pending.clone(),
                title.clone(),
                message.clone(),
                proceed.clone(),
            )),
            _ => None,
        })
        .unwrap();
    assert_eq!(confirm.1, "Upgrade plurx to 0.7.1?");
    assert!(confirm.2.contains("from 0.7.0"));
    assert_eq!(confirm.3, "Upgrade to 0.7.1");
    assert_eq!(
        runs(&core.confirmed(confirm.0.clone(), false)),
        [Command::UpdatesCancel(TXN.into())]
    );
    assert_eq!(
        runs(&core.confirmed(confirm.0, true)),
        [Command::UpdatesApply(TXN.into())]
    );
}

#[test]
fn prepare_of_another_version_is_never_applied() {
    let mut core = core_with(ServiceState::Running);
    core.completed(
        Purpose::Check,
        Ok(ControlResult::Update(UpdateV1 {
            transaction_id: None,
            ..update(UpdateState::Available, "0.7.1")
        })),
    );
    core.act(Action::Upgrade("0.7.1".into()));
    let effects = core.completed(
        Purpose::Prepare("0.7.1".into()),
        Ok(ControlResult::Update(update(
            UpdateState::Prepared,
            "0.8.0",
        ))),
    );
    assert!(!effects.iter().any(|e| matches!(e, Effect::Confirm { .. })));
    assert!(effects.iter().any(|e| matches!(e, Effect::ShowError(_))));
}

#[test]
fn recovery_transaction_is_followed_and_blocks_start() {
    let mut core = Core::new("0.7.0", Management::Supported, "pjunod/cinema");
    core.poll();
    let mut s = status(
        ServiceState::Stopped,
        StartupState::Enabled,
        StartupState::Enabled,
    );
    s.transaction_id = Some(TXN.into());
    let effects = core.completed(Purpose::Poll, Ok(ControlResult::Status(s)));
    assert_eq!(runs(&effects), [Command::UpdatesStatus(Some(TXN.into()))]);
    let effects = core.completed(
        Purpose::PollUpdate(TXN.into()),
        Ok(ControlResult::Update(update(
            UpdateState::RecoveryRequired,
            "0.7.1",
        ))),
    );
    assert!(effects.contains(&Effect::ShowUpgrade));
    assert!(!find(&core.entries(), &Action::Start).unwrap().enabled);
}

#[test]
fn polls_coalesce() {
    let mut core = core_with(ServiceState::Running);
    assert_eq!(core.poll().len(), 1);
    assert!(core.poll().is_empty());
}

// ---- session eligibility -------------------------------------------------

fn eligible_facts(kind: &str) -> SessionFacts {
    SessionFacts {
        uid: 1000,
        display: Some(":0".into()),
        wayland_display: Some("wayland-0".into()),
        wayland_socket: true,
        x11_local_socket: true,
        session_bus: true,
        logind: true,
        seats_exist: true,
        session: Some(LogindSession {
            id: "3".into(),
            uid: 1000,
            remote: false,
            active: true,
            kind: kind.into(),
            class: "user".into(),
            seat: "seat0".into(),
        }),
        session_source: Some("XDG_SESSION_ID".into()),
        ..SessionFacts::default()
    }
}

#[test]
fn local_active_x11_and_wayland_sessions_are_eligible() {
    for kind in ["x11", "wayland"] {
        let e = evaluate(&eligible_facts(kind));
        assert!(e.eligible, "{kind}: {}", e.reason);
    }
}

#[test]
fn root_ssh_forwarded_container_headless_and_ambiguous_sessions_are_refused() {
    let refused = |mutate: &dyn Fn(&mut SessionFacts), expect: &str| {
        let mut f = eligible_facts("x11");
        mutate(&mut f);
        let e = evaluate(&f);
        assert!(
            !e.eligible && e.reason.contains(expect),
            "{expect}: {}",
            e.reason
        );
    };
    refused(
        &|f| {
            f.uid = 0;
            f.session.as_mut().unwrap().uid = 0
        },
        "root",
    );
    refused(&|f| f.container = true, "container");
    refused(&|f| f.logind = false, "logind is unavailable");
    refused(&|f| f.session = None, "no logind session");
    refused(
        &|f| f.session.as_mut().unwrap().uid = 1001,
        "belongs to uid 1001",
    );
    refused(&|f| f.session.as_mut().unwrap().remote = true, "is remote");
    refused(
        &|f| f.session.as_mut().unwrap().active = false,
        "not the active session",
    );
    refused(
        &|f| f.session.as_mut().unwrap().kind = "tty".into(),
        "type tty",
    );
    refused(
        &|f| f.session.as_mut().unwrap().class = "greeter".into(),
        "class greeter",
    );
    refused(
        &|f| f.session.as_mut().unwrap().seat = String::new(),
        "no local seat",
    );
    refused(&|f| f.ssh = true, "SSH");
    refused(&|f| f.session_bus = false, "session bus");
    refused(
        &|f| f.display = Some("localhost:10.0".into()),
        "no usable local x11 display",
    );
    refused(
        &|f| f.x11_local_socket = false,
        "no usable local x11 display",
    );
    let mut wayland = eligible_facts("wayland");
    wayland.wayland_socket = false;
    assert!(evaluate(&wayland).reason.contains("wayland display"));
    // A DISPLAY variable alone, with no logind session, is never proof.
    let headless = SessionFacts {
        uid: 1000,
        display: Some(":0".into()),
        x11_local_socket: true,
        session_bus: true,
        logind: true,
        ..SessionFacts::default()
    };
    assert!(!evaluate(&headless).eligible);
    // Seatless sessions are fine only where logind knows no seats at all.
    let mut seatless = eligible_facts("wayland");
    seatless.seats_exist = false;
    seatless.session.as_mut().unwrap().seat = String::new();
    assert!(evaluate(&seatless).eligible);
}

#[test]
fn local_x11_display_strings() {
    for ok in [":0", ":1.0", "unix:0"] {
        assert!(is_local_x11(ok), "{ok}");
    }
    for bad in ["localhost:10.0", "host:0", "", ":", ":a"] {
        assert!(!is_local_x11(bad), "{bad}");
    }
}

// ---- autostart ---------------------------------------------------------------

#[test]
fn autostart_search_order_follows_xdg_with_absolute_paths_only() {
    let p = |s: &str| PathBuf::from(s).join(ENTRY_NAME);
    assert_eq!(
        autostart::search_paths(Some("/home/a"), None, None),
        [p("/home/a/.config/autostart"), p("/etc/xdg/autostart")]
    );
    assert_eq!(
        autostart::search_paths(Some("/home/a"), Some("/cfg"), Some("/opt/xdg:rel:/etc/xdg")),
        [
            p("/cfg/autostart"),
            p("/opt/xdg/autostart"),
            p("/etc/xdg/autostart")
        ]
    );
    // A relative XDG_CONFIG_HOME is invalid and ignored (the session manager does the same).
    assert_eq!(
        autostart::search_paths(Some("/home/a"), Some("cfg"), None)[0],
        p("/home/a/.config/autostart")
    );
    assert_eq!(
        autostart::search_paths(Some("rel"), None, None),
        [p("/etc/xdg/autostart")]
    );
}

#[test]
fn hidden_override_masks_system_entry_and_only_desktop_entry_group_counts() {
    assert!(autostart::entry_enabled(
        "[Desktop Entry]\nExec=plurx-tray --autostart\n"
    ));
    assert!(!autostart::entry_enabled("[Desktop Entry]\nHidden=true\n"));
    assert!(!autostart::entry_enabled(
        "[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"
    ));
    assert!(autostart::entry_enabled(
        "[Desktop Entry]\nHidden=false\n[Desktop Action x]\nHidden=true\n"
    ));
    let dir = std::env::temp_dir().join(format!("plurx-tray-autostart-{}", std::process::id()));
    let (user, system) = (dir.join("user"), dir.join("system"));
    std::fs::create_dir_all(&user).unwrap();
    std::fs::create_dir_all(&system).unwrap();
    let paths = [user.join(ENTRY_NAME), system.join(ENTRY_NAME)];
    assert_eq!(autostart::resolve(&paths), Preference::Missing);
    std::fs::write(&paths[1], "[Desktop Entry]\nExec=plurx-tray --autostart\n").unwrap();
    assert!(autostart::resolve(&paths).launches());
    std::fs::write(&paths[0], "[Desktop Entry]\nHidden=true\n").unwrap();
    assert_eq!(
        autostart::resolve(&paths),
        Preference::Disabled(paths[0].clone())
    );
    std::fs::remove_file(&paths[0]).unwrap();
    std::fs::create_dir(&paths[0]).unwrap();
    assert!(
        matches!(autostart::resolve(&paths), Preference::Unreadable(..)),
        "never launch on doubt"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

// ---- cli and receipt ---------------------------------------------------------

fn parse(args: &[&str]) -> Result<Parsed, String> {
    cli::parse(args.iter().map(|s| s.to_string()))
}

#[test]
fn cli_modes_and_refusals() {
    let Ok(Parsed::Run(o)) = parse(&[]) else {
        panic!()
    };
    assert_eq!(o.mode, Mode::Window);
    assert_eq!(o.control, PathBuf::from("/usr/bin/plurx-control"));
    let Ok(Parsed::Run(o)) = parse(&["--autostart"]) else {
        panic!()
    };
    assert_eq!(o.mode, Mode::Autostart);
    let Ok(Parsed::Run(o)) = parse(&[
        "--smoke-receipt",
        "/r.json",
        "--smoke-action",
        "stop",
        "--smoke-timeout",
        "5",
        "--control",
        "/c",
    ]) else {
        panic!()
    };
    let Mode::Smoke(s) = o.mode else { panic!() };
    assert_eq!((s.action, s.timeout.as_secs()), (SmokeAction::Stop, 5));
    for bad in [
        &["--control", "rel"][..],
        &["--smoke-receipt", "r.json"],
        &["--smoke-action", "stop"],
        &["--autostart", "--window"],
        &["--smoke-receipt", "/r", "--smoke-timeout", "0"],
        &["--smoke-receipt", "/r", "--smoke-action", "upgrade"],
        &["--bogus"],
    ] {
        assert!(parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn receipt_is_written_atomically_and_bounded() {
    let dir = std::env::temp_dir().join(format!("plurx-tray-receipt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("smoke.json");
    receipt::write_atomic(&path, &serde_json::json!({"schema": receipt::SCHEMA})).unwrap();
    let back: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(back["schema"], "plurx-tray-smoke-v1");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        1,
        "no temporary file left behind"
    );
    assert!(receipt::write_atomic(std::path::Path::new("rel.json"), &1).is_err());
    assert!(receipt::write_atomic(&path, &"x".repeat(receipt::MAX_RECEIPT_BYTES)).is_err());
    std::fs::remove_dir_all(dir).unwrap();
    let menu = receipt::Menu::from(&model::build(&snap(ServiceState::Running)));
    assert!(
        menu.enabled.contains(&"Stop…".to_string()) && !menu.enabled.contains(&"Start".to_string())
    );
}

#[test]
fn every_state_has_its_own_symbolic_icon_name() {
    let names: std::collections::BTreeSet<_> =
        ServiceState::ALL.iter().map(|s| s.icon_name()).collect();
    assert_eq!(names.len(), 5);
    assert!(names.contains("tv.plurx.Server-unknown-symbolic"));
}

#[test]
fn manual_window_keeps_fallback_without_registering_tray_or_starting_server() {
    let mut cases = Vec::new();
    let mut remote = eligible_facts("x11");
    remote.session.as_mut().unwrap().remote = true;
    cases.push(remote);
    let mut container = eligible_facts("wayland");
    container.container = true;
    cases.push(container);
    let mut unknown = eligible_facts("x11");
    unknown.session = None;
    cases.push(unknown);
    let mut headless = eligible_facts("x11");
    headless.display = None;
    cases.push(headless);
    for facts in cases {
        let eligibility = evaluate(&facts);
        assert!(!eligibility.eligible);
        let mut core = core_with(ServiceState::Stopped);
        // Even a reachable watcher cannot authorize an automatic icon.
        core.snapshot.tray_host =
            crate::core::tray_host_for_session(&eligibility, TrayHost::Present);
        assert_eq!(core.snapshot.tray_host, TrayHost::Absent);
        // The explicit fallback remains available and polling never starts it.
        assert!(find(&core.entries(), &Action::ShowWindow).unwrap().enabled);
        assert_eq!(runs(&core.poll()), vec![Command::Status]);
    }
    let local = evaluate(&eligible_facts("x11"));
    assert_eq!(
        crate::core::tray_host_for_session(&local, TrayHost::Present),
        TrayHost::Present
    );
    assert_eq!(
        crate::core::tray_host_for_session(&local, TrayHost::Absent),
        TrayHost::Absent
    );
    assert_eq!(
        crate::core::tray_host_for_session(&local, TrayHost::Unknown),
        TrayHost::Absent
    );
}

#[test]
fn confirmed_host_loss_exposes_fallback_and_return_never_mutates_server() {
    let mut core = Core::new("0.7.0", Management::Supported, "pjunod/cinema");
    assert_eq!(core.set_tray_host(TrayHost::Present), [Effect::Render]);
    assert_eq!(core.set_tray_host(TrayHost::Unknown), [Effect::Render]);
    assert_eq!(
        core.set_tray_host(TrayHost::Absent),
        [Effect::Render, Effect::ShowWindow]
    );
    assert_eq!(core.set_tray_host(TrayHost::Absent), [Effect::Render]);
    assert_eq!(core.set_tray_host(TrayHost::Present), [Effect::Render]);
    assert_eq!(
        core.set_tray_host(TrayHost::Absent),
        [Effect::Render, Effect::ShowWindow]
    );
    assert!(core.snapshot.busy.is_none());
}

#[test]
fn container_lifecycle_progress_never_claims_an_upgrade_or_offers_cancellation() {
    let mut snapshot = snap(ServiceState::Running);
    let mut progress = update(UpdateState::ShutdownCommitted, "0.7.1");
    progress.version = None;
    progress.detail = Some("Container stop is waiting for graceful exit".into());
    assert!(progress.is_server_operation());
    assert_eq!(
        progress.title(),
        "Container stop is waiting for graceful exit"
    );
    snapshot.update = Some(progress.clone());
    let entries = model::build(&snapshot);
    assert!(model::find(&entries, &Action::ShowUpgradeProgress)
        .unwrap()
        .title
        .starts_with("Server operation:"));
    assert!(model::find(&entries, &Action::CancelUpgrade).is_none());
    assert!(!model::upgrade_explanation(Some(&progress)).contains("upgrade"));
    progress.state = UpdateState::Complete;
    progress.detail = Some("Container stop completed".into());
    snapshot.update = Some(progress.clone());
    let entries = model::build(&snapshot);
    assert!(model::find(&entries, &Action::ShowUpgradeProgress)
        .unwrap()
        .title
        .starts_with("Last operation:"));
    assert_eq!(
        model::upgrade_explanation(Some(&progress)),
        "Container stop completed"
    );
    snapshot.update = Some(UpdateV1 {
        state: UpdateState::Idle,
        version: None,
        transaction_id: None,
        detail: Some("Use the selected deployment tools".into()),
    });
    assert!(!model::build(&snapshot)
        .iter()
        .any(|entry| entry.title == "No update available"));
}

#[test]
fn actual_compose_backend_wire_examples_decode_strictly() {
    let examples: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../tests/fixtures/compose-wire-v1.json")).unwrap();
    let kinds: Vec<_> = examples
        .iter()
        .map(|example| {
            decode_response(&serde_json::to_vec(example).unwrap())
                .unwrap()
                .kind()
        })
        .collect();
    assert_eq!(kinds, ["status", "startup", "logs", "update", "update"]);
}
