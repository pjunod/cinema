//! Durable library admission and execution; the existing scanner owns identity.
use super::*;
use plurx_core::store::background_jobs::{EnqueueOutcome, JobPayload, JobSettlement, JobState};
use plurx_core::store::background_jobs_library::{
    LibraryTrigger, LibraryWorkInput, LibraryWorkQuery, LibraryWorkRecord, LibraryWorkResult,
    NewLibraryWork, MAX_LIBRARY_RESULT_BYTES,
};
use plurx_worker_control::library_lifecycle::{
    self, Effect, Effects, Event, Interrupt, Settlement, SettlementKind, State,
};

/// All events come from the immediately preceding effect's result. A stale
/// pair here is an internal driver bug; it must not invent a recovery protocol.
fn advance_library_lifecycle(state: &mut State, event: Event) -> Effects {
    let (next, effects) = library_lifecycle::step(*state, event)
        .expect("library driver event must match its current protocol phase");
    *state = next;
    effects
}

impl JobManager {
    pub(super) async fn trigger(
        self: &Arc<Self>,
        library_id: i64,
        refresh: bool,
        why: ScanTrigger,
    ) -> bool {
        let trigger = match why {
            ScanTrigger::Scheduled => LibraryTrigger::Scheduled,
            ScanTrigger::Startup => LibraryTrigger::Startup,
            ScanTrigger::Manual | ScanTrigger::Targeted => LibraryTrigger::Manual,
        };
        match self
            .admit_library(NewLibraryWork {
                request_id: uuid::Uuid::new_v4().to_string(),
                library_id,
                input: LibraryWorkInput::Full { refresh, trigger },
                now_ms: clock_ms(),
            })
            .await
        {
            Ok(accepted) => accepted,
            Err(error) => {
                tracing::warn!(library_id, %error, "library intent was not accepted");
                false
            }
        }
    }

    /// Acceptance is durable before returning 202. A replay of a completed
    /// request can still return its result immediately; new work is dispatched
    /// by any eligible voter, independently of this HTTP process's lifetime.
    pub async fn request_scan(
        self: &Arc<Self>,
        req: ScanRequest,
    ) -> Result<Option<TargetedScan>, TargetError> {
        self.admit_library(NewLibraryWork {
            request_id: req.id.clone(),
            library_id: req.library_id,
            input: LibraryWorkInput::Targeted {
                path: req.path,
                ids: req.ids,
                book: req.book.map(Box::new),
                correlation_id: req.correlation_id,
                source: req.source,
            },
            now_ms: clock_ms(),
        })
        .await?;
        let records = self
            .store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some(req.id),
                limit: 1,
                ..Default::default()
            })
            .await?;
        match records.into_iter().next().and_then(|record| record.result) {
            Some(LibraryWorkResult::Completed { scan }) => Ok(Some(scan)),
            Some(LibraryWorkResult::Failed { error }) => {
                Err(TargetError::Store(StoreError::Task(error)))
            }
            None => Ok(None),
        }
    }

    async fn admit_library(&self, input: NewLibraryWork) -> Result<bool, TargetError> {
        match self.store.enqueue_library_work(input).await? {
            EnqueueOutcome::Accepted { .. } => {
                self.library_wake.notify_one();
                Ok(true)
            }
            EnqueueOutcome::Existing {
                cancelled: false, ..
            } => Ok(false),
            // Full, or the same request is still being cancelled: nothing is
            // wrong and trying again is the answer. A conflict, a library that
            // vanished, or fenced producer is not — those stay failures.
            outcome @ (EnqueueOutcome::QueueFull
            | EnqueueOutcome::JobCancelling { .. }
            | EnqueueOutcome::Existing {
                cancelled: true, ..
            }) => Err(TargetError::Refused(format!(
                "library work was not accepted: {outcome:?}"
            ))),
            outcome => Err(TargetError::Store(StoreError::Task(format!(
                "library work was not accepted: {outcome:?}"
            )))),
        }
    }

    pub async fn scan_requests(&self) -> Vec<ScanRequestRecord> {
        match self
            .store
            .library_work_requests(LibraryWorkQuery {
                limit: MAX_REQUESTS,
                ..Default::default()
            })
            .await
        {
            Ok(records) => records.into_iter().rev().map(request_record).collect(),
            Err(error) => {
                tracing::warn!(%error, "reading durable scan requests");
                Vec::new()
            }
        }
    }

    pub async fn scan_request(&self, id: &str) -> Option<ScanRequestRecord> {
        match self
            .store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some(id.into()),
                limit: 1,
                ..Default::default()
            })
            .await
        {
            Ok(records) => records.into_iter().next().map(request_record),
            Err(error) => {
                tracing::warn!(%error, "reading durable scan request");
                None
            }
        }
    }

    pub(super) async fn durable_library_statuses(&self) -> HashMap<i64, ScanStatus> {
        let records = match self
            .store
            .library_work_requests(LibraryWorkQuery {
                limit: MAX_REQUESTS,
                ..Default::default()
            })
            .await
        {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(%error, "reading durable library statuses");
                return HashMap::new();
            }
        };
        let mut statuses = HashMap::new();
        let readiness = self
            .library_readiness
            .current_problems(self.store.as_ref())
            .await;
        let now_ms = clock_ms();
        for record in records {
            if !matches!(record.input, LibraryWorkInput::Full { .. })
                || statuses.contains_key(&record.library_id)
            {
                continue;
            }
            let pending = record.state == "pending";
            let executing = pending
                && matches!(
                    record.job_state,
                    Some(JobState::Running | JobState::Cancelling)
                )
                && record
                    .job_lease_expires_ms
                    .is_some_and(|expires| expires > now_ms);
            let phase = pending.then(|| {
                if !executing {
                    "queued"
                } else if matches!(record.input, LibraryWorkInput::Full { refresh: true, .. }) {
                    "enriching"
                } else {
                    "scanning"
                }
                .into()
            });
            let (last_scan, mut error) = match record.result {
                Some(LibraryWorkResult::Completed { scan }) => (Some(scan.report), None),
                Some(LibraryWorkResult::Failed { error }) => (None, Some(error)),
                None => (None, record.error_code),
            };
            if pending && !executing && error.is_none() {
                error = readiness.get(&record.library_id).cloned();
            }
            statuses.insert(
                record.library_id,
                ScanStatus {
                    running: pending,
                    phase,
                    started_at: Some(record.created_at_ms / 1_000),
                    finished_at: (!pending).then_some(record.updated_at_ms / 1_000),
                    last_scan,
                    error,
                    ..Default::default()
                },
            );
        }
        statuses
    }

    pub(super) async fn work_library_queue(
        self: &Arc<Self>,
        transcode: &TranscodeManager,
        library_filter: Option<i64>,
    ) -> bool {
        let (job, active, admission) = match crate::background_jobs::claim_library(
            Arc::clone(&self.store),
            Arc::clone(&self.job_authority),
            transcode,
            self.coordinator.node_id(),
            library_filter,
            &self.library_readiness,
        )
        .await
        {
            Ok(Some(claim)) => claim,
            Ok(None) => return false,
            Err(error) => {
                tracing::warn!(%error, "claiming library work");
                return false;
            }
        };
        #[cfg(test)]
        crate::background_jobs::faults::worker::at(
            crate::background_jobs::faults::worker::Point::Claimed,
        )
        .await;
        let fence = active.fence();
        let library_id = match job.supported_payload() {
            Ok(
                JobPayload::LibraryScan { library_id, .. }
                | JobPayload::MetadataRefresh { library_id, .. },
            ) => library_id,
            _ => {
                active.finish().await;
                return false;
            }
        };
        // The controller owns protocol phases; these Options own the actual
        // guards. Only the corresponding effects consume them. Impossible
        // phase/resource combinations are driver programming errors, rather
        // than a second cleanup protocol alongside the controller.
        let mut active = Some(active);
        let mut admission = Some(admission);
        let mut lease = None;
        let mut publisher = None;
        let mut work_result = None;
        let mut controller = State::Initial;
        let mut effects = advance_library_lifecycle(&mut controller, Event::Start);
        while !effects.as_slice().is_empty() {
            let current = effects;
            effects = Effects::empty();
            for effect in current.as_slice() {
                match *effect {
                    Effect::AcquireDomain => {
                        let acquired = match self
                            .acquire_job(format!("scan:library:{library_id}"))
                            .await
                        {
                            Ok(Some(acquired)) => {
                                #[cfg(test)]
                                crate::background_jobs::faults::worker::at(
                                    crate::background_jobs::faults::worker::Point::DomainAcquired,
                                )
                                .await;
                                lease = Some(acquired);
                                true
                            }
                            result => {
                                #[cfg(test)]
                                crate::background_jobs::faults::worker::at(
                                    crate::background_jobs::faults::worker::Point::DomainUnavailable,
                                )
                                .await;
                                if let Err(error) = result {
                                    tracing::warn!(library_id, %error, "library domain lease unavailable");
                                }
                                false
                            }
                        };
                        effects = advance_library_lifecycle(
                            &mut controller,
                            Event::DomainOutcome {
                                acquired,
                                now_ms: if acquired { 0 } else { clock_ms() },
                            },
                        );
                    }
                    Effect::Bind => {
                        publisher = Some(
                            lease
                                .as_ref()
                                .expect("Bind requires an acquired library domain")
                                .publisher(self.store.as_ref()),
                        );
                        let accepted = match fence
                            .bind_library(publisher.as_ref().expect("Bind just created publisher"))
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
                                    library_id,
                                    ?result,
                                    "library publication binding refused"
                                );
                                false
                            }
                        };
                        effects = advance_library_lifecycle(
                            &mut controller,
                            Event::BindOutcome { accepted },
                        );
                    }
                    Effect::Process => {
                        let lease = lease.as_ref().expect("Process requires the bound domain");
                        let publisher = publisher
                            .as_ref()
                            .expect("Process requires the bound publisher");
                        let admission = admission
                            .as_ref()
                            .expect("Process retains physical admission");
                        let cancellation = tokio_util::sync::CancellationToken::new();
                        let queue_loss = fence.loss_token();
                        let domain_loss = lease.loss_token();
                        let result = {
                            #[cfg(test)]
                            crate::background_jobs::faults::worker::at_with_signals(
                                crate::background_jobs::faults::worker::Point::Processing,
                                Some(crate::background_jobs::faults::worker::Signals {
                                    queue_loss: queue_loss.clone(),
                                    domain_loss: domain_loss.clone(),
                                }),
                            )
                            .await;
                            let work = plurx_core::process::bounded::cancellable(
                                cancellation.clone(),
                                self.process_library_requests(
                                    &job.id, library_id, lease, publisher, &fence,
                                ),
                            );
                            tokio::pin!(work);
                            let mut pressure = tokio::time::interval(Duration::from_millis(250));
                            let (interrupt, mut completed) = loop {
                                tokio::select! {
                                    biased;
                                    () = queue_loss.cancelled() => {
                                        #[cfg(test)]
                                        crate::background_jobs::faults::worker::at(
                                            crate::background_jobs::faults::worker::Point::QueueLoss,
                                        )
                                        .await;
                                        break (Interrupt::QueueLoss, None);
                                    }
                                    () = domain_loss.cancelled() => {
                                        #[cfg(test)]
                                        crate::background_jobs::faults::worker::at(
                                            crate::background_jobs::faults::worker::Point::DomainLoss,
                                        )
                                        .await;
                                        break (Interrupt::DomainLoss, None);
                                    }
                                    _ = pressure.tick() => {
                                        let pressured = !transcode.fragment_worker_idle(admission);
                                        #[cfg(test)]
                                        let pressured = pressured
                                            || crate::background_jobs::faults::worker::force_pressure();
                                        if pressured {
                                            #[cfg(test)]
                                            crate::background_jobs::faults::worker::at(
                                                crate::background_jobs::faults::worker::Point::Pressure,
                                            )
                                            .await;
                                            break (Interrupt::Pressure, None);
                                        }
                                    }
                                    result = &mut work => break (Interrupt::Completed, Some(result)),
                                }
                            };
                            // This executor stays in the work future's lexical
                            // scope so cancellation is followed by its real join.
                            let interruption = advance_library_lifecycle(
                                &mut controller,
                                Event::Interrupt(interrupt),
                            );
                            let mut joined = None;
                            for effect in interruption.as_slice() {
                                match effect {
                                    Effect::RevokeDomain => lease.revoke(),
                                    Effect::CancelWork => cancellation.cancel(),
                                    Effect::AwaitWorkJoin => {
                                        joined = Some(match completed.take() {
                                            Some(result) => result,
                                            None => work.as_mut().await,
                                        });
                                    }
                                    _ => unreachable!(
                                        "interrupt emits only cancellation/join effects"
                                    ),
                                }
                            }
                            joined.expect("every interrupt consumes the work join")
                        };
                        #[cfg(test)]
                        crate::background_jobs::faults::worker::at(
                            crate::background_jobs::faults::worker::Point::WorkJoined,
                        )
                        .await;
                        // `work` has returned only after the blocking walker and
                        // any bounded media child joined. Admission is still held.
                        effects = advance_library_lifecycle(
                            &mut controller,
                            Event::WorkJoined { ok: result.is_ok() },
                        );
                        work_result = Some(result);
                    }
                    Effect::DropPublisher => {
                        drop(publisher.take().expect("cleanup requires the publisher"));
                    }
                    Effect::ReleaseDomain => {
                        let settlement = match controller {
                            State::Releasing { settlement } => settlement,
                            _ => unreachable!("ReleaseDomain is emitted only while Releasing"),
                        };
                        let unbound = settlement == SettlementKind::Refused;
                        if !unbound {
                            #[cfg(test)]
                            crate::background_jobs::faults::worker::at(
                                crate::background_jobs::faults::worker::Point::ReleaseStarted,
                            )
                            .await;
                        }
                        if let Err(error) = lease
                            .take()
                            .expect("release consumes the acquired domain")
                            .release()
                            .await
                        {
                            if unbound {
                                tracing::warn!(%error, "releasing unbound library lease");
                            } else {
                                tracing::warn!(library_id, %error, "releasing library domain lease");
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
                            let error = work_result
                                .as_ref()
                                .expect("retry follows a work result")
                                .as_ref()
                                .expect_err("retry follows failed work");
                            tracing::warn!(library_id, %error, "durable library attempt failed");
                        }
                        // Preserve sampling after release (and its fault point),
                        // including when release itself returned an ambiguity.
                        let now_ms = if settlement == SettlementKind::None {
                            0
                        } else {
                            clock_ms()
                        };
                        let retry_delay_ms = if settlement == SettlementKind::Retry {
                            crate::background_jobs::retry_delay_ms(&job.id, job.failed_attempts)
                        } else {
                            0
                        };
                        effects = advance_library_lifecycle(
                            &mut controller,
                            Event::DomainReleased {
                                now_ms,
                                retry_delay_ms,
                            },
                        );
                    }
                    Effect::Settle(settlement) => {
                        settle_library(
                            &fence,
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
                    }
                    Effect::RetireQueue => {
                        active.take().expect("queue is retired once").finish().await;
                        #[cfg(test)]
                        crate::background_jobs::faults::worker::at(
                            crate::background_jobs::faults::worker::Point::Finished,
                        )
                        .await;
                    }
                    Effect::DropAdmission => {
                        drop(admission.take().expect("admission is released once, last"));
                        #[cfg(test)]
                        crate::background_jobs::faults::worker::at(
                            crate::background_jobs::faults::worker::Point::AdmissionDropped,
                        )
                        .await;
                    }
                    Effect::RevokeDomain | Effect::CancelWork | Effect::AwaitWorkJoin => {
                        unreachable!("work effects are consumed inside the Process scope");
                    }
                }
            }
        }
        true
    }

    async fn process_library_requests(
        &self,
        job_id: &str,
        library_id: i64,
        lease: &ActiveJobLease,
        publisher: &PublicationStore<'_>,
        fence: &crate::background_jobs::JobFence,
    ) -> Result<(), StoreError> {
        // Bound one turn even when callers continuously add new interests.
        for _ in 0..8 {
            let requests = self
                .store
                .library_work_requests(LibraryWorkQuery {
                    job_id: Some(job_id.into()),
                    pending_only: true,
                    limit: MAX_PENDING_PER_LIBRARY,
                    ..Default::default()
                })
                .await?;
            if requests.is_empty() {
                return Ok(());
            }
            check_active()?;
            if self.store.get_library(library_id).await?.is_none() {
                for request in requests {
                    self.complete_library_request(
                        publisher,
                        fence,
                        request.request_id,
                        LibraryWorkResult::Failed {
                            error: "library no longer exists".into(),
                        },
                    )
                    .await?;
                }
                continue;
            }
            let mut full = Vec::new();
            let mut groups: BTreeMap<PathBuf, Vec<(String, ScanRequest)>> = BTreeMap::new();
            for request in requests {
                match request.input {
                    LibraryWorkInput::Full { refresh, trigger } => {
                        full.push((request.request_id, refresh, trigger))
                    }
                    LibraryWorkInput::Targeted {
                        path,
                        ids,
                        book,
                        correlation_id,
                        source,
                    } => {
                        let normalized = tokio::fs::canonicalize(&path)
                            .await
                            .unwrap_or_else(|_| path.clone());
                        groups.entry(normalized).or_default().push((
                            request.request_id.clone(),
                            ScanRequest {
                                id: request.request_id,
                                library_id,
                                path,
                                ids,
                                book: book.map(|value| *value),
                                correlation_id,
                                source,
                            },
                        ));
                    }
                }
            }
            if !full.is_empty() {
                #[cfg(test)]
                crate::background_jobs::faults::worker::at(
                    crate::background_jobs::faults::worker::Point::BeforeScan,
                )
                .await;
                let why = match full[0].2 {
                    LibraryTrigger::Manual => ScanTrigger::Manual,
                    LibraryTrigger::Scheduled => ScanTrigger::Scheduled,
                    LibraryTrigger::Startup => ScanTrigger::Startup,
                };
                self.metrics.count_scan(why);
                let progress = Arc::new(ScanProgress::default());
                self.live
                    .lock()
                    .await
                    .insert(library_id, Arc::clone(&progress));
                self.statuses.lock().await.insert(
                    library_id,
                    ScanStatus {
                        running: true,
                        phase: Some("scanning".into()),
                        started_at: Some(now()),
                        ..Default::default()
                    },
                );
                let scan = self
                    .run_scan(
                        library_id,
                        progress,
                        full.iter().any(|entry| entry.1),
                        lease,
                    )
                    .await?;
                for (id, _, _) in full {
                    self.complete_library_request(
                        publisher,
                        fence,
                        id,
                        LibraryWorkResult::Completed { scan: scan.clone() },
                    )
                    .await?;
                }
            }
            for requests in groups.into_values() {
                check_active()?;
                let group: Vec<_> = requests
                    .iter()
                    .map(|(_, request)| request.clone())
                    .collect();
                let result = match self.run_targeted(&group, publisher).await {
                    Ok(scan) => LibraryWorkResult::Completed { scan },
                    Err(error @ TargetError::OutsideRoots { .. }) => LibraryWorkResult::Failed {
                        error: error.to_string(),
                    },
                    Err(TargetError::Store(error)) => return Err(error),
                    Err(error @ TargetError::Refused(_)) => LibraryWorkResult::Failed {
                        error: error.to_string(),
                    },
                };
                for (id, _) in requests {
                    self.complete_library_request(publisher, fence, id, result.clone())
                        .await?;
                }
            }
        }
        settle_library(
            fence,
            JobSettlement::Yield {
                error_code: None,
                checkpoint: None,
                not_before_ms: clock_ms(),
            },
        )
        .await;
        Ok(())
    }

    async fn complete_library_request(
        &self,
        publisher: &PublicationStore<'_>,
        fence: &crate::background_jobs::JobFence,
        id: String,
        mut result: LibraryWorkResult,
    ) -> Result<(), StoreError> {
        if serde_json::to_vec(&result)
            .map_err(|error| StoreError::Task(error.to_string()))?
            .len()
            > MAX_LIBRARY_RESULT_BYTES
        {
            result = LibraryWorkResult::Failed { error: "scan result exceeded the durable 64 KiB response bound; narrow the requested path".into() };
        }
        #[cfg(test)]
        crate::background_jobs::faults::worker::at(
            crate::background_jobs::faults::worker::Point::BeforeCompletion,
        )
        .await;
        let reply = fence.complete_library(publisher, id.clone(), result).await;
        if matches!(reply, Ok(true)) {
            return Ok(());
        }
        // A lost completion acknowledgement must not rerun a published result.
        let current = self
            .store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some(id),
                limit: 1,
                ..Default::default()
            })
            .await?;
        #[cfg(test)]
        crate::background_jobs::faults::worker::at(
            crate::background_jobs::faults::worker::Point::CompletionReconciled,
        )
        .await;
        if current
            .first()
            .is_some_and(|record| record.state != "pending")
        {
            return Ok(());
        }
        Err(reply
            .err()
            .unwrap_or_else(|| StoreError::Task("library completion lost its owner".into())))
    }
}

pub(super) fn check_active() -> Result<(), StoreError> {
    plurx_core::process::bounded::check_cancellation()
        .map_err(|error| StoreError::Task(error.to_string()))
}

async fn settle_library(fence: &crate::background_jobs::JobFence, settlement: JobSettlement) {
    if let Err(error) = fence.settle(settlement).await {
        tracing::warn!(%error, "library settlement was not acknowledged");
    }
}

fn request_record(record: LibraryWorkRecord) -> ScanRequestRecord {
    let (path, correlation_id, source) = match record.input {
        LibraryWorkInput::Targeted {
            path,
            correlation_id,
            source,
            ..
        } => (path.display().to_string(), correlation_id, source),
        LibraryWorkInput::Full { .. } => (String::new(), None, Some("library".into())),
    };
    let (report, items, error) = match record.result {
        Some(LibraryWorkResult::Completed { scan }) => (Some(scan.report), Some(scan.items), None),
        Some(LibraryWorkResult::Failed { error }) => (None, None, Some(error)),
        None => (None, None, record.error_code),
    };
    ScanRequestRecord {
        request_id: record.request_id,
        at: record.created_at_ms / 1_000,
        library_id: record.library_id,
        path,
        correlation_id,
        source,
        status: match record.state.as_str() {
            "succeeded" => "done",
            "pending" => "queued",
            other => other,
        }
        .into(),
        report,
        items,
        error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::background_jobs::faults::worker::{Event as WorkerEvent, Point as WorkerPoint};
    use plurx_core::cluster::coordination::LeaseClaim;
    use plurx_core::domain::NewLibrary;
    use plurx_core::store::background_jobs_library::{LibraryTrigger, LibraryWorkInput};
    use plurx_core::store::{SqliteStore, Store};
    use plurx_core::transcode::{EncoderCaps, Pipeline};
    use tokio::sync::{mpsc, oneshot};

    type SpawnedWorker = (
        tokio::task::JoinHandle<(bool, u8)>,
        Arc<crate::background_jobs::faults::worker::Scope>,
        Option<oneshot::Sender<()>>,
    );

    fn worker_scope(
        worker: &str,
        events: mpsc::UnboundedSender<WorkerEvent>,
        pause_at: Option<WorkerPoint>,
        force_pressure: bool,
    ) -> (
        Arc<crate::background_jobs::faults::worker::Scope>,
        Option<oneshot::Sender<()>>,
    ) {
        let (resume, pause) = match pause_at {
            Some(point) => {
                let (resume, receiver) = oneshot::channel();
                (Some(resume), Some((point, receiver)))
            }
            None => (None, None),
        };
        (
            crate::background_jobs::faults::worker::scope(worker, events, pause, force_pressure),
            resume,
        )
    }

    async fn next_worker_event(
        events: &mut mpsc::UnboundedReceiver<WorkerEvent>,
        worker: &str,
        point: WorkerPoint,
    ) -> WorkerEvent {
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let event = events.recv().await.expect("worker event channel");
                if event.worker == worker && event.point == point {
                    return event;
                }
            }
        })
        .await
        .expect("worker reached scheduled point")
    }

    async fn enqueue_full(store: &dyn Store, library_id: i64, request_id: &str, refresh: bool) {
        assert!(matches!(
            store
                .enqueue_library_work(NewLibraryWork {
                    request_id: request_id.into(),
                    library_id,
                    input: LibraryWorkInput::Full {
                        refresh,
                        trigger: LibraryTrigger::Manual,
                    },
                    now_ms: clock_ms(),
                })
                .await
                .expect("enqueue full library request"),
            EnqueueOutcome::Accepted { .. }
        ));
    }

    fn transcode_for(store: Arc<dyn Store>, root: &std::path::Path) -> Arc<TranscodeManager> {
        Arc::new(TranscodeManager::new(
            store,
            root.join("scratch"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        ))
    }

    fn spawn_worker(
        worker: &str,
        jobs: Arc<JobManager>,
        transcode: Arc<TranscodeManager>,
        events: mpsc::UnboundedSender<WorkerEvent>,
        pause_at: Option<WorkerPoint>,
        force_pressure: bool,
        dropped_replies: u8,
    ) -> SpawnedWorker {
        let (scope, resume) = worker_scope(worker, events, pause_at, force_pressure);
        let worker_scope = Arc::clone(&scope);
        let task = tokio::spawn(async move {
            crate::background_jobs::faults::dropping(dropped_replies, async move {
                crate::background_jobs::faults::worker::run(worker_scope, async move {
                    jobs.work_library_queue(&transcode, None).await
                })
                .await
            })
            .await
        });
        (task, scope, resume)
    }

    async fn request_record(store: &dyn Store, request_id: &str) -> LibraryWorkRecord {
        store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some(request_id.into()),
                limit: 1,
                ..Default::default()
            })
            .await
            .expect("read library request")
            .into_iter()
            .next()
            .expect("library request exists")
    }

    fn assert_no_worker_event(
        events: &mut mpsc::UnboundedReceiver<WorkerEvent>,
        point: WorkerPoint,
    ) {
        while let Ok(event) = events.try_recv() {
            assert_ne!(event.point, point, "{point:?} occurred before work joined");
        }
    }

    #[tokio::test]
    async fn repeated_scheduled_ticks_do_not_report_new_scan_admissions() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("root");
        let library = store
            .create_library(&NewLibrary {
                name: "Waiting DVR".into(),
                kind: LibraryKind::Recordings,
                paths: vec![root.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        let jobs = Arc::new(JobManager::new(
            Arc::clone(&store),
            root.path().join("artwork"),
        ));
        assert!(
            jobs.trigger_scan_as(library.id, ScanTrigger::Scheduled)
                .await
        );
        assert!(
            !jobs
                .trigger_scan_as(library.id, ScanTrigger::Scheduled)
                .await
        );
        let records = store
            .library_work_requests(LibraryWorkQuery {
                pending_only: true,
                limit: 256,
                ..Default::default()
            })
            .await
            .expect("pending");
        assert_eq!(records.len(), 1);
        assert!(
            jobs.trigger_scan(library.id).await,
            "manual scan remains an explicit new request"
        );
    }

    #[tokio::test]
    async fn busy_library_yields_without_a_failure_or_physical_reservation() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("root");
        let library = store
            .create_library(&NewLibrary {
                name: "Contended".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        let jobs = Arc::new(JobManager::new(
            Arc::clone(&store),
            root.path().join("artwork"),
        ));
        let transcode = TranscodeManager::new(
            Arc::clone(&store),
            root.path().join("scratch"),
            EncoderCaps::default(),
            Pipeline::Cpu,
        );
        let clock = clock_ms();
        assert!(matches!(
            store
                .acquire_lease(
                    &format!("scan:library:{}", library.id),
                    "other-node",
                    clock,
                    clock + 60_000
                )
                .await
                .expect("other coordinator"),
            LeaseClaim::Acquired(_)
        ));
        assert!(jobs.trigger_scan(library.id).await);
        assert!(jobs.work_library_queue(&transcode, None).await);
        let record = store
            .library_work_requests(LibraryWorkQuery {
                limit: 1,
                ..Default::default()
            })
            .await
            .expect("request")
            .remove(0);
        let job = store
            .background_job(&record.job_id)
            .await
            .expect("job")
            .expect("retained");
        assert_eq!(
            job.state,
            plurx_core::store::background_jobs::JobState::Queued
        );
        assert_eq!(job.failed_attempts, 0);
        assert!(job.token.is_none());
        assert_eq!(record.state, "pending");
        assert!(
            transcode.admit_fragment().await.is_some(),
            "physical capacity returned after contention"
        );
    }

    #[tokio::test]
    async fn library_worker_two_jobs_contend_at_the_durable_queue_and_both_complete() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("root");
        let library = store
            .create_library(&NewLibrary {
                name: "Two workers".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        enqueue_full(store.as_ref(), library.id, "request-scan", false).await;
        enqueue_full(store.as_ref(), library.id, "request-refresh", true).await;
        let second_request = request_record(store.as_ref(), "request-refresh").await;
        let initially_queued = store
            .background_job(&second_request.job_id)
            .await
            .expect("read initially queued job")
            .expect("second queue job retained");
        let jobs =
            JobManager::test_new_for_node(Arc::clone(&store), root.path().join("artwork-w1"), "w1");
        let transcode = transcode_for(Arc::clone(&store), &root.path().join("transcode-w1"));
        let (tx, mut events) = mpsc::unbounded_channel();

        let (first, _, resume_first) = spawn_worker(
            "w1",
            Arc::clone(&jobs),
            Arc::clone(&transcode),
            tx.clone(),
            Some(WorkerPoint::BeforeScan),
            false,
            0,
        );
        next_worker_event(&mut events, "w1", WorkerPoint::BeforeScan).await;
        let second_jobs =
            JobManager::test_new_for_node(Arc::clone(&store), root.path().join("artwork-w2"), "w2");
        let second_transcode = transcode_for(Arc::clone(&store), &root.path().join("transcode-w2"));
        let (second, _, _) = spawn_worker(
            "w2",
            Arc::clone(&second_jobs),
            Arc::clone(&second_transcode),
            tx.clone(),
            None,
            false,
            0,
        );
        assert!(!second.await.expect("queue-contending worker joined").0);
        while let Ok(event) = events.try_recv() {
            assert_ne!(event.worker, "w2", "queue-refused worker emitted an event");
        }
        assert_eq!(
            request_record(store.as_ref(), "request-refresh")
                .await
                .state,
            "pending"
        );
        // The stable job ID is payload-derived; select it through the request
        // record so the assertion remains coupled to the real enqueue result.
        let queued = store
            .background_job(&second_request.job_id)
            .await
            .expect("read second queue job")
            .expect("retained second queue job");
        assert_eq!(
            queued.state,
            plurx_core::store::background_jobs::JobState::Queued
        );
        assert_eq!(queued.failed_attempts, 0);
        assert!(queued.token.is_none());
        assert_eq!(queued.not_before_ms, initially_queued.not_before_ms);
        resume_first
            .expect("first worker barrier")
            .send(())
            .expect("resume scan");
        assert!(first.await.expect("first worker joined").0);
        assert_eq!(
            request_record(store.as_ref(), "request-scan").await.state,
            "succeeded"
        );

        // Queue contention never claimed or yielded the second job, so its
        // legitimate retry is immediately eligible after W1 releases capacity.
        let (retry, _, _) = spawn_worker(
            "w2-retry",
            second_jobs,
            second_transcode,
            tx,
            None,
            false,
            0,
        );
        assert!(retry.await.expect("retry worker joined").0);
        assert_eq!(
            request_record(store.as_ref(), "request-refresh")
                .await
                .state,
            "succeeded"
        );
    }

    async fn cancellation_schedule(point: WorkerPoint, force_pressure: bool) {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("root");
        let library = store
            .create_library(&NewLibrary {
                name: "Cancellation schedule".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        enqueue_full(store.as_ref(), library.id, "cancel-request", false).await;
        let jobs = JobManager::test_new(Arc::clone(&store), root.path().join("artwork"));
        let transcode = transcode_for(Arc::clone(&store), root.path());
        let (tx, mut events) = mpsc::unbounded_channel();
        let (task, scope, resume) = spawn_worker(
            "w1",
            jobs,
            transcode,
            tx,
            Some(WorkerPoint::BeforeScan),
            false,
            0,
        );
        let processing = next_worker_event(&mut events, "w1", WorkerPoint::Processing).await;
        next_worker_event(&mut events, "w1", WorkerPoint::BeforeScan).await;
        if force_pressure {
            crate::background_jobs::faults::worker::set_force_pressure(&scope, true);
            next_worker_event(&mut events, "w1", WorkerPoint::Pressure).await;
        } else {
            let signals = processing.signals.expect("worker loss tokens");
            match point {
                WorkerPoint::QueueLoss => signals.queue_loss.cancel(),
                WorkerPoint::DomainLoss => signals.domain_loss.cancel(),
                _ => unreachable!("unsupported cancellation point"),
            }
            next_worker_event(&mut events, "w1", point).await;
        }

        // The controlled scan is still held at its worker-side checkpoint.
        // The production loss arm has run, but release cannot start until the
        // work future is allowed to return and the worker joins it.
        assert_no_worker_event(&mut events, WorkerPoint::ReleaseStarted);
        resume
            .expect("scan barrier")
            .send(())
            .expect("resume worker");
        next_worker_event(&mut events, "w1", WorkerPoint::WorkJoined).await;
        next_worker_event(&mut events, "w1", WorkerPoint::ReleaseStarted).await;
        next_worker_event(&mut events, "w1", WorkerPoint::ReleaseFinished).await;
        next_worker_event(&mut events, "w1", WorkerPoint::Finished).await;
        next_worker_event(&mut events, "w1", WorkerPoint::AdmissionDropped).await;
        assert!(task.await.expect("cancelled worker joined").0);
        assert_eq!(
            request_record(store.as_ref(), "cancel-request").await.state,
            "pending"
        );
        let job = store
            .background_job(
                &request_record(store.as_ref(), "cancel-request")
                    .await
                    .job_id,
            )
            .await
            .expect("read durable job")
            .expect("job retained");
        assert_eq!(job.state, JobState::Queued);
    }

    #[tokio::test]
    async fn library_worker_domain_loss_joins_before_release_and_admission_drop() {
        cancellation_schedule(WorkerPoint::DomainLoss, false).await;
    }

    #[tokio::test]
    async fn library_worker_queue_loss_joins_before_release_and_retirement() {
        cancellation_schedule(WorkerPoint::QueueLoss, false).await;
    }

    #[tokio::test]
    async fn library_worker_admission_pressure_joins_before_release_and_drop() {
        cancellation_schedule(WorkerPoint::Pressure, true).await;
    }

    #[tokio::test]
    async fn library_worker_reconciles_lost_completion_acknowledgement() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("root");
        let library = store
            .create_library(&NewLibrary {
                name: "Completion acknowledgement".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        enqueue_full(store.as_ref(), library.id, "completion-ack", false).await;
        let jobs = JobManager::test_new(Arc::clone(&store), root.path().join("artwork"));
        let transcode = transcode_for(Arc::clone(&store), root.path());
        let (tx, mut events) = mpsc::unbounded_channel();
        let (task, _, _) = spawn_worker(
            "w1",
            jobs,
            transcode,
            tx,
            None,
            false,
            crate::background_jobs::faults::COMPLETE,
        );
        next_worker_event(&mut events, "w1", WorkerPoint::CompletionReconciled).await;
        assert!(task.await.expect("worker reconciled completion").0);
        let record = request_record(store.as_ref(), "completion-ack").await;
        assert_eq!(record.state, "succeeded");
        assert!(
            record.result.is_some(),
            "committed completion survives lost acknowledgement"
        );
    }
}
