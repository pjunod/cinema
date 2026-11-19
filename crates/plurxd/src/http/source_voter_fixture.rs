//! Reap a failed test voter before retrying its endpoint allocation.
use plurx_core::{cluster::migration::SelectedStore, config::Config, error::StoreError};
use std::{future::Future, path::Path, process::Stdio, time::Duration};

const CHILD_CASE: &str = "PLURX_SOURCE_VOTER_FIXTURE_CASE";
const COLLISION: i32 = 75;

pub(crate) async fn run(case: &str, fixture: impl Future<Output = ()>) {
    if let Ok(child_case) = std::env::var(CHILD_CASE) {
        assert_eq!(child_case, case);
        fixture.await;
        stage(&std::env::temp_dir(), "completed", case);
        return;
    }

    for attempt in 1..=3 {
        let directory = crate::test_tempdir().expect("owned voter attempt");
        let root = directory.path();
        let mut child =
            tokio::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", case, "--nocapture", "--test-threads=1"])
                .env(CHILD_CASE, case)
                .env("TMPDIR", root)
                .env("TMP", root)
                .env("TEMP", root)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .expect("spawn owned voter fixture");
        let status = match tokio::time::timeout(Duration::from_secs(120), child.wait()).await {
            Ok(status) => status.expect("reap owned voter fixture"),
            Err(_) => {
                child.kill().await.expect("kill and reap timed-out fixture");
                panic!("{case} exceeded its owned child deadline");
            }
        };
        if status.success() {
            assert!(marked(root, "acquired", case) && marked(root, "completed", case));
            assert!(!root.join("refused").exists());
            return;
        }
        assert!(
            attempt < 3
                && status.code() == Some(COLLISION)
                && marked(root, "refused", case)
                && !root.join("acquired").exists()
                && !root.join("completed").exists(),
            "{case} child attempt {attempt} ended with {status}"
        );
    }
}

pub(crate) fn finish_boot(
    config: &Config,
    result: Result<SelectedStore, StoreError>,
    label: &str,
) -> SelectedStore {
    let Ok(case) = std::env::var(CHILD_CASE) else {
        return result.expect(label);
    };
    let root = std::env::temp_dir();
    let source = config.storage.data_dir.as_path();
    assert!(source != root && source.starts_with(&root));
    match result {
        Ok(selected) => {
            stage(&root, "acquired", &case);
            selected
        }
        Err(error) if !root.join("acquired").exists() && bind_collision(config, &error) => {
            eprintln!("Source voter endpoint occupied before acquisition: {error}");
            stage(&root, "refused", &case);
            std::process::exit(COLLISION);
        }
        Err(error) => panic!("{label}: {error:?}"),
    }
}

fn stage(root: &Path, name: &str, case: &str) {
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join(name))
        .expect("create voter stage marker")
        .write_all(case.as_bytes())
        .expect("write voter stage marker");
}

fn marked(root: &Path, name: &str, case: &str) -> bool {
    std::fs::read_to_string(root.join(name)).is_ok_and(|marked| marked == case)
}

fn bind_collision(config: &Config, error: &StoreError) -> bool {
    let StoreError::Migration(message) = error else {
        return false;
    };
    let source = config
        .storage
        .data_dir
        .join(plurx_core::cluster::migration::SQLITE_FILENAME);
    let suffix = format!(
        "; SQLite source {} remains available; rollback command: plurxd run",
        source.display()
    );
    let Some(message) = message.strip_suffix(&suffix) else {
        return false;
    };
    let occupied = std::io::Error::from_raw_os_error(libc::EADDRINUSE);
    [
        ("raft", "Raft", config.cluster.raft_bind),
        ("cluster API", "API", config.cluster.api_bind),
    ]
    .into_iter()
    .any(|(preflight, vendor, address)| {
        message == format!("database error: starting Hiqlite voter: Connect: bind {vendor} listener {address}: {occupied}")
            || message == format!("schema migration failed: the {preflight} port {address} is not available: {occupied}. Another plurxd on this host is using it — cluster ports are host-wide, so a second data directory needs its own raft_bind and api_bind")
    })
}
