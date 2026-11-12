//! Durable library admission and execution; the existing scanner owns identity.
use super::*;
use plurx_core::store::background_jobs::{EnqueueOutcome, JobPayload, JobSettlement, JobState};
use plurx_core::store::background_jobs_library::{
    LibraryTrigger, LibraryWorkInput, LibraryWorkQuery, LibraryWorkRecord, LibraryWorkResult,
    NewLibraryWork, MAX_LIBRARY_RESULT_BYTES,
};
use plurx_core::store::scan_request_v2::{ExpectedFile, PlacedReport, ScanRequestV2Seed};

/// How a contract v2 request was admitted.
#[derive(Debug)]
pub enum ScanAdmission {
    /// A new request, under this id.
    Admitted(String),
    /// The caller's `request_key` already names this request.
    Replay(String),
}
#[path = "library_work/env.rs"]
mod env;
#[path = "library_work/probe_input.rs"]
mod probe_input;

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

    /// Contract v2 admission (§5.1): the request and its sidecar in one
    /// transaction. A `request_key` another request of the same API key
    /// already holds — a replay, or a concurrent identical POST that won —
    /// answers with that request instead.
    pub async fn request_scan_v2(
        self: &Arc<Self>,
        req: ScanRequest,
        seed: ScanRequestV2Seed,
    ) -> Result<ScanAdmission, TargetError> {
        let (key_hash, request_key) = (seed.key_hash.clone(), seed.request_key.clone());
        let input = NewLibraryWork {
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
        };
        match self.store.enqueue_library_work_v2(input, seed).await {
            Ok(outcome) => {
                let running_job = match &outcome {
                    EnqueueOutcome::Accepted { job_id, .. } => Some(job_id.clone()),
                    _ => None,
                };
                self.admission_outcome(outcome)?;
                if let Some(job_id) = running_job {
                    self.note_queue_position(&job_id, &req.id).await;
                }
                Ok(ScanAdmission::Admitted(req.id))
            }
            Err(error) => {
                if !request_key.is_empty() {
                    if let Some(existing) = self
                        .store
                        .scan_request_by_key(&key_hash, &request_key)
                        .await?
                    {
                        return Ok(ScanAdmission::Replay(existing));
                    }
                }
                Err(TargetError::Store(error))
            }
        }
    }

    /// A request that joins a targeted job already running waits for that
    /// pass to reach it: record `queue_position:<n>`, the pending requests
    /// ahead of it (contract v2 §5.1). The pass clears it when it starts
    /// the next turn. Advisory, like every `waiting_on`.
    async fn note_queue_position(&self, job_id: &str, request_id: &str) {
        let running = matches!(
            self.store.background_job(job_id).await,
            Ok(Some(job)) if job.state == JobState::Running
        );
        if !running {
            return;
        }
        let pending = match self
            .store
            .library_work_requests(LibraryWorkQuery {
                job_id: Some(job_id.to_owned()),
                pending_only: true,
                limit: MAX_PENDING_PER_LIBRARY,
                ..Default::default()
            })
            .await
        {
            Ok(pending) => pending,
            Err(error) => {
                tracing::warn!(%error, job_id, "reading the targeted queue position");
                return;
            }
        };
        let ahead = pending
            .iter()
            .take_while(|record| record.request_id != request_id)
            .count();
        if ahead > 0 {
            if let Err(error) = self
                .store
                .set_scan_request_waiting_on(
                    request_id,
                    &format!("queue_position:{ahead}"),
                    clock_ms(),
                )
                .await
            {
                tracing::warn!(%error, request_id, "recording the targeted queue position");
            }
        }
    }

    /// Wait up to `wait` for a request to finish, on its local completion
    /// signal and a bounded re-read of the durable record — the request may
    /// be finished by another node, whose completion this one never hears.
    pub async fn await_scan_request(
        &self,
        request_id: &str,
        wait: Duration,
    ) -> Option<ScanRequestRecord> {
        let deadline = tokio::time::Instant::now() + wait;
        let waiter = self.scan_waiter(request_id);
        let record = loop {
            let record = self.scan_request(request_id).await;
            let finished = record
                .as_ref()
                .is_some_and(|record| matches!(record.status.as_str(), "done" | "failed"));
            let now = tokio::time::Instant::now();
            if finished || now >= deadline {
                break record;
            }
            tokio::select! {
                () = waiter.notified() => {}
                () = tokio::time::sleep((deadline - now).min(Duration::from_millis(500))) => {}
            }
        };
        // The signal is per request id, not per waiter: a second handler
        // waiting on the same request loses it here and falls back to the
        // bounded re-read above, which is still correct (review #1406, 10).
        self.forget_scan_waiter(request_id);
        record
    }

    async fn admit_library(&self, input: NewLibraryWork) -> Result<bool, TargetError> {
        let outcome = self.store.enqueue_library_work(input).await?;
        self.admission_outcome(outcome)
    }

    fn admission_outcome(&self, outcome: EnqueueOutcome) -> Result<bool, TargetError> {
        match outcome {
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

    /// One request, with its contract v2 sidecar (`waiting_on`, `files`)
    /// when it has one.
    pub async fn scan_request(&self, id: &str) -> Option<ScanRequestRecord> {
        let mut record = match self
            .store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some(id.into()),
                limit: 1,
                ..Default::default()
            })
            .await
        {
            Ok(records) => records.into_iter().next().map(request_record)?,
            Err(error) => {
                tracing::warn!(%error, "reading durable scan request");
                return None;
            }
        };
        match self.store.scan_request_v2(id).await {
            Ok(Some(row)) => {
                record.waiting_on = Some(row.waiting_on);
                record.files = Some(row.files);
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(%error, "reading scan request sidecar"),
        }
        Some(record)
    }

    /// The API key that created a contract v2 request, if it is one.
    pub async fn scan_request_creator(&self, id: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .store
            .scan_request_v2(id)
            .await?
            .map(|row| row.key_hash))
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

    pub(super) async fn drive_library_queue(
        self: &Arc<Self>,
        transcode: &TranscodeManager,
    ) -> bool {
        let refresh_successors = async {
            loop {
                tokio::select! {
                    () = self.library_wake.notified() => {},
                    () = tokio::time::sleep(Duration::from_secs(3)) => {},
                }
                if let Err(error) = crate::background_jobs::advertise_library_successors(
                    self.store.as_ref(),
                    self.job_authority.as_ref(),
                    transcode,
                    self.coordinator.node_id(),
                    &self.library_readiness,
                )
                .await
                {
                    tracing::warn!(%error,"refreshing ready library scans");
                }
            }
        };
        tokio::select! {
            biased;
            done = self.work_library_queue(transcode, None) => done,
            () = refresh_successors => unreachable!("discovery runs until the worker returns"),
        }
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
        let mut environment =
            env::TokioEnvironment::new(self, transcode, job, library_id, active, admission);
        plurx_worker_control::library_driver::run_driver(&mut environment)
            .await
            .expect("library environment replies must match the shared driver request");
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
            // Same-path requests share a pass, but a probe-only one never
            // shares with one that did not ask for it: it would walk what the
            // other forbade, or forbid what it wanted.
            let mut groups: BTreeMap<(PathBuf, bool), Vec<(String, ScanRequest)>> = BTreeMap::new();
            let mut sidecars_by_request: HashMap<
                String,
                plurx_core::store::scan_request_v2::ScanRequestV2,
            > = HashMap::new();
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
                        // Contract v2 sidecars: the files each request expects
                        // and removes. A request admitted by an older node has
                        // none.
                        let sidecar = self.store.scan_request_v2(&request.request_id).await?;
                        let probe_only = sidecar.as_ref().is_some_and(|row| row.probe_only);
                        if let Some(row) = sidecar {
                            sidecars_by_request.insert(request.request_id.clone(), row);
                        }
                        groups.entry((normalized, probe_only)).or_default().push((
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
            if full.is_empty() && !groups.is_empty() {
                // A targeted pass is under way: whatever its requests were
                // waiting on (the fence, a place in the queue) is over.
                crate::background_jobs::note_targeted_wait(self.store.as_ref(), job_id, "").await;
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
            for ((_, probe_only), requests) in groups {
                check_active()?;
                let group: Vec<_> = requests
                    .iter()
                    .map(|(_, request)| request.clone())
                    .collect();
                let sidecars: Vec<_> = requests
                    .iter()
                    .filter_map(|(id, _)| sidecars_by_request.remove(id))
                    .collect();
                let mut expected: Vec<ExpectedFile> = Vec::new();
                for file in sidecars.iter().flat_map(|row| &row.expected_files) {
                    if !expected.iter().any(|known| known.path == file.path) {
                        expected.push(file.clone());
                    }
                }
                let mut removed: Vec<String> = Vec::new();
                for path in sidecars.iter().flat_map(|row| &row.removed_paths) {
                    if !removed.contains(path) {
                        removed.push(path.clone());
                    }
                }
                let mut offloaded: Vec<String> = Vec::new();
                for path in sidecars.iter().flat_map(|row| &row.offloaded_paths) {
                    if !offloaded.contains(path) {
                        offloaded.push(path.clone());
                    }
                }
                let targeted = Targeted {
                    expected: &expected,
                    removed: &removed,
                    offloaded: &offloaded,
                    probe_only,
                };
                let result = match self.run_targeted(&group, &targeted, publisher).await {
                    Ok((scan, files)) => {
                        // Written before the completion, so a caller that
                        // reads `done` also reads its per-file report.
                        for row in &sidecars {
                            let own: Vec<_> = files
                                .iter()
                                .filter(|file| {
                                    row.expected_files
                                        .iter()
                                        .any(|expected| expected.path == file.path())
                                        || row.removed_paths.iter().any(|path| path == file.path())
                                        || row
                                            .offloaded_paths
                                            .iter()
                                            .any(|path| path == file.path())
                                })
                                .cloned()
                                .collect();
                            self.store
                                .set_scan_request_files(&row.request_id, &own, clock_ms())
                                .await?;
                        }
                        LibraryWorkResult::Completed { scan }
                    }
                    Err(error @ TargetError::OutsideRoots { .. }) => LibraryWorkResult::Failed {
                        error: error.to_string(),
                    },
                    // The share dropped between admission and the scan. The
                    // error names it `path_unreadable:` so the caller retries
                    // rather than discarding a valid notification.
                    Err(error @ TargetError::Unreadable { .. }) => LibraryWorkResult::Failed {
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
            self.notify_scan_waiter(&id);
            return Ok(());
        }
        // A lost completion acknowledgement must not rerun a published result.
        let current = self
            .store
            .library_work_requests(LibraryWorkQuery {
                request_id: Some(id.clone()),
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
            self.notify_scan_waiter(&id);
            return Ok(());
        }
        Err(reply
            .err()
            .unwrap_or_else(|| StoreError::Task("library completion lost its owner".into())))
    }
}

impl JobManager {
    /// The completion signal a waiting `POST /scan` holds for `request_id`.
    pub(crate) fn scan_waiter(&self, request_id: &str) -> Arc<tokio::sync::Notify> {
        let mut waiters = self
            .scan_waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(waiters.entry(request_id.to_owned()).or_default())
    }

    /// The waiting handler is done with its signal.
    pub(crate) fn forget_scan_waiter(&self, request_id: &str) {
        self.scan_waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(request_id);
    }

    fn notify_scan_waiter(&self, request_id: &str) {
        let waiter = self
            .scan_waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(request_id)
            .cloned();
        if let Some(waiter) = waiter {
            waiter.notify_waiters();
            waiter.notify_one();
        }
    }
}

/// What one targeted pass is asked to look at beyond its path.
pub(super) struct Targeted<'a> {
    pub expected: &'a [ExpectedFile],
    /// Removed paths (contract amendment `removed_paths`).
    pub removed: &'a [String],
    /// Offloaded paths (contract amendment `offload`): removed paths whose
    /// items stay.
    pub offloaded: &'a [String],
    /// Record exactly the expected files and walk nothing else.
    pub probe_only: bool,
}

/// One removed path, as the disk answered for it.
#[derive(Debug)]
pub(super) enum RemovedEntry {
    /// Not under the request path or the library; never looked at.
    Outside(String),
    Present(String, plurx_core::store::scan_request_v2::PresentReason),
    Absent(plurx_core::scan::removed::AbsentPath),
    /// An offloaded path under a library root that lists nothing on its
    /// parent's filesystem: nothing on this node tells it from an unmounted
    /// share, so nothing was removed (`retained`, `root_empty`).
    EmptyRoot(String),
}

/// Probe each removed path from its library root down, never following a
/// symlink. Absence is the expected answer; any other error on the chain,
/// or a root that cannot be listed or lists nothing (an empty mount point),
/// is `path_unreadable` for the whole request, so nothing is removed while
/// a share is away. Revalidates scope here rather than trusting admission.
pub(super) async fn probe_removed_paths(
    library: &plurx_core::domain::Library,
    target: &std::path::Path,
    removed: &[String],
    offload: bool,
) -> Result<Vec<RemovedEntry>, TargetError> {
    use plurx_core::scan::{containment, removed as probe};
    if removed.is_empty() {
        return Ok(Vec::new());
    }
    let library = library.clone();
    let target = containment::lexical(target);
    let removed = removed.to_vec();
    tokio::task::spawn_blocking(move || {
        let mut listable: Vec<PathBuf> = Vec::new();
        let mut entries = Vec::with_capacity(removed.len());
        for raw in removed {
            let path = std::path::Path::new(&raw);
            let root = (path.is_absolute() && containment::lexical(path).starts_with(&target))
                .then(|| containment::lexical_root(std::slice::from_ref(&library), path))
                .flatten()
                .map(|(_, root)| root);
            let Some(root) = root else {
                entries.push(RemovedEntry::Outside(raw));
                continue;
            };
            let unreadable = |failure: probe::RemovedUnreadable| TargetError::Unreadable {
                path: raw.clone(),
                root: failure.root.display().to_string(),
                error: failure.error,
            };
            if !listable.contains(&root) {
                match probe::root_listable(&root) {
                    Ok(()) => listable.push(root.clone()),
                    // An offload that empties a root is lasting, not a share
                    // that will come back.
                    Err(failure) if offload && failure.empty_root => {
                        entries.push(RemovedEntry::EmptyRoot(raw));
                        continue;
                    }
                    Err(failure) => return Err(unreadable(failure)),
                }
            }
            entries.push(
                match probe::probe_removed(&root, path).map_err(unreadable)? {
                    probe::RemovedProbe::Present { reason } => RemovedEntry::Present(raw, reason),
                    probe::RemovedProbe::Absent { spellings } => {
                        RemovedEntry::Absent(probe::AbsentPath {
                            path: raw,
                            spellings,
                            root: root.clone(),
                            offload,
                        })
                    }
                },
            );
        }
        Ok(entries)
    })
    .await
    .map_err(|error| {
        TargetError::Store(StoreError::Task(format!(
            "removed path probe task failed: {error}"
        )))
    })?
}

/// Forget what each absent removed path held and report every removed path
/// in the order the caller named them: `gone`, `unknown`, `present`, or
/// `retained` when plurx kept a record it could not safely remove.
pub(super) async fn remove_paths(
    publisher: &PublicationStore<'_>,
    library: &plurx_core::domain::Library,
    entries: Vec<RemovedEntry>,
    expected_not_placed: bool,
    report: &mut plurx_core::scan::ScanReport,
) -> Result<Vec<PlacedReport>, TargetError> {
    let absent: Vec<_> = entries
        .iter()
        .filter_map(|entry| match entry {
            RemovedEntry::Absent(absent) => Some(absent.clone()),
            _ => None,
        })
        .collect();
    let removal =
        plurx_core::scan::removed::remove_absent(publisher, library, &absent, expected_not_placed)
            .await?;
    report.removed_files += usize::try_from(removal.deleted_files).unwrap_or(usize::MAX);
    report.pruned_items += usize::try_from(removal.pruned_items).unwrap_or(usize::MAX);
    // `remove_absent` answers one report per absent path, in their order.
    let mut removal = removal.reports.into_iter();
    let mut reports = Vec::with_capacity(entries.len());
    for entry in entries {
        reports.push(match entry {
            RemovedEntry::Outside(path) => PlacedReport::Skipped {
                path,
                reason: "outside the requested path or library".into(),
            },
            RemovedEntry::Present(path, reason) => PlacedReport::Present { path, reason },
            RemovedEntry::EmptyRoot(path) => PlacedReport::Retained {
                path,
                item_id: 0,
                code: plurx_core::store::scan_request_v2::RetainedCode::RootEmpty,
                retryable: false,
                reason: "the offload emptied this library root, which this node cannot tell from \
                         an unmounted share, so nothing was removed; a full scan or the next \
                         placement settles it"
                    .to_owned(),
            },
            RemovedEntry::Absent(absent) => removal
                .next()
                .unwrap_or(PlacedReport::Unknown { path: absent.path }),
        });
    }
    Ok(reports)
}

/// An expected file may be outside the request scope even when it exists.
/// Keep that verdict separate from `Missing`, without probing outside paths.
#[derive(Debug)]
pub(super) enum ExpectedPath {
    Outside,
    Missing,
    Canonical(PathBuf),
}

/// Resolve expected files only after lexical scope and no-follow checks.
/// Revalidate stored intent in the worker, rather than trusting HTTP admission.
pub(super) async fn canonical_expected(
    expected: &[ExpectedFile],
    library: &plurx_core::domain::Library,
    target: &std::path::Path,
) -> BTreeMap<String, ExpectedPath> {
    use plurx_core::scan::containment;
    let target = containment::lexical(target);
    let mut canonical = BTreeMap::new();
    for file in expected {
        let path = std::path::Path::new(&file.path);
        let normalized = containment::lexical(path);
        let verdict = if !path.is_absolute() || !normalized.starts_with(&target) {
            ExpectedPath::Outside
        } else if let Some((_, root)) =
            containment::lexical_root(std::slice::from_ref(library), path)
        {
            let root_for_check = root.clone();
            let path_for_check = normalized.clone();
            match tokio::task::spawn_blocking(move || {
                containment::symlink_below_root(&root_for_check, &path_for_check)
            })
            .await
            {
                Ok(Ok(true)) => ExpectedPath::Outside,
                Ok(Ok(false)) => {
                    match (
                        tokio::fs::canonicalize(&root).await,
                        tokio::fs::canonicalize(&normalized).await,
                    ) {
                        (Ok(root), Ok(real)) if real.starts_with(&root) => {
                            ExpectedPath::Canonical(real)
                        }
                        (Ok(_), Ok(_)) => ExpectedPath::Outside,
                        _ => ExpectedPath::Missing,
                    }
                }
                _ => ExpectedPath::Missing,
            }
        } else {
            ExpectedPath::Outside
        };
        canonical.insert(file.path.clone(), verdict);
    }
    canonical
}

/// The per-file report for every expected file (contract v2 §4.2, §5.1):
/// `placed` with the file as plurx just read it — its size and its mtime in
/// nanoseconds from a fresh `stat`, never the whole-second catalogue value —
/// `skipped` with the reason, or `missing`.
pub(super) async fn placed_reports(
    expected: &[ExpectedFile],
    canonical: &BTreeMap<String, ExpectedPath>,
    scan: &TargetedScan,
    detail: &plurx_core::scan::TargetedDetail,
) -> Vec<PlacedReport> {
    let mut reports = Vec::with_capacity(expected.len());
    for file in expected {
        let path = file.path.clone();
        let real = match canonical.get(&file.path) {
            Some(ExpectedPath::Canonical(real)) => real,
            Some(ExpectedPath::Outside) => {
                reports.push(PlacedReport::Skipped {
                    path,
                    reason: "outside the requested path or library, or contains a symlink".into(),
                });
                continue;
            }
            _ => {
                reports.push(PlacedReport::Missing { path });
                continue;
            }
        };
        let real_str = real.to_string_lossy().into_owned();
        if let Some(placed) = scan.items.iter().find(|placed| placed.path == real_str) {
            match tokio::fs::metadata(real).await {
                Ok(meta) => reports.push(PlacedReport::Placed {
                    path,
                    item_id: placed.item_id,
                    file_id: placed.file_id,
                    bytes: i64::try_from(meta.len()).unwrap_or(i64::MAX),
                    mtime_ns: meta
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                        .and_then(|elapsed| i64::try_from(elapsed.as_nanos()).ok())
                        .unwrap_or(0),
                }),
                Err(_) => reports.push(PlacedReport::Missing { path }),
            }
            continue;
        }
        let reason = if let Some(reason) = detail.skipped.get(&real_str) {
            reason.clone()
        } else if !real.starts_with(&detail.canonical_target) {
            format!(
                "not under the requested path `{}`",
                detail.canonical_target.display()
            )
        } else if !detail.candidates.contains(&real_str) {
            "not a media file this library indexes".to_owned()
        } else {
            "could not be read; see the scan report".to_owned()
        };
        reports.push(PlacedReport::Skipped { path, reason });
    }
    reports
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

/// The `error` of a request whose job was cancelled before it finished.
pub(crate) const SCAN_REQUEST_CANCELLED: &str =
    "the scan request was cancelled before it finished; send a new attempt";

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
    // Pending and owned by a live claim is running; pending otherwise is
    // queued. The waiter's own state has no "running".
    let running = record.state == "pending"
        && matches!(record.job_state, Some(JobState::Running))
        && record
            .job_lease_expires_ms
            .is_some_and(|expires| expires > clock_ms());
    // Contract v2 has exactly four statuses (queued | running | done |
    // failed). A request whose job was cancelled is a typed failure,
    // `code: cancelled`, so a caller treats it like any other failed
    // request and sends a new attempt instead of polling a fifth state.
    let cancelled = record.state == "cancelled";
    let status = match record.state.as_str() {
        "succeeded" => "done",
        "pending" | "awaiting_hydration" if running => "running",
        "pending" | "awaiting_hydration" => "queued",
        _ => "failed",
    };
    let error = if cancelled {
        error.or_else(|| Some(SCAN_REQUEST_CANCELLED.to_owned()))
    } else {
        error
    };
    let code = if cancelled {
        Some("cancelled")
    } else {
        (status == "failed"
            && error
                .as_deref()
                .is_some_and(|error| error.starts_with("path_unreadable")))
        .then_some("path_unreadable")
    };
    ScanRequestRecord {
        request_id: record.request_id,
        at: record.created_at_ms / 1_000,
        library_id: record.library_id,
        path,
        correlation_id,
        source,
        status: status.into(),
        report,
        items,
        error,
        code,
        waiting_on: None,
        files: None,
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

    /// A bounded ready-source schedule: every selected non-pressuring tick
    /// advances one period, so restoring tick-before-work fails after three
    /// decisions. No timeout or assumptions about the runtime's poll cadence.
    #[tokio::test(start_paused = true)]
    async fn library_worker_ready_work_precedes_burst_pressure_ticks() {
        use plurx_worker_control::library_driver::{select_work, Selected};
        use std::future::{pending, poll_fn};
        use std::task::Poll;

        let mut tick = tokio::time::interval(Duration::from_millis(250));
        let work_polls = std::cell::Cell::new(0);
        let mut work = std::pin::pin!(poll_fn(|_| {
            work_polls.set(work_polls.get() + 1);
            Poll::Ready(())
        }));
        let mut completed_on = None;
        for decision in 0..3 {
            match select_work(pending::<()>(), pending::<()>(), work.as_mut(), tick.tick()).await {
                Selected::Work(()) => {
                    completed_on = Some(decision);
                    break;
                }
                Selected::Tick => tokio::time::advance(Duration::from_millis(250)).await,
                _ => panic!("loss futures are pending"),
            }
        }
        assert_eq!(
            completed_on,
            Some(0),
            "ready work must outrank every ready tick"
        );
        assert_eq!(work_polls.get(), 1);
    }

    #[tokio::test]
    async fn library_worker_selector_preserves_loss_precedence_and_pending_work_pressure() {
        use plurx_worker_control::library_driver::{select_work, Selected};
        use std::future::{pending, ready};
        assert_eq!(
            select_work(ready(()), ready(()), ready(()), ready(())).await,
            Selected::QueueLoss
        );
        assert_eq!(
            select_work(pending::<()>(), ready(()), ready(()), ready(())).await,
            Selected::DomainLoss
        );
        assert_eq!(
            select_work(pending::<()>(), pending::<()>(), pending::<()>(), ready(())).await,
            Selected::Tick
        );
    }

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
    async fn library_worker_releases_source_after_pressure_during_completion_acknowledgement() {
        let (store, _root, jobs, transcode) = queued_library("first-scan").await;
        let (tx, mut events) = mpsc::unbounded_channel();
        let (worker, _, resume) = spawn_worker(
            "library",
            Arc::clone(&jobs),
            Arc::clone(&transcode),
            tx,
            Some(WorkerPoint::CompletionCommitted),
            false,
            0,
        );
        next_worker_event(&mut events, "library", WorkerPoint::BeforeCompletion).await;
        // The Store can finish the write while its reply is still pending.
        // A source check can therefore see the settled waiter and move the
        // controller into JoinWork before the producer observes the commit.
        // JoinWork polls the same producer but never starts a second check.
        let mut observed = Vec::new();
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let point = events.recv().await.expect("library worker event").point;
                observed.push(point);
                let committed = observed.contains(&WorkerPoint::CompletionCommitted);
                let checked = observed.contains(&WorkerPoint::SourcePermissionCheck);
                if committed
                    && checked
                    && (point == WorkerPoint::SourcePermissionCheck
                        || observed.contains(&WorkerPoint::Pressure))
                {
                    break;
                }
            }
        })
        .await
        .unwrap_or_else(|_| panic!("completion and source check did not converge: {observed:?}"));
        let first = request_record(store.as_ref(), "first-scan").await;
        assert_eq!(first.state, "succeeded");
        resume
            .expect("completion barrier")
            .send(())
            .expect("deliver completion reply");
        tokio::time::timeout(Duration::from_secs(3), worker)
            .await
            .expect("a completed scan must release its source custody")
            .expect("library worker joins");
        enqueue_full(store.as_ref(), first.library_id, "next-scan", false).await;
        assert!(jobs.work_library_queue(&transcode, None).await);
        assert_eq!(
            request_record(store.as_ref(), "next-scan").await.state,
            "succeeded"
        );
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

    /// One queued full-library request on a fresh store, ready to be claimed.
    async fn queued_library(
        request_id: &str,
    ) -> (
        Arc<dyn Store>,
        tempfile::TempDir,
        Arc<JobManager>,
        Arc<TranscodeManager>,
    ) {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("root");
        let library = store
            .create_library(&NewLibrary {
                name: "Queued library".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        enqueue_full(store.as_ref(), library.id, request_id, false).await;
        let jobs = JobManager::test_new(Arc::clone(&store), root.path().join("artwork"));
        let transcode = transcode_for(Arc::clone(&store), root.path());
        (store, root, jobs, transcode)
    }

    /// What the environment still held at the instant its live work future was
    /// dropped.
    #[derive(Debug, Default)]
    struct HeldWhenWorkDropped {
        queue_released: bool,
        domain_released: bool,
        admission_released: bool,
    }

    /// Aborting a worker task mid-work drops its environment with the work
    /// future alive. The work must go first, then its publisher and lease, then
    /// physical admission, and the queue fence last, exactly as the pre-driver
    /// worker's stack locals released. The previous field order released the
    /// fence (cancelling its loss token) and admission (returning the transcode
    /// permit and the heavy gate) while the work future could still run.
    #[tokio::test]
    async fn library_worker_abort_releases_in_the_original_order() {
        let (_store, _root, jobs, transcode) = queued_library("abort-order").await;
        let (tx, mut events) = mpsc::unbounded_channel();
        let (task, scope, _resume) = spawn_worker(
            "w1",
            jobs,
            Arc::clone(&transcode),
            tx,
            Some(WorkerPoint::BeforeScan),
            false,
            0,
        );
        let processing = next_worker_event(&mut events, "w1", WorkerPoint::Processing).await;
        next_worker_event(&mut events, "w1", WorkerPoint::BeforeScan).await;
        let signals = processing.signals.expect("worker loss tokens");
        assert!(
            transcode.background_worker_in_use(),
            "the held scan holds physical admission"
        );
        assert!(!signals.queue_loss.is_cancelled() && !signals.domain_loss.is_cancelled());

        let observed = Arc::new(std::sync::Mutex::new(None));
        crate::background_jobs::faults::worker::on_pause_end(&scope, {
            let observed = Arc::clone(&observed);
            let signals = signals.clone();
            let transcode = Arc::clone(&transcode);
            move || {
                *observed.lock().expect("observation") = Some(HeldWhenWorkDropped {
                    queue_released: signals.queue_loss.is_cancelled(),
                    domain_released: signals.domain_loss.is_cancelled(),
                    admission_released: !transcode.background_worker_in_use(),
                });
            }
        });
        task.abort();
        let error = task.await.expect_err("aborted worker does not complete");
        assert!(error.is_cancelled(), "abort must not panic: {error:?}");

        let at_work_drop = observed
            .lock()
            .expect("observation")
            .take()
            .expect("the live work future was dropped by the abort");
        assert!(
            !at_work_drop.queue_released
                && !at_work_drop.domain_released
                && !at_work_drop.admission_released,
            "nothing is released before the work future: {at_work_drop:?}"
        );
        assert!(
            signals.queue_loss.is_cancelled() && signals.domain_loss.is_cancelled(),
            "the fence and lease are released once the environment is gone"
        );
        assert!(
            !transcode.background_worker_in_use(),
            "admission is released with the environment"
        );
    }

    #[tokio::test]
    async fn readable_manual_scan_requests_a_turn_while_maintenance_owns_the_worker() {
        use plurx_core::store::background_jobs::{ClaimJob, ClaimOutcome, JobKind, SettleJob};
        let (store, _root, jobs, transcode) = queued_library("busy-manual").await;
        let library_id = request_record(store.as_ref(), "busy-manual")
            .await
            .library_id;
        // First announce a genuinely idle worker; the subsequent occupied
        // observation must withdraw this cached positive advertisement.
        crate::background_jobs::advertise_library_successors(
            store.as_ref(),
            jobs.job_authority.as_ref(),
            &transcode,
            jobs.coordinator.node_id(),
            &jobs.library_readiness,
        )
        .await
        .expect("idle worker");
        let running = transcode
            .admit_fragment()
            .await
            .expect("running maintenance");
        assert!(!jobs.work_library_queue(&transcode, None).await);
        assert!(jobs.library_readiness.poll_interval() <= Duration::from_secs(1));
        assert!(
            transcode.fragment_worker_idle(&running),
            "maintenance retains physical custody"
        );
        let now = clock_ms();
        let EnqueueOutcome::Accepted { job_id, .. } = store
            .enqueue_library_work(NewLibraryWork {
                request_id: "remote-scheduled".into(),
                library_id,
                input: LibraryWorkInput::Full {
                    refresh: true,
                    trigger: LibraryTrigger::Scheduled,
                },
                now_ms: now,
            })
            .await
            .expect("remote maintenance")
        else {
            panic!("expected new maintenance")
        };
        let ClaimOutcome::Claimed { job } = store
            .claim_job(ClaimJob {
                job_id,
                expected_revision: 0,
                node_id: "remote-worker".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::MetadataRefresh,
                payload_version: 1,
                now_ms: now,
                dispatched_at_ms: now,
            })
            .await
            .expect("remote may use free disk")
        else {
            panic!("busy voter blocked remote work")
        };
        assert!(store
            .settle_job(SettleJob {
                token: job.token.expect("running"),
                now_ms: now,
                settlement: JobSettlement::Yield {
                    error_code: None,
                    checkpoint: None,
                    not_before_ms: now + 60000
                },
            })
            .await
            .expect("remote released disk"));
        drop(running);
        assert!(
            jobs.work_library_queue(&transcode, None).await,
            "same manual request runs after release"
        );
    }

    #[tokio::test]
    async fn busy_library_worker_renews_its_next_manual_scan_before_release() {
        let root = crate::test_tempdir().expect("root");
        let db = root.path().join("queue.sqlite");
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open(&db).expect("store"));
        let media = root.path().join("media");
        std::fs::create_dir(&media).expect("media directory");
        let library = store
            .create_library(&NewLibrary {
                name: "Movies".into(),
                kind: LibraryKind::Movies,
                paths: vec![media],
                anime: false,
            })
            .await
            .expect("library");
        enqueue_full(store.as_ref(), library.id, "first", false).await;
        let jobs = JobManager::test_new(Arc::clone(&store), root.path().join("artwork"));
        let transcode = transcode_for(Arc::clone(&store), root.path());
        let (tx, mut events) = mpsc::unbounded_channel();
        let (scope, resume) = worker_scope("only-reader", tx, Some(WorkerPoint::BeforeScan), false);
        let driver = tokio::spawn({
            let jobs = Arc::clone(&jobs);
            async move {
                crate::background_jobs::faults::worker::run(scope, async {
                    jobs.drive_library_queue(&transcode).await
                })
                .await
            }
        });
        next_worker_event(&mut events, "only-reader", WorkerPoint::BeforeScan).await;
        enqueue_full(store.as_ref(), library.id, "next", true).await;
        let next = request_record(store.as_ref(), "next").await.job_id;
        jobs.library_wake.notify_one();
        let read =
            rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                .expect("observation");
        tokio::time::timeout(Duration::from_secs(12), async {
            let mut first = None;
            loop {
                let until: Option<i64> = read
                    .query_row(
                        "SELECT max(expires_at_ms) FROM dispatch_turn_ready WHERE job_id=?1",
                        [&next],
                        |row| row.get(0),
                    )
                    .expect("turn expiry");
                if let Some(until) = until.filter(|until| *until > clock_ms()) {
                    if first.is_some_and(|old| until > old) {
                        break;
                    }
                    first = Some(until);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("second request advertised and renewed while the first is still running");
        resume.expect("resume").send(()).expect("release scan");
        assert!(driver.await.expect("driver joined"));
    }

    /// A panic that unwinds through a worker drops its environment while
    /// panicking. A `Drop` assertion there would be a double panic, which
    /// aborts the whole process, so this test crashing the test binary is the
    /// failure mode.
    #[tokio::test]
    async fn library_worker_environment_drops_quietly_while_unwinding() {
        let (_store, _root, jobs, transcode) = queued_library("unwind-drop").await;
        let (tx, mut events) = mpsc::unbounded_channel();
        let (scope, _resume) = worker_scope("w1", tx, Some(WorkerPoint::BeforeScan), false);
        let (unwind, unwinding) = oneshot::channel::<()>();
        let task = {
            let transcode = Arc::clone(&transcode);
            tokio::spawn(async move {
                crate::background_jobs::faults::worker::run(scope, async move {
                    let mut unwinding = unwinding;
                    let mut work = Box::pin(jobs.work_library_queue(&transcode, None));
                    tokio::select! {
                        biased;
                        _ = &mut unwinding => panic!("injected unwind with live library work"),
                        claimed = &mut work => claimed,
                    }
                })
                .await
            })
        };
        next_worker_event(&mut events, "w1", WorkerPoint::BeforeScan).await;
        assert!(transcode.background_worker_in_use());
        unwind.send(()).expect("start unwinding");
        let error = task.await.expect_err("the injected panic ends the task");
        assert!(error.is_panic(), "only the injected panic: {error:?}");
        let payload = error.into_panic();
        assert_eq!(
            payload.downcast_ref::<&str>().copied(),
            Some("injected unwind with live library work")
        );
        assert!(
            !transcode.background_worker_in_use(),
            "unwinding still released admission"
        );
    }

    /// With the clock paused the first tick of a fresh interval is ready at
    /// its first poll, so a ready work future and a ready tick are both
    /// available at once. Work must win;
    /// swapping the `work` and `pressure.tick()` arguments inside `select_next`
    /// (the only selection `TokioEnvironment::poll_work` makes) reports `Tick`.
    #[tokio::test(start_paused = true)]
    async fn library_worker_select_prefers_ready_work_to_a_ready_tick() {
        use plurx_worker_control::library_driver::Selected;
        use tokio_util::sync::CancellationToken;
        let (queue, domain) = (CancellationToken::new(), CancellationToken::new());
        let mut pressure = tokio::time::interval(Duration::from_millis(250));
        let mut work = Box::pin(std::future::ready(7_u8));
        assert_eq!(
            env::select_next(&queue, &domain, &mut work, &mut pressure).await,
            Selected::Work(7),
            "ready work must be taken before a ready pressure tick"
        );

        let mut pressure = tokio::time::interval(Duration::from_millis(250));
        let mut pending = Box::pin(std::future::pending::<u8>());
        assert_eq!(
            env::select_next(&queue, &domain, &mut pending, &mut pressure).await,
            Selected::Tick,
            "a pending scan still yields to the pressure tick"
        );

        domain.cancel();
        let mut pressure = tokio::time::interval(Duration::from_millis(250));
        let mut work = Box::pin(std::future::ready(7_u8));
        assert_eq!(
            env::select_next(&queue, &domain, &mut work, &mut pressure).await,
            Selected::DomainLoss,
            "loss outranks ready work"
        );
        queue.cancel();
        let mut work = Box::pin(std::future::ready(7_u8));
        assert_eq!(
            env::select_next(&queue, &domain, &mut work, &mut pressure).await,
            Selected::QueueLoss
        );
    }

    #[tokio::test]
    async fn expected_files_never_probe_outside_the_requested_scope() {
        let root = crate::test_tempdir().expect("library root");
        let outside = crate::test_tempdir().expect("outside root");
        let target = root.path().join("target");
        std::fs::create_dir(&target).expect("target");
        let valid = target.join("valid.mkv");
        std::fs::write(&valid, b"fixture").expect("valid file");
        let existing = outside.path().join("existing");
        std::fs::write(&existing, b"private").expect("outside file");
        let absent = outside.path().join("absent");
        let sibling = root.path().join("sibling");
        std::fs::write(&sibling, b"outside requested directory").expect("sibling");
        let library = Library {
            id: 1,
            name: "Scope".into(),
            kind: LibraryKind::Movies,
            paths: vec![root.path().to_path_buf()],
            anime: false,
            created_at: 0,
            scan_interval_mins: 0,
            refresh_interval_mins: 0,
            last_scan_at: None,
            last_refresh_at: None,
        };
        let expected: Vec<_> = [&existing, &absent, &sibling, &valid]
            .into_iter()
            .map(|path| ExpectedFile {
                path: path.display().to_string(),
                bytes: 0,
                mtime_ns: 0,
            })
            .collect();
        let resolved = canonical_expected(&expected, &library, &target).await;
        for file in &expected[..3] {
            assert!(matches!(
                resolved.get(&file.path),
                Some(ExpectedPath::Outside)
            ));
        }
        assert!(matches!(
            resolved.get(&expected[3].path),
            Some(ExpectedPath::Canonical(_))
        ));
        let reports = placed_reports(
            &expected,
            &resolved,
            &TargetedScan {
                report: Default::default(),
                items: vec![],
            },
            &plurx_core::scan::TargetedDetail::default(),
        )
        .await;
        let reasons: Vec<_> = reports[..3]
            .iter()
            .map(|report| match report {
                PlacedReport::Skipped { reason, .. } => reason,
                other => panic!("out-of-scope existence leaked: {other:?}"),
            })
            .collect();
        assert!(reasons.windows(2).all(|pair| pair[0] == pair[1]));
        #[cfg(unix)]
        {
            let link = target.join("link");
            std::os::unix::fs::symlink(outside.path(), &link).expect("outside symlink");
            let through_link = ["existing", "absent"].map(|name| ExpectedFile {
                path: link.join(name).display().to_string(),
                bytes: 0,
                mtime_ns: 0,
            });
            let resolved = canonical_expected(&through_link, &library, &target).await;
            assert!(through_link
                .iter()
                .all(|file| matches!(resolved.get(&file.path), Some(ExpectedPath::Outside))));
        }
    }

    async fn enqueue_targeted(
        store: &dyn Store,
        library_id: i64,
        request_id: &str,
        path: &std::path::Path,
        expected: &[&std::path::Path],
    ) {
        let expected_files = expected
            .iter()
            .map(|path| {
                let meta = std::fs::metadata(path).expect("expected fixture file");
                plurx_core::store::scan_request_v2::ExpectedFile {
                    path: path.display().to_string(),
                    bytes: meta.len() as i64,
                    mtime_ns: meta
                        .modified()
                        .expect("mtime")
                        .duration_since(std::time::UNIX_EPOCH)
                        .expect("after epoch")
                        .as_nanos() as i64,
                }
            })
            .collect();
        assert!(matches!(
            store
                .enqueue_library_work_v2(
                    NewLibraryWork {
                        request_id: request_id.into(),
                        library_id,
                        input: LibraryWorkInput::Targeted {
                            path: path.to_path_buf(),
                            ids: None,
                            book: None,
                            correlation_id: Some(format!("t-{request_id}")),
                            source: Some("test".into()),
                        },
                        now_ms: clock_ms(),
                    },
                    plurx_core::store::scan_request_v2::ScanRequestV2Seed {
                        key_id: 1,
                        key_hash: "a".repeat(64),
                        request_key: String::new(),
                        expected_files,
                        removed_paths: Vec::new(),
                        offloaded_paths: Vec::new(),
                        probe_only: false,
                    },
                )
                .await
                .expect("enqueue targeted library request"),
            EnqueueOutcome::Accepted { .. }
        ));
    }

    fn media_file(dir: &std::path::Path, title: &str) -> PathBuf {
        let folder = dir.join(title);
        std::fs::create_dir_all(&folder).expect("movie folder");
        let file = folder.join(format!("{title}.mkv"));
        std::fs::write(&file, b"not really matroska").expect("movie file");
        file
    }

    /// Review #1405 (2): a targeted job the claim refuses for a reason other
    /// than its own library's fence — here the provider cap, with two other
    /// libraries' running scans holding both `provider:tmdb` slots — does not
    /// keep the `waiting_on` an earlier fence refusal wrote. With its fence
    /// free it names no wait; with the fence held it names that.
    #[tokio::test]
    async fn a_targeted_claim_refused_by_the_provider_cap_refreshes_waiting_on() {
        use plurx_core::store::background_jobs::{ClaimJob, ClaimOutcome, JobKind};
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let volume = crate::test_tempdir().expect("volume");
        let mut libraries = Vec::new();
        for name in ["Alpha", "Beta", "Gamma"] {
            let root = volume.path().join(name);
            std::fs::create_dir_all(&root).expect("library root");
            media_file(&root, &format!("{name} Film (2001)"));
            libraries.push(
                store
                    .create_library(&NewLibrary {
                        name: name.into(),
                        kind: LibraryKind::Movies,
                        paths: vec![root],
                        anime: false,
                    })
                    .await
                    .expect("library"),
            );
        }
        // One storage domain each, so the two full scans do not share a reader.
        let mappings = libraries
            .iter()
            .flat_map(|library| {
                library.paths.iter().map(|root| {
                    plurx_core::store::background_jobs_resources::StorageDomainMapping {
                        library_id: library.id,
                        root_path: root.display().to_string(),
                        domain_id: format!("volume-{}", library.id),
                    }
                })
            })
            .collect();
        assert!(store
            .replace_storage_domains(mappings, clock_ms())
            .await
            .expect("storage domains"));
        for (library, request) in [(&libraries[1], "beta-full"), (&libraries[2], "gamma-full")] {
            enqueue_full(store.as_ref(), library.id, request, false).await;
            let job_id = request_record(store.as_ref(), request).await.job_id;
            let job = store
                .background_job(&job_id)
                .await
                .expect("read job")
                .expect("full scan job");
            let now_ms = clock_ms();
            let claim = store
                .claim_job(ClaimJob {
                    job_id: job.id,
                    expected_revision: job.revision,
                    node_id: "remote-worker".into(),
                    boot_id: uuid::Uuid::new_v4().to_string(),
                    claim_id: uuid::Uuid::new_v4().to_string(),
                    kind: JobKind::LibraryScan,
                    payload_version: 1,
                    now_ms,
                    dispatched_at_ms: now_ms,
                })
                .await
                .expect("remote full scan claim");
            assert!(matches!(claim, ClaimOutcome::Claimed { .. }), "{claim:?}");
        }
        let new = media_file(&libraries[0].paths[0], "Alpha Import (2025)");
        enqueue_targeted(
            store.as_ref(),
            libraries[0].id,
            "alpha-targeted",
            new.parent().expect("folder"),
            &[&new],
        )
        .await;
        let alpha_job = request_record(store.as_ref(), "alpha-targeted")
            .await
            .job_id;
        // What an earlier fence refusal wrote, before Alpha's fence was released.
        crate::background_jobs::note_targeted_wait(store.as_ref(), &alpha_job, "full_scan:Alpha")
            .await;
        let waiting_on = || async {
            store
                .scan_request_v2("alpha-targeted")
                .await
                .expect("sidecar")
                .expect("v2 request")
                .waiting_on
        };
        let jobs = JobManager::test_new_for_node(
            Arc::clone(&store),
            volume.path().join("artwork-w2"),
            "w2",
        );
        let transcode = transcode_for(Arc::clone(&store), &volume.path().join("transcode-w2"));
        assert!(
            !jobs.work_library_queue(&transcode, None).await,
            "the provider cap refuses the targeted claim"
        );
        assert_eq!(
            request_record(store.as_ref(), "alpha-targeted").await.state,
            "pending"
        );
        assert_eq!(
            waiting_on().await,
            "",
            "Alpha's fence is free: no named wait"
        );

        let fence = jobs
            .acquire_job(format!("scan:library:{}", libraries[0].id))
            .await
            .expect("acquire Alpha's fence")
            .expect("Alpha's fence was free");
        assert!(!jobs.work_library_queue(&transcode, None).await);
        assert_eq!(
            waiting_on().await,
            "full_scan:Alpha",
            "with its fence held, the refusal names it"
        );
        drop(fence);
    }

    /// Contract v2 §5.1: a targeted request is its own job. Behind a
    /// *pending* full scan of a 1,000-file library it is claimed first; beside
    /// a *running* full scan of another library on the same volume it runs,
    /// because it takes no background source reader; beside a *running* full
    /// scan of its own library it waits on the library's mutation fence with
    /// `waiting_on = full_scan:<library>`, runs once the fence is released,
    /// and what it inserted survives the next full scan's reconciliation.
    #[tokio::test]
    async fn targeted_job_runs_ahead_of_pending_full_scan_and_waits_for_running_one() {
        // A pending full scan of a 1,000-file library does not delay it.
        {
            let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
            let root = crate::test_tempdir().expect("root");
            for index in 0..1_000 {
                std::fs::write(
                    root.path().join(format!("Film {index} (2001).mkv")),
                    b"fixture",
                )
                .expect("fixture file");
            }
            let library = store
                .create_library(&NewLibrary {
                    name: "Thousand".into(),
                    kind: LibraryKind::Movies,
                    paths: vec![root.path().to_path_buf()],
                    anime: false,
                })
                .await
                .expect("library");
            enqueue_full(store.as_ref(), library.id, "full-pending", false).await;
            let new = media_file(root.path(), "New Film (2020)");
            enqueue_targeted(
                store.as_ref(),
                library.id,
                "targeted-ahead",
                new.parent().expect("folder"),
                &[&new],
            )
            .await;
            let targeted_job = request_record(store.as_ref(), "targeted-ahead")
                .await
                .job_id;
            let full_job = request_record(store.as_ref(), "full-pending").await.job_id;
            assert_ne!(targeted_job, full_job, "targeted work is its own job");
            let jobs = JobManager::test_new(Arc::clone(&store), root.path().join("artwork"));
            let transcode = transcode_for(Arc::clone(&store), &root.path().join("transcode"));
            assert!(jobs.work_library_queue(&transcode, None).await);
            assert_eq!(
                request_record(store.as_ref(), "targeted-ahead").await.state,
                "succeeded",
                "the targeted job is claimed before the pending full scan"
            );
            assert_eq!(
                request_record(store.as_ref(), "full-pending").await.state,
                "pending"
            );
            let sidecar = store
                .scan_request_v2("targeted-ahead")
                .await
                .expect("sidecar")
                .expect("v2 request");
            assert!(
                matches!(sidecar.files.as_slice(),
                    [plurx_core::store::scan_request_v2::PlacedReport::Placed { path, .. }]
                        if *path == new.display().to_string()),
                "{:?}",
                sidecar.files
            );
        }

        // A running full scan of another library on the same volume does not
        // hold it: a targeted probe takes no background source reader
        // (contract v2 §5.1), so the only wait it has is its own library's
        // fence.
        {
            let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
            let volume = crate::test_tempdir().expect("volume");
            let (alpha_root, beta_root) = (volume.path().join("alpha"), volume.path().join("beta"));
            std::fs::create_dir_all(&alpha_root).expect("alpha root");
            std::fs::create_dir_all(&beta_root).expect("beta root");
            media_file(&beta_root, "Long Scan (1990)");
            let mut libraries = Vec::new();
            for (name, root) in [("Alpha", &alpha_root), ("Beta", &beta_root)] {
                libraries.push(
                    store
                        .create_library(&NewLibrary {
                            name: name.into(),
                            kind: LibraryKind::Movies,
                            paths: vec![root.clone()],
                            anime: false,
                        })
                        .await
                        .expect("library"),
                );
            }
            let (alpha, beta) = (&libraries[0], &libraries[1]);
            let mappings = libraries
                .iter()
                .flat_map(|library| {
                    library.paths.iter().map(|root| {
                        plurx_core::store::background_jobs_resources::StorageDomainMapping {
                            library_id: library.id,
                            root_path: root.display().to_string(),
                            domain_id: "volume-1".into(),
                        }
                    })
                })
                .collect();
            assert!(store
                .replace_storage_domains(mappings, clock_ms())
                .await
                .expect("one storage domain for both libraries"));
            enqueue_full(store.as_ref(), beta.id, "beta-running", false).await;
            let full_jobs = JobManager::test_new_for_node(
                Arc::clone(&store),
                volume.path().join("artwork-w1"),
                "w1",
            );
            let full_transcode =
                transcode_for(Arc::clone(&store), &volume.path().join("transcode-w1"));
            let (tx, mut events) = mpsc::unbounded_channel();
            let (full, _, resume_full) = spawn_worker(
                "w1",
                full_jobs,
                full_transcode,
                tx,
                Some(WorkerPoint::BeforeScan),
                false,
                0,
            );
            next_worker_event(&mut events, "w1", WorkerPoint::BeforeScan).await;
            let beta_job = request_record(store.as_ref(), "beta-running").await.job_id;
            assert!(
                store
                    .source_io_holders(crate::media_sessions::unix_ms())
                    .await
                    .expect("source readers")
                    .iter()
                    .any(|holder| holder.work_id == beta_job
                        && holder.resource_key == "source_io:volume-1"),
                "the running full scan of Beta holds the volume's reader"
            );
            let new = media_file(&alpha_root, "Alpha Import (2025)");
            enqueue_targeted(
                store.as_ref(),
                alpha.id,
                "alpha-targeted",
                new.parent().expect("folder"),
                &[&new],
            )
            .await;
            let jobs = JobManager::test_new_for_node(
                Arc::clone(&store),
                volume.path().join("artwork-w2"),
                "w2",
            );
            let transcode = transcode_for(Arc::clone(&store), &volume.path().join("transcode-w2"));
            assert!(
                jobs.work_library_queue(&transcode, None).await,
                "the targeted job for Alpha is claimed beside Beta's running scan"
            );
            assert_eq!(
                request_record(store.as_ref(), "alpha-targeted").await.state,
                "succeeded"
            );
            assert_eq!(
                store
                    .scan_request_v2("alpha-targeted")
                    .await
                    .expect("sidecar")
                    .expect("v2 request")
                    .waiting_on,
                "",
                "it waited on nothing"
            );
            assert_ne!(
                request_record(store.as_ref(), "beta-running").await.state,
                "succeeded",
                "Beta's scan was still running"
            );
            resume_full
                .expect("full scan barrier")
                .send(())
                .expect("resume full scan");
            assert!(full.await.expect("full scan worker joined").0);
            assert_eq!(
                request_record(store.as_ref(), "beta-running").await.state,
                "succeeded"
            );
        }

        // Beside a running full scan of its own library it is claimed, yields
        // at the library's fence with `waiting_on = full_scan:<library>`, and
        // runs once the fence is released.
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("root");
        media_file(root.path(), "Old Film (1999)");
        let library = store
            .create_library(&NewLibrary {
                name: "Fenced".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        enqueue_full(store.as_ref(), library.id, "full-running", false).await;
        let full_jobs =
            JobManager::test_new_for_node(Arc::clone(&store), root.path().join("artwork-w1"), "w1");
        let full_transcode = transcode_for(Arc::clone(&store), &root.path().join("transcode-w1"));
        let (tx, mut events) = mpsc::unbounded_channel();
        let (full, _, resume_full) = spawn_worker(
            "w1",
            full_jobs,
            full_transcode,
            tx,
            Some(WorkerPoint::BeforeScan),
            false,
            0,
        );
        next_worker_event(&mut events, "w1", WorkerPoint::BeforeScan).await;

        let new = media_file(root.path(), "Just Imported (2024)");
        enqueue_targeted(
            store.as_ref(),
            library.id,
            "targeted-fenced",
            new.parent().expect("folder"),
            &[&new],
        )
        .await;
        let jobs =
            JobManager::test_new_for_node(Arc::clone(&store), root.path().join("artwork-w2"), "w2");
        let transcode = transcode_for(Arc::clone(&store), &root.path().join("transcode-w2"));
        assert!(
            jobs.work_library_queue(&transcode, None).await,
            "the targeted job is claimed and yields at the fence"
        );
        assert_eq!(
            request_record(store.as_ref(), "targeted-fenced")
                .await
                .state,
            "pending",
            "never concurrent with a running full scan"
        );
        assert_eq!(
            store
                .scan_request_v2("targeted-fenced")
                .await
                .expect("sidecar")
                .expect("v2 request")
                .waiting_on,
            "full_scan:Fenced"
        );

        resume_full
            .expect("full scan barrier")
            .send(())
            .expect("resume full scan");
        assert!(full.await.expect("full scan worker joined").0);
        assert_eq!(
            request_record(store.as_ref(), "full-running").await.state,
            "succeeded"
        );
        // A fence refusal defers a targeted job by a quarter second, not by
        // the shared driver's five (review #1405, finding 4).
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                jobs.work_library_queue(&transcode, None).await;
                if request_record(store.as_ref(), "targeted-fenced")
                    .await
                    .state
                    != "pending"
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
        })
        .await
        .expect("targeted job ran once the fence was released");
        let record = request_record(store.as_ref(), "targeted-fenced").await;
        assert_eq!(record.state, "succeeded");
        let sidecar = store
            .scan_request_v2("targeted-fenced")
            .await
            .expect("sidecar")
            .expect("v2 request");
        assert_eq!(sidecar.waiting_on, "", "a running request waits on nothing");
        let placed = new.canonicalize().expect("canonical");
        assert!(store
            .get_file_by_path(&placed.to_string_lossy())
            .await
            .expect("file lookup")
            .is_some());

        // The next full scan's known-minus-seen reconciliation keeps it.
        enqueue_full(store.as_ref(), library.id, "full-after", false).await;
        assert!(jobs.work_library_queue(&transcode, None).await);
        assert_eq!(
            request_record(store.as_ref(), "full-after").await.state,
            "succeeded"
        );
        assert!(store
            .get_file_by_path(&placed.to_string_lossy())
            .await
            .expect("file lookup")
            .is_some());
    }

    /// Contract v2 §5.1: the targeted job claims with a probe admission. With
    /// every heavy-lane permit held, a full scan is not claimed at all, and a
    /// targeted request still completes well inside fifteen seconds.
    #[tokio::test]
    async fn targeted_job_claims_without_heavy_admission() {
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("root");
        let library = store
            .create_library(&NewLibrary {
                name: "Saturated".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        let jobs = JobManager::test_new(Arc::clone(&store), root.path().join("artwork"));
        let transcode = transcode_for(Arc::clone(&store), &root.path().join("transcode"));
        let mut held = Vec::new();
        while let Some(admission) = transcode.admit_fragment().await {
            held.push(admission);
        }
        assert!(!held.is_empty(), "the fixture holds the heavy lane");

        enqueue_full(store.as_ref(), library.id, "full-saturated", false).await;
        assert!(
            !jobs.work_library_queue(&transcode, None).await,
            "a full scan needs heavy admission"
        );
        let new = media_file(root.path(), "Saturated Night (2023)");
        enqueue_targeted(
            store.as_ref(),
            library.id,
            "targeted-saturated",
            new.parent().expect("folder"),
            &[&new],
        )
        .await;
        let started = std::time::Instant::now();
        assert!(tokio::time::timeout(
            Duration::from_secs(15),
            jobs.work_library_queue(&transcode, None)
        )
        .await
        .expect("targeted claim does not wait for the heavy lane"));
        assert!(started.elapsed() < Duration::from_secs(15));
        assert_eq!(
            request_record(store.as_ref(), "targeted-saturated")
                .await
                .state,
            "succeeded"
        );
        assert_eq!(
            request_record(store.as_ref(), "full-saturated").await.state,
            "pending"
        );
        drop(held);
    }

    /// The targeted job keeps the `library_scan` kind and is told apart by its
    /// payload generation, so a claimer must run only the generations it
    /// knows: a `library_scan` row from a newer release is left queued, never
    /// run as a full scan or failed (review #1405, condition ii).
    #[tokio::test]
    async fn claim_library_leaves_an_unknown_library_scan_generation_queued() {
        use plurx_core::store::background_jobs::{EnqueueJob, JobRequest};
        let store: Arc<dyn Store> = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let root = crate::test_tempdir().expect("root");
        let library = store
            .create_library(&NewLibrary {
                name: "Future".into(),
                kind: LibraryKind::Movies,
                paths: vec![root.path().to_path_buf()],
                anime: false,
            })
            .await
            .expect("library");
        let now_ms = clock_ms();
        let EnqueueOutcome::Accepted { job_id, .. } = store
            .enqueue_job(EnqueueJob {
                id: uuid::Uuid::new_v4().to_string(),
                payload: JobPayload::LibraryScan {
                    library_id: library.id,
                    generation: "library-future-v9".into(),
                },
                dedupe_key: format!("library:future:{}", library.id),
                priority: 3,
                not_before_ms: now_ms,
                now_ms,
                request: JobRequest {
                    scope: "test".into(),
                    request_id: uuid::Uuid::new_v4().to_string(),
                    request_digest: "f".repeat(64),
                    consumer_kind: "test".into(),
                    consumer_ref: library.id.to_string(),
                    target_node_id: None,
                    deadline_ms: None,
                    retain_identity: false,
                },
            })
            .await
            .expect("enqueue a future generation")
        else {
            panic!("the future-generation row must be queued")
        };
        let jobs = JobManager::test_new(Arc::clone(&store), root.path().join("artwork"));
        let transcode = transcode_for(Arc::clone(&store), &root.path().join("transcode"));
        assert!(
            !jobs.work_library_queue(&transcode, None).await,
            "an unknown generation is not claimed"
        );
        let job = store
            .background_job(&job_id)
            .await
            .expect("read job")
            .expect("retained");
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.failed_attempts, 0);
        assert!(job.token.is_none());
    }
}
