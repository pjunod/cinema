#![cfg(target_os = "linux")]
use plurx_desktop_control::{installation::*, launch_lock, protocol::SupervisorKind};
use std::{
    ffi::CString,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
    },
    path::PathBuf,
};

/// Run only in an explicitly disposable Linux root test container. The normal
/// suite never changes UID or creates a root-owned fixture. Accounts must already
/// exist; this test never creates accounts, services, or startup configuration.
#[test]
#[ignore = "requires disposable Linux root and explicit service UID/GID and unrelated UID fixtures"]
fn linux_launch_lock_allows_only_the_service_group_and_root() {
    assert_eq!(unsafe { libc::geteuid() }, 0, "explicit root fixture only");
    let account = |name| {
        std::env::var(name)
            .expect("explicit fixture UID")
            .parse::<u32>()
            .expect("numeric fixture UID")
    };
    let service_uid = account("PLURX_TEST_SERVICE_UID");
    let other_uid = account("PLURX_TEST_OTHER_UID");
    let service_gid = account("PLURX_TEST_SERVICE_GID");
    assert_ne!(
        service_uid, service_gid,
        "fixture must prove UID-to-GID lookup, not UID equality"
    );
    assert_ne!(service_uid, 0);
    assert_ne!(other_uid, 0);
    assert_ne!(service_uid, other_uid);
    let root =
        PathBuf::from("/work/control-fixes-root-tests").join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&root).expect("owned durable root fixture");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755))
        .expect("public traversal fixture");
    let installation = InstallationV1 {
        schema_version: 1,
        installation_id: "00000000-0000-4000-8000-000000000001".into(),
        method: InstallMethod::LinuxDeb,
        owner_uid: 0,
        service_uid,
        architecture: Architecture::Arm64,
        desktop_installed: false,
        repository: REPOSITORY.into(),
        current_version: "0.7.0".into(),
        current_tag: "v0.7.0".into(),
        current_commit: "a".repeat(40),
        controller_protocol: 1,
        trust_key_ids: vec![],
        last_transaction_id: None,
        paths: ManagedPathsV1 {
            state: root.clone(),
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
        browser_origin: "http://127.0.0.1:32400".into(),
    };
    launch_lock::initialize(&installation).expect("real root:service-group initialization");
    let path = root.join("launch.lock");
    let metadata = std::fs::metadata(&path).expect("initialized lock");
    assert_eq!(metadata.uid(), 0);
    assert_eq!(
        metadata.gid(),
        service_gid,
        "must use the account primary GID, never its UID"
    );
    assert_eq!(metadata.mode() & 0o7777, 0o440);
    assert_ne!(
        metadata.gid(),
        0,
        "dedicated non-root service-group fixture"
    );
    launch_lock::initialize(&installation).expect("idempotent validation");
    assert_eq!(
        metadata.ino(),
        std::fs::metadata(&path).expect("same inode").ino()
    );
    drop(launch_lock::acquire(&installation, true).expect("root worker exclusive lock"));
    let path = CString::new(path.as_os_str().as_bytes()).expect("fixture C path");
    for (uid, gid, allowed) in [
        (service_uid, metadata.gid(), true),
        (other_uid, metadata.gid().wrapping_add(1), false),
    ] {
        // SAFETY: the child uses only async-signal-safe libc operations and _exit;
        // all strings are allocated in the parent before fork, with no child Rust
        // allocation, destructors, account lookup, or shared runtime locks.
        let child = unsafe { libc::fork() };
        assert!(child >= 0, "fork fixture process");
        if child == 0 {
            unsafe {
                if libc::setgroups(0, std::ptr::null()) != 0
                    || libc::setgid(gid) != 0
                    || libc::setuid(uid) != 0
                {
                    libc::_exit(10);
                }
                let fd = libc::open(
                    path.as_ptr(),
                    libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                );
                if allowed {
                    if fd < 0 || libc::flock(fd, libc::LOCK_SH | libc::LOCK_NB) != 0 {
                        libc::_exit(11);
                    }
                    libc::close(fd);
                } else if fd >= 0 || *libc::__errno_location() != libc::EACCES {
                    libc::_exit(12);
                }
                libc::_exit(0);
            }
        }
        let mut status = 0;
        assert_eq!(unsafe { libc::waitpid(child, &mut status, 0) }, child);
        assert!(libc::WIFEXITED(status));
        assert_eq!(
            libc::WEXITSTATUS(status),
            0,
            "kernel permission result for UID {uid}"
        );
    }
    std::fs::remove_dir_all(root).expect("remove isolated root fixture");
}
