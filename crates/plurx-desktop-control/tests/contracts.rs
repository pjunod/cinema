#![cfg(unix)]
use plurx_desktop_control::{
    installation::*,
    ipc,
    native::{ClosureFileV1, ReleaseIdentityV1, RuntimeClosureV1},
    package, package_script, process,
    protocol::*,
    secure_fs,
    transaction::*,
    update::*,
};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use sha2::{Digest, Sha256};
use std::{
    io::{Cursor, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
};

fn installed() -> InstallationV1 {
    InstallationV1 {
        schema_version: 1,
        installation_id: "00000000-0000-4000-8000-000000000001".into(),
        method: InstallMethod::LinuxDeb,
        owner_uid: 0,
        service_uid: 995,
        architecture: Architecture::Amd64,
        desktop_installed: false,
        repository: REPOSITORY.into(),
        current_version: "0.7.0".into(),
        current_tag: "v0.7.0".into(),
        current_commit: "a".repeat(40),
        controller_protocol: 1,
        trust_key_ids: vec!["fixture-only".into()],
        last_transaction_id: None,
        paths: ManagedPathsV1 {
            state: "/var/lib/plurx-control".into(),
            socket: "/run/plurx/desktop-control.sock".into(),
            runtime: "/usr/lib/plurx".into(),
            config: "/etc/plurx/plurx.toml".into(),
            data: "/var/lib/plurx".into(),
            log: "/var/log/plurxd.log".into(),
            canonical_plist: None,
            login_plist: None,
        },
        service: ServiceV1 {
            kind: SupervisorKind::Systemd,
            name: SYSTEM_UNIT.into(),
            domain: "system".into(),
        },
        browser_origin: "http://127.0.0.1:32400/base".into(),
    }
}
fn manifest() -> UpdateManifestV1 {
    UpdateManifestV1 {
        schema_version: 1,
        repository: REPOSITORY.into(),
        channel: "stable".into(),
        tag: "v0.7.10".into(),
        version: "0.7.10".into(),
        sequence: 10,
        source_commit: "b".repeat(40),
        source_tree: "c".repeat(40),
        packaging_commit: "d".repeat(40),
        created_unix: 1000,
        expires_unix: 2000,
        upgrade_from: vec![">=0.7.0, <0.8.0".into()],
        data_compatibility: "schema-1".into(),
        cluster_compatibility: "cluster-1".into(),
        minimum_controller: "0.7.0".into(),
        rollback: Rollback::BinaryCompatible,
        release_notes_url: format!("https://github.com/{REPOSITORY}/releases/tag/v0.7.10"),
        artifacts: vec![ArtifactV1 {
            name: "plurxd_0.7.10_amd64.deb".into(),
            platform: Platform::Linux,
            architecture: Architecture::Amd64,
            format: Format::Deb,
            component: Component::Server,
            package_name: "plurxd".into(),
            package_version: "1:0.7.10-2".into(),
            size: 7,
            sha256: hex::encode(Sha256::digest(b"package")),
        }],
    }
}
fn signer() -> (Ed25519KeyPair, Vec<TrustKey>) {
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
        .expect("generate ephemeral fixture key");
    let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("parse fixture key");
    let mut public_key = [0; 32];
    public_key.copy_from_slice(pair.public_key().as_ref());
    (
        pair,
        vec![TrustKey {
            id: "fixture-only".into(),
            public_key,
        }],
    )
}
fn signed(
    m: &UpdateManifestV1,
    i: &InstallationV1,
    ledger: &AcceptedReleasesV1,
) -> plurx_desktop_control::Result<VerifiedManifest> {
    let (pair, keys) = signer();
    let b = serde_json::to_vec(m)?;
    verify_manifest(&b, pair.sign(&b).as_ref(), &keys, i, ledger, 1500)
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/desktop-control-tests")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&root).expect("durable scratch");
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
            .expect("private fixture");
        Self(root.canonicalize().expect("canonical fixture"))
    }
    fn dir(&self) -> secure_fs::SecureDir {
        secure_fs::open_dir(&self.0, native_uid()).expect("owned fixture directory")
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn journal() -> JournalV1 {
    JournalV1 {
        schema_version: 1,
        transaction_id: "00000000-0000-4000-8000-000000000002".into(),
        installation_id: installed().installation_id,
        operation: Operation::Upgrade,
        old_version: "0.7.0".into(),
        target_version: Some("0.7.10".into()),
        manifest_digest: None,
        phase: Phase::Prepared,
        initial_running: true,
        initial_startup: StartupStateV1::new(StartupState::Disabled),
        policy_revision: "policy1".into(),
        config_digest: "config1".into(),
        process: Some(ProcessIdentityV1 {
            pid: 123,
            uid: 995,
            start_generation: "generation1".into(),
            nonce: "nonce-one-12345678".into(),
        }),
        shutdown_generation: Some("topology1".into()),
        clean_receipt: None,
        rollback: Rollback::BinaryCompatible,
        worker_pid: None,
        worker_generation: None,
        capsule_sha256: None,
        package_inventory_before: None,
        package_inventory_after: None,
        detail: None,
        expected_node_id: Some("node1".into()),
        start_permit_expires: None,
    }
}
#[test]
fn signed_manifest_requires_real_signature_and_provisioned_roots() {
    let m = manifest();
    let i = installed();
    let (pair, keys) = signer();
    let b = serde_json::to_vec(&m).expect("fixture");
    let sig = pair.sign(&b);
    assert!(verify_manifest(
        &b,
        sig.as_ref(),
        &keys,
        &i,
        &AcceptedReleasesV1::default(),
        1500
    )
    .is_ok());
    assert!(
        production_keys()
            .expect("valid production root source")
            .iter()
            .all(|production| keys
                .iter()
                .all(|fixture| production.public_key != fixture.public_key)),
        "fixture keys must never become production roots"
    );
    assert!(verify_manifest(
        &b,
        sig.as_ref(),
        &production_keys().expect("valid production root source"),
        &i,
        &AcceptedReleasesV1::default(),
        1500
    )
    .is_err());
    let mut altered = b.clone();
    altered.push(b' ');
    assert!(verify_manifest(
        &altered,
        sig.as_ref(),
        &keys,
        &i,
        &AcceptedReleasesV1::default(),
        1500
    )
    .is_err());
    assert!(verify_manifest(
        &b,
        &[0; 64],
        &keys,
        &i,
        &AcceptedReleasesV1::default(),
        1500
    )
    .is_err());
    let mut unbridged = i;
    unbridged.trust_key_ids.clear();
    assert!(verify_manifest(
        &b,
        sig.as_ref(),
        &keys,
        &unbridged,
        &AcceptedReleasesV1::default(),
        1500
    )
    .is_err());
}
#[test]
fn signed_feed_rejects_wrong_repository_channel_expiry_and_upgrade_path() {
    let i = installed();
    let ledger = AcceptedReleasesV1::default();
    for mutation in 0..8 {
        let mut m = manifest();
        match mutation {
            0 => m.repository = "attacker/cinema".into(),
            1 => m.channel = "nightly".into(),
            2 => m.expires_unix = 1400,
            3 => m.created_unix = 1900,
            4 => m.upgrade_from = vec![">=0.8.0".into()],
            5 => m.minimum_controller = "99.0.0".into(),
            6 => m.release_notes_url = "https://evil.invalid/notes".into(),
            _ => m.schema_version = 2,
        };
        assert!(signed(&m, &i, &ledger).is_err(), "mutation {mutation}");
    }
}
#[test]
fn monotonic_sequence_and_immutable_release_identity_are_enforced() {
    let i = installed();
    let m = manifest();
    let verified = signed(&m, &i, &AcceptedReleasesV1::default()).expect("valid signed fixture");
    let mut ledger = AcceptedReleasesV1::default();
    verified.accept(&mut ledger);
    assert!(signed(&m, &i, &ledger).is_ok());
    let mut changed = m.clone();
    changed.packaging_commit = "e".repeat(40);
    assert!(signed(&changed, &i, &ledger).is_err());
    let mut reused_sequence = m.clone();
    reused_sequence.version = "0.7.11".into();
    reused_sequence.tag = "v0.7.11".into();
    assert!(
        signed(&reused_sequence, &i, &ledger).is_err(),
        "a new release cannot reuse the highest accepted sequence"
    );
    let mut replay = m;
    replay.sequence = 9;
    assert!(signed(&replay, &i, &ledger).is_err());
}
#[test]
fn cohort_is_exact_and_server_only_never_adds_desktop() {
    let mut i = installed();
    let mut m = manifest();
    let v = signed(&m, &i, &AcceptedReleasesV1::default()).expect("server fixture");
    assert_eq!(select_cohort(&v, &i).expect("cohort").len(), 1);
    i.desktop_installed = true;
    assert!(signed(&m, &i, &AcceptedReleasesV1::default()).is_err());
    let mut desktop = m.artifacts[0].clone();
    desktop.name = "plurx-desktop_0.7.10_amd64.deb".into();
    desktop.package_name = "plurx-desktop".into();
    desktop.component = Component::Desktop;
    m.artifacts.push(desktop);
    let v = signed(&m, &i, &AcceptedReleasesV1::default()).expect("full desktop cohort");
    assert_eq!(select_cohort(&v, &i).expect("cohort").len(), 2);
    m.artifacts[1].package_version = "1:0.7.10-3".into();
    assert!(signed(&m, &i, &AcceptedReleasesV1::default()).is_err());
}
#[test]
fn wrong_architecture_method_duplicates_and_unsafe_asset_names_fail_closed() {
    let i = installed();
    for case in 0..5 {
        let mut m = manifest();
        match case {
            0 => m.artifacts[0].architecture = Architecture::Arm64,
            1 => m.artifacts[0].format = Format::Rpm,
            2 => m.artifacts.push(m.artifacts[0].clone()),
            3 => m.artifacts[0].name = "../../payload.deb".into(),
            _ => m.artifacts[0].size = MAX_ARTIFACT + 1,
        };
        assert!(
            signed(&m, &i, &AcceptedReleasesV1::default()).is_err(),
            "case {case}"
        );
    }
    let mut source = i;
    source.method = InstallMethod::Source;
    assert!(signed(&manifest(), &source, &AcceptedReleasesV1::default()).is_err());
}
#[test]
fn installation_routing_is_not_arbitrary_supervisor_or_filesystem_authority() {
    let i = installed();
    assert!(i.validate().is_ok());
    for case in 0..5 {
        let mut bad = i.clone();
        match case {
            0 => bad.paths.state = "/other/state".into(),
            1 => bad.service.name = "sshd.service".into(),
            2 => bad.paths.runtime = "/usr/lib/../other".into(),
            3 => bad.owner_uid = 1000,
            _ => bad.browser_origin = "https://user:secret@example.invalid".into(),
        };
        assert!(bad.validate().is_err(), "case {case}");
    }
}
#[test]
fn secure_state_rejects_symlink_ancestors_hardlinks_and_writable_authority() {
    let s = Scratch::new();
    let dir = s.dir();
    dir.write("a", b"safe", 0o600).expect("write");
    assert_eq!(dir.read("a", 4).expect("read"), b"safe");
    assert!(dir.read("a", 3).is_err());
    std::os::unix::fs::symlink(s.0.join("a"), s.0.join("link")).expect("fixture link");
    assert!(dir.read("link", 10).is_err());
    std::fs::hard_link(s.0.join("a"), s.0.join("hard")).expect("fixture hardlink");
    assert!(dir.read("a", 10).is_err());
    let child = s.0.join("child");
    std::fs::create_dir(&child).expect("child");
    std::os::unix::fs::symlink(&child, s.0.join("alias")).expect("alias");
    assert!(secure_fs::open_dir(&s.0.join("alias"), native_uid()).is_err());
    std::fs::set_permissions(&child, std::fs::Permissions::from_mode(0o777)).expect("mode");
    assert!(secure_fs::open_dir(&child, native_uid()).is_err());
}
#[test]
fn durable_atomic_json_and_nonblocking_coordination_lock() {
    let s = Scratch::new();
    let d = s.dir();
    let j = journal();
    d.write_json("journal.json", &j).expect("fsynced journal");
    let read: JournalV1 = d.json("journal.json").expect("durable roundtrip");
    assert_eq!(read.transaction_id, j.transaction_id);
    let held = d.lock().expect("lock");
    assert!(d.lock().is_err());
    drop(held);
    assert!(d.lock().is_ok());
    let bytes = d.read("journal.json", secure_fs::MAX_JSON).expect("read");
    assert!(!String::from_utf8_lossy(&bytes).contains("owner_capability"));
}
#[test]
fn launch_lock_excludes_installation_and_never_requires_writable_journal_access() {
    let s = Scratch::new();
    let mut i = installed();
    i.owner_uid = native_uid();
    i.service_uid = native_uid();
    i.paths.state = s.0.clone();
    plurx_desktop_control::launch_lock::initialize(&i).expect("init");
    let shared = plurx_desktop_control::launch_lock::acquire(&i, false).expect("launcher lock");
    assert!(plurx_desktop_control::launch_lock::acquire(&i, true).is_err());
    drop(shared);
    let exclusive = plurx_desktop_control::launch_lock::acquire(&i, true).expect("worker lock");
    assert!(plurx_desktop_control::launch_lock::acquire(&i, false).is_err());
    drop(exclusive);
}
#[test]
fn journal_transition_and_every_reboot_phase_fail_closed() {
    let mut j = journal();
    assert_eq!(j.recovery_action(), RecoveryAction::AllowUnchanged);
    assert!(j.transition(Phase::Installing).is_err());
    j.transition(Phase::ShutdownCommitted).expect("commit");
    assert_eq!(j.recovery_action(), RecoveryAction::HoldUncertainShutdown);
    assert!(j.transition(Phase::Cancelled).is_err());
    j.transition(Phase::CleanStop).expect("clean");
    assert_eq!(
        j.recovery_action(),
        RecoveryAction::RestoreOldIfPolicyUnchanged
    );
    j.transition(Phase::Installing).expect("install");
    assert_eq!(j.recovery_action(), RecoveryAction::HoldPackageRepair);
    j.transition(Phase::CandidateInstalled).expect("candidate");
    assert_eq!(j.recovery_action(), RecoveryAction::HoldCandidateHealth);
    j.transition(Phase::Restoring).expect("restore");
    j.transition(Phase::Committed).expect("success");
    assert_eq!(j.recovery_action(), RecoveryAction::AllowCommitted);
    assert!(j.transition(Phase::Installing).is_err());
    j.schema_version = 999;
    assert_eq!(j.recovery_action(), RecoveryAction::HoldUnknown);
}
#[test]
fn receipt_requires_exact_generation_nonce_clean_drain_and_store_close() {
    let j = journal();
    let receipt = ShutdownReceiptV1 {
        schema_version: 1,
        operation_id: j.transaction_id.clone(),
        process: j.process.clone().expect("fixture process"),
        generation: "topology1".into(),
        http_drained: true,
        store_closed: true,
        exit_class: ExitClass::Clean,
    };
    assert!(clean_receipt_matches(&j, &receipt));
    for case in 0..6 {
        let mut bad = receipt.clone();
        match case {
            0 => bad.process.start_generation = "reused-pid".into(),
            1 => bad.process.nonce = "other-process".into(),
            2 => bad.http_drained = false,
            3 => bad.store_closed = false,
            4 => bad.exit_class = ExitClass::Unclean,
            _ => bad.operation_id = "other-operation".into(),
        };
        assert!(!clean_receipt_matches(&j, &bad), "case {case}");
    }
}
#[test]
fn http_connection_framing_rejects_smuggling_and_preserves_next_response() {
    let mut input=Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}");
    assert_eq!(ipc::read_http(&mut input).expect("first").1, b"{}");
    assert_eq!(ipc::read_http(&mut input).expect("second").1, b"{}");
    for bytes in [
        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}".as_slice(),
        b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n",
        b"HTTP/1.1 200 OK\r\nContent-Length: 999999999\r\n\r\n",
    ] {
        assert!(ipc::read_http(&mut Cursor::new(bytes)).is_err());
    }
}
#[test]
fn kernel_peer_matches_actual_connected_process_not_claimed_json() {
    let (a, _b) = UnixStream::pair().expect("socketpair");
    let peer = process::socket_peer(&a).expect("kernel peer");
    let me = process::inspect(std::process::id()).expect("current process");
    assert_eq!(peer, me);
    assert!(process::still_same(&peer).expect("generation"));
}
#[test]
fn package_solver_rejects_new_dependencies_unrelated_removals_and_wrong_versions() {
    let expected = vec![("plurxd".into(), "1:0.7.10-2".into())];
    let good="Inst plurxd [0.7.0-1] (1:0.7.10-2 local-deb [amd64])\nConf plurxd (1:0.7.10-2 local-deb [amd64])\n";
    assert!(package::validate_apt_simulation(good, &expected).is_ok());
    assert!(package::validate_apt_simulation(
        &format!("{good}Inst gtk4 (4.8 local [amd64])\n"),
        &expected
    )
    .is_err());
    assert!(package::validate_apt_simulation(
        &format!("{good}Remv important-package\n"),
        &expected
    )
    .is_err());
    assert!(
        package::validate_apt_simulation("Inst plurxd (0.7.9-1 local [amd64])\n", &expected)
            .is_err()
    );
}
#[test]
fn package_identity_refresh_covers_external_upgrade_and_desktop_add_remove() {
    let i = installed();
    let release = ReleaseIdentityV1 {
        schema_version: 1,
        repository: REPOSITORY.into(),
        version: "0.7.10".into(),
        source_commit: "b".repeat(40),
        architecture: Architecture::Amd64,
    };
    let inv = package_script::parse_deb_inventory_for_hook(
        "plurxd\t1:0.7.10-2\tamd64\tinstalled\nplurx-desktop\t1:0.7.10-2\tamd64\thalf-configured\n",
        "plurx-desktop",
        "postinstall",
        &["configure".into()],
    )
    .expect("inventory");
    let fresh = package_script::refreshed(i.clone(), &release, &inv).expect("refresh");
    assert_eq!(fresh.current_version, "0.7.10");
    assert!(fresh.desktop_installed);
    assert_eq!(fresh.installation_id, i.installation_id);
    assert_eq!(fresh.paths.config, i.paths.config);
    let removed = package_script::parse_deb_inventory_for_hook(
        "plurxd\t1:0.7.10-2\tamd64\tinstalled\nplurx-desktop\t1:0.7.10-2\tamd64\thalf-installed\n",
        "plurx-desktop",
        "postremove",
        &["remove".into()],
    )
    .expect("desktop removed");
    assert!(
        !package_script::refreshed(fresh, &release, &removed)
            .expect("refresh removal")
            .desktop_installed
    );
    assert!(package_script::parse_deb_inventory("plurxd\t0.7.10-2\tamd64\tunpacked\n").is_err());
    let rpm = package_script::parse_rpm_inventory("plurxd\t0:0.7.10-2\taarch64\n").expect("rpm");
    assert_eq!(rpm.method, InstallMethod::LinuxRpm);
    assert_eq!(rpm.architecture, Architecture::Arm64);
}
#[test]
fn bounded_artifact_streaming_checks_signed_size_and_digest_before_publish() {
    let s = Scratch::new();
    let a = manifest().artifacts.remove(0);
    stage_artifact(&mut Cursor::new(b"package"), &a, &s.dir()).expect("verified stage");
    verify_staged(&a, &s.dir()).expect("independent recheck");
    assert!(stage_artifact(&mut Cursor::new(b"package!"), &a, &s.dir()).is_err());
    assert!(stage_artifact(&mut Cursor::new(b"altered"), &a, &s.dir()).is_err());
    verify_staged(&a, &s.dir()).expect("failed download cannot replace good package");
}
fn archive(names: &[(&str, tar::EntryType, &[u8])]) -> Vec<u8> {
    let enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    for (name, kind, bytes) in names {
        let mut h = tar::Header::new_ustar();
        h.set_entry_type(*kind);
        h.set_mode(0o500);
        h.set_size(bytes.len() as u64);
        if kind.is_symlink() {
            h.set_link_name("target").expect("link fixture");
        }
        h.set_cksum();
        tar.append_data(&mut h, name, *bytes).expect("tar fixture");
    }
    tar.into_inner()
        .expect("tar finish")
        .finish()
        .expect("gzip finish")
}
#[test]
fn archive_expansion_rejects_links_duplicates_and_expansion_bombs() {
    let valid = archive(&[("runtime/plurxd", tar::EntryType::Regular, b"binary")]);
    let s = Scratch::new();
    assert_eq!(
        extract_archive(Cursor::new(valid), &s.0, 100).expect("safe extract"),
        vec!["runtime/plurxd"]
    );
    for data in [
        archive(&[("bad", tar::EntryType::Symlink, b"")]),
        archive(&[
            ("a", tar::EntryType::Regular, b"x"),
            ("a", tar::EntryType::Regular, b"x"),
        ]),
        archive(&[("big", tar::EntryType::Regular, b"01234567890")]),
    ] {
        let out = Scratch::new();
        assert!(extract_archive(Cursor::new(data), &out.0, 10).is_err());
    }
}
#[test]
fn native_closure_requires_complete_inventory_and_rejects_unlisted_bytes() {
    let root = Scratch::new();
    let mut files = Vec::new();
    let mut required = vec![
        "runtime/plurxd".to_string(),
        "runtime/ffmpeg".into(),
        "runtime/ffprobe".into(),
        "Plurx Server.app/Contents/Info.plist".into(),
    ];
    required.extend(
        plurx_desktop_control::native::CONTROL_BINARIES
            .iter()
            .map(|n| format!("control/{n}")),
    );
    for path in required {
        let target = root.0.join(&path);
        std::fs::create_dir_all(target.parent().expect("parent")).expect("tree");
        let mut f = std::fs::File::create(&target).expect("fixture member");
        f.write_all(b"fixture").expect("write");
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o500)).expect("mode");
        files.push(ClosureFileV1 {
            path,
            size: 7,
            sha256: hex::encode(Sha256::digest(b"fixture")),
            executable: true,
        });
    }
    root.dir()
        .write_json(
            plurx_desktop_control::native::CLOSURE_NAME,
            &RuntimeClosureV1 {
                schema_version: 1,
                version: "0.7.10".into(),
                files,
            },
        )
        .expect("closure");
    assert!(plurx_desktop_control::native::verify_closure(&root.0, "0.7.10").is_ok());
    root.dir()
        .write("extra", b"unlisted", 0o400)
        .expect("extra");
    assert!(plurx_desktop_control::native::verify_closure(&root.0, "0.7.10").is_err());
}
#[test]
fn finite_cli_and_wire_types_reject_unbounded_or_unknown_inputs() {
    use clap::Parser;
    use plurx_desktop_control::cli::Cli;
    assert!(Cli::try_parse_from(["plurx-control", "logs", "--lines", "1001"]).is_err());
    assert!(Cli::try_parse_from(["plurx-control", "stop", "--force"]).is_err());
    assert!(Cli::try_parse_from([
        "plurx-control",
        "bootstrap",
        "macos",
        "--package",
        "/owned/package",
        "--no-start"
    ])
    .is_ok());
    assert!(Cli::try_parse_from([
        "plurx-control",
        "package-script",
        "postinstall",
        "desktop",
        "--",
        "configure",
        "0.7.0"
    ])
    .is_ok());
    assert!(serde_json::from_str::<ShutdownPrepareRequestV1>(r#"{"schema_version":1,"operation_id":"x","operation":"stop","process_nonce":"n","confirm_interruption":true,"target_version":null,"force":true}"#).is_err());
    let response = ControlResponseV1::success(ControlResultV1::Operation {
        operation: Operation::Start,
        service_state: ServiceState::Starting,
    });
    let value = serde_json::to_value(response).expect("wire");
    assert_eq!(value["result"]["kind"], "operation");
    assert_eq!(value["result"]["service_state"], "starting");
    assert!(value["error"].is_null());
}

#[test]
fn progress_readback_is_sanitized_and_never_needs_polkit_or_private_journals() {
    use plurx_desktop_control::{
        cli::{needs_authority, ControlCommand, UpdateCommand},
        controller, progress,
    };
    assert!(!needs_authority(&ControlCommand::Updates {
        command: UpdateCommand::Status { transaction: None }
    }));
    assert!(!needs_authority(&ControlCommand::Status));
    assert!(needs_authority(&ControlCommand::Updates {
        command: UpdateCommand::Cancel {
            transaction: journal().transaction_id
        }
    }));
    let scratch = Scratch::new();
    let root = scratch.dir();
    let mut i = installed();
    i.owner_uid = native_uid();
    i.paths.state = scratch.0.clone();
    let mut j = journal();
    j.phase = Phase::Installing;
    j.detail = Some("SECRET diagnostic /private/journal bearer-token".into());
    progress::publish(&root, &j).expect("atomic summary");
    let bytes = root.read("progress.json", 65536).expect("public snapshot");
    let text = String::from_utf8(bytes).expect("UTF8 snapshot");
    assert!(
        !text.contains("SECRET")
            && !text.contains("bearer")
            && !text.contains("worker_pid")
            && !text.contains("process")
    );
    assert_eq!(
        std::fs::metadata(scratch.0.join("progress.json"))
            .expect("summary inode")
            .permissions()
            .mode()
            & 0o777,
        if native_uid() == 0 { 0o644 } else { 0o600 }
    );
    let public = progress::read(&root, &i.installation_id)
        .expect("validated public progress")
        .expect("summary");
    assert_eq!(public.state(), UpdateState::RecoveryRequired);
    assert!(progress::read(&root, "00000000-0000-4000-8000-000000000099").is_err());
    let value = controller::update_status(&i, Some(&j.transaction_id))
        .expect("no private journal or supervisor required");
    let json = serde_json::to_string(&value).expect("update response");
    assert!(json.contains("recovery_required") && !json.contains("SECRET"));
    assert!(controller::update_status(&i, Some("not-uuid")).is_err());
    assert!(controller::update_status(&i, Some("00000000-0000-4000-8000-000000000099")).is_err());
    j.phase = Phase::Committed;
    j.detail = None;
    progress::publish(&root, &j).expect("completion snapshot");
    assert_eq!(
        progress::read(&root, &i.installation_id)
            .expect("read completion")
            .expect("summary")
            .state(),
        UpdateState::Complete
    );
    root.write("progress.json", b"{}", 0o600)
        .expect("corrupt fixture");
    assert!(progress::read(&root, &i.installation_id).is_err());
}

#[test]
fn old_or_candidate_restoration_permit_is_finite_and_stage_bound() {
    let mut j = journal();
    j.phase = Phase::Restoring;
    j.start_permit_expires = Some(2000);
    assert!(
        j.permits_start("0.7.0", 1999),
        "clean-stop recovery may restore the unchanged old release"
    );
    assert!(j.permits_start("0.7.10", 1999));
    assert!(!j.permits_start("0.7.11", 1999));
    assert!(!j.permits_start("0.7.0", 2000));
    j.phase = Phase::Installing;
    assert!(!j.permits_start("0.7.0", 1999));
    j.phase = Phase::Restoring;
    j.start_permit_expires = None;
    assert!(!j.permits_start("0.7.0", 1999));
}

#[test]
fn permission_denied_inspection_is_unknown_not_false_unhealthy() {
    use plurx_desktop_control::{controller, Error};
    assert!(controller::inspection_unavailable(
        &std::io::Error::from_raw_os_error(libc::EACCES).into()
    ));
    assert!(controller::inspection_unavailable(
        &std::io::Error::from_raw_os_error(libc::EPERM).into()
    ));
    assert!(controller::inspection_unavailable(&Error::new(
        ErrorCode::AuthorizationRequired,
        "native helper required"
    )));
    assert!(!controller::inspection_unavailable(&Error::new(
        ErrorCode::IdentityMismatch,
        "lookalike peer"
    )));
}

#[test]
fn old_key_bridge_record_promotion_then_new_signer_and_unpinned_rejection() {
    let (old_pair, old_keys) = signer();
    let (next_pair, mut next_keys) = signer();
    next_keys[0].id = "next-fixture-only".into();
    let mut current = old_keys.clone();
    current.extend(next_keys.clone());
    let roots = ProductionTrustRootsV1 {
        schema_version: 1,
        repository: REPOSITORY.into(),
        keys: current
            .iter()
            .map(|k| PublicTrustRootV1 {
                id: k.id.clone(),
                public_key_hex: hex::encode(k.public_key),
            })
            .collect(),
    };
    let mut record = installed();
    let bytes = serde_json::to_vec(&manifest()).expect("manifest fixture");
    let bridge = verify_manifest(
        &bytes,
        old_pair.sign(&bytes).as_ref(),
        &old_keys,
        &record,
        &AcceptedReleasesV1::default(),
        1500,
    )
    .expect("old signer authenticates bridge package");
    record.trust_key_ids = bridge_key_ids(&roots, bridge.key_id(), &old_keys)
        .expect("authenticated installed bridge roots");
    assert_eq!(
        record.trust_key_ids,
        vec!["fixture-only", "next-fixture-only"]
    );
    assert!(verify_manifest(
        &bytes,
        next_pair.sign(&bytes).as_ref(),
        &current,
        &record,
        &AcceptedReleasesV1::default(),
        1500
    )
    .is_ok());
    let (attacker, _) = signer();
    assert!(verify_manifest(
        &bytes,
        attacker.sign(&bytes).as_ref(),
        &current,
        &record,
        &AcceptedReleasesV1::default(),
        1500
    )
    .is_err());
    let new_only = ProductionTrustRootsV1 {
        keys: vec![roots.keys[1].clone()],
        ..roots.clone()
    };
    assert!(
        bridge_key_ids(&new_only, "fixture-only", &old_keys).is_err(),
        "cannot skip overlap bridge"
    );
    record.trust_key_ids = bridge_key_ids(&new_only, "next-fixture-only", &current)
        .expect("new signer may retire old root after bridge");
    assert_eq!(record.trust_key_ids, vec!["next-fixture-only"]);
    assert!(verify_manifest(
        &bytes,
        next_pair.sign(&bytes).as_ref(),
        &next_keys,
        &record,
        &AcceptedReleasesV1::default(),
        1500
    )
    .is_ok());
    let mut changed_key = roots;
    changed_key.keys[0].public_key_hex = changed_key.keys[1].public_key_hex.clone();
    assert!(bridge_key_ids(&changed_key, "fixture-only", &old_keys).is_err());
}

#[test]
fn boot_without_tray_cancels_only_untouched_preparation_and_holds_uncertain_shutdown() {
    use plurx_desktop_control::{controller, progress};
    let scratch = Scratch::new();
    let root = scratch.dir();
    let mut i = installed();
    i.owner_uid = native_uid();
    if cfg!(target_os = "macos") {
        i.method = InstallMethod::MacosNative;
    }
    i.paths.state = scratch.0.clone();
    let mut j = journal();
    let transactions = root.child("transactions", true).expect("private journals");
    let dir = transactions
        .child(&j.transaction_id, true)
        .expect("transaction");
    dir.write_json("journal.json", &j)
        .expect("prepared journal");
    root.write_json(
        "active.json",
        &serde_json::json!({"schema_version":1,"transaction_id":j.transaction_id}),
    )
    .expect("active pointer");
    controller::boot_reconcile(&i)
        .expect("no GUI, supervisor or manager invoked for untouched preparation");
    let cancelled: JournalV1 = dir.json("journal.json").expect("reconciled journal");
    assert_eq!(cancelled.phase, Phase::Cancelled);
    assert_eq!(
        progress::for_installation(&i)
            .expect("public readback")
            .expect("snapshot")
            .state(),
        UpdateState::Cancelled
    );
    j.transition(Phase::ShutdownCommitted)
        .expect("uncertain commitment fixture");
    dir.write_json("journal.json", &j)
        .expect("uncertain journal");
    assert!(
        controller::boot_reconcile(&i).is_err(),
        "cannot install or start without clean receipt"
    );
    assert_eq!(
        dir.json::<JournalV1>("journal.json")
            .expect("preserved evidence")
            .phase,
        Phase::ShutdownCommitted
    );
}

fn wrapped_native_archive(wrapper: Option<&str>) -> Vec<u8> {
    let mut paths = vec![
        "runtime/plurxd".to_string(),
        "runtime/ffmpeg".into(),
        "runtime/ffprobe".into(),
        "runtime/plurx-source-parser.wasm".into(),
        "Plurx Server.app/Contents/Info.plist".into(),
        "Plurx Server.app/Contents/MacOS/PlurxServer".into(),
    ];
    paths.extend(
        plurx_desktop_control::native::CONTROL_BINARIES
            .iter()
            .map(|name| format!("control/{name}")),
    );
    let mut members: Vec<(String, Vec<u8>)> = paths
        .into_iter()
        .map(|path| (path, b"fixture".to_vec()))
        .collect();
    members.push((
        "release.json".into(),
        serde_json::to_vec(&ReleaseIdentityV1 {
            schema_version: 1,
            repository: REPOSITORY.into(),
            version: "0.7.10".into(),
            source_commit: "b".repeat(40),
            architecture: Architecture::Universal,
        })
        .expect("release fixture"),
    ));
    let closure = RuntimeClosureV1 {
        schema_version: 1,
        version: "0.7.10".into(),
        files: members
            .iter()
            .map(|(path, bytes)| ClosureFileV1 {
                path: path.clone(),
                size: bytes.len() as u64,
                sha256: hex::encode(Sha256::digest(bytes)),
                executable: true,
            })
            .collect(),
    };
    members.push((
        plurx_desktop_control::native::CLOSURE_NAME.into(),
        serde_json::to_vec(&closure).expect("closure fixture"),
    ));
    if let Some(wrapper) = wrapper {
        for (path, _) in &mut members {
            *path = format!("{wrapper}/{path}");
        }
    }
    archive(
        &members
            .iter()
            .map(|(path, bytes)| (path.as_str(), tar::EntryType::Regular, bytes.as_slice()))
            .collect::<Vec<_>>(),
    )
}
fn native_archive_artifact(bytes: &[u8]) -> ArtifactV1 {
    ArtifactV1 {
        name: "plurxd-0.7.10-macos-universal.tar.gz".into(),
        platform: Platform::Macos,
        architecture: Architecture::Universal,
        format: Format::TarGz,
        component: Component::NativeRuntime,
        package_name: "plurx-native".into(),
        package_version: "0.7.10".into(),
        size: bytes.len() as u64,
        sha256: hex::encode(Sha256::digest(bytes)),
    }
}
#[test]
fn canonical_wrapped_native_archive_round_trip_shares_preparation_and_promotion_root() {
    use plurx_desktop_control::native;
    let wrapper = "plurxd-0.7.10-macos-universal";
    let bytes = wrapped_native_archive(Some(wrapper));
    let mut i = installed();
    i.method = InstallMethod::MacosNative;
    i.architecture = Architecture::Universal;
    let mut m = manifest();
    m.artifacts = vec![native_archive_artifact(&bytes)];
    let candidate =
        signed(&m, &i, &AcceptedReleasesV1::default()).expect("signed native candidate");
    let artifact = select_cohort(&candidate, &i).expect("native cohort")[0];
    let staged = Scratch::new();
    stage_artifact(&mut Cursor::new(bytes), artifact, &staged.dir())
        .expect("signed artifact size/hash");
    verify_staged(artifact, &staged.dir()).expect("independent artifact recheck");
    let extracted = Scratch::new();
    extract_archive(
        staged.dir().open(&artifact.name).expect("staged tar"),
        &extracted.0,
        artifact.size.saturating_mul(8),
    )
    .expect("canonical archive extraction");

    // Preparation verifies the package inside the release workflow's one wrapper.
    let preparation_root =
        native::archive_package_root(&extracted.0, artifact, &candidate.manifest().version)
            .expect("preparation root");
    assert_eq!(preparation_root, extracted.0.join(wrapper));
    let prepared = native::verify_closure(&preparation_root, &m.version)
        .expect("complete preparation closure");
    assert_eq!(prepared.files.len(), 12);

    // The worker independently resolves that same root before its immutable copy.
    let promotion_root =
        native::archive_package_root(&extracted.0, artifact, &candidate.manifest().version)
            .expect("promotion root");
    assert_eq!(promotion_root, preparation_root);
    let installed = Scratch::new();
    let immutable = installed.0.join("immutable-version");
    native::copy_tree(&promotion_root, &immutable, native_uid()).expect("promotion copy");
    native::verify_closure(&immutable, &m.version).expect("complete promoted direct-root closure");
    assert!(
        !immutable.join(wrapper).exists(),
        "wrapper is never installed inside the version"
    );
    // Bootstrap still verifies a direct package root, without archive resolution.
    native::verify_closure(&promotion_root, &m.version)
        .expect("direct bootstrap root remains valid");
    std::fs::write(immutable.join("unlisted"), b"not in signed closure").expect("unlisted fixture");
    assert!(native::verify_closure(&immutable, &m.version).is_err());
}
#[test]
fn native_archive_root_rejects_flat_wrong_wrappers_and_extra_siblings() {
    use plurx_desktop_control::native::archive_package_root;
    for wrapper in [None, Some("plurxd-0.7.9-macos-universal"), Some("package")] {
        let bytes = wrapped_native_archive(wrapper);
        let artifact = native_archive_artifact(&bytes);
        let extracted = Scratch::new();
        extract_archive(Cursor::new(bytes), &extracted.0, 1024 * 1024)
            .expect("malformed layout fixture");
        assert!(archive_package_root(&extracted.0, &artifact, "0.7.10").is_err());
    }
    for sibling_directory in [false, true] {
        let bytes = wrapped_native_archive(Some("plurxd-0.7.10-macos-universal"));
        let artifact = native_archive_artifact(&bytes);
        let extracted = Scratch::new();
        extract_archive(Cursor::new(bytes), &extracted.0, 1024 * 1024).expect("wrapped fixture");
        if sibling_directory {
            std::fs::create_dir(extracted.0.join("other-package")).expect("sibling directory");
        } else {
            std::fs::write(extracted.0.join(".extra"), b"unexpected").expect("sibling file");
        }
        assert!(archive_package_root(&extracted.0, &artifact, "0.7.10").is_err());
    }
}
#[test]
fn native_archive_root_binds_signed_identity_and_refuses_every_symlink_path() {
    use plurx_desktop_control::native::archive_package_root;
    use std::os::unix::fs::symlink;
    let wrapper = "plurxd-0.7.10-macos-universal";
    let bytes = wrapped_native_archive(Some(wrapper));
    let artifact = native_archive_artifact(&bytes);
    let extracted = Scratch::new();
    extract_archive(Cursor::new(bytes), &extracted.0, 1024 * 1024).expect("wrapped fixture");
    assert!(archive_package_root(&extracted.0, &artifact, "0.7.11").is_err());
    for mutation in 0..6 {
        let mut wrong = artifact.clone();
        match mutation {
            0 => wrong.name = "plurxd-0.7.9-macos-universal.tar.gz".into(),
            1 => wrong.platform = Platform::Linux,
            2 => wrong.architecture = Architecture::Arm64,
            3 => wrong.format = Format::Deb,
            4 => wrong.component = Component::Server,
            _ => wrong.package_name = "plurxd".into(),
        }
        assert!(archive_package_root(&extracted.0, &wrong, "0.7.10").is_err());
    }
    let linked_wrapper = Scratch::new();
    symlink(extracted.0.join(wrapper), linked_wrapper.0.join(wrapper)).expect("linked wrapper");
    assert!(archive_package_root(&linked_wrapper.0, &artifact, "0.7.10").is_err());
    let aliases = Scratch::new();
    let leaf = aliases.0.join("extracted");
    symlink(&extracted.0, &leaf).expect("linked extraction root");
    assert!(archive_package_root(&leaf, &artifact, "0.7.10").is_err());
    let ancestor = aliases.0.join("ancestor");
    symlink(extracted.0.parent().expect("fixture parent"), &ancestor).expect("linked ancestor");
    let through_ancestor = ancestor.join(extracted.0.file_name().expect("extraction name"));
    assert!(archive_package_root(&through_ancestor, &artifact, "0.7.10").is_err());
}

#[test]
fn launch_lock_rejects_public_modes_and_hardlinks_without_replacing_the_inode() {
    use plurx_desktop_control::launch_lock;
    use std::os::unix::fs::MetadataExt;
    let s = Scratch::new();
    let mut i = installed();
    i.owner_uid = native_uid();
    i.service_uid = native_uid();
    i.paths.state = s.0.clone();
    launch_lock::initialize(&i).expect("initialize fixture lock");
    let path = s.0.join("launch.lock");
    let original = std::fs::metadata(&path).expect("lock metadata");
    assert_eq!(original.mode() & 0o7777, 0o440);
    assert_eq!(original.uid(), i.owner_uid);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444))
        .expect("legacy public fixture");
    assert!(launch_lock::acquire(&i, true).is_err());
    assert!(launch_lock::initialize(&i).is_err());
    assert_eq!(
        std::fs::metadata(&path).expect("retained inode").ino(),
        original.ino()
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o440))
        .expect("restore fixture mode");
    let alias = s.0.join("alias.lock");
    std::fs::hard_link(&path, &alias).expect("hardlink fixture");
    assert!(launch_lock::acquire(&i, false).is_err());
    std::fs::remove_file(alias).expect("remove hardlink");
    launch_lock::acquire(&i, false).expect("revalidated original lock");
}

#[test]
fn bounded_journal_logs_require_the_narrow_helper_not_global_journal_membership() {
    use clap::Parser;
    use plurx_desktop_control::cli::{needs_authority, Cli};
    let logs = Cli::try_parse_from(["plurx-control", "logs", "--lines", "100"])
        .expect("bounded native log request");
    // Ordinary Linux desktop users are not generally members of systemd-journal.
    // Route this explicit read through the same fixed, finite polkit helper;
    // do not broaden journal permissions or return an apparently empty journal.
    assert!(needs_authority(&logs.command));
    let status = Cli::try_parse_from(["plurx-control", "status"]).expect("status");
    assert!(
        !needs_authority(&status.command),
        "polling must stay unprivileged"
    );
    assert!(Cli::try_parse_from(["plurx-control", "logs", "--lines", "1001"]).is_err());
    assert!(Cli::try_parse_from(["plurx-control", "logs", "--unit", "other.service"]).is_err());
}

#[test]
fn debian_postinst_accepts_only_the_exact_configuring_package() {
    let configure = vec!["configure".into()];
    let fresh_server = "plurxd\t1:0.7.10-2\tamd64\thalf-configured\n";
    let inv = package_script::parse_deb_inventory_for_hook(
        fresh_server,
        "plurxd",
        "postinstall",
        &configure,
    )
    .expect("fresh server postinst before dpkg marks installed");
    assert!(!inv.desktop_installed);
    assert_eq!(inv.server_package_version, "1:0.7.10-2");
    assert!(package_script::parse_deb_inventory(fresh_server).is_err());
    for (package, phase, args) in [
        ("plurx-desktop", "postinstall", configure.clone()),
        ("plurxd", "preinstall", configure.clone()),
        ("plurxd", "postinstall", vec!["abort-upgrade".into()]),
        ("plurxd", "postinstall", vec![]),
        (
            "plurxd",
            "postinstall",
            vec!["configure".into(), "0.7.9-1".into(), "extra".into()],
        ),
    ] {
        assert!(
            package_script::parse_deb_inventory_for_hook(fresh_server, package, phase, &args)
                .is_err()
        );
    }
    let desktop_add =
        "plurxd\t1:0.7.10-2\tamd64\tinstalled\nplurx-desktop\t1:0.7.10-2\tamd64\thalf-configured\n";
    let added = package_script::parse_deb_inventory_for_hook(
        desktop_add,
        "plurx-desktop",
        "postinstall",
        &configure,
    )
    .expect("separate desktop configure hook");
    assert!(added.desktop_installed);
    assert!(package_script::parse_deb_inventory_for_hook(
        desktop_add,
        "plurxd",
        "postinstall",
        &configure
    )
    .is_err());
    assert!(package_script::parse_deb_inventory(desktop_add).is_err());
    // A second, unrelated incomplete member remains a conflict during server configure.
    let both = desktop_add.replace("amd64\tinstalled", "amd64\thalf-configured");
    assert!(package_script::parse_deb_inventory_for_hook(
        &both,
        "plurxd",
        "postinstall",
        &configure
    )
    .is_err());
    assert!(package_script::parse_deb_inventory_for_hook(
        &both,
        "plurx-desktop",
        "postinstall",
        &configure
    )
    .is_err());
}

#[test]
fn debian_desktop_postremove_records_absence_without_blessing_other_incomplete_packages() {
    let removing =
        "plurxd\t1:0.7.10-2\tamd64\tinstalled\nplurx-desktop\t1:0.7.10-2\tamd64\thalf-installed\n";
    for action in ["remove", "purge"] {
        let inv = package_script::parse_deb_inventory_for_hook(
            removing,
            "plurx-desktop",
            "postremove",
            &[action.into()],
        )
        .expect("desktop removal is still in its postrm transitional state");
        assert!(!inv.desktop_installed);
    }
    assert!(package_script::parse_deb_inventory(removing).is_err());
    assert!(package_script::parse_deb_inventory_for_hook(
        removing,
        "plurx-desktop",
        "postremove",
        &["upgrade".into()]
    )
    .is_err());
    let server_incomplete = removing.replace("amd64\tinstalled", "amd64\thalf-configured");
    assert!(package_script::parse_deb_inventory_for_hook(
        &server_incomplete,
        "plurx-desktop",
        "postremove",
        &["remove".into()]
    )
    .is_err());
    let finalized = removing.replace("half-installed", "config-files");
    assert!(
        !package_script::parse_deb_inventory(&finalized)
            .expect("final removed inventory")
            .desktop_installed
    );
}

#[test]
fn final_candidate_validation_still_rejects_half_configured_debian_packages() {
    let i = installed();
    let plan = package::PackagePlan {
        program: "/usr/bin/apt-get",
        args: vec![],
        packages: vec![("plurxd".into(), "1:0.7.10-2".into())],
        paths: vec![],
    };
    let pending = "plurxd\t1:0.7.10-2\tamd64\thalf-configured\n";
    assert!(package::validate_installed(&i, &plan, pending).is_err());
    assert!(package::validate_installed(
        &i,
        &plan,
        &pending.replace("half-configured", "installed")
    )
    .is_ok());
}

#[test]
fn changed_package_unit_reloads_before_policy_readback_and_preserves_operator_policy() {
    use plurx_desktop_control::{controller, supervisor};
    let scratch = Scratch::new();
    let unit = scratch.0.join("plurxd.service");
    let old = b"[Service]\nTimeoutStartSec=60\n".to_vec();
    let candidate = b"[Service]\nTimeoutStartSec=120\n".to_vec();
    std::fs::write(&unit, &old).expect("old unit");
    let i = installed();
    let plan = package::PackagePlan {
        program: "/usr/bin/apt-get",
        args: vec![],
        packages: vec![("plurxd".into(), "1:0.7.10-2".into())],
        paths: vec![],
    };
    let inventory = "plurxd\t1:0.7.10-2\tamd64\tinstalled\n";
    let fragment = "/usr/lib/systemd/system/plurxd.service";
    let drop_ins = vec![(
        "/etc/systemd/system/plurxd.service.d/operator.conf".to_string(),
        b"[Service]\nNice=5\n".to_vec(),
    )];
    for startup in ["enabled", "disabled"] {
        std::fs::write(&unit, &old).expect("loaded old package unit");
        let initial = supervisor::systemd_policy_revision(startup, fragment, "no", &drop_ins);
        let mut loaded = old.clone();
        // The verified candidate really changes the on-disk package unit.
        std::fs::write(&unit, &candidate).expect("candidate unit");
        assert_ne!(std::fs::read(&unit).expect("disk unit"), loaded);
        assert_ne!(
            supervisor::systemd_policy_revision(startup, fragment, "yes", &drop_ins),
            initial
        );
        let mut reloads = 0;
        controller::finalize_package_install_with(&i, &plan, 0, inventory, |i| {
            supervisor::reload_after_package_install_with(i, |program, args, timeout| {
                assert_eq!(program, supervisor::SYSTEMCTL);
                assert_eq!(args, ["--no-ask-password", "daemon-reload"]);
                assert_eq!(timeout, std::time::Duration::from_secs(10));
                reloads += 1;
                loaded = std::fs::read(&unit).expect("systemd loads changed unit");
                Ok((0, String::new()))
            })
        })
        .expect("strict cohort followed by bounded reload");
        assert_eq!(reloads, 1);
        assert_eq!(loaded, candidate);
        // The expected package-owned base-unit replacement is accepted, while
        // the unchanged startup choice and external drop-ins remain fenced.
        assert_eq!(
            supervisor::systemd_policy_revision(startup, fragment, "no", &drop_ins),
            initial
        );
        let mut edited = drop_ins.clone();
        edited[0]
            .1
            .extend_from_slice(b"Environment=OUTSIDE_EDIT=1\n");
        assert_ne!(
            supervisor::systemd_policy_revision(startup, fragment, "no", &edited),
            initial
        );
        assert_ne!(
            supervisor::systemd_policy_revision(startup, fragment, "no", &[]),
            initial
        );
        let mut added = drop_ins.clone();
        added.push((
            "/etc/systemd/system/plurxd.service.d/new.conf".into(),
            b"[Service]\nNice=10\n".to_vec(),
        ));
        assert_ne!(
            supervisor::systemd_policy_revision(startup, fragment, "no", &added),
            initial
        );
        let changed_startup = if startup == "enabled" {
            "disabled"
        } else {
            "enabled"
        };
        assert_ne!(
            supervisor::systemd_policy_revision(changed_startup, fragment, "no", &drop_ins),
            initial
        );
    }
}

#[test]
fn worker_never_reloads_failed_or_incomplete_package_cohorts() {
    use plurx_desktop_control::controller;
    let i = installed();
    let plan = package::PackagePlan {
        program: "/usr/bin/apt-get",
        args: vec![],
        packages: vec![("plurxd".into(), "1:0.7.10-2".into())],
        paths: vec![],
    };
    let complete = "plurxd\t1:0.7.10-2\tamd64\tinstalled\n";
    for (exit, inventory) in [
        (1, complete.to_string()),
        (0, complete.replace("installed", "half-configured")),
        (0, complete.replace("0.7.10", "0.7.9")),
    ] {
        let mut reloads = 0;
        assert!(
            controller::finalize_package_install_with(&i, &plan, exit, &inventory, |_| {
                reloads += 1;
                Ok(())
            })
            .is_err()
        );
        assert_eq!(reloads, 0);
    }
    let error = controller::finalize_package_install_with(&i, &plan, 0, complete, |_| {
        Err(plurx_desktop_control::Error::new(
            ErrorCode::Unavailable,
            "bounded reload failed",
        ))
    })
    .expect_err("reload failure prevents restoration");
    assert_eq!(error.code, ErrorCode::Unavailable);
}

#[test]
fn debian_server_first_configuration_keeps_unpacked_desktop_absent_until_its_own_hook() {
    let pending =
        "plurxd\t1:0.7.10-2\tamd64\thalf-configured\nplurx-desktop\t1:0.7.10-2\tamd64\tunpacked\n";
    let server = package_script::parse_deb_inventory_for_hook(
        pending,
        "plurxd",
        "postinstall",
        &["configure".into(), "1:0.7.9-1".into()],
    )
    .expect("apt server dependency configures before the already-unpacked desktop");
    assert!(!server.desktop_installed);
    assert!(package_script::parse_deb_inventory(pending).is_err());
    for state in ["half-configured", "half-installed"] {
        assert!(package_script::parse_deb_inventory_for_hook(
            &pending.replace("unpacked", state),
            "plurxd",
            "postinstall",
            &["configure".into()]
        )
        .is_err());
    }
    let desktop_phase = pending
        .replace("amd64\thalf-configured", "amd64\tinstalled")
        .replace("unpacked", "half-configured");
    let desktop = package_script::parse_deb_inventory_for_hook(
        &desktop_phase,
        "plurx-desktop",
        "postinstall",
        &["configure".into()],
    )
    .expect("desktop becomes present in its own subsequent configure hook");
    assert!(desktop.desktop_installed);
    let complete = desktop_phase.replace("half-configured", "installed");
    assert!(
        package_script::parse_deb_inventory(&complete)
            .expect("completed transaction")
            .desktop_installed
    );
    let unrelated_pending = pending.replace("half-configured", "installed");
    assert!(package_script::parse_deb_inventory(&unrelated_pending).is_err());
    assert!(package_script::parse_deb_inventory_for_hook(
        &unrelated_pending,
        "plurx-desktop",
        "postinstall",
        &["configure".into()]
    )
    .is_err());
}

#[test]
fn thin_macos_source_install_selects_only_signed_universal_release_cohort() {
    for architecture in [
        Architecture::Arm64,
        Architecture::Amd64,
        Architecture::Universal,
    ] {
        let mut i = installed();
        i.method = InstallMethod::MacosNative;
        i.architecture = architecture;
        let mut m = manifest();
        m.artifacts = vec![native_archive_artifact(b"universal fixture")];
        let candidate = signed(&m, &i, &AcceptedReleasesV1::default())
            .expect("native source to signed universal release");
        let cohort = select_cohort(&candidate, &i).expect("universal cohort");
        assert_eq!(cohort.len(), 1);
        assert_eq!(cohort[0].architecture, Architecture::Universal);
        m.artifacts[0].architecture = Architecture::Arm64;
        assert!(
            signed(&m, &i, &AcceptedReleasesV1::default()).is_err(),
            "a thin runtime never enables thin release archives"
        );
    }
}
#[test]
fn source_bootstrap_is_an_explicit_cli_route_with_finite_arguments() {
    use clap::Parser;
    use plurx_desktop_control::cli::{BootstrapCommand, Cli, ControlCommand};
    let cli = Cli::try_parse_from([
        "plurx-control",
        "bootstrap",
        "macos-source",
        "--package",
        "/owned/source",
        "--no-start",
    ])
    .expect("explicit local source route");
    assert!(matches!(
        cli.command,
        ControlCommand::Bootstrap {
            command: BootstrapCommand::MacosSource { no_start: true, .. }
        }
    ));
    assert!(Cli::try_parse_from([
        "plurx-control",
        "bootstrap",
        "macos-source",
        "--package",
        "/owned/source",
        "--force"
    ])
    .is_err());
}

#[test]
fn unresolved_transaction_refusal_names_phase_and_explicit_recovery() {
    let mut j = journal();
    ensure_resolved(None).expect("no transaction");
    for phase in [
        Phase::Prepared,
        Phase::ShutdownCommitted,
        Phase::CleanStop,
        Phase::Installing,
        Phase::CandidateInstalled,
        Phase::Restoring,
        Phase::RecoveryRequired,
    ] {
        j.phase = phase;
        let error = ensure_resolved(Some(&j)).expect_err("unresolved transaction");
        assert_eq!(error.code, ErrorCode::Busy);
        assert!(error.message.contains(&format!("at {phase:?}")));
        assert!(error.message.contains(&format!(
            "plurx-recovery --transaction {}",
            j.transaction_id
        )));
        assert_eq!(j.phase, phase, "preflight never changes transaction state");
    }
    for phase in [Phase::Committed, Phase::Cancelled] {
        j.phase = phase;
        ensure_resolved(Some(&j)).expect("terminal transaction");
    }
}

#[test]
fn interrupted_committed_shutdown_recovers_only_with_exact_clean_exit_evidence() {
    let mut committed = journal();
    committed
        .transition(Phase::ShutdownCommitted)
        .expect("committed shutdown");
    let receipt = ShutdownReceiptV1 {
        schema_version: 1,
        operation_id: committed.transaction_id.clone(),
        process: committed.process.clone().expect("bound process"),
        generation: committed
            .shutdown_generation
            .clone()
            .expect("bound generation"),
        http_drained: true,
        store_closed: true,
        exit_class: ExitClass::Clean,
    };
    for case in 0..10 {
        let mut pending = committed.clone();
        let mut proof = receipt.clone();
        match case {
            0 => proof.operation_id = "other-transaction".into(),
            1 => proof.process.nonce = "other-process".into(),
            2 => proof.process.start_generation = "reused-pid".into(),
            3 => proof.generation = "other-shutdown".into(),
            4 => proof.http_drained = false,
            5 => proof.store_closed = false,
            6 => proof.exit_class = ExitClass::Unclean,
            9 => pending.phase = Phase::Installing,
            _ => {}
        }
        let before = serde_json::to_value(&pending).expect("journal before recovery");
        assert!(
            pending
                .complete_clean_shutdown(proof, case != 7, case != 8)
                .is_err(),
            "case {case}"
        );
        assert_eq!(
            serde_json::to_value(&pending).expect("held journal"),
            before,
            "failed recovery preserves evidence, case {case}"
        );
    }
    committed
        .complete_clean_shutdown(receipt.clone(), true, true)
        .expect("recover exact clean exit");
    assert_eq!(committed.phase, Phase::CleanStop);
    let scratch = Scratch::new();
    let mut installation = installed();
    installation.owner_uid = native_uid();
    installation.service_uid = native_uid();
    installation.paths.state = scratch.0.clone();
    let store = TransactionStore::locked(&installation).expect("held journal lock");
    store
        .transactions
        .child(&committed.transaction_id, true)
        .expect("transaction directory");
    store
        .save(&committed)
        .expect("durable recovered journal and progress");
    let reloaded = store
        .load(&committed.transaction_id)
        .expect("reload recovered journal");
    assert_eq!(reloaded.phase, Phase::CleanStop);
    assert!(reloaded.clean_receipt.is_some());
    assert_eq!(
        serde_json::to_value(committed.clean_receipt).expect("retained receipt"),
        serde_json::to_value(receipt).expect("original receipt")
    );
}
