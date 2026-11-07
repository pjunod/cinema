use super::super::SqliteStore;
use crate::storage::{open_native_store_at, NativeDirectoryLock};
use std::sync::Arc;
use std::time::Duration;

async fn closing(store: &SqliteStore) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !store.lifecycle.is_closing() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("close owner closed SQL admission");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn close_drains_cancelled_writer_and_reader_and_disables_retained_handles() {
    let directory = tempfile::tempdir().expect("native directory");
    let handle = open_native_store_at(directory.path(), &directory.path().join("credentials.key"))
        .await
        .expect("actual native Store and directory owner");
    let sqlite = Arc::clone(&handle.sqlite);
    let retained = Arc::clone(&handle.store);
    let (writer_started, writer_observed) = tokio::sync::oneshot::channel();
    let (writer_release, writer_wait) = std::sync::mpsc::channel();
    let (writer_finished, writer_done) = tokio::sync::oneshot::channel();
    let writer_store = Arc::clone(&sqlite);
    let writer = tokio::spawn(async move {
        writer_store
            .with_conn(move |connection| {
                let _ = writer_started.send(());
                writer_wait
                    .recv()
                    .map_err(|error| crate::error::StoreError::Task(error.to_string()))?;
                connection.execute(
                    "INSERT INTO settings(key,value) VALUES('shutdown.accepted','committed')",
                    [],
                )?;
                let _ = writer_finished.send(());
                Ok(())
            })
            .await
    });
    writer_observed
        .await
        .expect("actual blocking writer owns its connection");
    let (reader_started, reader_observed) = tokio::sync::oneshot::channel();
    let (reader_release, reader_wait) = std::sync::mpsc::channel();
    let reader_store = Arc::clone(&sqlite);
    let reader = tokio::spawn(async move {
        reader_store
            .with_read(move |connection| {
                let _ = reader_started.send(());
                reader_wait
                    .recv()
                    .map_err(|error| crate::error::StoreError::Task(error.to_string()))?;
                connection
                    .query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                    .map_err(Into::into)
            })
            .await
    });
    reader_observed
        .await
        .expect("actual read-pool connection acquired concurrently");
    writer.abort();
    reader.abort();
    let _ = writer.await;
    let _ = reader.await;
    let closer = Arc::clone(&sqlite);
    let close = tokio::spawn(async move { closer.close().await });
    closing(&sqlite).await;
    let pending_with_both = !close.is_finished();
    let refused_new_read = retained.get_setting("shutdown.accepted").await.is_err();
    let refused_new_write = retained
        .put_setting("shutdown.late", "must not write")
        .await
        .is_err();
    let held_owner = NativeDirectoryLock::try_acquire(directory.path()).is_err();
    // Release both actual workers before asserting, even on regression.
    writer_release.send(()).expect("release accepted writer");
    writer_done.await.expect("accepted write reached SQLite");
    let pending_with_reader = !close.is_finished();
    reader_release.send(()).expect("release accepted reader");
    tokio::time::timeout(Duration::from_secs(5), close)
        .await
        .expect("drained close deadline")
        .expect("close owner joined")
        .expect("all actual SQLite closes succeeded");
    assert!(pending_with_both && pending_with_reader && held_owner);
    assert!(refused_new_read && refused_new_write);
    assert!(sqlite
        .conn
        .connection
        .connection
        .lock()
        .expect("writer slot")
        .is_none());
    assert!(sqlite
        .reads
        .as_ref()
        .expect("file-backed read pool")
        .conns
        .iter()
        .all(|slot| slot.connection.lock().expect("reader slot").is_none()));
    assert!(retained.get_setting("shutdown.accepted").await.is_err());
    assert!(retained
        .put_setting("shutdown.late", "still refused")
        .await
        .is_err());
    sqlite
        .close()
        .await
        .expect("idempotent actual closed slots");
    drop(handle);
    drop(retained);
    drop(sqlite);
    let next = NativeDirectoryLock::acquire(directory.path())
        .await
        .expect("next exclusive owner");
    let connection =
        rusqlite::Connection::open(directory.path().join("plurx.db")).expect("next DB owner");
    let accepted: String = connection
        .query_row(
            "SELECT value FROM settings WHERE key='shutdown.accepted'",
            [],
            |row| row.get(0),
        )
        .expect("accepted write preserved");
    let late: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM settings WHERE key='shutdown.late'",
            [],
            |row| row.get(0),
        )
        .expect("refused writes absent");
    assert_eq!(accepted, "committed");
    assert_eq!(late, 0);
    drop(connection);
    drop(next);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_close_waiter_keeps_native_lease_until_accepted_sql_and_close_end() {
    let directory = tempfile::tempdir().expect("native directory");
    let handle = open_native_store_at(directory.path(), &directory.path().join("credentials.key"))
        .await
        .expect("native Store");
    let sqlite = Arc::clone(&handle.sqlite);
    let worker_store = Arc::clone(&sqlite);
    let (started, observed) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let worker = tokio::spawn(async move {
        worker_store
            .with_conn(move |connection| {
                let _ = started.send(());
                wait.recv()
                    .map_err(|error| crate::error::StoreError::Task(error.to_string()))?;
                connection.execute(
                    "INSERT INTO settings(key,value) VALUES('shutdown.cancelled','committed')",
                    [],
                )?;
                Ok(())
            })
            .await
    });
    observed.await.expect("actual writer started");
    let close_store = Arc::clone(&sqlite);
    let close = tokio::spawn(async move { close_store.close().await });
    closing(&sqlite).await;
    close.abort();
    worker.abort();
    let _ = close.await;
    let _ = worker.await;
    drop(handle);
    drop(sqlite);
    let held_owner = NativeDirectoryLock::try_acquire(directory.path()).is_err();
    release.send(()).expect("release actual accepted SQL");
    assert!(
        held_owner,
        "cancelling both waiters cannot release worker/close ownership"
    );
    let next = NativeDirectoryLock::acquire(directory.path())
        .await
        .expect("close worker released final owner");
    let connection = rusqlite::Connection::open(directory.path().join("plurx.db"))
        .expect("next exclusive DB owner");
    let value: String = connection
        .query_row(
            "SELECT value FROM settings WHERE key='shutdown.cancelled'",
            [],
            |row| row.get(0),
        )
        .expect("accepted write committed before physical close");
    assert_eq!(value, "committed");
    drop(connection);
    drop(next);
}

/// Exercise the real admission -> blocking-queue window with only one
/// blocking worker. Closing waiters must leave that worker available for SQL
/// which was admitted before close but has not entered the queue yet.
#[test]
fn queued_sql_progresses_past_concurrent_close_waiters_with_one_blocking_worker() {
    use std::future::Future;
    use std::task::Poll;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("bounded test runtime");
    let sqlite = Arc::new(SqliteStore::open_in_memory().expect("actual SQLite connection"));
    runtime.block_on(async {
        // This is the same real operation permit moved by with_conn into its
        // worker. Split at the queue boundary to deterministically schedule
        // close between production admission and spawn_blocking.
        let operation = sqlite.lifecycle.admit().expect("admitted SQL ownership");
        let pending = Arc::new(std::sync::Mutex::new(Some(operation)));
        let (stop_rescue, rescue_wait) = std::sync::mpsc::channel();
        let rescue_pending = Arc::clone(&pending);
        let rescue = std::thread::spawn(move || {
            if rescue_wait.recv_timeout(Duration::from_secs(5)).is_err() {
                // Release a regression's blocked close without permitting the
                // queued SQL to run without ownership. It checks this same
                // Option before touching its actual connection.
                rescue_pending.lock().expect("test operation slot").take();
                true
            } else {
                false
            }
        });
        let mut closes = Vec::new();
        for _ in 0..8 {
            let mut close = Box::pin(sqlite.close());
            std::future::poll_fn(|context| {
                assert!(
                    matches!(close.as_mut().poll(context), Poll::Pending),
                    "accepted SQL still owns its operation"
                );
                Poll::Ready(())
            })
            .await;
            closes.push(close);
        }
        let connection = Arc::clone(&sqlite.conn);
        let sql = tokio::task::spawn_blocking(move || {
            let _operation = pending
                .lock()
                .expect("test operation slot")
                .take()
                .ok_or_else(|| {
                    crate::error::StoreError::Task("close waiters starved accepted SQL".into())
                })?;
            let connection = connection
                .lock()
                .expect("actual writer acquired with ownership");
            connection.execute(
                "INSERT INTO settings(key,value) VALUES('shutdown.queue','progress')",
                [],
            )?;
            connection
                .query_row(
                    "SELECT value FROM settings WHERE key='shutdown.queue'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .map_err(crate::error::StoreError::from)
        })
        .await
        .expect("actual blocking SQL joined");
        let _ = stop_rescue.send(());
        let rescued = rescue.join().expect("bounded rescue owner joined");
        for close in closes {
            close.await.expect("concurrent actual connection close");
        }
        assert!(!rescued, "close waiters occupied the sole blocking worker");
        assert_eq!(
            sql.expect("accepted SQL made progress before close"),
            "progress"
        );
        assert!(sqlite
            .conn
            .connection
            .connection
            .lock()
            .expect("closed writer slot")
            .is_none());
    });
}
