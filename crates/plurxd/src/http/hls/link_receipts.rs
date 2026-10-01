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
    sessions: HashMap<String, SessionRow>,
    receipts: HashMap<String, Receipt>,
}
#[derive(Default)]
pub(crate) struct LinkReceipts(Mutex<Rows>);

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
        let captured = {
            let rows = self.0.lock().ok()?;
            rows.receipts.get(&sample.receipt)?.binding.clone()
        };
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
        if !Self::same_route(&captured, &route, &state.node_id) {
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
) -> Option<Vec<crate::vodserve::retained::MeasuredCandidateOutput>> {
    if !accepted.iter().any(|entry| entry.node_id == state.node_id) {
        return None;
    }
    let projected = tokio::time::timeout(
        Duration::from_millis(100),
        state.transcode.quality_candidates_with_measured_outputs(
            file,
            &request.caps,
            request.audio_index,
            request.audio_offset_ms,
            request.subtitle_burn,
            request.presentation,
            request.copy_contract,
            request.audio_delivery.as_ref(),
            request.audio_claim.as_ref(),
        ),
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
        })
        .cloned()
        .collect();
    if permitted.is_empty() {
        catalog
    } else {
        permitted
    }
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
