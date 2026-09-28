//! Durable library admission and execution; the existing scanner owns identity.
use super::*;
use plurx_core::store::background_jobs::{EnqueueOutcome, JobPayload, JobSettlement};
use plurx_core::store::background_jobs_library::{
    LibraryTrigger, LibraryWorkInput, LibraryWorkQuery, LibraryWorkRecord, LibraryWorkResult,
    NewLibraryWork, MAX_LIBRARY_RESULT_BYTES,
};

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
            Ok(()) => true,
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

    async fn admit_library(&self, input: NewLibraryWork) -> Result<(), TargetError> {
        match self.store.enqueue_library_work(input).await? {
            EnqueueOutcome::Accepted { .. }
            | EnqueueOutcome::Existing {
                cancelled: false, ..
            } => {
                self.library_wake.notify_one();
                Ok(())
            }
            outcome => Err(TargetError::Refused(format!(
                "library work was not accepted: {outcome:?}"
            ))),
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
        for record in records {
            if !matches!(record.input, LibraryWorkInput::Full { .. })
                || statuses.contains_key(&record.library_id)
            {
                continue;
            }
            let pending = record.state == "pending";
            let (last_scan, mut error) = match record.result {
                Some(LibraryWorkResult::Completed { scan }) => (Some(scan.report), None),
                Some(LibraryWorkResult::Failed { error }) => (None, Some(error)),
                None => (None, record.error_code),
            };
            if pending && error.is_none() {
                error = self.library_readiness.problem(record.library_id);
            }
            statuses.insert(
                record.library_id,
                ScanStatus {
                    running: pending,
                    phase: pending.then(|| "queued".into()),
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
        let lease = match self.acquire_job(format!("scan:library:{library_id}")).await {
            Ok(Some(lease)) => lease,
            result => {
                if let Err(error) = result {
                    tracing::warn!(library_id, %error, "library domain lease unavailable");
                }
                settle_library(
                    &fence,
                    JobSettlement::Yield {
                        checkpoint: None,
                        not_before_ms: clock_ms().saturating_add(5_000),
                    },
                )
                .await;
                active.finish().await;
                return true;
            }
        };
        let publisher = lease.publisher(self.store.as_ref());
        match fence.bind_library(&publisher).await {
            Ok(true) => {}
            result => {
                tracing::warn!(library_id, ?result, "library publication binding refused");
                drop(publisher);
                if let Err(error) = lease.release().await {
                    tracing::warn!(%error, "releasing unbound library lease");
                }
                settle_library(
                    &fence,
                    JobSettlement::Yield {
                        checkpoint: None,
                        not_before_ms: clock_ms().saturating_add(5_000),
                    },
                )
                .await;
                active.finish().await;
                return true;
            }
        }
        let cancellation = tokio_util::sync::CancellationToken::new();
        let queue_loss = fence.loss_token();
        let domain_loss = lease.loss_token();
        let (result, yielded) = {
            let work = plurx_core::process::bounded::cancellable(
                cancellation.clone(),
                self.process_library_requests(&job.id, library_id, &lease, &publisher, &fence),
            );
            tokio::pin!(work);
            let mut pressure = tokio::time::interval(Duration::from_millis(250));
            loop {
                tokio::select! {
                    biased;
                    () = queue_loss.cancelled() => {
                        lease.revoke(); cancellation.cancel();
                        break (work.await, false);
                    }
                    () = domain_loss.cancelled() => {
                        lease.revoke(); cancellation.cancel();
                        break (work.await, true);
                    }
                    _ = pressure.tick() => {
                        if !transcode.fragment_worker_idle(&admission) {
                            lease.revoke(); cancellation.cancel();
                            break (work.await, true);
                        }
                    }
                    result = &mut work => break (result, false),
                }
            }
        };
        // `work` has returned only after the blocking walker and any bounded
        // media child joined. Keep the physical admission until that point.
        drop(publisher);
        if let Err(error) = lease.release().await {
            tracing::warn!(library_id, %error, "releasing library domain lease");
        }
        if yielded {
            settle_library(
                &fence,
                JobSettlement::Yield {
                    checkpoint: None,
                    not_before_ms: clock_ms().saturating_add(1_000),
                },
            )
            .await;
        } else if let Err(error) = result {
            tracing::warn!(library_id, %error, "durable library attempt failed");
            settle_library(
                &fence,
                JobSettlement::Retry {
                    error_code: "library_execution_failed".into(),
                    not_before_ms: clock_ms().saturating_add(
                        crate::background_jobs::retry_delay_ms(&job.id, job.failed_attempts),
                    ),
                },
            )
            .await;
        }
        active.finish().await;
        drop(admission);
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
    use plurx_core::cluster::coordination::LeaseClaim;
    use plurx_core::domain::NewLibrary;
    use plurx_core::store::{SqliteStore, Store};
    use plurx_core::transcode::{EncoderCaps, Pipeline};

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
}
