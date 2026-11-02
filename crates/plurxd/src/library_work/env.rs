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

pub(super) struct TokioEnvironment<'a> {
    manager: &'a Arc<JobManager>,
    transcode: &'a TranscodeManager,
    job: plurx_core::store::background_jobs::BackgroundJob,
    library_id: i64,
    fence: crate::background_jobs::JobFence,
    active: Option<crate::background_jobs::ActiveBackgroundJob>,
    admission: Option<crate::transcode::FragmentAdmission>,
    lease: Option<Arc<ActiveJobLease>>,
    publisher: Option<PublicationStore<'a>>,
    cancellation: CancellationToken,
    work: Option<WorkFuture<'a>>,
    completed: Option<WorkResult<'a>>,
    work_result: Option<Result<(), StoreError>>,
}

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
            admission: Some(admission),
            lease: None,
            publisher: None,
            cancellation: CancellationToken::new(),
            work: None,
            completed: None,
            work_result: None,
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
        // The future owns its publisher and a domain reference. It stays in this
        // Option across RevokeDomain and CancelWork and is consumed by JoinWork.
        self.work = Some(Box::pin(async move {
            let result = plurx_core::process::bounded::cancellable(
                cancellation,
                manager.process_library_requests(&job_id, library_id, &lease, &publisher, &fence),
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
            match driver::select_work(
                queue_loss.cancelled(),
                domain_loss.cancelled(),
                work.as_mut(),
                pressure.tick(),
            )
            .await
            {
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
                        #[cfg(test)]
                        crate::background_jobs::faults::worker::at(
                            crate::background_jobs::faults::worker::Point::DomainUnavailable,
                        )
                        .await;
                        if let Err(error) = result {
                            tracing::warn!(library_id = self.library_id, %error, "library domain lease unavailable");
                        }
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
                settle_library(
                    &self.fence,
                    match settlement {
                        Settlement::Yield { not_before_ms } => JobSettlement::Yield {
                            error_code: None,
                            checkpoint: None,
                            not_before_ms,
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

impl Drop for TokioEnvironment<'_> {
    fn drop(&mut self) {
        debug_assert!(
            self.work.is_none() && self.completed.is_none(),
            "library work must be cancelled and joined, never dropped"
        );
    }
}
