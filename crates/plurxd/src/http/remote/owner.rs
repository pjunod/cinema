//! Node-local receiver owner. No ownership transfer and no replicated keypress writes.
use super::{fail, now_seconds, secret, wire::*};
use crate::{
    http::{error::ApiError, extract::authenticate_token_digest},
    state::AppState,
};
use hmac::{Hmac, Mac};
use plurx_core::{
    auth,
    remote_control::{Acknowledgement, Command, Outcome, MAX_SAFE_INTEGER},
    store::{
        remote::{NewRemoteGrant, NewRemoteReceiver, RemoteGrant, RemoteProof, RemoteReceiver},
        TokenAudience,
    },
};
use serde_json::{json, Value};
use sha2::Sha256;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::{Mutex, MutexGuard},
    time::Duration,
};
use tokio::{
    sync::{Notify, Semaphore},
    time::Instant,
};
use uuid::Uuid;

pub(crate) const FEATURE_KEY: &str = "cinema.remote_control";
pub(crate) struct Hub {
    sessions: Mutex<HashMap<Uuid, Session>>,
    pub changed: Notify,
    approvals: std::sync::Arc<Semaphore>,
    generation: std::sync::atomic::AtomicU64,
    #[cfg(test)]
    pub(crate) approve_pause: TestPause,
    #[cfg(test)]
    pub(crate) delivery_pause: TestPause,
}
impl Default for Hub {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            changed: Notify::new(),
            approvals: std::sync::Arc::new(Semaphore::new(16)),
            generation: std::sync::atomic::AtomicU64::new(0),
            #[cfg(test)]
            approve_pause: TestPause::default(),
            #[cfg(test)]
            delivery_pause: TestPause::default(),
        }
    }
}
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestPause {
    pub(crate) enabled: std::sync::atomic::AtomicBool,
    pub(crate) arrived: Notify,
    pub(crate) resume: Notify,
}
#[cfg(test)]
impl TestPause {
    async fn wait(&self) {
        if self
            .enabled
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            self.arrived.notify_one();
            self.resume.notified().await;
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum PairPhase {
    Pending,
    Approving,
    Approved,
    Denied,
}
struct PollPermit<'a> {
    hub: &'a Hub,
    target: plurx_core::remote_control::Target,
    grant: Option<Uuid>,
}
impl Drop for PollPermit<'_> {
    fn drop(&mut self) {
        if let Ok(mut sessions) = self.hub.sessions.lock() {
            if let Some(s) = sessions
                .get_mut(&self.target.session_id)
                .filter(|s| s.target == self.target)
            {
                if let Some(grant) = self.grant {
                    s.state_polls.remove(&grant);
                } else {
                    s.receiver_poll = false;
                }
            }
        }
    }
}
#[derive(Clone)]
struct Queued {
    command: Command,
    token_digest: String,
    grant_hash: String,
    at: Instant,
}
struct Lease {
    wire: Control,
    token_digest: String,
    grant_hash: String,
    until: Instant,
    last_sequence: u64,
}
struct Challenge {
    id: Uuid,
    key: String,
    code_hash: Vec<u8>,
    until: Instant,
    failures: u8,
}
struct Pending {
    id: Uuid,
    name: String,
    token_digest: String,
    poll_hash: String,
    until: Instant,
    phase: PairPhase,
    grant: Option<(Uuid, String)>,
}
struct Session {
    target: plurx_core::remote_control::Target,
    receiver: RemoteReceiver,
    foreground_id: Uuid,
    token_digest: String,
    receiver_hash: String,
    last_seen: Instant,
    revision: u64,
    lease: Option<Lease>,
    state: Option<ReceiverState>,
    queue: VecDeque<Queued>,
    batch: Vec<Queued>,
    delivery_id: u64,
    receiver_poll: bool,
    state_polls: HashSet<Uuid>,
    results: VecDeque<(Acknowledgement, Instant)>,
    challenge: Option<Challenge>,
    pending: Vec<Pending>,
}
fn bump(s: &mut Session) -> Result<(), ApiError> {
    s.revision = s
        .revision
        .checked_add(1)
        .filter(|v| *v <= MAX_SAFE_INTEGER)
        .ok_or_else(|| fail(503, "unavailable"))?;
    Ok(())
}
fn record_owner_outcome(s: &mut Session, command: &Command, outcome: Outcome) {
    if s.results.len() == 64 {
        s.results.pop_front();
    }
    s.results.push_back((
        Acknowledgement {
            control_epoch: command.control_epoch,
            sequence: command.sequence,
            outcome,
        },
        Instant::now(),
    ));
}
fn clear_control(s: &mut Session) -> Result<(), ApiError> {
    s.lease = None;
    // Unsent commands have a known failure. A delivered batch may already
    // have acted, so clearing it never fabricates a failure acknowledgement.
    let queued = s.queue.drain(..).map(|q| q.command).collect::<Vec<_>>();
    for command in queued {
        record_owner_outcome(s, &command, Outcome::StaleControl);
    }
    s.batch.clear();
    if let Some(state) = &mut s.state {
        state.credits.clear();
    }
    bump(s)
}
/// Native/web invitation URL: identity is query metadata, code is fragment.
pub(super) fn pairing_payload(
    instance: &str,
    target: &plurx_core::remote_control::Target,
    challenge: Uuid,
    code: &str,
) -> Option<String> {
    if !label(instance, 128)
        || !label(&target.owner_node_id, 128)
        || code.len() != 8
        || !code.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let mut url = reqwest::Url::parse("cinema-remote://pair").ok()?;
    url.query_pairs_mut()
        .append_pair("server_instance_id", instance)
        .append_pair("owner_node_id", &target.owner_node_id)
        .append_pair("session_id", &target.session_id.to_string())
        .append_pair("receiver_epoch", &target.receiver_epoch.to_string())
        .append_pair("challenge_id", &challenge.to_string());
    url.set_fragment(Some(&format!("code={code}")));
    Some(url.to_string())
}
pub(super) fn qr_modules(payload: &str) -> Option<Vec<String>> {
    if payload.len() > 2048 {
        return None;
    }
    let code = qrcode::QrCode::new(payload.as_bytes()).ok()?;
    let width = code.width();
    if !(21..=177).contains(&width) {
        return None;
    }
    Some(
        (0..width)
            .map(|y| {
                (0..width)
                    .map(|x| {
                        if code[(x, y)] == qrcode::Color::Dark {
                            '1'
                        } else {
                            '0'
                        }
                    })
                    .collect()
            })
            .collect(),
    )
}
fn pairing_hash(key: &str, code: &str) -> Result<Vec<u8>, ApiError> {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key.as_bytes()).map_err(|_| fail(503, "unavailable"))?;
    mac.update(code.as_bytes());
    Ok(mac.finalize().into_bytes().to_vec())
}
impl Hub {
    #[cfg(test)]
    pub(crate) fn expire_pending_for_test(&self, session: Uuid) {
        if let Ok(mut sessions) = self.sessions.lock() {
            if let Some(s) = sessions.get_mut(&session) {
                for p in &mut s.pending {
                    p.until = Instant::now();
                }
            }
        }
        self.expire().expect("fixture expiry");
    }
    #[cfg(test)]
    pub(crate) fn approvals_finished(&self) -> bool {
        self.approvals.available_permits() == 16
    }
    fn lock(&self) -> Result<MutexGuard<'_, HashMap<Uuid, Session>>, ApiError> {
        self.sessions.lock().map_err(|_| fail(503, "unavailable"))
    }
    pub fn disable(&self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            self.generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            sessions.clear();
        }
        self.changed.notify_waiters();
    }
    pub(crate) fn revoke_installation(&self, user: i64, receiver: &str) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.retain(|_, s| !(s.receiver.user_id == user && s.receiver.id == receiver));
        }
        self.changed.notify_waiters();
    }
    pub(crate) fn revoke_grant(&self, user: i64, grant: &str) -> Result<(), ApiError> {
        let mut sessions = self.lock()?;
        for s in sessions.values_mut().filter(|s| s.receiver.user_id == user) {
            if s.lease
                .as_ref()
                .is_some_and(|l| l.wire.active_grant_id.to_string() == grant)
            {
                clear_control(s)?;
            }
        }
        self.changed.notify_waiters();
        Ok(())
    }
    fn expire(&self) -> Result<(), ApiError> {
        let now = Instant::now();
        let mut sessions = self.lock()?;
        sessions.retain(|_, s| now.duration_since(s.last_seen) < Duration::from_secs(15));
        for s in sessions.values_mut() {
            if s.lease.as_ref().is_some_and(|l| now >= l.until) {
                clear_control(s)?;
                self.changed.notify_waiters();
            }
            let mut expired = Vec::new();
            s.queue.retain(|q| {
                if now.duration_since(q.at) < Duration::from_millis(500) {
                    true
                } else {
                    expired.push(q.command.clone());
                    false
                }
            });
            if !expired.is_empty() {
                for command in expired {
                    record_owner_outcome(s, &command, Outcome::Expired);
                }
                bump(s)?;
                self.changed.notify_waiters();
            }
            s.batch
                .retain(|q| now.duration_since(q.at) < Duration::from_millis(500));
            s.results
                .retain(|(_, at)| now.duration_since(*at) < Duration::from_secs(10));
            s.pending.retain(|p| now < p.until);
            if s.challenge.as_ref().is_some_and(|c| now >= c.until) {
                s.challenge = None;
            }
        }
        Ok(())
    }
    fn session<'a>(
        sessions: &'a mut HashMap<Uuid, Session>,
        target: &plurx_core::remote_control::Target,
    ) -> Result<&'a mut Session, ApiError> {
        sessions
            .get_mut(&target.session_id)
            .filter(|s| s.target == *target)
            .ok_or_else(|| fail(404, "stale_target"))
    }
    async fn authorize_session(
        &self,
        state: &AppState,
        dispatch: &Dispatch,
    ) -> Result<RemoteReceiver, ApiError> {
        let target = dispatch
            .request
            .target()
            .ok_or_else(|| fail(400, "invalid"))?;
        let (receiver, receiver_digest, receiver_hash) = {
            let mut sessions = self.lock()?;
            let s = Self::session(&mut sessions, target)?;
            (
                s.receiver.clone(),
                s.token_digest.clone(),
                s.receiver_hash.clone(),
            )
        };
        if receiver.user_id != dispatch.user_id {
            return Err(fail(403, "unauthorized"));
        }
        let user = authenticate_token_digest(state, receiver_digest.clone(), TokenAudience::Native)
            .await?;
        if user.id != dispatch.user_id {
            return Err(fail(403, "unauthorized"));
        }
        state
            .store
            .remote_authority(
                &receiver.id,
                user.id,
                RemoteProof::Receiver {
                    secret_hash: receiver_hash,
                },
            )
            .await?
            .ok_or_else(|| fail(403, "unauthorized"))?;
        if dispatch.request.receiver_side() {
            if receiver_digest != dispatch.token_digest {
                return Err(fail(403, "unauthorized"));
            }
            let hash = dispatch
                .proof
                .receiver_hash
                .clone()
                .ok_or_else(|| fail(401, "unauthorized"))?;
            state
                .store
                .remote_authority(
                    &receiver.id,
                    dispatch.user_id,
                    RemoteProof::Receiver { secret_hash: hash },
                )
                .await?
                .ok_or_else(|| fail(403, "unauthorized"))?;
        } else if let Some(grant) = dispatch.request.grant_id() {
            let hash = dispatch
                .proof
                .grant_hash
                .clone()
                .ok_or_else(|| fail(401, "unauthorized"))?;
            state
                .store
                .remote_authority(
                    &receiver.id,
                    dispatch.user_id,
                    RemoteProof::Grant {
                        grant_id: grant.to_string(),
                        secret_hash: hash,
                    },
                )
                .await?
                .ok_or_else(|| fail(403, "unauthorized"))?;
        }
        Ok(receiver)
    }
    async fn refresh_authority(
        &self,
        state: &AppState,
        target: &plurx_core::remote_control::Target,
    ) -> Result<(), ApiError> {
        let lease = {
            let mut sessions = self.lock()?;
            let s = Self::session(&mut sessions, target)?;
            s.lease.as_ref().map(|l| {
                (
                    s.receiver.id.clone(),
                    s.receiver.user_id,
                    l.wire.control_epoch,
                    l.wire.active_grant_id,
                    l.token_digest.clone(),
                    l.grant_hash.clone(),
                )
            })
        };
        if let Some((rid, user, epoch, gid, digest, hash)) = lease {
            let login = authenticate_token_digest(state, digest, TokenAudience::Native).await;
            let valid = match login {
                Ok(u) if u.id == user => state
                    .store
                    .remote_authority(
                        &rid,
                        user,
                        RemoteProof::Grant {
                            grant_id: gid.to_string(),
                            secret_hash: hash,
                        },
                    )
                    .await?
                    .is_some(),
                _ => false,
            };
            if !valid {
                let mut sessions = self.lock()?;
                let s = Self::session(&mut sessions, target)?;
                if s.lease
                    .as_ref()
                    .is_some_and(|l| l.wire.control_epoch == epoch)
                {
                    clear_control(s)?;
                    self.changed.notify_waiters();
                }
            }
        }
        Ok(())
    }
    fn poll_permit(&self, d: &Dispatch) -> Result<Option<PollPermit<'_>>, ApiError> {
        let grant = match &d.request {
            Request::Poll(_) => None,
            Request::ReadState(r) => Some(r.grant_id),
            _ => return Ok(None),
        };
        let target = d.request.target().ok_or_else(|| fail(400, "invalid"))?;
        let mut sessions = self.lock()?;
        let s = Self::session(&mut sessions, target)?;
        if s.receiver.user_id != d.user_id {
            return Err(fail(403, "unauthorized"));
        }
        if let Some(grant) = grant {
            if s.state_polls.len() >= 8 || !s.state_polls.insert(grant) {
                return Err(fail(429, "busy"));
            }
        } else {
            if s.receiver_poll {
                return Err(fail(429, "busy"));
            }
            s.receiver_poll = true;
        }
        Ok(Some(PollPermit {
            hub: self,
            target: target.clone(),
            grant,
        }))
    }
    async fn finish_approval(
        &self,
        state: &AppState,
        target: plurx_core::remote_control::Target,
        pending_id: Uuid,
        user_id: i64,
        claimant: (String, String, String),
    ) {
        let grant_id = Uuid::new_v4();
        let result = async {
            let user =
                authenticate_token_digest(state, claimant.1.clone(), TokenAudience::Native).await?;
            if user.id != user_id {
                return Err(fail(403, "unauthorized"));
            }
            let (raw, hash) = secret()?;
            #[cfg(test)]
            self.approve_pause.wait().await;
            if !state
                .store
                .create_remote_grant(NewRemoteGrant {
                    grant: RemoteGrant {
                        id: grant_id.to_string(),
                        receiver_id: claimant.2.clone(),
                        name: claimant.0.clone(),
                        created_at: now_seconds()?,
                    },
                    user_id,
                    secret_hash: hash,
                })
                .await?
            {
                return Err(fail(429, "busy"));
            }
            // Durable creation may yield. Revalidate both human logins and
            // installation before publishing its one-time proof; a signed
            // transport and an earlier approval do not survive logout.
            let claimant_user =
                authenticate_token_digest(state, claimant.1.clone(), TokenAudience::Native).await?;
            if claimant_user.id != user_id {
                return Err(fail(403, "unauthorized"));
            }
            let (receiver_digest, receiver_hash) = {
                let mut sessions = self.lock()?;
                let session = Self::session(&mut sessions, &target)?;
                (session.token_digest.clone(), session.receiver_hash.clone())
            };
            let receiver_user =
                authenticate_token_digest(state, receiver_digest, TokenAudience::Native).await?;
            if receiver_user.id != user_id
                || state
                    .store
                    .remote_authority(
                        &claimant.2,
                        user_id,
                        RemoteProof::Receiver {
                            secret_hash: receiver_hash,
                        },
                    )
                    .await?
                    .is_none()
            {
                return Err(fail(403, "unauthorized"));
            }
            if state.store.get_setting(FEATURE_KEY).await?.as_deref() != Some("1") {
                return Err(fail(503, "unavailable"));
            }
            Ok::<_, ApiError>(raw)
        }
        .await;
        let published = {
            if let Ok(mut sessions) = self.sessions.lock() {
                if let Ok(s) = Self::session(&mut sessions, &target) {
                    if let Some(p) = s.pending.iter_mut().find(|p| {
                        p.id == pending_id
                            && p.phase == PairPhase::Approving
                            && Instant::now() < p.until
                    }) {
                        match &result {
                            Ok(raw) => {
                                p.phase = PairPhase::Approved;
                                p.grant = Some((grant_id, raw.clone()));
                            }
                            Err(_) => p.phase = PairPhase::Denied,
                        };
                        p.until = Instant::now() + Duration::from_secs(60);
                        let _ = bump(s);
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            } else {
                false
            }
        };
        // Revocation follows even an uncertain create failure. A completed
        // orphan cannot occupy the receiver's eight-grant allowance.
        if !published || result.is_err() {
            let _ = state
                .store
                .revoke_remote_grant(&grant_id.to_string(), user_id, now_seconds().unwrap_or(0))
                .await;
        }
        self.changed.notify_waiters();
    }
    pub async fn perform(
        &self,
        state: &AppState,
        dispatch: Dispatch,
    ) -> Result<(u16, Value), ApiError> {
        let _poll_permit = self.poll_permit(&dispatch)?;
        let deadline =
            Instant::now() + Duration::from_millis(dispatch.request.wait_ms().min(20_000));
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            self.expire()?;
            // Every loop includes authority reads; the caller's User cannot
            // remain cached through a20s poll while logout/revocation commits.
            let user = authenticate_token_digest(
                state,
                dispatch.token_digest.clone(),
                TokenAudience::Native,
            )
            .await?;
            if user.id != dispatch.user_id {
                return Err(fail(403, "unauthorized"));
            }
            if state.store.get_setting(FEATURE_KEY).await?.as_deref() != Some("1") {
                self.disable();
                return Err(fail(503, "unavailable"));
            }
            if dispatch.request.target().is_some() {
                self.authorize_session(state, &dispatch).await?;
                self.refresh_authority(
                    state,
                    dispatch
                        .request
                        .target()
                        .ok_or_else(|| fail(400, "invalid"))?,
                )
                .await?;
            }
            self.deliver(state, &dispatch).await?;
            self.expire()?;
            let reply = self.once(state, &dispatch).await?;
            if let Some(reply) = reply {
                return Ok(reply);
            }
            let next = (Instant::now() + Duration::from_secs(1)).min(deadline);
            if Instant::now() >= deadline {
                return self.empty_reply(&dispatch);
            }
            tokio::select! {_=notified=>{},_=tokio::time::sleep_until(next)=>{}}
        }
    }
    fn empty_reply(&self, d: &Dispatch) -> Result<(u16, Value), ApiError> {
        let mut sessions = self.lock()?;
        let s = Self::session(
            &mut sessions,
            d.request.target().ok_or_else(|| fail(400, "invalid"))?,
        )?;
        Ok((
            200,
            match d.request {
                Request::Poll(_) => poll_reply(s),
                Request::ReadState(_) => state_reply(s, false),
                _ => return Err(fail(400, "invalid")),
            },
        ))
    }
    async fn once(&self, state: &AppState, d: &Dispatch) -> Result<Option<(u16, Value)>, ApiError> {
        match &d.request {
            Request::CreateReceiver(r) => {
                if !label(&r.name, 80) {
                    return Err(fail(400, "invalid"));
                }
                let (raw, hash) = secret()?;
                let receiver = RemoteReceiver {
                    id: Uuid::new_v4().to_string(),
                    user_id: d.user_id,
                    name: r.name.clone(),
                    platform: r.platform.name().into(),
                    created_at: now_seconds()?,
                };
                if !state
                    .store
                    .create_remote_receiver(NewRemoteReceiver {
                        receiver: receiver.clone(),
                        secret_hash: hash,
                    })
                    .await?
                {
                    return Err(fail(429, "busy"));
                }
                return Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","receiver_id":receiver.id,"receiver_secret":raw}),
                )));
            }
            Request::CreateSession(r) => {
                let generation = self.generation.load(std::sync::atomic::Ordering::SeqCst);
                let hash = d
                    .proof
                    .receiver_hash
                    .clone()
                    .ok_or_else(|| fail(401, "unauthorized"))?;
                let receiver = state
                    .store
                    .remote_authority(
                        &r.receiver_id.to_string(),
                        d.user_id,
                        RemoteProof::Receiver {
                            secret_hash: hash.clone(),
                        },
                    )
                    .await?
                    .ok_or_else(|| fail(403, "unauthorized"))?;
                let mut sessions = self.lock()?;
                if self.generation.load(std::sync::atomic::Ordering::SeqCst) != generation {
                    return Err(fail(503, "unavailable"));
                }
                // Supersede this installation's prior session atomically, but
                // never another installation (duplicate tabs register separately).
                sessions.retain(|_, s| s.receiver.id != receiver.id);
                if sessions.len() >= 100
                    || sessions
                        .values()
                        .filter(|s| s.receiver.user_id == d.user_id)
                        .count()
                        >= 20
                {
                    return Err(fail(429, "busy"));
                }
                let target = plurx_core::remote_control::Target {
                    owner_node_id: state.node_id.clone(),
                    session_id: Uuid::new_v4(),
                    receiver_epoch: Uuid::new_v4(),
                };
                sessions.insert(
                    target.session_id,
                    Session {
                        target: target.clone(),
                        receiver,
                        foreground_id: r.foreground_id,
                        token_digest: d.token_digest.clone(),
                        receiver_hash: hash,
                        last_seen: Instant::now(),
                        revision: 1,
                        lease: None,
                        state: None,
                        queue: VecDeque::new(),
                        batch: Vec::new(),
                        delivery_id: 0,
                        receiver_poll: false,
                        state_polls: HashSet::new(),
                        results: VecDeque::new(),
                        challenge: None,
                        pending: Vec::new(),
                    },
                );
                self.changed.notify_waiters();
                return Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","target":target}),
                )));
            }
            Request::ListSessions { .. } => {
                let sessions = self.lock()?;
                let list=sessions.values().filter(|s|s.receiver.user_id==d.user_id).map(|s|json!({"receiver_id":s.receiver.id,"target":s.target,"busy":s.lease.is_some(),"foreground_id":s.foreground_id})).collect::<Vec<_>>();
                return Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","sessions":list}),
                )));
            }
            Request::PairClaim(r) => {
                if !label(&r.controller_name, 80)
                    || r.code.len() != 8
                    || !r.code.bytes().all(|b| b.is_ascii_digit())
                {
                    return Err(fail(400, "invalid"));
                }
                if !state
                    .store
                    .admit_remote_pair_claim(d.user_id, now_seconds()?)
                    .await?
                {
                    return Err(fail(429, "busy"));
                }
                let (raw, hash) = secret()?;
                let mut sessions = self.lock()?;
                let s = Self::session(&mut sessions, &r.target)?;
                let c = s
                    .challenge
                    .as_mut()
                    .filter(|c| r.challenge_id.is_none_or(|id| c.id == id))
                    .ok_or_else(|| fail(404, "unavailable"))?;
                if c.failures >= 5 {
                    return Err(fail(429, "busy"));
                }
                let mut mac = Hmac::<Sha256>::new_from_slice(c.key.as_bytes())
                    .map_err(|_| fail(503, "unavailable"))?;
                mac.update(r.code.as_bytes());
                if mac.verify_slice(&c.code_hash).is_err() {
                    c.failures += 1;
                    return Err(fail(403, "unauthorized"));
                }
                if s.pending.len() >= 8 {
                    return Err(fail(429, "busy"));
                }
                let id = Uuid::new_v4();
                let until = c.until;
                s.challenge = None;
                s.pending.push(Pending {
                    id,
                    name: r.controller_name.clone(),
                    token_digest: d.token_digest.clone(),
                    poll_hash: hash,
                    until,
                    phase: PairPhase::Pending,
                    grant: None,
                });
                bump(s)?;
                self.changed.notify_waiters();
                return Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","pending_id":id,"poll_secret":raw}),
                )));
            }
            Request::PairApprove(r) => {
                if !r.approve {
                    let mut sessions = self.lock()?;
                    let s = Self::session(&mut sessions, &r.target)?;
                    let p = s
                        .pending
                        .iter_mut()
                        .find(|p| p.id == r.pending_id && p.phase == PairPhase::Pending)
                        .ok_or_else(|| fail(409, "busy"))?;
                    p.phase = PairPhase::Denied;
                    p.until = Instant::now() + Duration::from_secs(60);
                    bump(s)?;
                    self.changed.notify_waiters();
                } else {
                    let permit = self
                        .approvals
                        .clone()
                        .try_acquire_owned()
                        .map_err(|_| fail(429, "busy"))?;
                    let claimant = {
                        let mut sessions = self.lock()?;
                        let s = Self::session(&mut sessions, &r.target)?;
                        let p = s
                            .pending
                            .iter_mut()
                            .find(|p| p.id == r.pending_id && p.phase == PairPhase::Pending)
                            .ok_or_else(|| fail(409, "busy"))?;
                        p.phase = PairPhase::Approving;
                        (
                            p.name.clone(),
                            p.token_digest.clone(),
                            s.receiver.id.clone(),
                        )
                    };
                    let state = state.clone();
                    let target = r.target.clone();
                    let pending = r.pending_id;
                    let user = d.user_id;
                    // One bounded operation per accepted physical approval,
                    // independent of HTTP cancellation. No task per keypress.
                    tokio::spawn(async move {
                        let _permit = permit;
                        state
                            .remote
                            .finish_approval(&state, target, pending, user, claimant)
                            .await;
                    });
                }
                return Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","accepted":true}),
                )));
            }
            _ => {}
        }
        let controller_name = if let Request::Control(r) = &d.request {
            state
                .store
                .remote_grants(d.user_id)
                .await?
                .into_iter()
                .find(|g| g.id == r.grant_id.to_string())
                .map(|g| g.name)
                .ok_or_else(|| fail(403, "unauthorized"))?
        } else {
            String::new()
        };
        // QR failure does not break manual pairing. Fetch durable identity
        // before the owner critical section, never while holding its mutex.
        let instance_id = if matches!(d.request, Request::PairStart(_)) {
            state.store.instance_id().await.ok()
        } else {
            None
        };
        // No await below: ordering/snapshot/control are one actor critical section.
        let mut sessions = self.lock()?;
        let s = Self::session(
            &mut sessions,
            d.request.target().ok_or_else(|| fail(400, "invalid"))?,
        )?;
        match &d.request {
            Request::Presence(r) => {
                let mut value = r.state.clone();
                if !value.validate() {
                    return Err(fail(400, "invalid"));
                }
                // Retain room for target/control and all 64 bounded ACKs.
                // Reject oversized normalized metadata rather than trimming
                // protocol fields or suppressing acknowledgements at read time.
                if serde_json::to_vec(&value)
                    .map_err(|_| fail(400, "invalid"))?
                    .len()
                    > 48 * 1024
                {
                    return Err(fail(413, "invalid"));
                }
                if s.state
                    .as_ref()
                    .is_some_and(|old| value.state_revision <= old.state_revision)
                {
                    return Err(fail(409, "stale_context"));
                }
                s.state = Some(value);
                s.last_seen = Instant::now();
                bump(s)?;
                self.changed.notify_waiters();
                Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","accepted":true}),
                )))
            }
            Request::Control(r) => {
                let hash = d
                    .proof
                    .grant_hash
                    .clone()
                    .ok_or_else(|| fail(401, "unauthorized"))?;
                let same = s
                    .lease
                    .as_ref()
                    .is_some_and(|l| l.wire.active_grant_id == r.grant_id);
                if matches!(r.action, ControlAction::Takeover)
                    && r.control_epoch.is_some()
                    && s.lease.as_ref().map(|l| l.wire.control_epoch) != r.control_epoch
                {
                    return Err(fail(409, "stale_control"));
                }
                match r.action {
                    ControlAction::Release | ControlAction::Renew => {
                        let l = s
                            .lease
                            .as_mut()
                            .filter(|l| same && Some(l.wire.control_epoch) == r.control_epoch)
                            .ok_or_else(|| fail(409, "stale_control"))?;
                        if matches!(r.action, ControlAction::Release) {
                            clear_control(s)?;
                        } else {
                            l.until = Instant::now() + Duration::from_secs(15);
                            l.token_digest = d.token_digest.clone();
                            l.grant_hash = hash;
                            bump(s)?;
                        }
                    }
                    ControlAction::Acquire if s.lease.is_some() && !same => {
                        return Err(fail(409, "busy"))
                    }
                    ControlAction::Acquire if same => {
                        if let Some(l) = &mut s.lease {
                            l.until = Instant::now() + Duration::from_secs(15);
                            l.token_digest = d.token_digest.clone();
                            l.grant_hash = hash;
                        }
                        bump(s)?;
                    }
                    _ => {
                        clear_control(s)?;
                        s.lease = Some(Lease {
                            wire: Control {
                                control_epoch: Uuid::new_v4(),
                                active_grant_id: r.grant_id,
                                controller_name,
                            },
                            token_digest: d.token_digest.clone(),
                            grant_hash: hash,
                            until: Instant::now() + Duration::from_secs(15),
                            last_sequence: 0,
                        });
                    }
                }
                self.changed.notify_waiters();
                Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","target":s.target,"response_revision":s.revision,"control":s.lease.as_ref().map(|l|&l.wire)}),
                )))
            }
            Request::Commands(command) => {
                command.validate().map_err(|_| fail(400, "invalid"))?;
                if s.state
                    .as_ref()
                    .is_some_and(|state| state.route == Route::Restricted)
                {
                    return Err(fail(403, "restricted_surface"));
                }
                let l = s
                    .lease
                    .as_mut()
                    .filter(|l| {
                        l.wire.control_epoch == command.control_epoch
                            && l.wire.active_grant_id == command.grant_id
                    })
                    .ok_or_else(|| fail(409, "stale_control"))?;
                if command.sequence <= l.last_sequence {
                    return Err(fail(409, "duplicate_or_old"));
                }
                if s.queue.len() + s.batch.len() >= 32 {
                    return Err(fail(429, "busy"));
                }
                l.last_sequence = command.sequence;
                s.queue.push_back(Queued {
                    command: command.clone(),
                    token_digest: d.token_digest.clone(),
                    grant_hash: d
                        .proof
                        .grant_hash
                        .clone()
                        .ok_or_else(|| fail(401, "unauthorized"))?,
                    at: Instant::now(),
                });
                self.changed.notify_waiters();
                Ok(Some((
                    202,
                    json!({"version":"cinema.remote.v1","queued":true,"control_epoch":command.control_epoch,"sequence":command.sequence}),
                )))
            }
            Request::Poll(r) => {
                if !safe(r.after_delivery_id)
                    || !safe(r.after_response_revision)
                    || r.wait_ms > 20_000
                {
                    return Err(fail(400, "invalid"));
                }
                if r.after_delivery_id > s.delivery_id {
                    return Err(fail(400, "invalid"));
                }
                if r.after_delivery_id == s.delivery_id {
                    s.batch.clear();
                }
                // Delivery candidates were independently reauthorized before this
                // function by deliver(); never hand raw queue rows to a receiver.
                if !s.batch.is_empty() || s.revision > r.after_response_revision || r.wait_ms == 0 {
                    return Ok(Some((200, poll_reply(s))));
                }
                Ok(None)
            }
            Request::Ack(r) => {
                if r.outcomes.len() > 64 || r.outcomes.iter().any(|a| !positive(a.sequence)) {
                    return Err(fail(400, "invalid"));
                }
                for ack in &r.outcomes {
                    if s.lease.as_ref().is_some_and(|l| {
                        l.wire.control_epoch == ack.control_epoch && ack.sequence <= l.last_sequence
                    }) && !s.results.iter().any(|(a, _)| {
                        a.control_epoch == ack.control_epoch && a.sequence == ack.sequence
                    }) {
                        if s.results.len() == 64 {
                            s.results.pop_front();
                        }
                        s.results.push_back((*ack, Instant::now()));
                    }
                }
                bump(s)?;
                self.changed.notify_waiters();
                Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","accepted":true}),
                )))
            }
            Request::ReadState(r) => {
                if !safe(r.after_revision) || r.wait_ms > 20_000 {
                    return Err(fail(400, "invalid"));
                }
                if s.revision > r.after_revision || r.wait_ms == 0 {
                    Ok(Some((200, state_reply(s, s.revision > r.after_revision))))
                } else {
                    Ok(None)
                }
            }
            Request::PairStart(_) => {
                let (key, _) = secret()?;
                let bytes =
                    hex::decode(auth::generate_token().map_err(|_| fail(503, "unavailable"))?)
                        .map_err(|_| fail(503, "unavailable"))?;
                let n = u64::from_be_bytes(
                    bytes[..8]
                        .try_into()
                        .map_err(|_| fail(503, "unavailable"))?,
                );
                let code = format!("{:08}", n % 100_000_000);
                let id = Uuid::new_v4();
                let hash = pairing_hash(&key, &code)?;
                s.challenge = Some(Challenge {
                    id,
                    key,
                    code_hash: hash,
                    until: Instant::now() + Duration::from_secs(120),
                    failures: 0,
                });
                bump(s)?;
                Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","target":s.target,"challenge_id":id,"code":code,"expires_in_ms":120_000,"qr_modules":instance_id.as_deref().and_then(|instance|pairing_payload(instance,&s.target,id,&code)).and_then(|payload|qr_modules(&payload))}),
                )))
            }
            Request::PairResult(r) => {
                let hash = d
                    .proof
                    .pairing_hash
                    .as_ref()
                    .ok_or_else(|| fail(401, "unauthorized"))?;
                let pos = s
                    .pending
                    .iter()
                    .position(|p| {
                        p.id == r.pending_id
                            && p.token_digest == d.token_digest
                            && &p.poll_hash == hash
                    })
                    .ok_or_else(|| fail(403, "unauthorized"))?;
                if matches!(
                    s.pending[pos].phase,
                    PairPhase::Pending | PairPhase::Approving
                ) {
                    return Ok(Some((
                        200,
                        json!({"version":"cinema.remote.v1","status":"pending","grant_id":null,"grant_secret":null,"receiver_id":null}),
                    )));
                }
                let p = s.pending.remove(pos);
                let status = if p.phase == PairPhase::Approved {
                    "approved"
                } else {
                    "denied"
                };
                let (id, raw) = p
                    .grant
                    .map(|(id, raw)| (Some(id), Some(raw)))
                    .unwrap_or((None, None));
                bump(s)?;
                Ok(Some((
                    200,
                    json!({"version":"cinema.remote.v1","status":status,"grant_id":id,"grant_secret":raw,"receiver_id":s.receiver.id}),
                )))
            }
            _ => Err(fail(400, "invalid")),
        }
    }
    /// Authenticate each queued controller login and grant before every send,
    /// including retry of a retained batch. No peer assertion substitutes for it.
    pub async fn deliver(&self, state: &AppState, d: &Dispatch) -> Result<(), ApiError> {
        let Some(target) = d.request.target() else {
            return Ok(());
        };
        if !matches!(d.request, Request::Poll(_)) {
            return Ok(());
        }
        let candidates = {
            let mut sessions = self.lock()?;
            let s = Self::session(&mut sessions, target)?;
            s.batch
                .iter()
                .chain(s.queue.iter())
                .cloned()
                .collect::<Vec<_>>()
        };
        #[cfg(test)]
        self.delivery_pause.wait().await;
        let examined = candidates
            .iter()
            .map(|q| (q.command.control_epoch, q.command.sequence))
            .collect::<HashSet<_>>();
        let mut valid = Vec::new();
        for q in candidates {
            let user =
                authenticate_token_digest(state, q.token_digest.clone(), TokenAudience::Native)
                    .await;
            if let Ok(user) = user {
                if user.id == d.user_id
                    && state
                        .store
                        .remote_authority(
                            &self.receiver_id(target)?,
                            user.id,
                            RemoteProof::Grant {
                                grant_id: q.command.grant_id.to_string(),
                                secret_hash: q.grant_hash.clone(),
                            },
                        )
                        .await?
                        .is_some()
                {
                    valid.push((q.command.control_epoch, q.command.sequence));
                }
            }
        }
        let mut sessions = self.lock()?;
        let s = Self::session(&mut sessions, target)?;
        let now = Instant::now();
        let epoch = s.lease.as_ref().map(|l| l.wire.control_epoch);
        let retain = |q: &Queued| {
            Some(q.command.control_epoch) == epoch
                && now.duration_since(q.at) < Duration::from_millis(500)
                && valid.contains(&(q.command.control_epoch, q.command.sequence))
        };
        s.batch.retain(&retain);
        let mut deliverable = Vec::new();
        let mut rejected = Vec::new();
        s.queue.retain(|q| {
            if !examined.contains(&(q.command.control_epoch, q.command.sequence)) {
                return true;
            }
            if retain(q) {
                deliverable.push(q.clone());
            } else {
                let outcome = if Some(q.command.control_epoch) != epoch {
                    Outcome::StaleControl
                } else if now.duration_since(q.at) >= Duration::from_millis(500) {
                    Outcome::Expired
                } else {
                    Outcome::Unauthorized
                };
                rejected.push((q.command.clone(), outcome));
            }
            false
        });
        if !rejected.is_empty() {
            for (command, outcome) in rejected {
                record_owner_outcome(s, &command, outcome);
            }
            bump(s)?;
            self.changed.notify_waiters();
        }
        if let Request::Poll(r) = &d.request {
            if r.after_delivery_id == s.delivery_id {
                s.batch.clear();
            }
        }
        if s.batch.is_empty() && !deliverable.is_empty() {
            s.delivery_id = s
                .delivery_id
                .checked_add(1)
                .filter(|v| *v <= MAX_SAFE_INTEGER)
                .ok_or_else(|| fail(503, "unavailable"))?;
            // The bounded target/control/counters and eight 80-byte pairing
            // names fit in 8 KiB, including JSON escaping. Reserve their worst
            // case so later metadata growth cannot oversize a retained retry.
            let mut bytes = 8 * 1024;
            let mut full = false;
            for q in deliverable {
                let size = serde_json::to_vec(&q.command)
                    .map_err(|_| fail(503, "unavailable"))?
                    .len()
                    + 1;
                if !full && bytes + size <= 64 * 1024 {
                    bytes += size;
                    s.batch.push(q);
                } else {
                    full = true;
                    s.queue.push_back(q);
                }
            }
        } else {
            s.queue.extend(deliverable);
        }
        // Admission enforces increasing sequences for one active epoch.
        // Restored examined rows can otherwise follow newer unexamined rows.
        s.queue
            .make_contiguous()
            .sort_by_key(|q| q.command.sequence);
        Ok(())
    }
    fn receiver_id(&self, t: &plurx_core::remote_control::Target) -> Result<String, ApiError> {
        let mut sessions = self.lock()?;
        Ok(Self::session(&mut sessions, t)?.receiver.id.clone())
    }
}
fn poll_reply(s: &Session) -> Value {
    json!({"version":"cinema.remote.v1","target":s.target,"response_revision":s.revision,"delivery_id":s.delivery_id,"control":s.lease.as_ref().map(|l|&l.wire),"commands":s.batch.iter().map(|q|&q.command).collect::<Vec<_>>(),"pairings":s.pending.iter().filter(|p|p.phase==PairPhase::Pending).map(|p|json!({"pending_id":p.id,"controller_name":p.name})).collect::<Vec<_>>()})
}
fn state_reply(s: &Session, changed: bool) -> Value {
    json!({"version":"cinema.remote.v1","target":s.target,"response_revision":s.revision,"control":s.lease.as_ref().map(|l|&l.wire),"state":if changed{s.state.as_ref()}else{None},"outcomes":s.results.iter().map(|(a,_)|a).collect::<Vec<_>>()})
}
