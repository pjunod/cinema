use openraft::Raft;
use openraft::RaftTypeConfig;
use openraft::error::{InstallSnapshotError, RaftError};
use openraft::raft::{InstallSnapshotRequest, InstallSnapshotResponse};
use std::future::Future;
use std::time::Duration;
use tokio::sync::{Mutex, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time;
use tracing::{error, warn};

type SnapshotResult<C> = Result<
    InstallSnapshotResponse<<C as RaftTypeConfig>::NodeId>,
    RaftError<<C as RaftTypeConfig>::NodeId, InstallSnapshotError>,
>;

struct Job<Req, Resp> {
    request: Req,
    response: oneshot::Sender<Resp>,
}

/// One node-owned worker plus one queued request.
///
/// A socket owns admission and the response receiver only. Once the worker
/// starts a request, dropping that receiver cannot cancel the operation.
pub(crate) struct NodeOwnedExecutor<Req, Resp> {
    tx: flume::Sender<Job<Req, Resp>>,
    admission_timeout: Duration,
    shutdown: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SubmitError {
    AdmissionTimeout,
    ConnectionClosed,
    ExecutorClosed,
    ResponseClosed,
}

impl<Req, Resp> NodeOwnedExecutor<Req, Resp>
where
    Req: Send + 'static,
    Resp: Send + 'static,
{
    pub(crate) fn start<Execute, ExecuteFuture>(
        admission_timeout: Duration,
        mut execute: Execute,
    ) -> Self
    where
        Execute: FnMut(Req) -> ExecuteFuture + Send + 'static,
        ExecuteFuture: Future<Output = Resp> + Send + 'static,
    {
        let (tx, rx) = flume::bounded::<Job<Req, Resp>>(1);
        let (shutdown, mut shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(async move {
            loop {
                let job = tokio::select! {
                    biased;
                    _ = shutdown_rx.changed() => break,
                    job = rx.recv_async() => match job {
                        Ok(job) => job,
                        Err(_) => break,
                    },
                };

                if job.response.is_closed() {
                    continue;
                }

                let result = execute(job.request).await;
                let _ = job.response.send(result);
            }
        });

        Self {
            tx,
            admission_timeout,
            shutdown,
            task: Mutex::new(Some(task)),
        }
    }

    pub(crate) async fn submit(
        &self,
        request: Req,
        connection_closed: &mut watch::Receiver<bool>,
    ) -> Result<Resp, SubmitError> {
        if *connection_closed.borrow() {
            return Err(SubmitError::ConnectionClosed);
        }
        // Subscribe before inspecting the value so a concurrent shutdown is
        // either observed here or wakes the admission/response select below.
        // Subscribing after the check can lose the notification because a new
        // receiver treats the current value as already seen.
        let mut shutdown = self.shutdown.subscribe();
        if *shutdown.borrow() {
            return Err(SubmitError::ExecutorClosed);
        }

        let (response, response_rx) = oneshot::channel();
        let mut pending = Job { request, response };
        let admission_deadline = time::Instant::now() + self.admission_timeout;
        loop {
            if *connection_closed.borrow() {
                return Err(SubmitError::ConnectionClosed);
            }
            if *shutdown.borrow() {
                return Err(SubmitError::ExecutorClosed);
            }
            // Check the absolute boundary before attempting ownership transfer.
            // A successful `try_send` is the only point after which execution
            // is permitted, so AdmissionTimeout can never race an already
            // queued or running job.
            if time::Instant::now() >= admission_deadline {
                return Err(SubmitError::AdmissionTimeout);
            }
            match self.tx.try_send(pending) {
                Ok(()) => break,
                Err(flume::TrySendError::Disconnected(_)) => {
                    return Err(SubmitError::ExecutorClosed);
                }
                Err(flume::TrySendError::Full(job)) => pending = job,
            }
            tokio::select! {
                biased;
                _ = connection_closed.changed() => return Err(SubmitError::ConnectionClosed),
                _ = shutdown.changed() => return Err(SubmitError::ExecutorClosed),
                _ = time::sleep_until(admission_deadline) => {
                    return Err(SubmitError::AdmissionTimeout);
                }
                () = time::sleep(Duration::from_millis(1)) => {}
            }
        }

        tokio::select! {
            biased;
            _ = connection_closed.changed() => Err(SubmitError::ConnectionClosed),
            _ = shutdown.changed() => Err(SubmitError::ExecutorClosed),
            result = response_rx => result.map_err(|_| SubmitError::ResponseClosed),
        }
    }

    pub(crate) fn request_shutdown(&self) {
        self.shutdown.send_replace(true);
    }

    /// Wait without detaching or aborting an accepted operation.
    pub(crate) async fn wait_for_shutdown(&self, timeout: Duration) -> bool {
        self.request_shutdown();
        let mut task_slot = self.task.lock().await;
        let Some(task) = task_slot.as_mut() else {
            return true;
        };

        match time::timeout(timeout, task).await {
            Ok(Ok(())) => {
                task_slot.take();
                true
            }
            Ok(Err(join_error)) => {
                error!("snapshot executor task failed during shutdown: {join_error}");
                task_slot.take();
                false
            }
            Err(_) => {
                warn!("snapshot executor still owns work after bounded shutdown wait");
                false
            }
        }
    }
}

pub(crate) type SnapshotExecutor<C> =
    NodeOwnedExecutor<InstallSnapshotRequest<C>, SnapshotResult<C>>;

pub(crate) fn start_snapshot_executor<C>(
    raft: Raft<C>,
    admission_timeout: Duration,
) -> SnapshotExecutor<C>
where
    C: RaftTypeConfig,
    C::SnapshotData: tokio::io::AsyncRead + tokio::io::AsyncWrite + tokio::io::AsyncSeek + Unpin,
{
    NodeOwnedExecutor::start(admission_timeout, move |request| {
        let raft = raft.clone();
        async move { raft.install_snapshot(request).await }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};
    use tokio::sync::{Notify, Semaphore};

    #[derive(Clone)]
    struct FileWrite {
        offset: usize,
        data: Vec<u8>,
    }

    #[tokio::test]
    async fn executor_runs_one_queues_one_and_drops_abandoned_queued_work() {
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        let completed = Arc::new(AtomicUsize::new(0));
        let executor = Arc::new(NodeOwnedExecutor::start(Duration::from_millis(20), {
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            let completed = Arc::clone(&completed);
            move |request: usize| {
                let gate = Arc::clone(&gate);
                let started = Arc::clone(&started);
                let completed = Arc::clone(&completed);
                async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    gate.acquire().await.expect("test gate").forget();
                    completed.fetch_add(1, Ordering::SeqCst);
                    request
                }
            }
        }));

        let (_close_first, mut first_closed) = watch::channel(false);
        let first = tokio::spawn({
            let executor = Arc::clone(&executor);
            async move { executor.submit(1, &mut first_closed).await }
        });
        while started.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }

        let (close_second, mut second_closed) = watch::channel(false);
        let second = tokio::spawn({
            let executor = Arc::clone(&executor);
            async move { executor.submit(2, &mut second_closed).await }
        });
        while !executor.tx.is_full() {
            tokio::task::yield_now().await;
        }

        let (_close_third, mut third_closed) = watch::channel(false);
        assert_eq!(
            executor.submit(3, &mut third_closed).await,
            Err(SubmitError::AdmissionTimeout)
        );

        close_second.send_replace(true);
        assert_eq!(
            second.await.expect("second caller task"),
            Err(SubmitError::ConnectionClosed)
        );
        gate.add_permits(1);
        assert_eq!(first.await.expect("first caller task"), Ok(1));

        tokio::task::yield_now().await;
        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert!(executor.wait_for_shutdown(Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn shutdown_reports_running_work_without_cancelling_it() {
        let gate = Arc::new(Semaphore::new(0));
        let executor = Arc::new(NodeOwnedExecutor::start(Duration::from_secs(1), {
            let gate = Arc::clone(&gate);
            move |request: usize| {
                let gate = Arc::clone(&gate);
                async move {
                    gate.acquire().await.expect("test gate").forget();
                    request
                }
            }
        }));
        let (_close, mut connection_closed) = watch::channel(false);
        let caller = tokio::spawn({
            let executor = Arc::clone(&executor);
            async move { executor.submit(1, &mut connection_closed).await }
        });
        tokio::task::yield_now().await;

        assert!(!executor.wait_for_shutdown(Duration::from_millis(20)).await);
        gate.add_permits(1);
        assert_eq!(
            caller.await.expect("caller task"),
            Err(SubmitError::ExecutorClosed)
        );
        assert!(executor.wait_for_shutdown(Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn shutdown_before_submission_is_latched_and_admits_no_work() {
        let started = Arc::new(AtomicUsize::new(0));
        let executor = NodeOwnedExecutor::start(Duration::from_secs(1), {
            let started = Arc::clone(&started);
            move |request: usize| {
                let started = Arc::clone(&started);
                async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    request
                }
            }
        });
        executor.request_shutdown();
        let (_close, mut connection_closed) = watch::channel(false);

        assert_eq!(
            executor.submit(1, &mut connection_closed).await,
            Err(SubmitError::ExecutorClosed)
        );
        assert!(executor.tx.is_empty());
        assert_eq!(started.load(Ordering::SeqCst), 0);
        assert!(executor.wait_for_shutdown(Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn cancelled_shutdown_wait_retains_the_executor_handle() {
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(Notify::new());
        let executor = Arc::new(NodeOwnedExecutor::start(Duration::from_secs(1), {
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            move |request: usize| {
                let gate = Arc::clone(&gate);
                let started = Arc::clone(&started);
                async move {
                    started.notify_one();
                    gate.acquire().await.expect("test gate").forget();
                    request
                }
            }
        }));
        let (_close, mut connection_closed) = watch::channel(false);
        let caller = tokio::spawn({
            let executor = Arc::clone(&executor);
            async move { executor.submit(1, &mut connection_closed).await }
        });
        started.notified().await;

        let waiter = tokio::spawn({
            let executor = Arc::clone(&executor);
            async move { executor.wait_for_shutdown(Duration::from_secs(60)).await }
        });
        tokio::task::yield_now().await;
        waiter.abort();
        assert!(
            waiter
                .await
                .expect_err("cancel first shutdown wait")
                .is_cancelled()
        );

        gate.add_permits(1);
        assert_eq!(
            caller.await.expect("caller task"),
            Err(SubmitError::ExecutorClosed)
        );
        assert!(executor.wait_for_shutdown(Duration::from_secs(1)).await);
        assert!(executor.task.lock().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn admission_deadline_never_executes_the_timed_out_job() {
        let gate = Arc::new(Semaphore::new(0));
        let started = Arc::new(Mutex::new(Vec::new()));
        let executor = Arc::new(NodeOwnedExecutor::start(Duration::from_millis(10), {
            let gate = Arc::clone(&gate);
            let started = Arc::clone(&started);
            move |request: usize| {
                let gate = Arc::clone(&gate);
                let started = Arc::clone(&started);
                async move {
                    started.lock().await.push(request);
                    gate.acquire().await.expect("test gate").forget();
                    request
                }
            }
        }));

        let (_close_first, mut first_closed) = watch::channel(false);
        let first = tokio::spawn({
            let executor = Arc::clone(&executor);
            async move { executor.submit(1, &mut first_closed).await }
        });
        while started.lock().await.as_slice() != [1] {
            tokio::task::yield_now().await;
        }
        let (close_second, mut second_closed) = watch::channel(false);
        let second = tokio::spawn({
            let executor = Arc::clone(&executor);
            async move { executor.submit(2, &mut second_closed).await }
        });
        while !executor.tx.is_full() {
            tokio::task::yield_now().await;
        }

        let (_close_third, mut third_closed) = watch::channel(false);
        let third = tokio::spawn({
            let executor = Arc::clone(&executor);
            async move { executor.submit(3, &mut third_closed).await }
        });
        tokio::spawn({
            let gate = Arc::clone(&gate);
            async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                gate.add_permits(1);
            }
        });
        assert_eq!(
            third.await.expect("third caller task"),
            Err(SubmitError::AdmissionTimeout)
        );

        close_second.send_replace(true);
        assert_eq!(
            second.await.expect("second caller task"),
            Err(SubmitError::ConnectionClosed)
        );
        assert_eq!(first.await.expect("first caller task"), Ok(1));
        gate.add_permits(1);
        tokio::task::yield_now().await;
        assert_eq!(started.lock().await.as_slice(), [1, 2]);
        assert!(!started.lock().await.contains(&3));
        assert!(executor.wait_for_shutdown(Duration::from_secs(1)).await);
    }

    #[tokio::test]
    async fn accepted_partial_write_finishes_before_same_offset_retry() {
        let image = (0..1024 * 1024)
            .map(|index| (index % 251) as u8)
            .collect::<Vec<_>>();
        let root =
            std::env::temp_dir().join(format!("hiqlite-partial-snapshot-{}", uuid::Uuid::now_v7()));
        tokio::fs::create_dir_all(&root)
            .await
            .expect("create snapshot test directory");
        let file = root.join("received.snapshot");
        let partial_written = Arc::new(Notify::new());
        let release = Arc::new(Semaphore::new(0));
        let executor = Arc::new(NodeOwnedExecutor::start(Duration::from_secs(1), {
            let file = file.clone();
            let partial_written = Arc::clone(&partial_written);
            let release = Arc::clone(&release);
            move |write: FileWrite| {
                let file = file.clone();
                let partial_written = Arc::clone(&partial_written);
                let release = Arc::clone(&release);
                async move {
                    let split = write.data.len() / 2;
                    let mut received = tokio::fs::OpenOptions::new()
                        .create(true)
                        .truncate(false)
                        .read(true)
                        .write(true)
                        .open(&file)
                        .await
                        .expect("open real snapshot file");
                    received
                        .set_len(write.offset as u64)
                        .await
                        .expect("truncate to accepted offset");
                    received
                        .seek(std::io::SeekFrom::Start(write.offset as u64))
                        .await
                        .expect("seek accepted offset");
                    received
                        .write_all(&write.data[..split])
                        .await
                        .expect("write controlled first half");
                    received.flush().await.expect("flush controlled first half");
                    partial_written.notify_one();
                    release.acquire().await.expect("write release").forget();
                    received
                        .write_all(&write.data[split..])
                        .await
                        .expect("finish accepted snapshot write");
                    received.flush().await.expect("flush complete snapshot");
                    write.data.len()
                }
            }
        }));

        let (close_first, mut first_closed) = watch::channel(false);
        let first = tokio::spawn({
            let executor = Arc::clone(&executor);
            let image = image.clone();
            async move {
                executor
                    .submit(
                        FileWrite {
                            offset: 0,
                            data: image,
                        },
                        &mut first_closed,
                    )
                    .await
            }
        });
        partial_written.notified().await;
        close_first.send_replace(true);
        assert_eq!(
            first.await.expect("first caller task"),
            Err(SubmitError::ConnectionClosed)
        );

        let (_close_retry, mut retry_closed) = watch::channel(false);
        let retry = tokio::spawn({
            let executor = Arc::clone(&executor);
            let image = image.clone();
            async move {
                executor
                    .submit(
                        FileWrite {
                            offset: 0,
                            data: image,
                        },
                        &mut retry_closed,
                    )
                    .await
            }
        });
        while !executor.tx.is_full() {
            tokio::task::yield_now().await;
        }

        release.add_permits(2);
        assert_eq!(retry.await.expect("retry caller task"), Ok(image.len()));
        let actual = tokio::fs::read(&file)
            .await
            .expect("read final snapshot image");
        assert_eq!(Sha256::digest(&actual), Sha256::digest(&image));
        assert_eq!(actual, image);
        assert!(executor.wait_for_shutdown(Duration::from_secs(1)).await);
        tokio::fs::remove_dir_all(root)
            .await
            .expect("remove snapshot test directory");
    }
}
