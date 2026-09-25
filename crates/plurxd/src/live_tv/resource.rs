//! Cluster authority for starts and shared channel transports.
use super::*;
use plurx_core::live_tv_resource::{
    self as ledger, Command, Outcome, Record, Start, StartPhase, Worker,
};

pub(crate) fn now_ms() -> i64 {
    unix_seconds().saturating_mul(1000)
}
fn store_error(error: plurx_core::error::StoreError) -> LiveTvError {
    LiveTvError::OwnerUnavailable(format!("Live TV authority is unavailable: {error}"))
}
fn fenced() -> LiveTvError {
    LiveTvError::CapabilityExpired("the cluster no longer authorizes this Live TV attempt".into())
}
fn start_record(
    request: &LiveTvStartRequest,
    device_id: &str,
    now: i64,
) -> Result<Start, LiveTvError> {
    let digest = serde_json::to_string(&(
        request.channel_id.as_str(),
        request.config_generation,
        &request.playback,
    ))
    .map_err(|e| LiveTvError::InvalidResponse(e.to_string()))?;
    use sha2::Digest;
    let digest = format!("{:x}", sha2::Sha256::digest(digest.as_bytes()));
    Ok(Start {
        user_id: request.user_id,
        request_id: request.request_id.clone(),
        digest,
        generation: request.config_generation,
        device_id: device_id.into(),
        channel_id: request.channel_id.clone(),
        issued_at_ms: now,
        admission_until_ms: now.saturating_add(ledger::INTENT_MS),
        epoch: 0,
        worker: None,
        ingest_id: None,
        phase: StartPhase::Issued,
        response: None,
    })
}
impl LiveTvManager {
    pub(crate) fn resource_worker(&self) -> Worker {
        Worker {
            node_id: self.node_id.clone(),
            boot_id: self
                .incarnation
                .current
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .incarnation
                .clone(),
        }
    }

    pub(crate) async fn resource_snapshot(
        &self,
        user: i64,
        id: &str,
    ) -> Result<ledger::Snapshot, LiveTvError> {
        self.store
            .live_tv_resource_snapshot(user, id, now_ms())
            .await
            .map_err(store_error)
    }

    pub(crate) async fn resource_issue(
        &self,
        request: &LiveTvStartRequest,
        device_id: &str,
    ) -> Result<Start, LiveTvError> {
        match self
            .store
            .live_tv_resource_command(
                Command::Issue(start_record(request, device_id, now_ms())?),
                now_ms(),
            )
            .await
            .map_err(store_error)?
        {
            Outcome::Record(Record::Start(s)) => Ok(s),
            Outcome::Capacity => Err(LiveTvError::Capacity(
                "too many outstanding Live TV intents".into(),
            )),
            _ => Err(fenced()),
        }
    }

    pub(crate) async fn resource_start(
        &self,
        request: &LiveTvStartRequest,
        device_id: &str,
        limit: u8,
    ) -> Result<Start, LiveTvError> {
        let now = now_ms();
        let start = start_record(request, device_id, now)?;
        let outcome = self
            .store
            .live_tv_resource_command(
                Command::Reserve(ledger::Reserve {
                    start,
                    worker: self.resource_worker(),
                    ingest_id: uuid::Uuid::new_v4().simple().to_string(),
                    limit,
                }),
                now,
            )
            .await
            .map_err(store_error)?;
        match outcome {
            Outcome::Record(Record::Start(start)) if !start.phase.terminal() => {
                if start.worker.as_ref() != Some(&self.resource_worker()) {
                    return Err(LiveTvError::OwnerUnavailable(
                        "this request is assigned to another channel worker".into(),
                    ));
                }
                if start.phase == StartPhase::Reserved {
                    self.resource_advance(request, StartPhase::Opening, None)
                        .await?;
                }
                Ok(start)
            }
            Outcome::Capacity => Err(LiveTvError::Capacity(
                "the cluster's tuner or request capacity is occupied".into(),
            )),
            Outcome::Conflict => Err(LiveTvError::Conflict(
                "this request ID already names different playback, or its channel is draining"
                    .into(),
            )),
            _ => Err(fenced()),
        }
    }

    pub(crate) async fn resource_advance(
        &self,
        request: &LiveTvStartRequest,
        phase: StartPhase,
        response: Option<String>,
    ) -> Result<(), LiveTvError> {
        let snapshot = self
            .resource_snapshot(request.user_id, &request.request_id)
            .await?;
        let start = snapshot
            .records
            .iter()
            .find_map(|r| match r {
                Record::Start(s)
                    if s.user_id == request.user_id && s.request_id == request.request_id =>
                {
                    Some(s)
                }
                _ => None,
            })
            .ok_or_else(fenced)?;
        if start.phase == phase && start.worker.as_ref() == Some(&self.resource_worker()) {
            return Ok(());
        }
        match self
            .store
            .live_tv_resource_command(
                Command::Advance {
                    user_id: request.user_id,
                    request_id: request.request_id.clone(),
                    epoch: start.epoch,
                    worker: self.resource_worker(),
                    phase,
                    response,
                },
                now_ms(),
            )
            .await
            .map_err(store_error)?
        {
            Outcome::Record(_) => Ok(()),
            _ => Err(fenced()),
        }
    }

    pub(crate) async fn resource_retire(&self, user: i64, id: &str) -> Result<(), LiveTvError> {
        self.store
            .live_tv_resource_command(
                Command::Retire {
                    user_id: user,
                    request_id: id.into(),
                },
                now_ms(),
            )
            .await
            .map_err(store_error)?;
        Ok(())
    }

    pub(super) async fn resource_session_fence(
        &self,
        session: &LiveTvSession,
    ) -> Result<(), LiveTvError> {
        let snapshot = self
            .resource_snapshot(session.request.user_id, &session.request.request_id)
            .await?;
        if !snapshot.enabled
            || snapshot.generation != session.request.config_generation
            || !self.serving.is_current(session.owner_serving_generation)
        {
            return Err(fenced());
        }
        let start = snapshot
            .records
            .iter()
            .find_map(|r| match r {
                Record::Start(s)
                    if s.user_id == session.request.user_id
                        && s.request_id == session.request.request_id =>
                {
                    Some(s)
                }
                _ => None,
            })
            .ok_or_else(fenced)?;
        if start.phase.terminal() || start.worker.as_ref() != Some(&self.resource_worker()) {
            return Err(fenced());
        }
        if !snapshot.records.iter().any(|r| {
            matches!(r, Record::Ingest(i) if
            Some(&i.id) == start.ingest_id.as_ref() && i.epoch == start.epoch &&
            i.worker == self.resource_worker() && i.expires_at_ms > now_ms() && !i.draining)
        }) {
            return Err(fenced());
        }
        Ok(())
    }

    pub(super) async fn resource_transport_release(
        &self,
        ingest: &ledger::Ingest,
    ) -> Result<(), LiveTvError> {
        self.store
            .live_tv_resource_command(
                Command::Release {
                    ingest_id: ingest.id.clone(),
                    epoch: ingest.epoch,
                    worker: self.resource_worker(),
                },
                now_ms(),
            )
            .await
            .map_err(store_error)?;
        Ok(())
    }

    pub(super) async fn resource_transport_admit(
        &self,
        transport: &dvr::DvrTransport,
    ) -> Result<(tokio::time::Instant, ledger::Ingest), LiveTvError> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ledger::LEASE_MS as u64);
        let snapshot = tokio::time::timeout_at(deadline, self.resource_snapshot(0, ""))
            .await
            .map_err(|_| fenced())??;
        let ingest = snapshot
            .records
            .into_iter()
            .find_map(|r| match r {
                Record::Ingest(i)
                    if i.device_id == transport.device_id
                        && i.channel_id == transport.channel.id
                        && i.generation == transport.generation
                        && i.worker == self.resource_worker() =>
                {
                    Some(i)
                }
                _ => None,
            })
            .ok_or_else(fenced)?;
        let result = tokio::time::timeout_at(
            deadline,
            self.store.live_tv_resource_command(
                Command::Renew {
                    ingest_id: ingest.id,
                    epoch: ingest.epoch,
                    revision: ingest.revision,
                    worker: self.resource_worker(),
                },
                now_ms(),
            ),
        )
        .await
        .map_err(|_| fenced())?
        .map_err(store_error)?;
        let next = match result {
            Outcome::Record(Record::Ingest(i))
                if !i.draining && tokio::time::Instant::now() < deadline =>
            {
                i
            }
            _ => return Err(fenced()),
        };
        Ok((deadline, next))
    }

    /// The lease RPC starts the monotonic deadline. Neither a late reply nor a
    /// stalled database can extend a socket's local authority.
    pub(super) async fn resource_transport_lease(
        &self,
        transport: &dvr::DvrTransport,
        mut valid_until: tokio::time::Instant,
        mut ingest: ledger::Ingest,
    ) -> Result<(), LiveTvError> {
        let worker = self.resource_worker();
        loop {
            let began = tokio::time::Instant::now();
            let deadline = valid_until.min(began + Duration::from_millis(ledger::LEASE_MS as u64));
            let outcome = tokio::time::timeout_at(
                deadline,
                self.store.live_tv_resource_command(
                    Command::Renew {
                        ingest_id: ingest.id.clone(),
                        epoch: ingest.epoch,
                        revision: ingest.revision,
                        worker: worker.clone(),
                    },
                    now_ms(),
                ),
            )
            .await
            .map_err(|_| fenced())?
            .map_err(store_error)?;
            ingest = match outcome {
                Outcome::Record(Record::Ingest(i)) => i,
                _ => return Err(fenced()),
            };
            if tokio::time::Instant::now() >= deadline
                || !self.serving.is_current(transport.owner_serving_generation)
            {
                return Err(fenced());
            }
            valid_until = began + Duration::from_millis(ledger::LEASE_MS as u64);
            tokio::select! {
                _ = transport.cancel.cancelled() => return Ok(()),
                _ = tokio::time::sleep_until(began + Duration::from_secs(5)) => {},
            }
        }
    }
}

/// The marker names a filesystem namespace, not a pathname that happens to
/// look identical on two nodes. Creation is exclusive; a concurrent creator
/// reads the winner's stable identity.
pub(super) async fn storage_identity(root: &str) -> Result<String, LiveTvError> {
    use tokio::io::AsyncWriteExt;
    tokio::fs::create_dir_all(root)
        .await
        .map_err(|e| LiveTvError::InvalidConfig(e.to_string()))?;
    let marker = Path::new(root).join(".plurx-storage-id");
    let id = uuid::Uuid::new_v4().simple().to_string();
    match tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&marker)
        .await
    {
        Ok(mut file) => {
            file.write_all(id.as_bytes())
                .await
                .map_err(|e| LiveTvError::InvalidConfig(e.to_string()))?;
            file.sync_all()
                .await
                .map_err(|e| LiveTvError::InvalidConfig(e.to_string()))?;
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => {
            return Err(LiveTvError::InvalidConfig(format!(
                "DVR storage identity: {e}"
            )))
        }
    }
    let value = tokio::fs::read_to_string(&marker)
        .await
        .map_err(|e| LiveTvError::InvalidConfig(e.to_string()))?;
    if value.len() != 32 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(LiveTvError::InvalidConfig(
            "DVR storage identity is incomplete or invalid".into(),
        ));
    }
    Ok(value)
}

impl LiveTvManager {
    pub(super) async fn resource_capture(
        &self,
        id: &str,
    ) -> Result<Option<ledger::Capture>, LiveTvError> {
        Ok(
            match self
                .store
                .live_tv_resource_lookup(&format!("capture:{id}"), now_ms())
                .await
                .map_err(store_error)?
            {
                Some(Record::Capture(c)) => Some(c),
                _ => None,
            },
        )
    }
    pub(super) async fn resource_capture_claim(
        &self,
        live: &LiveTvConfig,
        dvr: &DvrConfig,
        row: &plurx_core::dvr::DvrRecording,
        device: &str,
        base: &Path,
    ) -> Result<ledger::Capture, LiveTvError> {
        let capture = ledger::Capture {
            recording_id: row.id.clone(),
            epoch: 0,
            worker: self.resource_worker(),
            ingest_id: String::new(),
            ingest_epoch: 0,
            storage_id: storage_identity(&dvr.root).await?,
            base_path: base.to_string_lossy().into_owned(),
            generation: live.generation,
            expires_at_ms: 0,
            stopped: false,
            deleted: false,
            finalizer: None,
            finalizer_epoch: 0,
            published_path: None,
        };
        match self
            .store
            .live_tv_resource_command(
                Command::ClaimCapture {
                    capture,
                    ingest_id: uuid::Uuid::new_v4().simple().to_string(),
                    limit: live.max_sessions,
                    reserve: dvr.tuner_reserve,
                    device_id: device.into(),
                    channel_id: row.channel_id.clone(),
                },
                now_ms(),
            )
            .await
            .map_err(store_error)?
        {
            Outcome::Record(Record::Capture(c)) if c.worker == self.resource_worker() => Ok(c),
            Outcome::Capacity => Err(LiveTvError::Capacity(
                "the cluster's recording capacity is occupied".into(),
            )),
            _ => Err(fenced()),
        }
    }
    pub(super) async fn resource_capture_detach(
        &self,
        id: &str,
        epoch: i64,
    ) -> Result<(), LiveTvError> {
        self.store
            .live_tv_resource_command(
                Command::DetachCapture {
                    recording_id: id.into(),
                    epoch,
                    worker: self.resource_worker(),
                },
                now_ms(),
            )
            .await
            .map_err(store_error)?;
        Ok(())
    }
    pub(super) async fn resource_finalize(
        &self,
        id: &str,
        root: &str,
    ) -> Result<ledger::Capture, LiveTvError> {
        let storage_id = storage_identity(root).await?;
        let legacy = self
            .store
            .get_dvr_recording(id)
            .await
            .map_err(store_error)?
            .and_then(|row| {
                let base_path = row.path.as_deref()?.strip_suffix(".ts")?.to_owned();
                Some(ledger::Capture {
                    recording_id: id.into(),
                    epoch: row.attempt,
                    worker: self.resource_worker(),
                    ingest_id: String::new(),
                    ingest_epoch: 0,
                    storage_id: storage_id.clone(),
                    base_path,
                    generation: 0,
                    expires_at_ms: 0,
                    stopped: true,
                    deleted: false,
                    finalizer: None,
                    finalizer_epoch: 0,
                    published_path: None,
                })
            });
        match self
            .store
            .live_tv_resource_command(
                Command::ClaimFinalizer {
                    recording_id: id.into(),
                    worker: self.resource_worker(),
                    storage_id,
                    legacy,
                },
                now_ms(),
            )
            .await
            .map_err(store_error)?
        {
            Outcome::Record(Record::Capture(c)) => Ok(c),
            _ => Err(fenced()),
        }
    }
    pub(super) async fn resource_finalize_lease(
        &self,
        claim: &ledger::Capture,
        mut deadline: tokio::time::Instant,
    ) -> Result<(), LiveTvError> {
        loop {
            tokio::time::sleep(Duration::from_secs(5)).await;
            let began = tokio::time::Instant::now();
            let result = tokio::time::timeout_at(
                deadline,
                self.store.live_tv_resource_command(
                    Command::RenewFinalizer {
                        recording_id: claim.recording_id.clone(),
                        worker: self.resource_worker(),
                        epoch: claim.finalizer_epoch,
                    },
                    now_ms(),
                ),
            )
            .await
            .map_err(|_| fenced())?
            .map_err(store_error)?;
            if !matches!(result, Outcome::Record(Record::Capture(_)))
                || tokio::time::Instant::now() >= deadline
            {
                return Err(fenced());
            }
            deadline = began + Duration::from_millis(ledger::LEASE_MS as u64);
        }
    }

    pub(super) async fn resource_publish(
        &self,
        claim: &ledger::Capture,
        path: &Path,
        bytes: i64,
        gap_s: i64,
        stopped_by: Option<i64>,
    ) -> Result<(), LiveTvError> {
        match self
            .store
            .live_tv_resource_command(
                Command::PublishCapture {
                    recording_id: claim.recording_id.clone(),
                    worker: self.resource_worker(),
                    epoch: claim.finalizer_epoch,
                    path: path.to_string_lossy().into_owned(),
                    bytes,
                    gap_s,
                    stopped_by,
                },
                now_ms(),
            )
            .await
            .map_err(store_error)?
        {
            Outcome::Applied => Ok(()),
            _ => Err(fenced()),
        }
    }
}
