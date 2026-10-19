#![cfg(unix)]
use plurx_desktop_control::{installation::native_uid, package_locks, secure_fs};
use std::{
    io::{Read, Write},
    os::{fd::AsRawFd, unix::fs::PermissionsExt},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
struct Fixture {
    path: std::path::PathBuf,
    child: Option<Child>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "Spawns and reaps only this fixture process to prove cross-process POSIX lock contention"
)]
fn native_package_lock_probe_is_nonblocking_and_detects_other_process() {
    if let Some(path) = std::env::var_os("PLURX_LOCK_TEST_CHILD") {
        let path = std::path::PathBuf::from(path);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.join("manager.lock"))
            .expect("fixture lock");
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as _;
        lock.l_whence = libc::SEEK_SET as _;
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &lock) },
            0
        );
        std::fs::write(path.join("ready"), b"ready").expect("fixture handshake");
        let mut byte = [0];
        std::io::stdin()
            .read_exact(&mut byte)
            .expect("release lock");
        return;
    }
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/desktop-control-tests")
        .join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir_all(&path).expect("durable fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
        .expect("private fixture");
    let path = path.canonicalize().expect("canonical fixture");
    let mut fixture = Fixture { path, child: None };
    let root = secure_fs::open_dir(&fixture.path, native_uid()).expect("owned fixture");
    root.write("manager.lock", b"", 0o600)
        .expect("manager lock");
    let file = root
        .open("manager.lock")
        .expect("read-only lock descriptor");
    assert!(!package_locks::file_is_locked(&file).expect("unlocked probe"));
    fixture.child = Some(
        Command::new(std::env::current_exe().expect("fixture executable"))
            .args([
                "--exact",
                "native_package_lock_probe_is_nonblocking_and_detects_other_process",
            ])
            .env("PLURX_LOCK_TEST_CHILD", &fixture.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .expect("fixture child"),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fixture.path.join("ready").exists() {
        assert!(Instant::now() < deadline, "child lock handshake timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(package_locks::file_is_locked(&file).expect("contended probe returns without waiting"));
    let child = fixture.child.as_mut().expect("fixture child");
    child
        .stdin
        .take()
        .expect("release pipe")
        .write_all(b"x")
        .expect("release");
    assert!(child.wait().expect("reap child").success());
    fixture.child = None;
    assert!(!package_locks::file_is_locked(&file).expect("lock released"));
}
