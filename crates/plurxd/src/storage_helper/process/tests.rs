use super::*;

#[cfg(unix)]
fn script(text: &str) -> Vec<OsString> {
    vec!["-c".into(), text.into(), "fixture".into()]
}

#[cfg(unix)]
#[tokio::test]
async fn returns_only_owned_bounded_output_and_the_original_generation() {
    let generation = Arc::new(43_u32);
    let (returned, output) = run_owned(
        Arc::clone(&generation),
        "/bin/sh".into(),
        script("printf 'value'; printf 'diagnostic' >&2"),
        Duration::from_secs(5),
    )
    .await
    .expect("short helper");
    assert!(Arc::ptr_eq(&returned, &generation));
    assert_eq!(output.stdout, b"value");
    assert_eq!(output.stderr, b"diagnostic");
}

#[cfg(unix)]
#[tokio::test]
async fn even_zero_exit_cannot_authorize_truncated_stdout_or_stderr() {
    for text in ["printf '%065537d' 0", "printf '%016385d' 0 >&2"] {
        let error = run_owned((), "/bin/sh".into(), script(text), Duration::from_secs(5))
            .await
            .expect_err("oversize output");
        assert!(error.to_string().contains("output exceeded"));
    }
    let error = run_owned(
        (),
        "/bin/sh".into(),
        script("printf 'bounded reason' >&2; exit 7"),
        Duration::from_secs(5),
    )
    .await
    .expect_err("nonzero helper");
    assert!(error.to_string().contains("bounded reason"));
}

#[cfg(unix)]
struct ReapWitness {
    pid_file: PathBuf,
    observed: Option<tokio::sync::oneshot::Sender<bool>>,
}
#[cfg(unix)]
impl Drop for ReapWitness {
    fn drop(&mut self) {
        let reaped = std::fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|text| text.trim().parse::<libc::pid_t>().ok())
            .is_some_and(|pid| {
                let mut status = 0;
                // SAFETY: fixture direct child; WNOHANG never sends a signal.
                let result = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
                result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ECHILD)
            });
        if let Some(observed) = self.observed.take() {
            let _ = observed.send(reaped);
        }
    }
}

#[cfg(unix)]
fn sleeping_fixture(path: &std::path::Path) -> Vec<OsString> {
    // exec replaces the shell, preserving its PID. There is no descendant.
    let mut arguments = script("printf '%s' \"$$\" > \"$1\"; exec /bin/sleep 30");
    arguments.push(path.as_os_str().to_owned());
    arguments
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_reaps_actual_child_before_releasing_generation() {
    let dir = tempfile::tempdir().expect("fixture directory");
    let pid_file = dir.path().join("pid");
    let (send, observed) = tokio::sync::oneshot::channel();
    let result = run_owned(
        ReapWitness {
            pid_file: pid_file.clone(),
            observed: Some(send),
        },
        "/bin/sh".into(),
        sleeping_fixture(&pid_file),
        Duration::from_secs(1),
    )
    .await;
    assert!(result.is_err());
    assert!(observed.await.expect("generation released after timeout"));
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_waiter_transfers_reap_and_generation_to_retained_owner() {
    let dir = tempfile::tempdir().expect("fixture directory");
    let pid_file = dir.path().join("pid");
    let (send, observed) = tokio::sync::oneshot::channel();
    let waiter = tokio::spawn(run_owned(
        ReapWitness {
            pid_file: pid_file.clone(),
            observed: Some(send),
        },
        "/bin/sh".into(),
        sleeping_fixture(&pid_file),
        Duration::from_secs(30),
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while !std::fs::read_to_string(&pid_file).is_ok_and(|text| !text.is_empty()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("real direct child started");
    waiter.abort();
    let _ = waiter.await;
    assert!(tokio::time::timeout(Duration::from_secs(5), observed)
        .await
        .expect("retained cleanup finished")
        .expect("generation released"));
}

#[cfg(unix)]
#[tokio::test]
async fn pipe_held_after_direct_exit_refuses_and_retains_generation_until_eof() {
    let dir = tempfile::tempdir().expect("fixture directory");
    let pid_file = dir.path().join("pid");
    let (send, mut observed) = tokio::sync::oneshot::channel();
    // Fault injection only: the real helper does not spawn children. No group
    // gets signalled after its leader exits, even if a pipe unexpectedly lives.
    let mut args = script("printf '%s' \"$$\" > \"$1\"; /bin/sleep 7 & exit 0");
    args.push(pid_file.as_os_str().to_owned());
    let result = run_owned(
        ReapWitness {
            pid_file,
            observed: Some(send),
        },
        "/bin/sh".into(),
        args,
        Duration::from_secs(10),
    )
    .await;
    let Err(error) = result else {
        panic!("unexpected open pipe accepted")
    };
    assert!(error.to_string().contains("pipes remain open"));
    assert!(matches!(
        observed.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(tokio::time::timeout(Duration::from_secs(5), observed)
        .await
        .expect("late EOF")
        .expect("generation released"));
}
