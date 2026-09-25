//! Durable Live TV authority, independent of the node reaching the tuner.
//!
//! One revision serializes small record mutations. Records are individually
//! indexed and only changed records enter Raft; terminal history is never
//! rewritten on renewal. Both backends run the same transition function.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::error::StoreError;

pub const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS live_tv_resource_revision (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1),
    revision INTEGER NOT NULL CHECK(revision >= 0), nonce TEXT NOT NULL
) STRICT;
INSERT OR IGNORE INTO live_tv_resource_revision VALUES (1,0,'');
CREATE TABLE IF NOT EXISTS live_tv_resource_records (
    id TEXT PRIMARY KEY, kind TEXT NOT NULL, user_id INTEGER NOT NULL,
    live INTEGER NOT NULL CHECK(live IN (0,1)), expires_at_ms INTEGER NOT NULL,
    body TEXT NOT NULL CHECK(json_valid(body))
) STRICT;
CREATE INDEX IF NOT EXISTS live_tv_resource_live ON live_tv_resource_records(live, expires_at_ms);
CREATE INDEX IF NOT EXISTS live_tv_resource_user ON live_tv_resource_records(user_id, live);
CREATE INDEX IF NOT EXISTS live_tv_resource_kind ON live_tv_resource_records(kind, live);
CREATE TRIGGER IF NOT EXISTS live_tv_capture_revision_update AFTER UPDATE ON dvr_recordings BEGIN UPDATE live_tv_resource_revision SET revision=revision+1 WHERE singleton=1; END;
CREATE TRIGGER IF NOT EXISTS live_tv_capture_revision_delete AFTER DELETE ON dvr_recordings BEGIN UPDATE live_tv_resource_revision SET revision=revision+1 WHERE singleton=1; END;";

pub const LEASE_MS: i64 = 30_000;
pub const INTENT_MS: i64 = 300_000;
pub const HISTORY_MS: i64 = 86_400_000;
const ACTIVE_USER_MAX: usize = 32;
const ACTIVE_MAX: usize = 4096;
const HISTORY_USER_MAX: usize = 1024;
const HISTORY_MAX: usize = 65_536;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worker {
    pub node_id: String,
    pub boot_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Start {
    pub user_id: i64,
    pub request_id: String,
    pub digest: String,
    pub generation: i64,
    pub device_id: String,
    pub channel_id: String,
    pub issued_at_ms: i64,
    pub admission_until_ms: i64,
    pub epoch: i64,
    pub worker: Option<Worker>,
    pub ingest_id: Option<String>,
    pub phase: StartPhase,
    /// The daemon's provisional response. Never expose through list views.
    pub response: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartPhase {
    Issued,
    Reserved,
    Opening,
    Active,
    Draining,
    Closed,
    Failed,
    Expired,
    Retired,
}
impl StartPhase {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Closed | Self::Failed | Self::Expired | Self::Retired
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ingest {
    pub id: String,
    pub device_id: String,
    pub channel_id: String,
    pub generation: i64,
    pub epoch: i64,
    pub revision: i64,
    pub worker: Worker,
    pub recording: bool,
    pub draining: bool,
    pub expires_at_ms: i64,
    /// Pending attachments count as demand, just like attached consumers.
    pub consumers: BTreeMap<String, i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capture {
    pub recording_id: String,
    pub epoch: i64,
    pub worker: Worker,
    pub ingest_id: String,
    pub ingest_epoch: i64,
    pub storage_id: String,
    pub base_path: String,
    pub generation: i64,
    pub expires_at_ms: i64,
    pub stopped: bool,
    pub deleted: bool,
    pub finalizer: Option<Worker>,
    pub finalizer_epoch: i64,
    #[serde(default)]
    pub published_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Record {
    Start(Start),
    Ingest(Ingest),
    Capture(Capture),
    LegacyBlock { user_id: i64, until_ms: i64 },
}

impl Record {
    pub fn id(&self) -> String {
        match self {
            Self::Start(s) => start_key(s.user_id, &s.request_id),
            Self::Ingest(i) => format!("ingest:{}", i.id),
            Self::Capture(c) => format!("capture:{}", c.recording_id),
            Self::LegacyBlock { user_id, .. } => format!("legacy-block:{user_id}"),
        }
    }
    pub(crate) fn columns(&self, now: i64) -> (&'static str, i64, bool, i64) {
        match self {
            Self::Start(s) => (
                "start",
                s.user_id,
                !s.phase.terminal(),
                if s.phase.terminal() {
                    now.saturating_add(HISTORY_MS)
                } else {
                    i64::MAX
                },
            ),
            Self::Ingest(i) => ("ingest", 0, true, i.expires_at_ms),
            Self::Capture(c) => ("capture", 0, c.expires_at_ms > now, i64::MAX),
            Self::LegacyBlock { user_id, until_ms } => ("legacy_block", *user_id, true, *until_ms),
        }
    }
}

pub fn start_key(user: i64, id: &str) -> String {
    format!("start:{user}:{id}")
}

pub fn valid_request_id(id: &str) -> bool {
    let raw = id.strip_prefix("v4_").unwrap_or(id);
    raw.len() == 32
        && raw.bytes().all(|c| c.is_ascii_hexdigit())
        && (!id.starts_with("v4_") || raw.bytes().all(|c| !c.is_ascii_uppercase()))
}

/// Only the server mints prefixed IDs. Missing-ticket fallback must never
/// reinterpret this namespace as a legacy start.
pub fn ticketed(id: &str) -> bool {
    id.starts_with("v4_")
}

#[derive(Clone, Debug)]
pub struct Reserve {
    pub start: Start,
    pub worker: Worker,
    pub ingest_id: String,
    pub limit: u8,
}

#[derive(Clone, Debug)]
pub enum Command {
    Issue(Start),
    Reserve(Reserve),
    Retire {
        user_id: i64,
        request_id: String,
    },
    Advance {
        user_id: i64,
        request_id: String,
        epoch: i64,
        worker: Worker,
        phase: StartPhase,
        response: Option<String>,
    },
    Renew {
        ingest_id: String,
        epoch: i64,
        revision: i64,
        worker: Worker,
    },
    Release {
        ingest_id: String,
        epoch: i64,
        worker: Worker,
    },
    ClaimCapture {
        capture: Capture,
        ingest_id: String,
        limit: u8,
        reserve: u8,
        device_id: String,
        channel_id: String,
    },
    StopCapture {
        recording_id: String,
        delete: bool,
    },
    DetachCapture {
        recording_id: String,
        epoch: i64,
        worker: Worker,
    },
    ClaimFinalizer {
        recording_id: String,
        worker: Worker,
        storage_id: String,
    },
    RenewFinalizer {
        recording_id: String,
        worker: Worker,
        epoch: i64,
    },
    ProgressCapture {
        recording_id: String,
        worker: Worker,
        epoch: i64,
        bytes: i64,
    },
    PublishCapture {
        recording_id: String,
        worker: Worker,
        epoch: i64,
        path: String,
        bytes: i64,
        gap_s: i64,
        stopped_by: Option<i64>,
    },
}

impl Command {
    pub(crate) fn lookup_key(&self) -> String {
        match self {
            Self::Issue(s) => start_key(s.user_id, &s.request_id),
            Self::Reserve(r) => start_key(r.start.user_id, &r.start.request_id),
            Self::Retire {
                user_id,
                request_id,
            }
            | Self::Advance {
                user_id,
                request_id,
                ..
            } => start_key(*user_id, request_id),
            Self::Renew { ingest_id, .. } | Self::Release { ingest_id, .. } => {
                format!("ingest:{ingest_id}")
            }
            Self::ClaimCapture { capture, .. } => format!("capture:{}", capture.recording_id),
            Self::StopCapture { recording_id, .. }
            | Self::DetachCapture { recording_id, .. }
            | Self::ClaimFinalizer { recording_id, .. }
            | Self::PublishCapture { recording_id, .. }
            | Self::ProgressCapture { recording_id, .. }
            | Self::RenewFinalizer { recording_id, .. } => format!("capture:{recording_id}"),
        }
    }
    fn admitting_worker(&self) -> Option<&Worker> {
        match self {
            Self::Reserve(r) => Some(&r.worker),
            Self::ClaimCapture { capture, .. } => Some(&capture.worker),
            Self::Advance { worker, phase, .. } if !phase.terminal() => Some(worker),
            Self::Renew { worker, .. }
            | Self::ClaimFinalizer { worker, .. }
            | Self::RenewFinalizer { worker, .. }
            | Self::ProgressCapture { worker, .. }
            | Self::PublishCapture { worker, .. } => Some(worker),
            _ => None,
        }
    }
    pub(crate) fn user_id(&self) -> i64 {
        match self {
            Self::Issue(s) => s.user_id,
            Self::Reserve(r) => r.start.user_id,
            Self::Retire { user_id, .. } | Self::Advance { user_id, .. } => *user_id,
            _ => 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
// Most successful commands return a record; keep this short-lived result allocation-free.
#[allow(clippy::large_enum_variant)]
pub enum Outcome {
    Record(Record),
    Absent,
    Capacity,
    Conflict,
    Retired,
    Expired,
    Fenced,
    Applied,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub revision: i64,
    pub generation: i64,
    pub enabled: bool,
    pub records: Vec<Record>,
    pub history_count: usize,
    pub user_history_count: usize,
    #[serde(default)]
    pub recordings: Vec<CaptureInput>,
    #[serde(default)]
    pub removed_workers: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureInput {
    pub id: String,
    pub state: String,
    pub stop: bool,
    pub capture_start: i64,
    pub capture_end: i64,
}

#[derive(Default)]
pub(crate) struct Changes {
    pub upsert: Vec<Record>,
    pub remove: Vec<String>,
    pub publish: Option<(String, String, i64, i64, Option<i64>)>,
    pub progress: Option<(String, i64)>,
    pub claim: Option<(String, String, i64, String)>,
}

#[async_trait]
pub trait LiveTvResourceStore: Send + Sync {
    async fn live_tv_resource_command(
        &self,
        command: Command,
        now_ms: i64,
    ) -> Result<Outcome, StoreError>;
    async fn live_tv_resource_lookup(
        &self,
        key: &str,
        now_ms: i64,
    ) -> Result<Option<Record>, StoreError>;
    async fn live_tv_resource_snapshot(
        &self,
        user_id: i64,
        request_id: &str,
        now_ms: i64,
    ) -> Result<Snapshot, StoreError>;
}

fn error(message: &str) -> StoreError {
    StoreError::Database(message.into())
}
fn worker_valid(worker: &Worker) -> bool {
    !worker.node_id.is_empty()
        && worker.node_id.len() <= 256
        && !worker.boot_id.is_empty()
        && worker.boot_id.len() <= 128
}
fn existing<'a>(s: &'a Snapshot, key: &str) -> Option<&'a Record> {
    s.records.iter().find(|r| r.id() == key)
}
fn ingests(s: &Snapshot, now: i64) -> impl Iterator<Item = &Ingest> {
    s.records.iter().filter_map(move |r| match r {
        Record::Ingest(i) if i.expires_at_ms > now => Some(i),
        _ => None,
    })
}
fn full(s: &Snapshot, user: i64) -> bool {
    let active = s
        .records
        .iter()
        .filter(|r| matches!(r, Record::Start(x) if !x.phase.terminal()))
        .count();
    let own = s
        .records
        .iter()
        .filter(|r| matches!(r, Record::Start(x) if x.user_id == user && !x.phase.terminal()))
        .count();
    active >= ACTIVE_MAX
        || own >= ACTIVE_USER_MAX
        || s.history_count + active >= HISTORY_MAX
        || s.user_history_count + own >= HISTORY_USER_MAX
}

/// Backend-independent decisions; the revision CAS makes the complete input
/// snapshot authoritative until all output writes commit atomically.
pub(crate) fn transition(
    s: &Snapshot,
    command: &Command,
    now: i64,
) -> Result<(Outcome, Changes), StoreError> {
    if now < 0 {
        return Err(error("invalid Live TV resource clock"));
    }
    let mut changes = Changes::default();
    if command
        .admitting_worker()
        .is_some_and(|w| s.removed_workers.contains(&w.node_id))
    {
        return Ok((Outcome::Fenced, changes));
    }
    let current = existing(s, &command.lookup_key());
    let outcome = match command {
        Command::Issue(start) => {
            if !valid_request_id(&start.request_id)
                || !ticketed(&start.request_id)
                || start.user_id <= 0
                || start.digest.len() > 128
                || start.phase != StartPhase::Issued
                || start.worker.is_some()
                || start.ingest_id.is_some()
                || start.admission_until_ms <= now
                || start.admission_until_ms > now.saturating_add(INTENT_MS)
            {
                return Err(error("invalid Live TV intent"));
            }
            if let Some(record) = current {
                return Ok((Outcome::Record(record.clone()), changes));
            }
            if full(s, start.user_id) {
                return Ok((Outcome::Capacity, changes));
            }
            let record = Record::Start(start.clone());
            changes.upsert.push(record.clone());
            Outcome::Record(record)
        }
        Command::Reserve(request) => {
            let start = &request.start;
            if !valid_request_id(&start.request_id)
                || !worker_valid(&request.worker)
                || request.ingest_id.is_empty()
                || request.ingest_id.len() > 128
                || !(1..=4).contains(&request.limit)
                || start.digest.len() > 128
            {
                return Err(error("invalid Live TV reservation"));
            }
            if !s.enabled || s.generation != start.generation {
                return Ok((Outcome::Fenced, changes));
            }
            let mut next = match current {
                Some(Record::Start(prior)) => {
                    if prior.phase == StartPhase::Retired {
                        return Ok((Outcome::Retired, changes));
                    }
                    if prior.digest != start.digest || prior.device_id != start.device_id {
                        return Ok((Outcome::Conflict, changes));
                    }
                    if prior.phase != StartPhase::Issued {
                        return Ok((Outcome::Record(Record::Start(prior.clone())), changes));
                    }
                    if prior.admission_until_ms <= now {
                        return Ok((Outcome::Expired, changes));
                    }
                    prior.clone()
                }
                Some(_) => return Ok((Outcome::Conflict, changes)),
                None if ticketed(&start.request_id) => return Ok((Outcome::Expired, changes)),
                None => {
                    if s.records.iter().any(|r| matches!(r, Record::LegacyBlock { user_id, until_ms } if *user_id==start.user_id && *until_ms>now)) {
                        return Ok((Outcome::Capacity, changes));
                    }
                    if full(s, start.user_id) {
                        return Ok((Outcome::Capacity, changes));
                    }
                    start.clone()
                }
            };
            let shared = ingests(s, now).find(|i| {
                i.device_id == start.device_id
                    && i.generation == start.generation
                    && i.channel_id == start.channel_id
            });
            let mut ingest = if let Some(i) = shared {
                if i.draining {
                    return Ok((Outcome::Conflict, changes));
                }
                i.clone()
            } else {
                if ingests(s, now)
                    .filter(|i| i.device_id == start.device_id)
                    .count()
                    >= usize::from(request.limit)
                {
                    return Ok((Outcome::Capacity, changes));
                }
                if existing(s, &format!("ingest:{}", request.ingest_id)).is_some() {
                    return Ok((Outcome::Conflict, changes));
                }
                Ingest {
                    id: request.ingest_id.clone(),
                    device_id: start.device_id.clone(),
                    channel_id: start.channel_id.clone(),
                    generation: start.generation,
                    epoch: s
                        .revision
                        .checked_add(1)
                        .ok_or_else(|| error("Live TV epoch exhausted"))?,
                    revision: 1,
                    worker: request.worker.clone(),
                    recording: false,
                    draining: false,
                    expires_at_ms: now.saturating_add(LEASE_MS),
                    consumers: BTreeMap::new(),
                }
            };
            next.phase = StartPhase::Reserved;
            next.worker = Some(ingest.worker.clone());
            next.ingest_id = Some(ingest.id.clone());
            next.epoch = ingest.epoch;
            ingest
                .consumers
                .insert(start_key(next.user_id, &next.request_id), next.epoch);
            changes.upsert.push(Record::Ingest(ingest));
            let record = Record::Start(next);
            changes.upsert.push(record.clone());
            Outcome::Record(record)
        }
        Command::Retire {
            user_id,
            request_id,
        } => {
            if *user_id <= 0 || !valid_request_id(request_id) {
                return Err(error("invalid retired Live TV intent"));
            }
            let mut start = match current {
                Some(Record::Start(prior)) => prior.clone(),
                None if ticketed(request_id) => return Ok((Outcome::Applied, changes)),
                None => {
                    if full(s, *user_id) {
                        changes.upsert.push(Record::LegacyBlock {
                            user_id: *user_id,
                            until_ms: now.saturating_add(HISTORY_MS),
                        });
                        return Ok((Outcome::Applied, changes));
                    }
                    Start {
                        user_id: *user_id,
                        request_id: request_id.clone(),
                        digest: String::new(),
                        generation: s.generation,
                        device_id: String::new(),
                        channel_id: String::new(),
                        issued_at_ms: now,
                        admission_until_ms: now,
                        epoch: 0,
                        worker: None,
                        ingest_id: None,
                        phase: StartPhase::Retired,
                        response: None,
                    }
                }
                _ => return Ok((Outcome::Conflict, changes)),
            };
            start.phase = StartPhase::Retired;
            detach_start(s, &start, &mut changes);
            changes.upsert.push(Record::Start(start));
            Outcome::Applied
        }
        Command::Advance {
            user_id: _,
            request_id: _,
            epoch,
            worker,
            phase,
            response,
        } => {
            let Some(Record::Start(start)) = current else {
                return Ok((Outcome::Absent, changes));
            };
            if start.epoch != *epoch
                || start.worker.as_ref() != Some(worker)
                || start.phase.terminal()
                || (!phase.terminal() && (!s.enabled || s.generation != start.generation))
            {
                return Ok((Outcome::Fenced, changes));
            }
            if !phase.terminal()
                && start.ingest_id.as_ref().is_none_or(|id| {
                    !ingests(s, now).any(|i| i.id == *id && i.worker == *worker && !i.draining)
                })
            {
                return Ok((Outcome::Fenced, changes));
            }
            let allowed = matches!(
                (start.phase, phase),
                (StartPhase::Reserved, StartPhase::Opening)
                    | (StartPhase::Opening, StartPhase::Active)
                    | (
                        StartPhase::Reserved | StartPhase::Opening | StartPhase::Active,
                        StartPhase::Draining
                    )
            ) || phase.terminal();
            if !allowed || response.as_ref().is_some_and(|v| v.len() > 65_536) {
                return Ok((Outcome::Conflict, changes));
            }
            let mut next = start.clone();
            next.phase = *phase;
            if phase.terminal() {
                detach_start(s, start, &mut changes);
            }
            if let Some(response) = response {
                next.response = Some(response.clone());
            }
            changes.upsert.push(Record::Start(next.clone()));
            Outcome::Record(Record::Start(next))
        }
        Command::Renew {
            ingest_id: _,
            epoch,
            revision,
            worker,
        } => {
            let Some(Record::Ingest(ingest)) = current else {
                return Ok((Outcome::Absent, changes));
            };
            if ingest.epoch != *epoch
                || ingest.revision != *revision
                || ingest.worker != *worker
                || ingest.draining
                || ingest.expires_at_ms <= now
                || !s.enabled
                || s.generation != ingest.generation
            {
                return Ok((Outcome::Fenced, changes));
            }
            let mut next = ingest.clone();
            next.revision = next
                .revision
                .checked_add(1)
                .ok_or_else(|| error("Live TV revision exhausted"))?;
            next.expires_at_ms = now.saturating_add(LEASE_MS);
            for record in &s.records {
                if let Record::Capture(c) = record {
                    if c.ingest_id == ingest.id
                        && c.ingest_epoch == ingest.epoch
                        && c.finalizer.is_none()
                    {
                        let key = format!("capture:{}", c.recording_id);
                        let mut c = c.clone();
                        if c.stopped
                            || c.deleted
                            || c.expires_at_ms <= now
                            || s.recordings.iter().any(|r| {
                                r.id == c.recording_id
                                    && (r.stop
                                        || !matches!(r.state.as_str(), "scheduled" | "recording"))
                            })
                        {
                            c.stopped = true;
                            c.expires_at_ms = now;
                            next.consumers.remove(&key);
                        } else if next.consumers.contains_key(&key) {
                            c.expires_at_ms = next.expires_at_ms;
                        }
                        changes.upsert.push(Record::Capture(c));
                    }
                }
            }
            next.recording = next.consumers.keys().any(|key| key.starts_with("capture:"));
            next.draining = next.consumers.is_empty();
            changes.upsert.push(Record::Ingest(next.clone()));
            Outcome::Record(Record::Ingest(next))
        }
        Command::Release {
            ingest_id,
            epoch,
            worker,
        } => {
            let Some(Record::Ingest(i)) = current else {
                return Ok((Outcome::Absent, changes));
            };
            if i.epoch != *epoch || i.worker != *worker {
                return Ok((Outcome::Fenced, changes));
            }
            // Only the exact worker may report its HTTP body closed. Pending
            // consumers cannot keep a closed socket alive; they recover from
            // this terminal result instead of attaching after closure.
            for record in &s.records {
                match record {
                    Record::Start(start)
                        if start.ingest_id.as_deref() == Some(ingest_id)
                            && !start.phase.terminal() =>
                    {
                        let mut next = start.clone();
                        next.phase = StartPhase::Closed;
                        changes.upsert.push(Record::Start(next));
                    }
                    Record::Capture(c)
                        if c.ingest_id == *ingest_id
                            && c.ingest_epoch == *epoch
                            && c.finalizer.is_none() =>
                    {
                        let mut next = c.clone();
                        next.expires_at_ms = now;
                        changes.upsert.push(Record::Capture(next));
                    }
                    _ => {}
                }
            }
            changes.remove.push(format!("ingest:{ingest_id}"));
            Outcome::Applied
        }
        Command::ClaimCapture {
            capture,
            ingest_id,
            limit,
            reserve,
            device_id,
            channel_id,
        } => {
            if !worker_valid(&capture.worker)
                || capture.recording_id.is_empty()
                || capture.storage_id.is_empty()
                || !(1..=4).contains(limit)
            {
                return Err(error("invalid capture claim"));
            }
            if !s.enabled || s.generation != capture.generation {
                return Ok((Outcome::Fenced, changes));
            }
            let Some(row) = s.recordings.iter().find(|r| r.id == capture.recording_id) else {
                return Ok((Outcome::Absent, changes));
            };
            if row.stop || !matches!(row.state.as_str(), "scheduled" | "recording") {
                return Ok((Outcome::Retired, changes));
            }
            if row.capture_start > now / 1000 || row.capture_end <= now / 1000 {
                return Ok((Outcome::Expired, changes));
            }
            let epoch = match current {
                Some(Record::Capture(c)) if c.stopped || c.deleted => {
                    return Ok((Outcome::Retired, changes))
                }
                Some(Record::Capture(c))
                    if c.expires_at_ms > now && c.generation == capture.generation =>
                {
                    return Ok((Outcome::Record(Record::Capture(c.clone())), changes))
                }
                Some(Record::Capture(c)) if c.storage_id != capture.storage_id => {
                    return Ok((Outcome::Conflict, changes));
                }
                Some(Record::Capture(c)) => c
                    .epoch
                    .checked_add(1)
                    .ok_or_else(|| error("capture epoch exhausted"))?,
                None => 1,
                _ => return Ok((Outcome::Conflict, changes)),
            };
            let candidates = ingests(s, now)
                .filter(|i| i.device_id == *device_id)
                .collect::<Vec<_>>();
            let shared = candidates
                .iter()
                .find(|i| i.generation == capture.generation && i.channel_id == *channel_id);
            let mut ingest = if let Some(i) = shared {
                if i.draining || i.worker != capture.worker {
                    return Ok((Outcome::Conflict, changes));
                }
                (*i).clone()
            } else {
                if candidates.len() >= usize::from(*limit)
                    || candidates.iter().filter(|i| i.recording).count()
                        >= usize::from(limit.saturating_sub(*reserve))
                {
                    return Ok((Outcome::Capacity, changes));
                }
                Ingest {
                    id: ingest_id.clone(),
                    device_id: device_id.clone(),
                    channel_id: channel_id.clone(),
                    generation: capture.generation,
                    epoch,
                    revision: 1,
                    worker: capture.worker.clone(),
                    recording: true,
                    draining: false,
                    expires_at_ms: now.saturating_add(LEASE_MS),
                    consumers: BTreeMap::new(),
                }
            };
            if !ingest.recording
                && candidates.iter().filter(|i| i.recording).count()
                    >= usize::from(limit.saturating_sub(*reserve))
            {
                return Ok((Outcome::Capacity, changes));
            }
            ingest.recording = true;
            let mut next = capture.clone();
            next.epoch = epoch;
            next.ingest_id = ingest.id.clone();
            next.ingest_epoch = ingest.epoch;
            next.expires_at_ms = ingest.expires_at_ms;
            next.stopped = false;
            next.deleted = false;
            next.finalizer = None;
            next.finalizer_epoch = 0;
            next.published_path = None;
            ingest
                .consumers
                .insert(format!("capture:{}", capture.recording_id), epoch);
            changes.upsert.push(Record::Ingest(ingest));
            changes.claim = Some((
                next.recording_id.clone(),
                next.worker.node_id.clone(),
                next.epoch,
                format!("{}.ts", next.base_path),
            ));
            changes.upsert.push(Record::Capture(next.clone()));
            Outcome::Record(Record::Capture(next))
        }
        Command::StopCapture {
            recording_id: _,
            delete,
        } => {
            let Some(Record::Capture(c)) = current else {
                return Ok((Outcome::Absent, changes));
            };
            let mut next = c.clone();
            next.stopped = true;
            next.deleted |= *delete;
            changes.upsert.push(Record::Capture(next));
            Outcome::Applied
        }
        Command::DetachCapture {
            recording_id,
            epoch,
            worker,
        } => {
            let Some(Record::Capture(c)) = current else {
                return Ok((Outcome::Absent, changes));
            };
            if c.epoch != *epoch || c.worker != *worker {
                return Ok((Outcome::Fenced, changes));
            }
            if let Some(Record::Ingest(i)) = existing(s, &format!("ingest:{}", c.ingest_id)) {
                if i.epoch == c.ingest_epoch {
                    let mut next = i.clone();
                    next.consumers.remove(&format!("capture:{recording_id}"));
                    next.recording = next.consumers.keys().any(|key| key.starts_with("capture:"));
                    if next.consumers.is_empty() {
                        next.draining = true;
                    }
                    changes.upsert.push(Record::Ingest(next));
                }
            }
            let mut next = c.clone();
            next.expires_at_ms = now;
            changes.upsert.push(Record::Capture(next));
            Outcome::Applied
        }
        Command::ClaimFinalizer {
            recording_id: _,
            worker,
            storage_id,
        } => {
            if !worker_valid(worker) {
                return Err(error("invalid capture finalizer"));
            }
            let Some(Record::Capture(c)) = current else {
                return Ok((Outcome::Absent, changes));
            };
            if c.storage_id != *storage_id {
                return Ok((Outcome::Conflict, changes));
            }
            if c.deleted
                || s.recordings
                    .iter()
                    .any(|r| r.id == c.recording_id && r.state != "recording")
            {
                return Ok((Outcome::Retired, changes));
            }
            if c.expires_at_ms > now {
                return Ok((Outcome::Conflict, changes));
            }
            let mut next = c.clone();
            next.finalizer = Some(worker.clone());
            next.expires_at_ms = now.saturating_add(LEASE_MS);
            next.finalizer_epoch = next
                .finalizer_epoch
                .checked_add(1)
                .ok_or_else(|| error("finalizer epoch exhausted"))?;
            changes.upsert.push(Record::Capture(next.clone()));
            Outcome::Record(Record::Capture(next))
        }
        Command::RenewFinalizer {
            recording_id,
            worker,
            epoch,
        } => {
            let Some(Record::Capture(c)) = current else {
                return Ok((Outcome::Absent, changes));
            };
            if c.deleted
                || c.finalizer.as_ref() != Some(worker)
                || c.finalizer_epoch != *epoch
                || c.expires_at_ms <= now
                || !s
                    .recordings
                    .iter()
                    .any(|r| r.id == *recording_id && r.state == "recording")
            {
                return Ok((Outcome::Fenced, changes));
            }
            let mut next = c.clone();
            next.expires_at_ms = now.saturating_add(LEASE_MS);
            changes.upsert.push(Record::Capture(next.clone()));
            Outcome::Record(Record::Capture(next))
        }
        Command::ProgressCapture {
            recording_id,
            worker,
            epoch,
            bytes,
        } => {
            let Some(Record::Capture(c)) = current else {
                return Ok((Outcome::Absent, changes));
            };
            if c.worker != *worker
                || c.epoch != *epoch
                || c.expires_at_ms <= now
                || c.stopped
                || c.deleted
                || c.finalizer.is_some()
                || *bytes < 0
                || !s.enabled
                || s.generation != c.generation
            {
                return Ok((Outcome::Fenced, changes));
            }
            changes.progress = Some((recording_id.clone(), *bytes));
            Outcome::Applied
        }
        Command::PublishCapture {
            recording_id,
            worker,
            epoch,
            path,
            bytes,
            gap_s,
            stopped_by,
        } => {
            let Some(Record::Capture(c)) = current else {
                return Ok((Outcome::Absent, changes));
            };
            if c.deleted
                || c.finalizer.as_ref() != Some(worker)
                || c.finalizer_epoch != *epoch
                || c.expires_at_ms <= now
                || *bytes < 0
                || *gap_s < 0
                || path.len() > 8192
                || !s
                    .recordings
                    .iter()
                    .any(|r| r.id == *recording_id && r.state == "recording")
            {
                return Ok((Outcome::Fenced, changes));
            }
            let mut next = c.clone();
            next.published_path = Some(path.clone());
            next.expires_at_ms = now;
            next.stopped = true;
            changes.upsert.push(Record::Capture(next));
            changes.publish = Some((
                recording_id.clone(),
                path.clone(),
                *bytes,
                *gap_s,
                *stopped_by,
            ));
            Outcome::Applied
        }
    };
    Ok((outcome, changes))
}

/// One consistent SQL snapshot: active rows and the requested terminal row,
/// plus history counts. The 24-hour history is not downloaded on heartbeats.
pub(crate) const SNAPSHOT_SQL: &str = "WITH input(user_id, request_key, now_ms) AS (VALUES($1,$2,$3))
SELECT json_object(
 'revision', revision,
 'generation', COALESCE((SELECT CAST(value AS INTEGER) FROM settings WHERE key='live_tv.config_generation'),0),
 'enabled', json(CASE WHEN (SELECT value FROM settings WHERE key='live_tv.enabled')='1' THEN 'true' ELSE 'false' END),
 'removed_workers', json((SELECT COALESCE(json_group_array(substr(key,length('internal.cluster_job_owner_removed.')+1)),'[]') FROM settings WHERE key GLOB 'internal.cluster_job_owner_removed.*')),
 'records', json((SELECT COALESCE(json_group_array(json(body)),'[]') FROM live_tv_resource_records, input
    WHERE live=1 OR (id=input.request_key AND expires_at_ms>input.now_ms))),
 'history_count', (SELECT COUNT(*) FROM live_tv_resource_records,input WHERE kind='start' AND live=0 AND expires_at_ms>input.now_ms),
 'user_history_count', (SELECT COUNT(*) FROM live_tv_resource_records,input WHERE kind='start' AND live=0 AND live_tv_resource_records.user_id=input.user_id AND expires_at_ms>input.now_ms),
 'recordings', json((SELECT COALESCE(json_group_array(json_object('id',id,'state',state,
    'stop',json(CASE WHEN stop_requested_at_ms IS NULL THEN 'false' ELSE 'true' END),
    'capture_start',capture_start,'capture_end',capture_end)),'[]') FROM dvr_recordings
    WHERE id=substr((SELECT request_key FROM input),9) OR id IN
        (SELECT substr(id,9) FROM live_tv_resource_records WHERE kind='capture' AND live=1)))
) AS payload FROM live_tv_resource_revision WHERE singleton=1";

pub(crate) enum Value {
    Text(String),
    Integer(i64),
}
pub(crate) struct Statement {
    pub sql: String,
    pub values: Vec<Value>,
}

#[async_trait]
pub(crate) trait Backend: Send + Sync {
    async fn read_ledger(&self, user_id: i64, key: &str, now: i64) -> Result<Snapshot, StoreError>;
    async fn commit_ledger(&self, statements: Vec<Statement>) -> Result<bool, StoreError>;
}

#[async_trait]
impl<T: Backend> LiveTvResourceStore for T {
    async fn live_tv_resource_lookup(
        &self,
        key: &str,
        now_ms: i64,
    ) -> Result<Option<Record>, StoreError> {
        Ok(self
            .read_ledger(0, key, now_ms)
            .await?
            .records
            .into_iter()
            .find(|r| r.id() == key))
    }
    async fn live_tv_resource_snapshot(
        &self,
        user_id: i64,
        request_id: &str,
        now_ms: i64,
    ) -> Result<Snapshot, StoreError> {
        self.read_ledger(user_id, &start_key(user_id, request_id), now_ms)
            .await
    }
    async fn live_tv_resource_command(
        &self,
        command: Command,
        now_ms: i64,
    ) -> Result<Outcome, StoreError> {
        for _ in 0..16 {
            let mut snapshot = self
                .read_ledger(command.user_id(), &command.lookup_key(), now_ms)
                .await?;
            let mut changes = expire(&mut snapshot, command.user_id(), now_ms);
            let (outcome, mut mutation) = transition(&snapshot, &command, now_ms)?;
            // The operation's later mutation takes precedence over expiry.
            changes
                .upsert
                .retain(|r| !mutation.upsert.iter().any(|next| next.id() == r.id()));
            changes.upsert.append(&mut mutation.upsert);
            changes.remove.append(&mut mutation.remove);
            changes.publish = mutation.publish;
            changes.progress = mutation.progress;
            changes.claim = mutation.claim;
            if changes.upsert.is_empty() && changes.remove.is_empty() && changes.progress.is_none()
            {
                return Ok(outcome);
            }
            let statements = statements(&snapshot, changes, now_ms, command.admitting_worker())?;
            if self.commit_ledger(statements).await? {
                return Ok(outcome);
            }
        }
        Err(error(
            "Live TV resource changed repeatedly; retry the same operation",
        ))
    }
}

fn expire(snapshot: &mut Snapshot, user_id: i64, now: i64) -> Changes {
    let expired_ingests = snapshot
        .records
        .iter()
        .filter_map(|r| match r {
            Record::Ingest(i) if i.expires_at_ms <= now => Some(i.id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let mut changes = Changes::default();
    for record in &mut snapshot.records {
        if let Record::Start(s) = record {
            if !s.phase.terminal()
                && ((s.phase == StartPhase::Issued && s.admission_until_ms <= now)
                    || s.ingest_id
                        .as_ref()
                        .is_some_and(|id| expired_ingests.contains(id)))
            {
                s.phase = StartPhase::Expired;
                snapshot.history_count += 1;
                if s.user_id == user_id {
                    snapshot.user_history_count += 1;
                }
                changes.upsert.push(record.clone());
            }
        }
    }
    snapshot.records.retain(|r| {
        let remove = matches!(r, Record::Ingest(i) if i.expires_at_ms <= now)
            || matches!(r, Record::LegacyBlock {until_ms,..} if *until_ms <= now);
        if remove {
            changes.remove.push(r.id());
        }
        !remove
    });
    for record in &snapshot.records {
        if matches!(record, Record::Capture(c) if c.expires_at_ms <= now) {
            changes.upsert.push(record.clone());
        }
    }
    changes
}

fn detach_start(s: &Snapshot, start: &Start, changes: &mut Changes) {
    if let Some(id) = &start.ingest_id {
        if let Some(Record::Ingest(i)) = existing(s, &format!("ingest:{id}")) {
            let mut next = i.clone();
            next.consumers
                .remove(&start_key(start.user_id, &start.request_id));
            next.draining = next.consumers.is_empty();
            changes.upsert.push(Record::Ingest(next));
        }
    }
}

fn statements(
    snapshot: &Snapshot,
    changes: Changes,
    now: i64,
    worker: Option<&Worker>,
) -> Result<Vec<Statement>, StoreError> {
    use Value::{Integer as I, Text as T};
    let next = snapshot
        .revision
        .checked_add(1)
        .ok_or_else(|| error("Live TV ledger exhausted"))?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let mut out=vec![Statement {sql:"UPDATE live_tv_resource_revision SET nonce=$1,revision=revision+1
        WHERE singleton=1 AND revision=$2
        AND COALESCE((SELECT CAST(value AS INTEGER) FROM settings WHERE key='live_tv.config_generation'),0)=$3
        AND COALESCE((SELECT value FROM settings WHERE key='live_tv.enabled'),'0')=$4
        AND NOT EXISTS (SELECT 1 FROM settings WHERE key=$5)".into(),
        values:vec![T(nonce.clone()),I(snapshot.revision),I(snapshot.generation),T(if snapshot.enabled{"1"}else{"0"}.into()),T(worker.map(|w|format!("internal.cluster_job_owner_removed.{}",w.node_id)).unwrap_or_default())]}];
    for record in changes.upsert {
        let (kind, user, live, expiry) = record.columns(now);
        out.push(Statement {sql:"INSERT INTO live_tv_resource_records(id,kind,user_id,live,expires_at_ms,body)
            SELECT $1,$2,$3,$4,$5,$6 FROM live_tv_resource_revision WHERE singleton=1 AND revision=$7 AND nonce=$8
            ON CONFLICT(id) DO UPDATE SET kind=excluded.kind,user_id=excluded.user_id,live=excluded.live,
                expires_at_ms=excluded.expires_at_ms,body=excluded.body".into(),
            values:vec![T(record.id()),T(kind.into()),I(user),I(i64::from(live)),I(expiry),
                T(serde_json::to_string(&record).map_err(|_|error("serializing Live TV authority"))?),I(next),T(nonce.clone())]});
    }
    for id in changes.remove {
        out.push(Statement {sql:"DELETE FROM live_tv_resource_records WHERE id=$1 AND EXISTS
            (SELECT 1 FROM live_tv_resource_revision WHERE singleton=1 AND revision=$2 AND nonce=$3)".into(),
            values:vec![T(id),I(next),T(nonce.clone())]});
    }
    // Bounded historical GC. Unknown ticketed IDs never enter legacy admission.
    out.push(Statement {sql:"DELETE FROM live_tv_resource_records WHERE id IN
        (SELECT id FROM live_tv_resource_records WHERE live=0 AND expires_at_ms<=$1 ORDER BY expires_at_ms LIMIT 64)
        AND EXISTS (SELECT 1 FROM live_tv_resource_revision WHERE singleton=1 AND revision=$2 AND nonce=$3)".into(),
        values:vec![I(now),I(next),T(nonce.clone())]});
    if let Some((id, worker, epoch, path)) = changes.claim {
        out.push(Statement { sql: "UPDATE dvr_recordings SET
            gap_s=gap_s+CASE WHEN state='recording' THEN MAX(0,$1/1000-COALESCE(last_progress_ms,started_at_ms,$1)/1000) ELSE 0 END,
            state='recording',attempt=$2,tuner_owner_node_id=$3,path=$4,
            started_at_ms=COALESCE(started_at_ms,$1),updated_at_ms=$1,
            late_start_s=CASE WHEN started_at_ms IS NULL THEN MAX(0,$1/1000-capture_start) ELSE late_start_s END
            WHERE id=$5 AND state IN ('scheduled','recording') AND stop_requested_at_ms IS NULL AND EXISTS
            (SELECT 1 FROM live_tv_resource_revision WHERE singleton=1 AND revision=$6 AND nonce=$7)".into(),
            values: vec![I(now),I(epoch),T(worker),T(path),T(id),I(next),T(nonce.clone())] });
    }
    if let Some((id, bytes)) = changes.progress {
        out.push(Statement { sql: "UPDATE dvr_recordings SET bytes=MAX(bytes,$1),last_progress_ms=$2
            WHERE id=$3 AND state='recording' AND stop_requested_at_ms IS NULL AND EXISTS
            (SELECT 1 FROM live_tv_resource_revision WHERE singleton=1 AND revision=$4 AND nonce=$5)".into(),
            values: vec![I(bytes),I(now),T(id),I(next),T(nonce.clone())] });
    }
    if let Some((id, path, bytes, gap, stopped_by)) = changes.publish {
        out.push(Statement { sql: "UPDATE dvr_recordings SET state=CASE WHEN $1=0 THEN 'failed'
            WHEN $2>0 OR late_start_s>0 THEN 'partial' ELSE 'done' END,
            bytes=$1,gap_s=$2,path=$3,finished_at_ms=$4,updated_at_ms=$4,
            stopped_by_user_id=NULLIF($5,0) WHERE id=$6 AND state='recording' AND EXISTS
            (SELECT 1 FROM live_tv_resource_revision WHERE singleton=1 AND revision=$7 AND nonce=$8)".into(),
            values: vec![I(bytes),I(gap),T(path),I(now),I(stopped_by.unwrap_or(0)),T(id),I(next),T(nonce.clone())] });
    }
    Ok(out)
}
