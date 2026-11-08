//! Real resources for the shared production library driver.
use super::*;
use plurx_worker_control::library_driver::{
    self as driver, Environment, Reply, Request, Selected, WorkOutcome,
};
use plurx_worker_control::library_lifecycle::{Settlement, SettlementKind};
use std::future::Future;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

type WorkResult<'a> = (PublicationStore<'a>, Result<(), StoreError>);
type WorkFuture<'a> = Pin<Box<dyn Future<Output = WorkResult<'a>> + Send + 'a>>;

fn probe_work<'a>(
    manager: &'a JobManager,
    job_id: &'a str,
    library_id: i64,
    lease: &'a ActiveJobLease,
    publisher: &'a PublicationStore<'a>,
    fence: &'a crate::background_jobs::JobFence,
) -> impl Future<Output = Result<(), StoreError>> + Send + 'a {
    // Each generic task-local/admission scope otherwise carries the complete
    // producer state and amplifies its native poll frame. Keep that state on
    // the heap before entering any scope, while retaining direct await and the
    // same publisher/domain/fence ownership through JoinWork.
    let work =
        Box::pin(manager.process_library_requests(job_id, library_id, lease, publisher, fence));
    plurx_core::scan::probe::with_probe_inputs(
        Arc::new(super::probe_input::LibraryProbeInputs(fence.clone())),
        work,
    )
}

/// One selection over the worker's four sources. The parameter types fix the
/// call-site order (`work` is a future, `pressure` an interval, so swapping
/// them does not compile), and `library_worker_select_prefers_ready_work_to_a_ready_tick`
/// pins the order inside: loss, then work, then the pressure tick.
pub(super) async fn select_next<W>(
    queue_loss: &CancellationToken,
    domain_loss: &CancellationToken,
    work: &mut W,
    pressure: &mut tokio::time::Interval,
) -> Selected<W::Output>
where
    W: Future + Unpin,
{
    driver::select_work(
        queue_loss.cancelled(),
        domain_loss.cancelled(),
        work,
        pressure.tick(),
    )
    .await
}

/// Whether a claimed library job is a contract v2 targeted job.
pub(super) fn is_targeted(job: &plurx_core::store::background_jobs::BackgroundJob) -> bool {
    matches!(
        job.supported_payload(),
        Ok(JobPayload::LibraryScan { generation, .. })
            if generation == plurx_core::store::background_jobs_library::LIBRARY_TARGETED_GENERATION
    )
}

pub(super) struct TokioEnvironment<'a> {
    // Rust drops fields in declaration order, so the order below IS the release
    // order when this environment is dropped without the driver finishing
    // (task abort, runtime shutdown). It must stay the order the pre-driver
    // worker released its stack locals: the live work future first, then its
    // publisher, then the domain lease, then physical admission, and the queue
    // fence (`active`, which cancels the loss tokens) last. Reordering this
    // block changes what a cancelled worker releases while work is still
    // running; `library_worker_abort_releases_in_the_original_order` pins it.
    // There is deliberately no `Drop` impl asserting "joined": dropping with
    // live work is the legitimate abort and shutdown path, and an assertion
    // there is a double panic when the drop happens while unwinding.
    work: Option<WorkFuture<'a>>,
    completed: Option<WorkResult<'a>>,
    publisher: Option<PublicationStore<'a>>,
    cancellation: CancellationToken,
    lease: Option<Arc<ActiveJobLease>>,
    admission: Option<Arc<crate::transcode::FragmentAdmission>>,
    active: Option<crate::background_jobs::ActiveBackgroundJob>,
    // Plain data and borrows: no release order to preserve.
    manager: &'a Arc<JobManager>,
    transcode: &'a TranscodeManager,
    job: plurx_core::store::background_jobs::BackgroundJob,
    library_id: i64,
    fence: crate::background_jobs::JobFence,
    work_result: Option<Result<(), StoreError>>,
    /// The library's mutation fence was held by another job when this one
    /// asked for it.
    domain_refused: bool,
}

/// How soon a targeted job refused by the library fence may be claimed again.
/// The fence holder's release wakes this node's library loop at once; other
/// nodes reach it within their next idle poll.
const TARGETED_FENCE_REPOLL_MS: i64 = 250;

impl<'a> TokioEnvironment<'a> {
    pub(super) fn new(
        manager: &'a Arc<JobManager>,
        transcode: &'a TranscodeManager,
        job: plurx_core::store::background_jobs::BackgroundJob,
        library_id: i64,
        active: crate::background_jobs::ActiveBackgroundJob,
        admission: crate::transcode::FragmentAdmission,
    ) -> Self {
        Self {
            manager,
            transcode,
            job,
            library_id,
            fence: active.fence(),
            active: Some(active),
            admission: Some(Arc::new(admission)),
            lease: None,
            publisher: None,
            cancellation: CancellationToken::new(),
            work: None,
            completed: None,
            work_result: None,
            domain_refused: false,
        }
    }

    async fn poll_work(&mut self) -> WorkOutcome {
        let lease = Arc::clone(
            self.lease
                .as_ref()
                .expect("Process requires the bound domain"),
        );
        let queue_loss = self.fence.loss_token();
        let domain_loss = lease.loss_token();
        #[cfg(test)]
        crate::background_jobs::faults::worker::at_with_signals(
            crate::background_jobs::faults::worker::Point::Processing,
            Some(crate::background_jobs::faults::worker::Signals {
                queue_loss: queue_loss.clone(),
                domain_loss: domain_loss.clone(),
            }),
        )
        .await;
        let manager = self.manager;
        let job_id = self.job.id.clone();
        let library_id = self.library_id;
        let fence = self.fence.clone();
        let publisher = self
            .publisher
            .take()
            .expect("Process requires the bound publisher");
        let cancellation = self.cancellation.clone();
        let admission = Arc::clone(
            self.admission
                .as_ref()
                .expect("Process retains physical admission"),
        );
        let transcode = self.transcode;
        let shutdown = queue_loss.clone();
        // The future owns its publisher and a domain reference. It stays in this
        // Option across RevokeDomain and CancelWork and is consumed by JoinWork.
        self.work = Some(Box::pin(async move {
            // This is the library job's owned stop/read-join driver. Its
            // controller reaches the probe child and the private source bridge
            // through the directly awaited task-local scope. Finite byte holds
            // exclude active probe time only after every child and daemon read
            // has been verified stopped; the helper retains a finite wall age.
            let result = crate::source_probe::run_source_admitted_probe(
                transcode,
                &admission,
                &cancellation,
                &shutdown,
                None,
                &fence,
                || async { true },
                probe_work(manager, &job_id, library_id, &lease, &publisher, &fence),
            )
            .await;
            (publisher, result)
        }));
        let work = self.work.as_mut().expect("work was just created");
        let admission = self
            .admission
            .as_ref()
            .expect("Process retains physical admission");
        let mut pressure = tokio::time::interval(Duration::from_millis(250));
        loop {
            // Shared priority: queue loss > domain loss > work > pressure tick.
            // A ready tick cannot hide a ready work result or prevent its poll.
            match select_next(&queue_loss, &domain_loss, work, &mut pressure).await {
                Selected::QueueLoss => {
                    #[cfg(test)]
                    crate::background_jobs::faults::worker::at(
                        crate::background_jobs::faults::worker::Point::QueueLoss,
                    )
                    .await;
                    return WorkOutcome::QueueLoss;
                }
                Selected::DomainLoss => {
                    #[cfg(test)]
                    crate::background_jobs::faults::worker::at(
                        crate::background_jobs::faults::worker::Point::DomainLoss,
                    )
                    .await;
                    return WorkOutcome::DomainLoss;
                }
                Selected::Work(result) => {
                    let ok = result.1.is_ok();
                    self.completed = Some(result);
                    return WorkOutcome::Completed { ok };
                }
                Selected::Tick => {
                    let pressured = !self.transcode.fragment_worker_idle(admission);
                    // On Linux the owned source driver above handles source
                    // pressure by verified suspension and slot reacquisition.
                    #[cfg(not(target_os = "linux"))]
                    let pressured =
                        pressured || !self.fence.source_may_continue().await.unwrap_or(false);
                    #[cfg(test)]
                    let pressured =
                        pressured || crate::background_jobs::faults::worker::force_pressure();
                    if pressured {
                        #[cfg(test)]
                        crate::background_jobs::faults::worker::at(
                            crate::background_jobs::faults::worker::Point::Pressure,
                        )
                        .await;
                        return WorkOutcome::Pressure;
                    }
                }
            }
        }
    }
}

impl Environment for TokioEnvironment<'_> {
    async fn perform(&mut self, request: Request) -> Reply {
        match request {
            Request::AcquireDomain => {
                let acquired = match self
                    .manager
                    .acquire_job(format!("scan:library:{}", self.library_id))
                    .await
                {
                    Ok(Some(acquired)) => {
                        #[cfg(test)]
                        crate::background_jobs::faults::worker::at(
                            crate::background_jobs::faults::worker::Point::DomainAcquired,
                        )
                        .await;
                        self.lease = Some(Arc::new(acquired));
                        true
                    }
                    result => {
                        let fence_held = result.is_ok();
                        if let Err(error) = result {
                            tracing::warn!(library_id = self.library_id, %error, "library domain lease unavailable");
                        }
                        self.domain_refused = true;
                        // Only a lease another worker holds is `full_scan:`;
                        // an unreachable coordinator names no wait.
                        if fence_held && is_targeted(&self.job) {
                            // Another worker holds the library's mutation
                            // fence — a full scan or a metadata refresh, both
                            // reported as `full_scan:<library>`. A targeted job
                            // never runs beside it (its reconcile would prune
                            // what this one inserts), so it yields and says so.
                            let name = match self.manager.store.get_library(self.library_id).await {
                                Ok(Some(library)) => library.name,
                                _ => self.library_id.to_string(),
                            };
                            crate::background_jobs::note_targeted_wait(
                                self.manager.store.as_ref(),
                                &self.job.id,
                                &format!("full_scan:{name}"),
                            )
                            .await;
                        }
                        #[cfg(test)]
                        crate::background_jobs::faults::worker::at(
                            crate::background_jobs::faults::worker::Point::DomainUnavailable,
                        )
                        .await;
                        false
                    }
                };
                Reply::Domain {
                    acquired,
                    now_ms: if acquired { 0 } else { clock_ms() },
                }
            }
            Request::Bind => {
                self.publisher = Some(
                    self.lease
                        .as_ref()
                        .expect("Bind requires domain")
                        .publisher(self.manager.store.as_ref()),
                );
                let accepted = match self
                    .fence
                    .bind_library(self.publisher.as_ref().expect("Bind created publisher"))
                    .await
                {
                    Ok(true) => {
                        if is_targeted(&self.job) {
                            // Running now: nothing is in its way any more.
                            crate::background_jobs::note_targeted_wait(
                                self.manager.store.as_ref(),
                                &self.job.id,
                                "",
                            )
                            .await;
                        }
                        #[cfg(test)]
                        crate::background_jobs::faults::worker::at(
                            crate::background_jobs::faults::worker::Point::Bound,
                        )
                        .await;
                        true
                    }
                    result => {
                        tracing::warn!(
                            library_id = self.library_id,
                            ?result,
                            "library publication binding refused"
                        );
                        false
                    }
                };
                Reply::Bound { accepted }
            }
            Request::PollWork => Reply::Work(self.poll_work().await),
            Request::RevokeDomain => {
                self.lease
                    .as_ref()
                    .expect("revocation retains domain")
                    .revoke();
                Reply::Done
            }
            Request::CancelWork => {
                self.cancellation.cancel();
                Reply::Done
            }
            Request::JoinWork => {
                let work = self
                    .work
                    .take()
                    .expect("join consumes the same work future once");
                let (publisher, result) = match self.completed.take() {
                    Some(result) => {
                        drop(work);
                        result
                    }
                    None => work.await,
                };
                self.publisher = Some(publisher);
                let ok = result.is_ok();
                self.work_result = Some(result);
                #[cfg(test)]
                crate::background_jobs::faults::worker::at(
                    crate::background_jobs::faults::worker::Point::WorkJoined,
                )
                .await;
                Reply::Joined { ok }
            }
            Request::DropPublisher => {
                drop(self.publisher.take().expect("cleanup requires publisher"));
                Reply::Done
            }
            Request::ReleaseDomain { settlement } => {
                let unbound = settlement == SettlementKind::Refused;
                if !unbound {
                    #[cfg(test)]
                    crate::background_jobs::faults::worker::at(
                        crate::background_jobs::faults::worker::Point::ReleaseStarted,
                    )
                    .await;
                }
                let lease = Arc::try_unwrap(self.lease.take().expect("release consumes domain"))
                    .unwrap_or_else(|_| {
                        panic!("joined work must have returned its domain reference")
                    });
                if let Err(error) = lease.release().await {
                    if unbound {
                        tracing::warn!(%error, "releasing unbound library lease");
                    } else {
                        tracing::warn!(library_id = self.library_id, %error, "releasing library domain lease");
                    }
                }
                if !unbound {
                    // A targeted request refused by this fence may be waiting:
                    // let this node's library loop claim it now rather than at
                    // its next idle poll (contract v2 §5.1).
                    if !is_targeted(&self.job) {
                        self.manager.library_wake.notify_one();
                    }
                    #[cfg(test)]
                    crate::background_jobs::faults::worker::at(
                        crate::background_jobs::faults::worker::Point::ReleaseFinished,
                    )
                    .await;
                }
                if settlement == SettlementKind::Retry {
                    let error = self
                        .work_result
                        .as_ref()
                        .expect("retry follows work")
                        .as_ref()
                        .expect_err("retry follows failed work");
                    tracing::warn!(library_id = self.library_id, %error, "durable library attempt failed");
                }
                Reply::Released {
                    now_ms: if settlement == SettlementKind::None {
                        0
                    } else {
                        clock_ms()
                    },
                    retry_delay_ms: if settlement == SettlementKind::Retry {
                        crate::background_jobs::retry_delay_ms(
                            &self.job.id,
                            self.job.failed_attempts,
                        )
                    } else {
                        0
                    },
                }
            }
            Request::Settle(settlement) => {
                // The shared driver defers any fence refusal by five seconds.
                // A targeted request waits only for the fence itself: its
                // retry is due almost at once, and the holder's release wakes
                // the loop that claims it.
                let targeted_refusal = self.domain_refused && is_targeted(&self.job);
                settle_library(
                    &self.fence,
                    match settlement {
                        Settlement::Yield { not_before_ms } => JobSettlement::Yield {
                            error_code: None,
                            checkpoint: None,
                            not_before_ms: if targeted_refusal {
                                not_before_ms.min(clock_ms() + TARGETED_FENCE_REPOLL_MS)
                            } else {
                                not_before_ms
                            },
                        },
                        Settlement::Retry { not_before_ms } => JobSettlement::Retry {
                            error_code: "library_execution_failed".into(),
                            not_before_ms,
                        },
                    },
                )
                .await;
                Reply::Done
            }
            Request::RetireQueue => {
                self.active
                    .take()
                    .expect("queue is retired once")
                    .finish()
                    .await;
                #[cfg(test)]
                crate::background_jobs::faults::worker::at(
                    crate::background_jobs::faults::worker::Point::Finished,
                )
                .await;
                Reply::Done
            }
            Request::DropAdmission => {
                drop(
                    self.admission
                        .take()
                        .expect("admission is released once, last"),
                );
                #[cfg(test)]
                crate::background_jobs::faults::worker::at(
                    crate::background_jobs::faults::worker::Point::AdmissionDropped,
                )
                .await;
                Reply::Done
            }
        }
    }
}

#[cfg(test)]
mod stack_tests {
    use super::*;

    #[test]
    fn library_scan_scope_futures_fit_a_normal_worker_stack() {
        fn scoped_size<F: Future>(
            _: impl FnOnce(
                &'static JobManager,
                &'static str,
                i64,
                &'static ActiveJobLease,
                &'static PublicationStore<'static>,
                &'static crate::background_jobs::JobFence,
            ) -> F,
        ) -> usize {
            std::mem::size_of::<F>()
        }
        fn driver_size<F: Future>(
            _: impl FnOnce(&'static mut TokioEnvironment<'static>) -> F,
        ) -> usize {
            std::mem::size_of::<F>()
        }
        let scoped = scoped_size(probe_work);
        let driver = driver_size(TokioEnvironment::poll_work);
        eprintln!("library producer scope: {scoped} bytes; driver: {driver} bytes");
        // Generic admission/task-local scopes must carry a small payload.
        // The real-daemon seed separately exercises the ordinary worker stack;
        // future size alone cannot prove its complete native poll-stack bound.
        assert!(
            scoped < 4 * 1024,
            "library producer scope is {scoped} bytes"
        );
        assert!(
            driver < 32 * 1024,
            "library driver future is {driver} bytes"
        );
    }
}
