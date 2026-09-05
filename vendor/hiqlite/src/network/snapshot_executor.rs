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
    pub(crate) fn start<Execute, ExecuteFuture>(mut execute: Execute) -> Self
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
            shutdown,
            task: Mutex::new(Some(task)),
        }
    }

    pub(crate) async fn submit(
        &self,
        request: Req,
        admission_timeout: Duration,
        connection_closed: &mut watch::Receiver<bool>,
    ) -> Result<Resp, SubmitError> {
        if *connection_closed.borrow() {
            return Err(SubmitError::ConnectionClosed);
        }
        if *self.shutdown.borrow() {
            return Err(SubmitError::ExecutorClosed);
        }

        let (response, response_rx) = oneshot::channel();
        let admission = self.tx.send_async(Job { request, response });
        tokio::pin!(admission);
        let mut shutdown = self.shutdown.subscribe();
        tokio::select! {
            biased;
            _ = connection_closed.changed() => return Err(SubmitError::ConnectionClosed),
            _ = shutdown.changed() => return Err(SubmitError::ExecutorClosed),
            result = time::timeout(admission_timeout, &mut admission) => match result {
                Ok(Ok(())) => {}
                Ok(Err(_)) => return Err(SubmitError::ExecutorClosed),
                Err(_) => return Err(SubmitError::AdmissionTimeout),
            },
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
        let Some(mut task) = task_slot.take() else {
            return true;
        };

        match time::timeout(timeout, &mut task).await {
            Ok(Ok(())) => true,
            Ok(Err(join_error)) => {
                error!("snapshot executor task failed during shutdown: {join_error}");
                true
            }
            Err(_) => {
                warn!("snapshot executor still owns work after bounded shutdown wait");
                *task_slot = Some(task);
                false
            }
        }
    }
}

pub(crate) type SnapshotExecutor<C> =
    NodeOwnedExecutor<InstallSnapshotRequest<C>, SnapshotResult<C>>;

pub(crate) fn start_snapshot_executor<C>(raft: Raft<C>) -> SnapshotExecutor<C>
where
    C: RaftTypeConfig,
    C::SnapshotData: tokio::io::AsyncRead + tokio::io::AsyncWrite + tokio::io::AsyncSeek + Unpin,
{
    NodeOwnedExecutor::start(move |request| {
        let raft = raft.clone();
        async move { raft.install_snapshot(request).await }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
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
        let executor = Arc::new(NodeOwnedExecutor::start({
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
            async move {
                executor
                    .submit(1, Duration::from_secs(1), &mut first_closed)
                    .await
            }
        });
        while started.load(Ordering::SeqCst) != 1 {
            tokio::task::yield_now().await;
        }

        let (close_second, mut second_closed) = watch::channel(false);
        let second = tokio::spawn({
            let executor = Arc::clone(&executor);
            async move {
                executor
                    .submit(2, Duration::from_secs(1), &mut second_closed)
                    .await
            }
        });
        while !executor.tx.is_full() {
            tokio::task::yield_now().await;
        }

        let (_close_third, mut third_closed) = watch::channel(false);
        assert_eq!(
            executor
                .submit(3, Duration::from_millis(20), &mut third_closed)
                .await,
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
        let executor = Arc::new(NodeOwnedExecutor::start({
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
            async move {
                executor
                    .submit(1, Duration::from_secs(1), &mut connection_closed)
                    .await
            }
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
    async fn accepted_partial_write_finishes_before_same_offset_retry() {
        let image = b"complete snapshot image".to_vec();
        let file = Arc::new(Mutex::new(Vec::<u8>::new()));
        let partial_written = Arc::new(Notify::new());
        let release = Arc::new(Semaphore::new(0));
        let executor = Arc::new(NodeOwnedExecutor::start({
            let file = Arc::clone(&file);
            let partial_written = Arc::clone(&partial_written);
            let release = Arc::clone(&release);
            move |write: FileWrite| {
                let file = Arc::clone(&file);
                let partial_written = Arc::clone(&partial_written);
                let release = Arc::clone(&release);
                async move {
                    let split = write.data.len() / 2;
                    {
                        let mut bytes = file.lock().await;
                        bytes.truncate(write.offset);
                        bytes.extend_from_slice(&write.data[..split]);
                    }
                    partial_written.notify_one();
                    release.acquire().await.expect("write release").forget();
                    file.lock().await.extend_from_slice(&write.data[split..]);
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
                        Duration::from_secs(1),
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
                        Duration::from_secs(1),
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
        assert_eq!(*file.lock().await, image);
        assert!(executor.wait_for_shutdown(Duration::from_secs(1)).await);
    }
}
