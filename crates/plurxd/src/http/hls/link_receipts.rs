//! Observational completed-body proof. Failure never changes media authority.
use crate::{state::AppState, telemetry::NetworkIdentity};
use plurx_core::domain::{
    CandidateLinkBinding, CandidateLinkObservation, MediaFile, MediaSessionRoute, NetworkPriorCause,
};
use plurx_core::playback::candidate::CandidateRoute;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const NONCES: usize = 4096;
const SESSIONS: usize = 512;
const NONCE_TTL: Duration = Duration::from_secs(30);
const FRESH: Duration = Duration::from_secs(15);
const SESSION_TTL: Duration = Duration::from_secs(2 * 60 * 60);
pub(super) type CompletionObserver = Box<dyn FnOnce(Instant, i64) + Send>;

/// Optional observational request input: malformed/duplicate values are Unknown.
pub(crate) fn requested_receipt(headers: &axum::http::HeaderMap) -> Option<&str> {
    let mut values = headers.get_all("x-plurx-link-receipt").iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() || value.len() != 36 {
        return None;
    }
    (uuid::Uuid::parse_str(value).ok()?.to_string() == value).then_some(value)
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct SessionBinding {
    pub source: CandidateLinkBinding,
    pub session: String,
    pub incarnation: String,
    pub owner_epoch: i64,
}
/// Nonserializable live provenance; work after capture cannot renew its EOF.
pub(crate) struct LiveLinkProof {
    binding: SessionBinding,
    completed: Instant,
    transfer: plurx_core::playback::candidate::NetworkTransferEvidence,
}
impl LiveLinkProof {
    pub(crate) fn incumbent_session(&self) -> &str {
        &self.binding.session
    }
    pub(crate) fn transfer(
        &self,
    ) -> Option<plurx_core::playback::candidate::NetworkTransferEvidence> {
        let age = Instant::now().checked_duration_since(self.completed)?;
        if age > FRESH {
            return None;
        }
        let mut transfer = self.transfer;
        transfer.age_ms = u32::try_from(age.as_millis()).ok()?;
        transfer.usable_bps().map(|_| transfer)
    }
    pub(crate) fn incumbent_recipe(&self) -> ([u8; 32], CandidateRoute) {
        (self.binding.source.recipe_digest, self.binding.source.route)
    }
}
struct SessionRow {
    binding: SessionBinding,
    touched: Instant,
}
#[derive(Clone, PartialEq, Eq)]
struct RawBody {
    bytes: u64,
    duration_ms: u32,
}
struct Receipt {
    binding: SessionBinding,
    expected_bytes: u64,
    media_duration_ms: Option<u32>,
    object_name: String,
    etag: String,
    born: Instant,
    completion: Option<(Instant, i64)>,
    raw: Option<RawBody>,
    negative_claimed: bool,
}
impl Receipt {
    fn claim(
        &mut self,
        sample: &ClientLinkSample,
        now: Instant,
    ) -> Option<CandidateLinkObservation> {
        if sample.cause != NetworkPriorCause::Link
            || sample.age_ms > 15_000
            || sample.network_load != Some(true)
            || sample.from_cache != Some(false)
            || sample.producer_paced != Some(false)
            || !(1..=120_000).contains(&sample.body_duration_ms)
        {
            return None;
        }
        let (completed, completed_at_ms) = self.completion?;
        if now.checked_duration_since(completed)? > FRESH
            || sample.body_bytes != self.expected_bytes
            || sample.object_name != self.object_name
            || sample.etag != self.etag
        {
            return None;
        }
        let raw = RawBody {
            bytes: sample.body_bytes,
            duration_ms: sample.body_duration_ms,
        };
        if self.raw.as_ref().is_some_and(|saved| saved != &raw) {
            return None;
        }
        if sample.negative {
            if self.raw.is_none()
                || self.negative_claimed
                || !sample.presenting
                || !sample.stalled
                || sample.runway_ms > 1500
                || sample.media_duration_ms != self.media_duration_ms
                || !self
                    .media_duration_ms
                    .is_some_and(|media| media > 0 && sample.body_duration_ms > media)
            {
                return None;
            }
            self.negative_claimed = true;
        } else if self.raw.is_some() {
            return None;
        }
        self.raw = Some(raw);
        Some(CandidateLinkObservation {
            binding: self.binding.source.clone(),
            body_bytes: sample.body_bytes,
            body_duration_ms: sample.body_duration_ms,
            completed_at_ms,
            negative: sample.negative,
        })
    }
}
#[derive(Default)]
struct Rows {
    decoder: HashMap<String, super::candidate_recovery::AcceptedDecoderProof>,
    staged: HashMap<String, super::prepared_link::StagedProof>,
    sessions: HashMap<String, SessionRow>,
    receipts: HashMap<String, Receipt>,
}
#[derive(Default)]
pub(crate) struct LinkReceipts(
    Mutex<Rows>,
    #[cfg(test)] crate::seam_hooks::PauseSlot,
    #[cfg(test)] crate::seam_hooks::PauseSlot,
);

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientLinkSample {
    pub receipt: String,
    pub object_name: String,
    pub etag: String,
    pub body_bytes: u64,
    pub body_duration_ms: u32,
    pub age_ms: u32,
    pub network_load: Option<bool>,
    pub from_cache: Option<bool>,
    pub producer_paced: Option<bool>,
    pub cause: NetworkPriorCause,
    pub negative: bool,
    pub media_duration_ms: Option<u32>,
    pub presenting: bool,
    pub stalled: bool,
    pub runway_ms: u32,
}

pub(crate) async fn binding(
    network: &NetworkIdentity,
    file: &MediaFile,
    recipe_digest: [u8; 32],
    route: CandidateRoute,
) -> Option<CandidateLinkBinding> {
    let fence = tokio::time::timeout(
        Duration::from_millis(100),
        crate::fragment_index_cluster::open_source_fence(file, None),
    )
    .await
    .ok()?
    .ok()?;
    if !fence.unchanged() {
        return None;
    }
    Some(CandidateLinkBinding {
        user_id: network.user_id?,
        credential_generation: network.credential_generation.as_ref()?.as_str().to_owned(),
        client_class: network.client_class.clone(),
        network_fingerprint: network.network_fingerprint.clone(),
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
        source_object_version: fence.object_version().to_owned(),
        recipe_digest,
        route,
    })
}

impl LinkReceipts {
    pub(super) fn record_decoder(
        &self,
        proof: super::candidate_recovery::AcceptedDecoderProof,
    ) -> Option<()> {
        let mut rows = self.0.lock().ok()?;
        rows.decoder.retain(|_, value| value.fresh());
        if rows.decoder.len() >= NONCES {
            return None;
        }
        if rows.decoder.contains_key(proof.session()) {
            return None;
        }
        rows.decoder.insert(proof.session().to_owned(), proof);
        Some(())
    }

    pub(super) fn decoder_current(&self, bound: &super::candidate_recovery::BoundRecovery) -> bool {
        self.0.lock().ok().is_some_and(|rows| {
            rows.decoder
                .get(&bound.route.session_id)
                .is_some_and(|proof| proof.matches(bound))
        })
    }

    /// A native recovery may await one explicit negative acknowledgement;
    /// ordinary diagnostics remain detached. This never mints from a cause
    /// label, renews EOF or turns an absent proof into a playback error.
    pub(crate) async fn accept_negative_for_ack(
        &self,
        state: &AppState,
        network: &NetworkIdentity,
        session: Option<&str>,
        sample: &ClientLinkSample,
    ) -> Option<String> {
        if !sample.negative {
            return None;
        }
        tokio::time::timeout(std::time::Duration::from_millis(100), async {
            let session = session?;
            let enabled = state
                .store
                .get_setting(plurx_core::store::keys::PLAYBACK_NETWORK_PRIORS)
                .await
                .ok()
                .flatten()
                .is_some_and(|value| value.trim() == "1");
            if !enabled {
                return None;
            }
            let value = self.accept(state, network, Some(session), sample).await?;
            state
                .store
                .observe_candidate_link(&value, crate::media_sessions::unix_ms())
                .await
                .ok()?;
            // Ok(()) also covers a refused/no-op fold. Only an exact durable
            // readback can acknowledge this immutable completion's negative.
            let saved = state
                .store
                .candidate_link_prior(&value.binding)
                .await
                .ok()??;
            if saved.binding != value.binding
                || saved.body_bytes != value.body_bytes
                || saved.body_duration_ms != value.body_duration_ms
                || saved.completed_at_ms != value.completed_at_ms
                || saved.negative_at_ms != Some(value.completed_at_ms)
            {
                return None;
            }
            let file = state.store.get_file(value.binding.file_id).await.ok()??;
            let route = state.store.media_session_route(session).await.ok()??;
            let proof = self
                .current_negative(
                    state,
                    network,
                    &file,
                    Some(&sample.receipt),
                    &route.playback_id,
                )
                .await?;
            if proof.incumbent_session() != session
                || proof.incumbent_recipe().0 != value.binding.recipe_digest
            {
                return None;
            }
            proof.transfer()?;
            Some(sample.receipt.clone())
        })
        .await
        .ok()
        .flatten()
    }

    /// A cause label cannot mint a Link fault. Only this incumbent's already
    /// accepted exact-negative claim qualifies, with its original live EOF.
    pub(crate) async fn current_negative(
        &self,
        state: &AppState,
        network: &NetworkIdentity,
        file: &MediaFile,
        nonce: Option<&str>,
        playback: &str,
    ) -> Option<LiveLinkProof> {
        let proof = self
            .current_positive(
                state,
                network,
                file,
                nonce,
                Some(playback),
                Some(&state.node_id),
            )
            .await?;
        let rows = self.0.lock().ok()?;
        let row = rows.receipts.get(nonce?)?;
        if !row.negative_claimed
            || row.binding != proof.binding
            || row.completion?.0 != proof.completed
            || proof.transfer().is_none()
        {
            return None;
        }
        Some(proof)
    }
    /// Read a live incumbent transfer, not a serialized network prior. Processing
    /// routes may differ for an upgrade, but its delivery owner must stay local.
    pub(crate) async fn current_positive(
        &self,
        state: &AppState,
        network: &NetworkIdentity,
        file: &MediaFile,
        incumbent_receipt: Option<&str>,
        playback_id: Option<&str>,
        proposed_owner: Option<&str>,
    ) -> Option<LiveLinkProof> {
        if proposed_owner != Some(state.node_id.as_str()) {
            return None;
        }
        tokio::time::timeout(Duration::from_millis(100), async {
            if !state
                .store
                .get_setting(plurx_core::store::keys::PLAYBACK_NETWORK_PRIORS)
                .await
                .ok()
                .flatten()
                .is_some_and(|value| value.trim() == "1")
            {
                return None;
            }
            let nonce = incumbent_receipt?;
            if nonce.len() != 36 || uuid::Uuid::parse_str(nonce).is_err() {
                return None;
            }
            let captured = {
                let rows = self.0.lock().ok()?;
                let now = Instant::now();
                let row = rows.receipts.get(nonce)?;
                if rows.staged.contains_key(&row.binding.session) {
                    return None;
                }
                let (eof, _) = row.completion?;
                if Some(row.binding.source.user_id) != network.user_id
                    || !network
                        .credential_generation
                        .as_ref()
                        .is_some_and(|generation| {
                            generation.as_str() == row.binding.source.credential_generation
                        })
                    || row.binding.source.client_class != network.client_class
                    || row.binding.source.network_fingerprint != network.network_fingerprint
                    || row.binding.source.file_id != file.id
                    || row.binding.source.source_size != file.size
                    || row.binding.source.source_mtime != file.mtime
                    || row.raw.is_none()
                    || now.checked_duration_since(eof)? > FRESH
                {
                    return None;
                }
                row.binding.clone()
            };
            let route = state
                .store
                .media_session_route(&captured.session)
                .await
                .ok()??;
            if !Self::same_route(&captured, &route, &state.node_id) {
                return None;
            }
            if playback_id.is_some_and(|playback| playback != route.playback_id) {
                return None;
            }
            let current_source = binding(
                network,
                file,
                captured.source.recipe_digest,
                captured.source.route,
            )
            .await?;
            if current_source != captured.source {
                return None;
            }
            let current_route = state
                .store
                .media_session_route(&captured.session)
                .await
                .ok()??;
            if !Self::same_route(&captured, &current_route, &state.node_id)
                || playback_id.is_some_and(|playback| playback != current_route.playback_id)
            {
                return None;
            }
            // Async authority/source queries cannot refresh the original EOF.
            // Re-read this exact sample, never another receipt sharing a tuple.
            self.exact_nonce_proof(nonce, &captured)
        })
        .await
        .ok()
        .flatten()
    }

    fn exact_nonce_proof(&self, nonce: &str, binding: &SessionBinding) -> Option<LiveLinkProof> {
        let rows = self.0.lock().ok()?;
        let row = rows.receipts.get(nonce)?;
        if &row.binding != binding {
            return None;
        }
        let (completed, _) = row.completion?;
        let raw = row.raw.as_ref()?;
        let proof = LiveLinkProof {
            binding: binding.clone(),
            completed,
            transfer: plurx_core::playback::candidate::NetworkTransferEvidence {
                bytes: raw.bytes,
                elapsed_ms: raw.duration_ms,
                age_ms: 0,
                completed: true,
                from_cache: false,
                producer_paced: false,
            },
        };
        proof.transfer().map(|_| proof)
    }

    /// Raw acquisition evidence is usable only while its issuing process retains
    /// the exact accepted EOF. Durable timestamps cannot recreate this proof.
    pub(crate) fn fresh_positive(
        &self,
        binding: &SessionBinding,
    ) -> Option<plurx_core::playback::candidate::NetworkTransferEvidence> {
        let rows = self.0.lock().ok()?;
        let now = Instant::now();
        rows.receipts
            .values()
            .filter(|row| &row.binding == binding)
            .filter_map(|row| {
                let (eof, _) = row.completion?;
                let age = now.checked_duration_since(eof)?;
                if age > FRESH {
                    return None;
                }
                let raw = row.raw.as_ref()?;
                let value = plurx_core::playback::candidate::NetworkTransferEvidence {
                    bytes: raw.bytes,
                    elapsed_ms: raw.duration_ms,
                    age_ms: u32::try_from(age.as_millis()).ok()?,
                    completed: true,
                    from_cache: false,
                    producer_paced: false,
                };
                value.usable_bps().map(|_| (eof, value))
            })
            .max_by_key(|(eof, _)| *eof)
            .map(|(_, value)| value)
    }
    fn prune(rows: &mut Rows, now: Instant) {
        rows.staged.retain(|_, proof| {
            !proof.cancelled.is_cancelled()
                && proof.deadline_unix_ms > crate::media_sessions::unix_ms()
                && proof.trial_deadline.is_none_or(|deadline| now < deadline)
        });
        rows.receipts
            .retain(|_, row| now.saturating_duration_since(row.born) <= NONCE_TTL);
        rows.sessions
            .retain(|_, row| now.saturating_duration_since(row.touched) <= SESSION_TTL);
    }
    pub(crate) fn register(&self, binding: SessionBinding) {
        let Ok(mut rows) = self.0.lock() else {
            return;
        };
        let now = Instant::now();
        Self::prune(&mut rows, now);
        if rows.sessions.len() >= SESSIONS && !rows.sessions.contains_key(&binding.session) {
            return;
        }
        rows.sessions.insert(
            binding.session.clone(),
            SessionRow {
                binding,
                touched: now,
            },
        );
    }
    pub(super) fn register_staged(
        &self,
        binding: SessionBinding,
        proof: super::prepared_link::StagedProof,
    ) {
        let Ok(mut rows) = self.0.lock() else {
            return;
        };
        Self::prune(&mut rows, Instant::now());
        if proof.cancelled.is_cancelled()
            || !proof.stage.still_live()
            || proof.deadline_unix_ms <= crate::media_sessions::unix_ms()
            || proof
                .trial_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            || rows.sessions.len() >= SESSIONS && !rows.sessions.contains_key(&binding.session)
        {
            return;
        }
        rows.staged.insert(binding.session.clone(), proof);
        rows.sessions.insert(
            binding.session.clone(),
            SessionRow {
                binding,
                touched: Instant::now(),
            },
        );
    }
    #[cfg(test)]
    pub(super) fn replace_staged_gate_for_test(
        &self,
        session: &str,
        gate: Arc<dyn crate::playback_control::PreparationGate>,
    ) {
        self.0
            .lock()
            .expect("registry lock")
            .staged
            .get_mut(session)
            .expect("registered stage")
            .gate = gate;
    }
    #[cfg(test)]
    pub(super) fn pause_final_stage_route_for_test(&self) -> Arc<crate::seam_hooks::AsyncPause> {
        self.2.arm("staged final route result")
    }
    #[cfg(test)]
    pub(super) fn raw_claimed_for_test(&self, nonce: &str) -> bool {
        self.0
            .lock()
            .expect("registry lock")
            .receipts
            .get(nonce)
            .expect("minted nonce")
            .raw
            .is_some()
    }
    /// Pending headers alone are not proof; only successful exact completion arms it.
    pub(super) fn mint(
        self: &Arc<Self>,
        session: &str,
        object: &str,
        etag: &str,
        bytes: u64,
        media_duration_ms: Option<u32>,
        full200: bool,
    ) -> Option<(String, CompletionObserver)> {
        if !full200
            || bytes == 0
            || bytes > 1_073_741_824
            || !crate::transcode::is_safe_segment(object)
            || etag.is_empty()
            || etag.len() > 128
            || !(object.starts_with("seg") && (object.ends_with(".m4s") || object.ends_with(".ts")))
        {
            return None;
        }
        let mut rows = self.0.lock().ok()?;
        let now = Instant::now();
        Self::prune(&mut rows, now);
        if rows.receipts.len() >= NONCES {
            return None;
        }
        if rows.staged.get(session).is_some_and(|proof| {
            proof.cancelled.is_cancelled()
                || !proof.fence.still_live()
                || !proof.stage.still_live()
                || proof.deadline_unix_ms <= crate::media_sessions::unix_ms()
                || proof.trial_deadline.is_some_and(|deadline| now >= deadline)
        }) {
            return None;
        }
        let source = rows.sessions.get_mut(session)?;
        source.touched = now;
        let binding = source.binding.clone();
        let nonce = uuid::Uuid::new_v4().to_string();
        rows.receipts.insert(
            nonce.clone(),
            Receipt {
                binding,
                expected_bytes: bytes,
                media_duration_ms,
                object_name: object.to_owned(),
                etag: etag.to_owned(),
                born: now,
                completion: None,
                raw: None,
                negative_claimed: false,
            },
        );
        let receipts = Arc::clone(self);
        let completed_nonce = nonce.clone();
        Some((
            nonce,
            Box::new(move |eof, eof_unix_ms| {
                let Ok(mut rows) = receipts.0.lock() else {
                    return;
                };
                let now = Instant::now();
                Self::prune(&mut rows, now);
                if let Some(row) = rows.receipts.get_mut(&completed_nonce) {
                    if row.completion.is_none() {
                        row.completion = Some((eof, eof_unix_ms));
                    }
                }
            }),
        ))
    }

    pub(crate) async fn accept(
        &self,
        state: &AppState,
        network: &NetworkIdentity,
        session: Option<&str>,
        sample: &ClientLinkSample,
    ) -> Option<CandidateLinkObservation> {
        if sample.receipt.len() != 36
            || uuid::Uuid::parse_str(&sample.receipt).is_err()
            || sample.cause != NetworkPriorCause::Link
            || sample.age_ms > 15_000
            || sample.network_load != Some(true)
            || sample.from_cache != Some(false)
            || sample.producer_paced != Some(false)
            || !(1..=120_000).contains(&sample.body_duration_ms)
        {
            return None;
        }
        let (captured, staged) = {
            let rows = self.0.lock().ok()?;
            let binding = rows.receipts.get(&sample.receipt)?.binding.clone();
            let staged = rows.staged.get(&binding.session).cloned();
            (binding, staged)
        };
        if staged.is_some() && sample.negative {
            return None;
        }
        if session != Some(captured.session.as_str())
            || network.user_id != Some(captured.source.user_id)
            || network.credential_generation.as_ref()?.as_str()
                != captured.source.credential_generation
            || network.client_class != captured.source.client_class
            || network.network_fingerprint != captured.source.network_fingerprint
        {
            return None;
        }
        let route = tokio::time::timeout(
            Duration::from_secs(1),
            state.store.media_session_route(&captured.session),
        )
        .await
        .ok()?
        .ok()??;
        if !Self::route_for_observation(&captured, &route, &state.node_id, staged.as_ref()) {
            return None;
        }
        let file = tokio::time::timeout(
            Duration::from_secs(1),
            state.store.get_file(captured.source.file_id),
        )
        .await
        .ok()?
        .ok()??;
        if file.size != captured.source.source_size || file.mtime != captured.source.source_mtime {
            return None;
        }
        let fence = tokio::time::timeout(
            Duration::from_millis(100),
            crate::fragment_index_cluster::open_source_fence(&file, None),
        )
        .await
        .ok()?
        .ok()?;
        if !fence.unchanged() || fence.object_version() != captured.source.source_object_version {
            return None;
        }
        #[cfg(test)]
        self.1.hold().await;
        if let Some(proof) = staged.as_ref() {
            let budget = Duration::from_millis(
                u64::try_from(
                    proof
                        .deadline_unix_ms
                        .saturating_sub(crate::media_sessions::unix_ms()),
                )
                .unwrap_or(0)
                .min(100),
            );
            let current = tokio::time::timeout(budget, async {
                proof.gate.observation_is_current(proof.fence.clone()).await
                    && proof
                        .gate
                        .staged_observation_is_current(
                            proof.fence.clone(),
                            proof.incarnation.clone(),
                            proof.deadline_unix_ms,
                        )
                        .await
                        .is_some_and(|stage| stage.still_live())
            })
            .await
            .unwrap_or(false);
            if !current {
                return None;
            }
        }
        // Source lookup can suspend while the exact serving attachment is
        // retired or replaced. Claim only after its current authority fence.
        let route = tokio::time::timeout(Duration::from_secs(1), async {
            let route = state.store.media_session_route(&captured.session).await;
            #[cfg(test)]
            if staged.is_some() {
                self.2.hold().await;
            }
            route
        })
        .await
        .ok()?
        .ok()??;
        if !fence.unchanged()
            || !Self::route_for_observation(&captured, &route, &state.node_id, staged.as_ref())
        {
            return None;
        }
        let mut rows = self.0.lock().ok()?;
        let now = Instant::now();
        Self::prune(&mut rows, now);
        let row = rows.receipts.get_mut(&sample.receipt)?;
        let observation = row.claim(sample, now)?;
        drop(rows);
        self.fresh_positive(&captured)?;
        Some(observation)
    }
    fn same_route(binding: &SessionBinding, route: &MediaSessionRoute, node: &str) -> bool {
        route.session_id == binding.session
            && route.incarnation_id == binding.incarnation
            && route.owner_epoch == binding.owner_epoch
            && route.user_id == binding.source.user_id
            && route.owner_node_id == node
            && route.state == "active"
            && route.publication_ready_at_ms == 0
            && route.lease_expires_at_ms > crate::media_sessions::unix_ms()
    }
    fn route_for_observation(
        binding: &SessionBinding,
        route: &MediaSessionRoute,
        node: &str,
        staged: Option<&super::prepared_link::StagedProof>,
    ) -> bool {
        let Some(proof) = staged else {
            return Self::same_route(binding, route, node);
        };
        !proof.cancelled.is_cancelled()
            && proof.fence.still_live()
            && proof.stage.still_live()
            && proof.deadline_unix_ms > crate::media_sessions::unix_ms()
            && proof
                .trial_deadline
                .is_none_or(|deadline| Instant::now() < deadline)
            && route.session_id == binding.session
            && route.incarnation_id == binding.incarnation
            && route.owner_epoch == binding.owner_epoch
            && route.owner_epoch == proof.owner_epoch
            && route.incarnation_id == proof.incarnation
            && route.user_id == binding.source.user_id
            && route.owner_node_id == node
            && route.state == "active"
            && route.publication_ready_at_ms
                == plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
            && route.lease_expires_at_ms > crate::media_sessions::unix_ms()
    }
}

/// Exact negatives only; raw throughput cannot be compared to planned budgets.
pub(crate) async fn admissible(
    state: &AppState,
    network: Option<&NetworkIdentity>,
    file: &MediaFile,
    candidate: &plurx_core::playback::candidate::QualityCandidate,
) -> bool {
    let Some(network) = network else {
        return true;
    };
    let Some(binding) = binding(network, file, candidate.recipe_digest, candidate.route).await
    else {
        return true;
    };
    let prior = state
        .store
        .candidate_link_prior(&binding)
        .await
        .ok()
        .flatten();
    !prior.is_some_and(|prior| prior.negative_active(crate::media_sessions::unix_ms()))
}

pub(crate) async fn filter_catalog(
    state: &AppState,
    network: Option<&NetworkIdentity>,
    file: &MediaFile,
    catalog: Vec<plurx_core::playback::candidate::QualityCandidate>,
) -> Vec<plurx_core::playback::candidate::QualityCandidate> {
    let fallback = catalog.clone();
    tokio::time::timeout(Duration::from_millis(100), async {
        if !state
            .store
            .get_setting(plurx_core::store::keys::PLAYBACK_NETWORK_PRIORS)
            .await
            .ok()
            .flatten()
            .is_some_and(|value| value.trim() == "1")
        {
            return catalog;
        }
        let mut permitted = Vec::with_capacity(catalog.len());
        for candidate in catalog {
            if admissible(state, network, file, &candidate).await {
                permitted.push(candidate);
            }
        }
        permitted
    })
    .await
    .unwrap_or(fallback)
}

/// Public advisory projection from the exact existing local resolver. Remote
/// worker responses remain strict and cannot supply private measured authority.
pub(crate) async fn measured_outputs(
    state: &AppState,
    file: &MediaFile,
    request: &crate::media_pool::QualityCatalogRequest,
    accepted: &[crate::media_pool::WorkerQualityCandidate],
    snapshot: Option<&plurx_core::store::PlaybackPlanningSnapshot>,
) -> Option<Vec<crate::vodserve::retained::MeasuredCandidateOutput>> {
    if !accepted.iter().any(|entry| entry.node_id == state.node_id) {
        return None;
    }
    let projected = tokio::time::timeout_at(
        crate::media_pool::create_stage_deadline(Duration::from_millis(100)),
        async {
            if let Some(snapshot) = snapshot {
                state
                    .transcode
                    .quality_catalog_from_snapshot_progress(
                        snapshot,
                        &request.caps,
                        request.audio_index,
                        request.audio_offset_ms,
                        request.subtitle_burn,
                        request.presentation,
                        request.copy_contract,
                        request.audio_delivery.as_ref(),
                        request.audio_claim.as_ref(),
                        None,
                        None,
                    )
                    .await
            } else {
                state
                    .transcode
                    .quality_candidates_with_measured_outputs(
                        file,
                        &request.caps,
                        request.audio_index,
                        request.audio_offset_ms,
                        request.subtitle_burn,
                        request.presentation,
                        request.copy_contract,
                        request.audio_delivery.as_ref(),
                        request.audio_claim.as_ref(),
                    )
                    .await
            }
        },
    )
    .await
    .ok()?;
    let outputs: Vec<_> = projected
        .measured_candidate_outputs
        .into_iter()
        .filter(|output| {
            output.average_bps > 0
                && output.peak_bps >= output.average_bps
                && output.peak_bps <= 9_007_199_254_740_991
                && accepted
                    .iter()
                    .find(|entry| {
                        entry.candidate.id == output.candidate_id
                            && entry.candidate.recipe_digest == output.recipe_digest
                            && entry.candidate.route == output.route
                    })
                    .is_some_and(|entry| entry.node_id == state.node_id)
        })
        .take(64)
        .collect();
    (!outputs.is_empty()).then_some(outputs)
}

/// Warm advisory Auto selection may use a fresh incumbent acquisition only
/// against this node's qualified complete-output costs. Cold/recovery choices
/// stay ordinary playable catalog entries; absence is not an enable gate.
pub(crate) async fn positive_catalog(
    state: &AppState,
    network: Option<&NetworkIdentity>,
    file: &MediaFile,
    nonce: Option<&str>,
    playback_id: Option<&str>,
    catalog: Vec<plurx_core::playback::candidate::QualityCandidate>,
    measured: Option<&[crate::vodserve::retained::MeasuredCandidateOutput]>,
) -> Vec<plurx_core::playback::candidate::QualityCandidate> {
    let Some(network) = network else {
        return catalog;
    };
    let Some(proof) = state
        .link_receipts
        .current_positive(
            state,
            network,
            file,
            nonce,
            playback_id,
            Some(&state.node_id),
        )
        .await
    else {
        return catalog;
    };
    let (digest, route) = proof.incumbent_recipe();
    let Some(current) = catalog
        .iter()
        .find(|candidate| candidate.recipe_digest == digest && candidate.route == route)
    else {
        return catalog;
    };
    let current_id = current.id;
    let current_area = u64::from(current.width) * u64::from(current.height);
    let Some(link) = proof.transfer().and_then(|transfer| transfer.usable_bps()) else {
        return catalog;
    };
    let permitted: Vec<_> = catalog
        .iter()
        .filter(|candidate| {
            candidate.id == current_id
                || u64::from(candidate.width) * u64::from(candidate.height) <= current_area
                    && !(route == CandidateRoute::Encode
                        && candidate.route != CandidateRoute::Encode)
                || measured_margin(candidate, link, measured.unwrap_or_default())
                || unknown_stageable_original(candidate, measured.unwrap_or_default())
        })
        .cloned()
        .collect();
    if permitted.is_empty() {
        catalog
    } else {
        permitted
    }
}

/// Exposes compatible source-copy choices for a bounded trial, without claiming
/// a warm recommendation or full-output cost. Original file routes cannot be
/// staged by the current HLS resolver; its source-copy contract is Remux.
pub(super) fn unknown_stageable_original(
    candidate: &plurx_core::playback::candidate::QualityCandidate,
    outputs: &[crate::vodserve::retained::MeasuredCandidateOutput],
) -> bool {
    candidate.identity_matches()
        && candidate.route == CandidateRoute::Remux
        && candidate.decoder_compatible
        && candidate.width > 0
        && candidate.height > 0
        && candidate.peak_bps.is_none()
        && outputs.len() <= 64
        && !outputs.iter().any(|output| {
            output.candidate_id == candidate.id
                && output.recipe_digest == candidate.recipe_digest
                && output.route == candidate.route
        })
}

fn measured_margin(
    candidate: &plurx_core::playback::candidate::QualityCandidate,
    link: u64,
    outputs: &[crate::vodserve::retained::MeasuredCandidateOutput],
) -> bool {
    if outputs.len() > 64 {
        return false;
    }
    let mut matching = outputs.iter().filter(|output| {
        output.candidate_id == candidate.id
            && output.recipe_digest == candidate.recipe_digest
            && output.route == candidate.route
    });
    let Some(output) = matching.next() else {
        return false;
    };
    matching.next().is_none()
        && output.qualification == "complete_full_mux_rfc8216_v1"
        && output.average_bps > 0
        && output.peak_bps >= output.average_bps
        && output.peak_bps <= 9_007_199_254_740_991
        && u128::from(link) * 10 >= u128::from(output.peak_bps) * 18
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a05_client_log_ack_header_is_exact_negative_only_and_never_batch_authority() {
        client_log_ack_controls(false, false).await;
    }

    #[tokio::test]
    async fn a05_client_log_ack_refuses_conflicting_durable_fold() {
        client_log_ack_controls(true, false).await;
    }

    #[tokio::test]
    async fn a05_client_log_ack_requires_exact_durable_completion_and_refuses_older_sample() {
        client_log_ack_controls(false, true).await;
    }

    async fn client_log_ack_controls(conflicting_fold: bool, exact_fold: bool) {
        use axum::{extract::State, http::HeaderMap, Json};
        use plurx_core::domain::{MediaSessionActivation, MediaSessionActivationSettlement};
        let (state, user, file, _root) = actual_intake_state().await;
        state
            .store
            .put_setting(plurx_core::store::keys::PLAYBACK_NETWORK_PRIORS, "1")
            .await
            .expect("setting");
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "Mozilla/5.0".parse().expect("UA"));
        let peer = "192.168.4.9:1234".parse().expect("socket");
        let mut network = crate::http::network::identity(&headers, Some(peer)).expect("namespace");
        network.user_id = Some(user.id);
        network.credential_generation = Some(plurx_core::domain::CredentialGeneration::derive(
            user.id,
            user.created_at,
            &user.password_hash,
        ));
        let now = crate::media_sessions::unix_ms();
        let activation = MediaSessionActivation {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            user_id: user.id,
            playback_id: "http-ack-player".into(),
            recovery_epoch: String::new(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: "a".repeat(64),
            owner_node_id: state.node_id.clone(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 60000,
            expected_desired_revision: None,
        };
        state
            .store
            .activate_media_session(&activation)
            .await
            .expect("activate")
            .expect("accepted");
        state
            .store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                now,
            )
            .await
            .expect("confirm")
            .expect("published");
        let session = SessionBinding {
            source: binding(&network, &file, [4; 32], CandidateRoute::Encode)
                .await
                .expect("source"),
            session: activation.session_id.clone(),
            incarnation: activation.incarnation_id.clone(),
            owner_epoch: 1,
        };
        state.link_receipts.register(session.clone());
        let (nonce, eof) = state
            .link_receipts
            .mint(
                &session.session,
                "seg00001.m4s",
                "etag",
                4096,
                Some(4000),
                true,
            )
            .expect("receipt");
        eof(Instant::now(), now);
        let mut body = serde_json::json!({"level":"warn","event":"candidate_link_sample","message":"actual ack",
            "session_id":session.session,"link_sample":{"receipt":nonce,"object_name":"seg00001.m4s","etag":"etag",
            "body_bytes":4096,"body_duration_ms":5000,"age_ms":0,"network_load":true,"from_cache":false,"producer_paced":false,
            "cause":"link","negative":false,"media_duration_ms":4000,"presenting":true,"stalled":true,"runway_ms":1000}});
        let raw: ClientLinkSample =
            serde_json::from_value(body["link_sample"].clone()).expect("sample");
        assert!(state
            .link_receipts
            .accept(&state, &network, Some(&session.session), &raw)
            .await
            .is_some());
        async fn send(
            state: &AppState,
            user: &plurx_core::domain::User,
            headers: &HeaderMap,
            peer: std::net::SocketAddr,
            body: serde_json::Value,
        ) -> axum::response::Response {
            crate::http::system::client_log(
                crate::http::extract::AuthUser(user.clone()),
                State(state.clone()),
                headers.clone(),
                crate::http::network::RemoteAddress(Some(peer)),
                Json(serde_json::from_value(body).expect("single DTO")),
            )
            .await
        }
        let positive = send(&state, &user, &headers, peer, body.clone()).await;
        assert_eq!(positive.status(), 204);
        assert!(!positive.headers().contains_key("x-plurx-link-accepted"));
        body["link_sample"]["negative"] = true.into();
        if conflicting_fold {
            // The durable namespace key deliberately conflicts with this exact
            // source binding. Store returns Ok for the refused fold, not proof.
            let mut conflicting = session.source.clone();
            conflicting.source_object_version.push_str("-foreign");
            state
                .store
                .observe_candidate_link(
                    &CandidateLinkObservation {
                        binding: conflicting.clone(),
                        body_bytes: 8192,
                        body_duration_ms: 6000,
                        completed_at_ms: now,
                        negative: true,
                    },
                    now,
                )
                .await
                .expect("foreign durable row");
            let refused = send(&state, &user, &headers, peer, body.clone()).await;
            assert_eq!(refused.status(), 204);
            assert!(!refused.headers().contains_key("x-plurx-link-accepted"));
            let saved = state
                .store
                .candidate_link_prior(&conflicting)
                .await
                .expect("readback")
                .expect("unchanged row");
            assert!(saved.binding == conflicting);
            assert_eq!(saved.body_bytes, 8192);
            assert_eq!(saved.completed_at_ms, now);
            return;
        }
        let mut malformed = body.clone();
        malformed["link_sample"]["receipt"] = "bad,nonce".into();
        let unknown = send(&state, &user, &headers, peer, malformed).await;
        assert_eq!(unknown.status(), 204);
        assert!(!unknown.headers().contains_key("x-plurx-link-accepted"));
        assert!(
            serde_json::from_value::<crate::http::system::ClientLog>(serde_json::json!([
                body.clone(),
                body.clone()
            ]))
            .is_err()
        );
        let accepted = send(&state, &user, &headers, peer, body.clone()).await;
        assert_eq!(accepted.status(), 204);
        assert_eq!(
            accepted
                .headers()
                .get_all("x-plurx-link-accepted")
                .iter()
                .count(),
            1
        );
        assert_eq!(
            accepted.headers()["x-plurx-link-accepted"]
                .to_str()
                .expect("nonce"),
            nonce
        );
        let duplicate = send(&state, &user, &headers, peer, body.clone()).await;
        assert_eq!(duplicate.status(), 204);
        assert!(!duplicate.headers().contains_key("x-plurx-link-accepted"));
        if exact_fold {
            let saved = state
                .store
                .candidate_link_prior(&session.source)
                .await
                .expect("durable readback")
                .expect("accepted completion");
            assert!(saved.binding == session.source);
            assert_eq!(saved.body_bytes, 4096);
            assert_eq!(saved.body_duration_ms, 5000);
            assert_eq!(saved.completed_at_ms, now);
            assert_eq!(saved.negative_at_ms, Some(now));
            let (older_nonce, older_eof) = state
                .link_receipts
                .mint(
                    &session.session,
                    "seg00001.m4s",
                    "etag",
                    4096,
                    Some(4000),
                    true,
                )
                .expect("distinct older completion");
            older_eof(Instant::now(), now - 1);
            body["link_sample"]["receipt"] = older_nonce.into();
            let refused = send(&state, &user, &headers, peer, body).await;
            assert_eq!(refused.status(), 204);
            assert!(!refused.headers().contains_key("x-plurx-link-accepted"));
            let unchanged = state
                .store
                .candidate_link_prior(&session.source)
                .await
                .expect("durable readback")
                .expect("original completion remains");
            assert_eq!(unchanged.completed_at_ms, saved.completed_at_ms);
            assert_eq!(unchanged.negative_at_ms, saved.negative_at_ms);
            assert_eq!(unchanged.body_bytes, saved.body_bytes);
            assert_eq!(unchanged.body_duration_ms, saved.body_duration_ms);
        }
    }

    #[tokio::test]
    async fn a05_typed_recovery_authenticates_candidate_and_spends_only_one_decoder_response() {
        typed_recovery_controls(false).await;
    }

    #[tokio::test]
    async fn a05_decode_label_requires_accepted_exact_fault_without_replay_or_refresh() {
        typed_recovery_controls(true).await;
    }

    async fn typed_recovery_controls(review_controls: bool) {
        use crate::transcode::{ReopenReason, SessionKind, SessionRequest};
        use axum::{
            extract::{Path, Query, State},
            http::HeaderMap,
        };
        use plurx_core::domain::{MediaSessionActivation, MediaSessionActivationSettlement};
        let (state, user, file, _root) = actual_intake_state().await;
        state
            .store
            .put_setting(plurx_core::store::keys::PLAYBACK_DISPLAY_AWARE_AUTO, "1")
            .await
            .expect("Auto");
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "Mozilla/5.0".parse().expect("UA"));
        let remote = "192.168.4.9:1234".parse().expect("socket");
        let caps: plurx_core::playback::DeviceCaps = serde_json::from_value(
            serde_json::json!({"v":2,"video":[{"codec":"h264","present":["sdr"]}],"containers":["mp4"]})
        ).expect("actual request caps");
        let decision = crate::http::stream::decision(
            crate::http::extract::AuthUser(user.clone()),
            State(state.clone()),
            Path(file.id),
            Query(crate::http::stream::Caps {
                caps_v2: Some(caps.clone()),
                force: Some("auto".into()),
                ..Default::default()
            }),
            headers.clone(),
            crate::http::network::RemoteAddress(Some(remote)),
        )
        .await
        .expect("actual catalog")
        .0;
        let catalog = decision.quality_candidates.expect("catalog");
        assert!(catalog.len() >= 2, "real distinct candidate recipes");
        let incumbent = catalog[0].clone();
        let proposed = catalog
            .iter()
            .find(|row| row.recipe_digest != incumbent.recipe_digest)
            .expect("distinct")
            .clone();
        let mut network =
            crate::http::network::identity(&headers, Some(remote)).expect("namespace");
        network.user_id = Some(user.id);
        network.credential_generation = Some(plurx_core::domain::CredentialGeneration::derive(
            user.id,
            user.created_at,
            &user.password_hash,
        ));
        let mut request = SessionRequest {
            quality_catalog: None,
            candidate_context: None,
            file_id: file.id,
            playback_id: "typed-player".into(),
            request_id: None,
            control_sequence: None,
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Transcode { height: 720 },
            start_seconds: 0.0,
            audio_index: None,
            audio_delivery: None,
            audio_claim: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
            transport: Some("hlsjs".into()),
        };
        let now = crate::media_sessions::unix_ms();
        let incarnation = uuid::Uuid::new_v4().to_string();
        request.request_id = Some(incarnation.clone());
        let planning = state
            .store
            .playback_planning_snapshot(file.id, &crate::transcode::QUALITY_PLANNING_KEYS)
            .await
            .expect("planning snapshot")
            .expect("source");
        let session = uuid::Uuid::new_v4().to_string();
        let recipe = crate::media_sessions::RemoteStartRequest {
            retained_output_receiver: None,
            retained_output: None,
            candidate_id: Some(incumbent.id),
            candidate_catalog: Some(crate::media_sessions::CandidateCatalogContext {
                caps: caps.clone(),
                candidate: incumbent.clone(),
                binding: crate::media_pool::PlanningBinding::from_snapshot(&planning),
            }),
            presentation_target: None,
            decoder_caps: Some(
                crate::playback_control::DecoderCapsSnapshot::from_device_caps(&caps, 1)
                    .expect("actual decoder constraints"),
            ),
            protocol_version: crate::media_pool::PROTOCOL_VERSION,
            incarnation_id: incarnation.clone(),
            user_id: user.id,
            source_size: file.size,
            source_mtime: file.mtime,
            typeless_playlist: false,
            library_channel: None,
            request: request.clone(),
        };
        let response = serde_json::json!({"session_id":session,"playlist_url":"owned.m3u8","duration_ms":12000,
            "start_seconds":0.0,"media_origin_ms":0,"height":720,"encoder":"fixture","vod":true,"ladder":[],
            "quality_candidate_id":incumbent.id,"quality_candidates":catalog});
        let activation = MediaSessionActivation {
            incarnation_id: incarnation,
            session_id: session.clone(),
            user_id: user.id,
            playback_id: request.playback_id.clone(),
            recovery_epoch: uuid::Uuid::new_v4().to_string(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: "a".repeat(64),
            owner_node_id: state.node_id.clone(),
            recipe_json: serde_json::to_string(&recipe).expect("recipe"),
            response_json: response.to_string(),
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 60000,
            expected_desired_revision: None,
        };
        state
            .store
            .activate_media_session(&activation)
            .await
            .expect("activate")
            .expect("accepted");
        state
            .store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                now,
            )
            .await
            .expect("confirm")
            .expect("published");
        let bound = crate::http::hls::candidate_recovery::incumbent(
            &state,
            &network,
            &file,
            &request.playback_id,
            Some(&session),
        )
        .await
        .expect("authenticated full current candidate");
        request.previous_session_id = Some(session.clone());
        let mut context = crate::transcode::TranscodeManager::candidate_context(&proposed);
        context.canonical_caps = Some(caps);
        context.planning_binding =
            Some(crate::media_pool::PlanningBinding::from_snapshot(&planning));
        context.planning_snapshot = Some(Arc::new(planning));
        context.owner_node_id = Some(state.node_id.clone());
        request.candidate_context = Some(Box::new(context));
        for cause in [
            ReopenReason::Link,
            ReopenReason::Encode,
            ReopenReason::Hold,
            ReopenReason::Authority,
        ] {
            request.reopen_reason = Some(cause);
            assert!(
                !crate::http::hls::candidate_recovery::observe(
                    &state,
                    Some(&network),
                    Some(&file),
                    &request,
                    None,
                    &uuid::Uuid::new_v4().to_string()
                )
                .await
                .recorded(),
                "labels cannot mint missing proof or change held authority"
            );
        }
        assert!(state
            .store
            .candidate_recovery_memory(&bound.scope)
            .await
            .expect("memory")
            .decode_step_recipe
            .is_none());
        request.reopen_reason = Some(ReopenReason::Decode);
        let event = uuid::Uuid::new_v4().to_string();
        assert!(!crate::http::hls::candidate_recovery::observe(
            &state,
            None,
            Some(&file),
            &request,
            None,
            &event
        )
        .await
        .recorded());
        if review_controls {
            assert!(
                !crate::http::hls::candidate_recovery::observe(
                    &state,
                    Some(&network),
                    Some(&file),
                    &request,
                    None,
                    &event
                )
                .await
                .recorded(),
                "authenticated Decode label cannot mint evidence"
            );
        }
        let mut fault = crate::http::hls::candidate_recovery::ClientRecoverySample {
            cause: plurx_core::store::CandidateRecoveryCause::Decode,
            event_id: uuid::Uuid::new_v4().to_string(),
            candidate_id: incumbent.id,
            recipe_digest: incumbent.recipe_digest,
            age_ms: 0,
            decoder_failed: true,
            rendered_elapsed_ms: 0,
            position_progress_ms: 0,
            dropped_frames: 0,
            runway_ms: 0,
        };
        if review_controls {
            fault.decoder_failed = false;
            assert!(crate::http::hls::candidate_recovery::accept_sample(
                &state,
                &network,
                Some(&session),
                &fault
            )
            .await
            .is_none());
            fault.decoder_failed = true;
            fault.age_ms = 15_001;
            assert!(crate::http::hls::candidate_recovery::accept_sample(
                &state,
                &network,
                Some(&session),
                &fault
            )
            .await
            .is_none());
            fault.age_ms = 0;
            assert!(crate::http::hls::candidate_recovery::accept_sample(
                &state,
                &network,
                Some("foreign-session"),
                &fault
            )
            .await
            .is_none());
            let mut foreign = network.clone();
            foreign.user_id = Some(user.id + 1);
            assert!(crate::http::hls::candidate_recovery::accept_sample(
                &state,
                &foreign,
                Some(&session),
                &fault
            )
            .await
            .is_none());
            // The new case exercises the actual supplied progressing-pressure
            // alternative, without inventing a fatal decoder flag.
            fault.decoder_failed = false;
            fault.rendered_elapsed_ms = 4_000;
            fault.position_progress_ms = 2_000;
            fault.dropped_frames = 6;
            fault.runway_ms = 10_000;
        }
        assert_eq!(
            crate::http::hls::candidate_recovery::accept_sample(
                &state,
                &network,
                Some(&session),
                &fault
            )
            .await,
            Some(fault.event_id.clone()),
            "only actual accepted exact decoder failure is acknowledged"
        );
        if review_controls {
            assert!(
                crate::http::hls::candidate_recovery::accept_sample(
                    &state,
                    &network,
                    Some(&session),
                    &fault
                )
                .await
                .is_none(),
                "replay cannot acknowledge or refresh its original proof"
            );
        }
        assert_eq!(
            crate::http::hls::candidate_recovery::observe(
                &state,
                Some(&network),
                Some(&file),
                &request,
                None,
                &event
            )
            .await,
            crate::http::hls::candidate_recovery::CauseRecord::Recorded(
                plurx_core::store::CandidateRecoveryCause::Decode
            )
        );
        assert!(
            !crate::http::hls::candidate_recovery::observe(
                &state,
                Some(&network),
                Some(&file),
                &request,
                None,
                &uuid::Uuid::new_v4().to_string()
            )
            .await
            .recorded(),
            "second quality response stays with compatibility owner"
        );
        let memory = state
            .store
            .candidate_recovery_memory(&bound.scope)
            .await
            .expect("memory");
        assert_eq!(memory.decode_step_recipe, Some(incumbent.recipe_digest));
        let private_auto = crate::http::hls::candidate_recovery::auto_catalog(
            &state,
            Some(&network),
            &file,
            &request.playback_id,
            catalog.clone(),
        )
        .await;
        assert!(!private_auto.iter().any(|row| row.id == incumbent.id));
        assert!(
            catalog.iter().any(|row| row.id == incumbent.id),
            "manual/public catalog is untouched"
        );
    }

    #[tokio::test]
    async fn a05_negative_ack_requires_real_fold_and_exact_nonce_without_replay_refresh() {
        use plurx_core::domain::{MediaSessionActivation, MediaSessionActivationSettlement};
        let (state, user, file, _root) = actual_intake_state().await;
        state
            .store
            .put_setting(plurx_core::store::keys::PLAYBACK_NETWORK_PRIORS, "1")
            .await
            .expect("setting");
        let now = crate::media_sessions::unix_ms();
        let activation = MediaSessionActivation {
            incarnation_id: uuid::Uuid::new_v4().to_string(),
            session_id: uuid::Uuid::new_v4().to_string(),
            user_id: user.id,
            playback_id: "ack-player".into(),
            recovery_epoch: String::new(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: false,
            request_id: None,
            request_fingerprint: "a".repeat(64),
            owner_node_id: state.node_id.clone(),
            recipe_json: "{}".into(),
            response_json: "{}".into(),
            publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 60000,
            expected_desired_revision: None,
        };
        state
            .store
            .activate_media_session(&activation)
            .await
            .expect("activate")
            .expect("accepted");
        state
            .store
            .settle_media_session_activation(
                &activation,
                MediaSessionActivationSettlement::Confirm {
                    publication_ready_at_ms: 0,
                },
                now,
            )
            .await
            .expect("confirm")
            .expect("published");
        let network = NetworkIdentity {
            user_id: Some(user.id),
            credential_generation: Some(plurx_core::domain::CredentialGeneration::derive(
                user.id,
                user.created_at,
                &user.password_hash,
            )),
            client_class: "web".into(),
            network_fingerprint: "192.168.4.0/24".into(),
        };
        let source = binding(&network, &file, [4; 32], CandidateRoute::Encode)
            .await
            .expect("source");
        let session = SessionBinding {
            source,
            session: activation.session_id.clone(),
            incarnation: activation.incarnation_id.clone(),
            owner_epoch: 1,
        };
        state.link_receipts.register(session.clone());
        let (nonce, eof) = state
            .link_receipts
            .mint(
                &session.session,
                "seg00001.m4s",
                "etag",
                4096,
                Some(4000),
                true,
            )
            .expect("receipt");
        let completed = Instant::now();
        eof(completed, now);
        let mut sample = ClientLinkSample {
            receipt: nonce.clone(),
            object_name: "seg00001.m4s".into(),
            etag: "etag".into(),
            body_bytes: 4096,
            body_duration_ms: 5000,
            age_ms: 0,
            network_load: Some(true),
            from_cache: Some(false),
            producer_paced: Some(false),
            cause: NetworkPriorCause::Link,
            negative: false,
            media_duration_ms: Some(4000),
            presenting: true,
            stalled: true,
            runway_ms: 1000,
        };
        assert!(
            state
                .link_receipts
                .accept_negative_for_ack(&state, &network, Some(&session.session), &sample)
                .await
                .is_none(),
            "positive telemetry is never an acknowledgement"
        );
        assert!(
            state
                .link_receipts
                .accept(&state, &network, Some(&session.session), &sample)
                .await
                .is_some(),
            "real raw fold first"
        );
        sample.negative = true;
        sample.receipt = "malformed".into();
        assert!(state
            .link_receipts
            .accept_negative_for_ack(&state, &network, Some(&session.session), &sample)
            .await
            .is_none());
        sample.receipt = nonce.clone();
        sample.etag = "other-response".into();
        assert!(state
            .link_receipts
            .accept_negative_for_ack(&state, &network, Some(&session.session), &sample)
            .await
            .is_none());
        sample.etag = "etag".into();
        assert!(state
            .link_receipts
            .accept_negative_for_ack(&state, &network, Some("different-incumbent"), &sample)
            .await
            .is_none());
        assert_eq!(
            state
                .link_receipts
                .accept_negative_for_ack(&state, &network, Some(&session.session), &sample)
                .await,
            Some(nonce.clone())
        );
        assert!(
            state
                .link_receipts
                .accept_negative_for_ack(&state, &network, Some(&session.session), &sample)
                .await
                .is_none(),
            "duplicate cannot acknowledge or refresh"
        );
        let rows = state.link_receipts.0.lock().expect("registry");
        let row = rows.receipts.get(&nonce).expect("same immutable response");
        assert_eq!(row.completion.expect("EOF").0, completed);
        assert!(row.negative_claimed);
    }

    async fn actual_intake_state() -> (
        AppState,
        plurx_core::domain::User,
        MediaFile,
        tempfile::TempDir,
    ) {
        use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary, ProbeResult};
        let root = crate::test_tempdir().expect("private fixture");
        let path = root.path().join("source.mp4");
        std::fs::write(&path, vec![0_u8; 4096]).expect("physical source identity");
        let store: Arc<dyn plurx_core::store::Store> =
            Arc::new(plurx_core::store::SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "A05".into(),
                kind: LibraryKind::Movies,
                paths: vec![],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "A05".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let metadata = std::fs::metadata(&path).expect("metadata");
        let mtime = metadata
            .modified()
            .expect("mtime")
            .duration_since(std::time::UNIX_EPOCH)
            .expect("epoch")
            .as_secs() as i64;
        let id = store
            .upsert_file(
                item,
                path.to_str().expect("path"),
                4096,
                mtime,
                &ProbeResult {
                    container: Some("mp4".into()),
                    video_codec: Some("h264".into()),
                    width: Some(1920),
                    height: Some(1080),
                    bit_depth: Some(8),
                    duration_ms: Some(12000),
                    raw_json: Some(
                        serde_json::json!({"streams":[{"index":0,"codec_type":"video",
                "codec_name":"h264","profile":"High","width":1920,"height":1080,
                "pix_fmt":"yuv420p","sample_aspect_ratio":"1:1",
                "r_frame_rate":"24/1","avg_frame_rate":"24/1"}]})
                        .to_string(),
                    ),
                    ..Default::default()
                },
            )
            .await
            .expect("file");
        let user = store.create_user("a05", "hash", true).await.expect("user");
        let state = AppState::new(
            "test".into(),
            store,
            crate::state::Dirs {
                artwork: root.path().join("artwork"),
                transcode: root.path().join("transcode"),
                cache: root.path().join("cache"),
                subs: root.path().join("subs"),
                runtime_cache: root.path().join("runtime"),
                renditions: root.path().join("renditions"),
            },
            "test-node".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(8)),
        );
        let file = state
            .store
            .get_file(id)
            .await
            .expect("file read")
            .expect("file");
        (state, user, file, root)
    }

    #[tokio::test]
    async fn a05_actual_decision_keeps_negative_candidate_public_and_manual_but_not_auto() {
        use axum::{
            extract::{Path, Query, State},
            http::HeaderMap,
        };
        let (state, user, file, _root) = actual_intake_state().await;
        state
            .store
            .put_setting(plurx_core::store::keys::PLAYBACK_DISPLAY_AWARE_AUTO, "1")
            .await
            .expect("Auto setting");
        state
            .store
            .put_setting(plurx_core::store::keys::PLAYBACK_NETWORK_PRIORS, "1")
            .await
            .expect("prior setting");
        let caps = serde_json::from_value(serde_json::json!({"v":2,
            "video":[{"codec":"h264","present":["sdr"]}],"containers":["mp4"]}))
        .expect("caps");
        let mut headers = HeaderMap::new();
        headers.insert("user-agent", "Mozilla/5.0".parse().expect("UA"));
        let remote = "192.168.4.9:1234".parse().expect("peer");
        let q = crate::http::stream::Caps {
            caps_v2: Some(caps),
            force: Some("original".into()),
            ..Default::default()
        };
        let before = crate::http::stream::decision(
            crate::http::extract::AuthUser(user.clone()),
            State(state.clone()),
            Path(file.id),
            Query(q.clone()),
            headers.clone(),
            crate::http::network::RemoteAddress(Some(remote)),
        )
        .await
        .expect("manual decision")
        .0;
        let selected = before
            .quality_candidate_id
            .expect("real selected candidate");
        let catalog = before.quality_candidates.expect("real public catalog");
        let candidate = catalog
            .iter()
            .find(|row| row.id == selected)
            .expect("selected in menu");
        let mut network = crate::http::network::identity(&headers, Some(remote)).expect("network");
        network.user_id = Some(user.id);
        network.credential_generation = Some(plurx_core::domain::CredentialGeneration::derive(
            user.id,
            user.created_at,
            &user.password_hash,
        ));
        let source = binding(&network, &file, candidate.recipe_digest, candidate.route)
            .await
            .expect("exact source");
        let now = crate::media_sessions::unix_ms();
        state
            .store
            .observe_candidate_link(
                &CandidateLinkObservation {
                    binding: source,
                    body_bytes: 4096,
                    body_duration_ms: 5000,
                    completed_at_ms: now,
                    negative: true,
                },
                now,
            )
            .await
            .expect("exact negative");
        let manual = crate::http::stream::decision(
            crate::http::extract::AuthUser(user.clone()),
            State(state.clone()),
            Path(file.id),
            Query(q.clone()),
            headers.clone(),
            crate::http::network::RemoteAddress(Some(remote)),
        )
        .await
        .expect("manual with negative")
        .0;
        assert_eq!(manual.quality_candidate_id, Some(selected));
        assert_eq!(
            serde_json::to_value(&manual.quality_candidates).expect("menu"),
            serde_json::to_value(&catalog).expect("original menu")
        );
        let auto = crate::http::stream::decision(
            crate::http::extract::AuthUser(user),
            State(state),
            Path(file.id),
            Query(crate::http::stream::Caps {
                force: Some("auto".into()),
                ..q
            }),
            headers,
            crate::http::network::RemoteAddress(Some(remote)),
        )
        .await
        .expect("Auto with negative")
        .0;
        assert_ne!(
            auto.quality_candidate_id,
            Some(selected),
            "exact negative excluded only from Auto admission"
        );
        assert!(auto
            .quality_candidates
            .expect("Auto public menu")
            .iter()
            .any(|row| row.id == selected));
    }

    #[tokio::test]
    async fn a05_real_intake_rechecks_retired_or_changed_owner_after_source_await_without_claim() {
        use plurx_core::domain::{
            MediaSessionActivation, MediaSessionActivationSettlement, MediaSessionTakeover,
        };
        for retire in [true, false] {
            let (state, user, file, _root) = actual_intake_state().await;
            let now = crate::media_sessions::unix_ms();
            let activation = MediaSessionActivation {
                incarnation_id: uuid::Uuid::new_v4().to_string(),
                session_id: uuid::Uuid::new_v4().to_string(),
                user_id: user.id,
                playback_id: "player".into(),
                recovery_epoch: String::new(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: false,
                request_id: None,
                request_fingerprint: "a".repeat(64),
                owner_node_id: state.node_id.clone(),
                recipe_json: "{}".into(),
                response_json: "{}".into(),
                publication_ready_at_ms: plurx_core::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
                media_origin_ms: 0,
                now_ms: now,
                lease_expires_at_ms: now + 60000,
                expected_desired_revision: None,
            };
            state
                .store
                .activate_media_session(&activation)
                .await
                .expect("activate")
                .expect("activation accepted");
            state
                .store
                .settle_media_session_activation(
                    &activation,
                    MediaSessionActivationSettlement::Confirm {
                        publication_ready_at_ms: 0,
                    },
                    now,
                )
                .await
                .expect("confirm")
                .expect("published");
            let network = NetworkIdentity {
                user_id: Some(user.id),
                credential_generation: Some(plurx_core::domain::CredentialGeneration::derive(
                    user.id,
                    user.created_at,
                    &user.password_hash,
                )),
                client_class: "web".into(),
                network_fingerprint: "192.168.4.0/24".into(),
            };
            let source = binding(&network, &file, [4; 32], CandidateRoute::Encode)
                .await
                .expect("physical source");
            let session = SessionBinding {
                source,
                session: activation.session_id.clone(),
                incarnation: activation.incarnation_id.clone(),
                owner_epoch: 1,
            };
            state.link_receipts.register(session.clone());
            let (nonce, eof) = state
                .link_receipts
                .mint(
                    &session.session,
                    "seg00001.m4s",
                    "etag",
                    4096,
                    Some(4000),
                    true,
                )
                .expect("mint");
            let original_eof = Instant::now();
            eof(original_eof, now);
            let mut sample = ClientLinkSample {
                receipt: nonce.clone(),
                object_name: "seg00001.m4s".into(),
                etag: "etag".into(),
                body_bytes: 4096,
                body_duration_ms: 5000,
                age_ms: 0,
                network_load: Some(true),
                from_cache: Some(false),
                producer_paced: Some(false),
                cause: NetworkPriorCause::Link,
                negative: false,
                media_duration_ms: Some(4000),
                presenting: true,
                stalled: true,
                runway_ms: 1000,
            };
            assert!(
                state
                    .link_receipts
                    .accept(&state, &network, Some(&session.session), &sample)
                    .await
                    .is_some(),
                "unraced real intake must accept the raw claim"
            );
            sample.negative = true;
            let pause = state.link_receipts.1.arm("intake after source validation");
            let task = {
                let state = state.clone();
                let id = session.session.clone();
                tokio::spawn(async move {
                    state
                        .link_receipts
                        .accept(&state, &network, Some(&id), &sample)
                        .await
                })
            };
            let held = pause.reached().await;
            if retire {
                state
                    .store
                    .end_media_session(&session.session, "deleted", now)
                    .await
                    .expect("retire")
                    .expect("retired");
            } else {
                // Real storage CAS with its explicit server-clock input; no sleep,
                // registry age reset or claim-time replacement is used.
                state
                    .store
                    .claim_media_session_takeover(&MediaSessionTakeover {
                        incarnation_id: session.incarnation.clone(),
                        expected_owner_node_id: state.node_id.clone(),
                        expected_owner_epoch: 1,
                        next_owner_node_id: "replacement-node".into(),
                        now_ms: now + 60001,
                        lease_expires_at_ms: now + 120000,
                    })
                    .await
                    .expect("takeover")
                    .expect("changed epoch");
            }
            held.release();
            assert!(task.await.expect("intake task").is_none());
            let rows = state.link_receipts.0.lock().expect("receipts");
            let row = rows.receipts.get(&nonce).expect("same receipt");
            assert_eq!(
                row.completion,
                Some((original_eof, now)),
                "authority refusal cannot refresh EOF"
            );
            assert!(
                row.raw
                    .as_ref()
                    .is_some_and(|raw| raw.bytes == 4096 && raw.duration_ms == 5000),
                "refused negative must preserve original raw claim"
            );
            assert!(
                !row.negative_claimed,
                "refused claim must not consume negative stage"
            );
        }
    }
    #[test]
    fn a05_unknown_original_catalog_exposure_preserves_exact_cost_distinction() {
        use plurx_core::playback::candidate::{CandidateId, QualityCandidate};
        let digest = [12; 32];
        let original = QualityCandidate {
            id: CandidateId::for_recipe_digest(digest),
            recipe_digest: digest,
            route: CandidateRoute::Remux,
            normalized_geometry: true,
            width: 3840,
            height: 2160,
            target_height: 2160,
            average_bps: Some(90_000_000),
            peak_bps: None,
            grade: plurx_core::transcode::OutputGrade::Sdr,
            decoder_compatible: true,
            complete_cache: false,
            sustainable: true,
        };
        let output = crate::vodserve::retained::MeasuredCandidateOutput {
            candidate_id: original.id,
            recipe_digest: digest,
            route: original.route,
            artifact_id: uuid::Uuid::new_v4().to_string(),
            output_identity: "a".repeat(64),
            qualification: "complete_full_mux_rfc8216_v1",
            average_bps: 12_000_000,
            peak_bps: 20_000_000,
        };
        assert!(
            unknown_stageable_original(&original, &[]),
            "source average is not whole-output peak"
        );
        assert!(
            !measured_margin(&original, 30_000_000, &[]),
            "trial exposure is not a recommendation"
        );
        for change in 0..5 {
            let mut candidate = original.clone();
            match change {
                0 => candidate.route = CandidateRoute::Encode,
                1 => candidate.route = CandidateRoute::Original,
                2 => candidate.decoder_compatible = false,
                3 => candidate.peak_bps = Some(20_000_000),
                _ => candidate.recipe_digest[31] ^= 1,
            }
            assert!(
                !unknown_stageable_original(&candidate, &[]),
                "case {change}"
            );
        }
        assert!(!unknown_stageable_original(
            &original,
            std::slice::from_ref(&output)
        ));
        assert!(!measured_margin(
            &original,
            30_000_000,
            std::slice::from_ref(&output)
        ));
        let mut other = output.clone();
        other.recipe_digest[31] ^= 1;
        assert!(
            unknown_stageable_original(&original, &[other]),
            "full digest, not shortened ID, identifies cost"
        );
        let mut fitting = output;
        fitting.peak_bps = 14_000_000;
        assert!(measured_margin(&original, 30_000_000, &[fitting]));
    }

    #[test]
    fn a05_warm_server_margin_uses_qualified_full_output_not_planned_candidate_rates() {
        use plurx_core::playback::candidate::{CandidateId, QualityCandidate};
        let digest = [4; 32];
        let candidate = QualityCandidate {
            id: CandidateId::for_recipe_digest(digest),
            recipe_digest: digest,
            route: CandidateRoute::Encode,
            normalized_geometry: true,
            width: 1920,
            height: 1080,
            target_height: 1080,
            average_bps: Some(1),
            peak_bps: Some(1),
            grade: plurx_core::transcode::OutputGrade::Sdr,
            decoder_compatible: true,
            complete_cache: true,
            sustainable: true,
        };
        assert!(
            !measured_margin(&candidate, 30_000_000, &[]),
            "planned/cache fields do not prove wire cost"
        );
        let mut output = crate::vodserve::retained::MeasuredCandidateOutput {
            candidate_id: candidate.id,
            recipe_digest: digest,
            route: candidate.route,
            artifact_id: "00000000-0000-0000-0000-000000000001".to_owned(),
            output_identity: "a".repeat(64),
            qualification: "complete_full_mux_rfc8216_v1",
            average_bps: 12_000_000,
            peak_bps: 14_000_000,
        };
        assert!(measured_margin(&candidate, 30_000_000, &[output.clone()]));
        output.peak_bps = 20_000_000;
        assert!(!measured_margin(&candidate, 30_000_000, &[output.clone()]));
        output.peak_bps = 14_000_000;
        output.recipe_digest[0] = 5;
        assert!(!measured_margin(&candidate, 30_000_000, &[output.clone()]));
        output.recipe_digest = digest;
        output.qualification = "planned";
        assert!(!measured_margin(&candidate, 30_000_000, &[output.clone()]));
        output.qualification = "complete_full_mux_rfc8216_v1";
        assert!(!measured_margin(
            &candidate,
            30_000_000,
            &[output.clone(), output]
        ));
    }
    #[test]
    fn a05_exact_nonce_proof_never_borrows_faster_body_or_renews_during_async_work() {
        let registry = Arc::new(LinkReceipts::default());
        let binding = session();
        registry.register(binding.clone());
        let (first, complete_first) = registry
            .mint("session", "seg00001.m4s", "etag-a", 4096, Some(4000), true)
            .expect("first immutable body");
        let (second, complete_second) = registry
            .mint("session", "seg00002.m4s", "etag-b", 4096, Some(4000), true)
            .expect("second immutable body");
        complete_first(Instant::now(), 100_000);
        complete_second(Instant::now(), 100_001);
        {
            let mut rows = registry.0.lock().expect("rows");
            rows.receipts.get_mut(&first).expect("first").raw = Some(RawBody {
                bytes: 4096,
                duration_ms: 5000,
            });
            rows.receipts.get_mut(&second).expect("second").raw = Some(RawBody {
                bytes: 4096,
                duration_ms: 1,
            });
        }
        let mut proof = registry
            .exact_nonce_proof(&first, &binding)
            .expect("exact proof");
        assert_eq!(proof.transfer().expect("fresh").elapsed_ms, 5000);
        assert_eq!(
            proof.incumbent_recipe(),
            (binding.source.recipe_digest, binding.source.route)
        );
        let mut other_attachment = binding.clone();
        other_attachment.incarnation = "another-installed-item".to_owned();
        assert!(registry
            .exact_nonce_proof(&first, &other_attachment)
            .is_none());
        proof.completed = Instant::now() - Duration::from_secs(16);
        assert!(
            proof.transfer().is_none(),
            "work after capture cannot renew EOF"
        );
        assert!(registry.exact_nonce_proof(&second, &binding).is_some());
        registry
            .0
            .lock()
            .expect("rows")
            .receipts
            .get_mut(&first)
            .expect("first")
            .completion = Some((Instant::now() - Duration::from_secs(16), 100_000));
        assert!(
            registry.exact_nonce_proof(&first, &binding).is_none(),
            "fresh other body cannot replace requested nonce"
        );
    }
    #[test]
    fn a05_http_incumbent_nonce_refuses_duplicate_noncanonical_or_missing_headers() {
        use axum::http::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        assert!(requested_receipt(&headers).is_none());
        let nonce = "00000000-0000-0000-0000-00000000000a";
        headers.insert("x-plurx-link-receipt", HeaderValue::from_static(nonce));
        assert_eq!(requested_receipt(&headers), Some(nonce));
        headers.append("x-plurx-link-receipt", HeaderValue::from_static(nonce));
        assert!(
            requested_receipt(&headers).is_none(),
            "duplicates are not merged"
        );
        headers.insert(
            "x-plurx-link-receipt",
            HeaderValue::from_static("00000000-0000-0000-0000-00000000000A"),
        );
        assert!(
            requested_receipt(&headers).is_none(),
            "canonical lowercase only"
        );
        headers.insert(
            "x-plurx-link-receipt",
            HeaderValue::from_static("0000000000000000000000000000000a"),
        );
        assert!(
            requested_receipt(&headers).is_none(),
            "no alternative UUID spelling"
        );
        headers.insert(
            "x-plurx-link-receipt",
            HeaderValue::from_static(
                "00000000-0000-0000-0000-00000000000a,00000000-0000-0000-0000-00000000000a",
            ),
        );
        assert!(
            requested_receipt(&headers).is_none(),
            "comma joining is Unknown"
        );
    }
    fn session() -> SessionBinding {
        SessionBinding {
            source: CandidateLinkBinding {
                user_id: 1,
                credential_generation: "a".repeat(64),
                client_class: "web".into(),
                network_fingerprint: "network".into(),
                file_id: 2,
                source_size: 4096,
                source_mtime: 3,
                source_object_version: "object:v1".into(),
                recipe_digest: [4; 32],
                route: CandidateRoute::Encode,
            },
            session: "session".into(),
            incarnation: "incarnation".into(),
            owner_epoch: 1,
        }
    }
    #[test]
    fn a05_exact_negative_requires_two_immutable_claims_with_original_eof() {
        let eof = Instant::now();
        let mut row = Receipt {
            binding: session(),
            expected_bytes: 4096,
            media_duration_ms: Some(4000),
            object_name: "seg00001.m4s".into(),
            etag: "etag".into(),
            born: eof,
            completion: Some((eof, 100_000)),
            raw: None,
            negative_claimed: false,
        };
        let mut sample = ClientLinkSample {
            receipt: uuid::Uuid::new_v4().to_string(),
            object_name: "seg00001.m4s".into(),
            etag: "etag".into(),
            body_bytes: 4096,
            body_duration_ms: 5000,
            age_ms: 0,
            network_load: Some(true),
            from_cache: Some(false),
            producer_paced: Some(false),
            cause: NetworkPriorCause::Link,
            negative: true,
            media_duration_ms: Some(4000),
            presenting: true,
            stalled: true,
            runway_ms: 1000,
        };
        assert!(
            row.claim(&sample, eof).is_none(),
            "negative cannot create raw acquisition"
        );
        sample.negative = false;
        sample.cause = NetworkPriorCause::Encode;
        assert!(row.claim(&sample, eof).is_none());
        sample.cause = NetworkPriorCause::Link;
        sample.from_cache = Some(true);
        assert!(row.claim(&sample, eof).is_none());
        sample.from_cache = Some(false);
        let positive = row.claim(&sample, eof).expect("raw claim");
        assert_eq!(positive.completed_at_ms, 100_000);
        assert!(row.claim(&sample, eof).is_none(), "raw replay");
        sample.negative = true;
        sample.body_duration_ms = 5001;
        assert!(
            row.claim(&sample, eof).is_none(),
            "same nonce cannot replace sample"
        );
        sample.body_duration_ms = 5000;
        sample.media_duration_ms = Some(1);
        assert!(
            row.claim(&sample, eof).is_none(),
            "client cannot invent segment cost"
        );
        sample.media_duration_ms = Some(4000);
        let negative = row
            .claim(&sample, eof + Duration::from_secs(14))
            .expect("later exact negative");
        assert_eq!(negative.completed_at_ms, 100_000);
        assert!(
            row.claim(&sample, eof + Duration::from_secs(14)).is_none(),
            "negative replay"
        );
        row.negative_claimed = false;
        assert!(
            row.claim(&sample, eof + Duration::from_secs(16)).is_none(),
            "original EOF ages out"
        );
    }
    #[test]
    fn a05_live_positive_requires_original_accepted_eof_and_exact_owner() {
        let registry = Arc::new(LinkReceipts::default());
        let binding = session();
        registry.register(binding.clone());
        let (nonce, completed) = registry
            .mint("session", "seg00001.m4s", "etag", 4096, Some(4000), true)
            .expect("pending receipt");
        {
            let mut rows = registry.0.lock().expect("rows");
            rows.receipts.get_mut(&nonce).expect("pending").raw = Some(RawBody {
                bytes: 4096,
                duration_ms: 5000,
            });
        }
        assert!(registry.fresh_positive(&binding).is_none());
        completed(Instant::now(), 100_000);
        assert!(registry.fresh_positive(&binding).is_some());
        let mut wrong = binding.clone();
        wrong.owner_epoch += 1;
        assert!(registry.fresh_positive(&wrong).is_none());
        let restarted = LinkReceipts::default();
        assert!(restarted.fresh_positive(&binding).is_none());
        registry
            .0
            .lock()
            .expect("rows")
            .receipts
            .get_mut(&nonce)
            .expect("receipt")
            .completion = Some((Instant::now() - Duration::from_secs(16), 100_000));
        assert!(registry.fresh_positive(&binding).is_none());
    }
    #[test]
    fn a05_nonce_mint_refuses_partial_init_and_unknown_session_without_authority_effect() {
        let registry = Arc::new(LinkReceipts::default());
        registry.register(session());
        assert!(registry
            .mint("session", "init.mp4", "etag", 4096, None, true)
            .is_none());
        assert!(registry
            .mint("session", "seg00001.m4s", "etag", 4096, None, false)
            .is_none());
        assert!(registry
            .mint("unknown", "seg00001.m4s", "etag", 4096, None, true)
            .is_none());
        assert!(registry.0.lock().expect("rows").receipts.is_empty());
    }
}
