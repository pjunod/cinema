//! Private media-capability directory and diagnostics-only placement offers.
//!
//! P4 observes and ranks; it deliberately does not forward or start playback.
//! Snapshot polling never touches source paths, and offer fan-out only asks a
//! node to inspect replicated facts, recent readability, and its local cache.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{stream, StreamExt};
use plurx_core::cluster::membership::{ActivityPeer, MembershipError, MembershipManager};
use plurx_core::domain::MediaFile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::RwLock;

use crate::http::peer_transport::{deadline_after, PeerAuthMode, PeerTransport};
use crate::state::AppState;

pub(crate) const SNAPSHOT_PATH: &str = "/internal/v1/media/snapshot";
pub(crate) const OFFERS_PATH: &str = "/internal/v1/media/offers";
pub(crate) const PROTOCOL_VERSION: i64 = 1;
pub(crate) const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(10);
pub(crate) const SNAPSHOT_DEADLINE: Duration = Duration::from_secs(2);
pub(crate) const SNAPSHOT_EXPIRY: Duration = Duration::from_secs(15);
pub(crate) const OFFER_DEADLINE: Duration = Duration::from_millis(200);
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
const MAX_OFFER_BYTES: usize = 64 * 1024;
pub(crate) const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_PEERS: usize = 64;
const MAX_CAPABILITIES: usize = 16;
const MAX_CAPABILITY_BYTES: usize = 64;
const MAX_TRACK_INDEX: i64 = 1_024;
const MAX_START_MILLIS: i64 = 366 * 24 * 60 * 60 * 1_000;
const ROOT_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const ROOT_READABILITY_TTL: Duration = Duration::from_secs(45);
const ROOT_PROBE_COLLECTION_DEADLINE: Duration = Duration::from_secs(2);
const MAX_LIBRARY_ROOTS: usize = 64;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PressureBand {
    #[default]
    Idle,
    Moderate,
    High,
    Unavailable,
}

impl PressureBand {
    fn preference(self) -> u8 {
        match self {
            Self::Unavailable => 0,
            Self::High => 1,
            Self::Moderate => 2,
            Self::Idle => 3,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct EncoderOfferCapability {
    pub family: String,
    pub max_target_height: i64,
    pub decoders: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToneMapOfferCapability {
    pub pipeline: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MediaNodeSnapshot {
    pub node_id: String,
    pub observed_at_unix_ms: i64,
    pub build: String,
    pub protocol_version: i64,
    pub encoders: Vec<EncoderOfferCapability>,
    pub tone_map: Vec<ToneMapOfferCapability>,
    pub hardware_slots_used: u32,
    pub hardware_slots_max: u32,
    pub software_threads_used: u32,
    pub software_threads_max: u32,
    pub scratch_bytes_free: u64,
    pub scratch_pressure: PressureBand,
    pub source_io_pressure: PressureBand,
    pub egress_pressure: PressureBand,
    pub live_waiting: bool,
    pub background_active: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct MediaOfferRequest {
    pub protocol_version: i64,
    pub request_fingerprint: String,
    pub file_id: i64,
    pub source_size: i64,
    pub source_mtime: i64,
    pub decoder: String,
    pub target_height: i64,
    pub start_millis: i64,
    pub audio_index: Option<i64>,
    pub subtitle_index: Option<i64>,
    pub hdr10: bool,
    pub output_contract: String,
    pub scratch_bytes_required: u64,
}

impl MediaOfferRequest {
    pub(crate) fn new(
        file: &MediaFile,
        target_height: i64,
        start_millis: i64,
        audio_index: Option<i64>,
        subtitle_index: Option<i64>,
        hdr10: bool,
    ) -> Result<Self, &'static str> {
        let decoder = decoder_contract(file).ok_or("unsupported_decoder")?;
        let scratch_bytes_required = scratch_estimate(file, target_height);
        let mut request = Self {
            protocol_version: PROTOCOL_VERSION,
            request_fingerprint: String::new(),
            file_id: file.id,
            source_size: file.size,
            source_mtime: file.mtime,
            decoder: decoder.to_owned(),
            target_height,
            start_millis,
            audio_index,
            subtitle_index,
            hdr10,
            output_contract: "hls-mpegts-v1".to_owned(),
            scratch_bytes_required,
        };
        request.request_fingerprint = request.fingerprint();
        request
            .is_valid()
            .then_some(request)
            .ok_or("invalid_request")
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.protocol_version == PROTOCOL_VERSION
            && self.file_id > 0
            && self.source_size >= 0
            && self.source_mtime >= 0
            && (crate::transcode::MIN_HEIGHT..=crate::transcode::MAX_HEIGHT)
                .contains(&self.target_height)
            && (0..=MAX_START_MILLIS).contains(&self.start_millis)
            && self
                .audio_index
                .is_none_or(|index| (0..=MAX_TRACK_INDEX).contains(&index))
            && self
                .subtitle_index
                .is_none_or(|index| (0..=MAX_TRACK_INDEX).contains(&index))
            && self.decoder.len() <= MAX_CAPABILITY_BYTES
            && !self.decoder.is_empty()
            && self.output_contract == "hls-mpegts-v1"
            && self.scratch_bytes_required > 0
            && self.request_fingerprint.len() == 64
            && self.request_fingerprint == self.fingerprint()
    }

    fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        for value in [
            self.protocol_version.to_string(),
            self.file_id.to_string(),
            self.source_size.to_string(),
            self.source_mtime.to_string(),
            self.decoder.clone(),
            self.target_height.to_string(),
            self.start_millis.to_string(),
            self.audio_index
                .map_or_else(String::new, |value| value.to_string()),
            self.subtitle_index
                .map_or_else(String::new, |value| value.to_string()),
            u8::from(self.hdr10).to_string(),
            self.output_contract.clone(),
            self.scratch_bytes_required.to_string(),
        ] {
            hasher.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
            hasher.update(value.as_bytes());
        }
        hex::encode(hasher.finalize())
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MediaDeliveryMethod {
    Transcode,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub(crate) struct MediaOffer {
    pub node_id: String,
    pub request_fingerprint: String,
    pub eligible: bool,
    pub refusal_code: Option<String>,
    pub cache_hit: bool,
    pub source_likely_readable: bool,
    pub scratch_bytes_required: u64,
    pub scratch_bytes_free: u64,
    pub source_io_pressure: PressureBand,
    pub egress_pressure: PressureBand,
    pub method: MediaDeliveryMethod,
    pub encoder: String,
    pub pipeline: String,
    pub free_hardware_slots: u32,
    pub free_software_threads: u32,
    pub recent_speed: Option<f64>,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub(crate) struct PlacementDiagnostics {
    pub request_fingerprint: String,
    pub budget_ms: u64,
    pub selected_node_id: Option<String>,
    pub offers: Vec<MediaOffer>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct MediaDirectoryDiagnostics {
    pub protocol_version: i64,
    pub snapshot_interval_seconds: u64,
    pub snapshot_expiry_seconds: u64,
    pub nodes: Vec<MediaNodeSnapshot>,
}

#[derive(Clone, Debug)]
struct CachedSnapshot {
    snapshot: MediaNodeSnapshot,
    expires_at: tokio::time::Instant,
}

#[derive(Clone, Copy, Debug)]
struct RootReadability {
    readable: bool,
    observed_at: tokio::time::Instant,
}

struct RootProbe {
    child: tokio::process::Child,
    kill_sent: bool,
}

#[derive(Default)]
struct RootProbeRegistry {
    running: BTreeMap<PathBuf, RootProbe>,
}

#[derive(Clone)]
pub(crate) struct MediaPool {
    membership: MembershipManager,
    transport: PeerTransport,
    snapshots: Arc<RwLock<BTreeMap<String, CachedSnapshot>>>,
    root_readability: Arc<RwLock<BTreeMap<PathBuf, RootReadability>>>,
    root_probes: Arc<Mutex<RootProbeRegistry>>,
}

impl MediaPool {
    pub(crate) fn new(membership: MembershipManager) -> Arc<Self> {
        Arc::new(Self {
            transport: PeerTransport::new(membership.clone()),
            membership,
            snapshots: Arc::new(RwLock::new(BTreeMap::new())),
            root_readability: Arc::new(RwLock::new(BTreeMap::new())),
            root_probes: Arc::new(Mutex::new(RootProbeRegistry::default())),
        })
    }

    pub(crate) async fn poll_loop(self: Arc<Self>) {
        let mut interval = tokio::time::interval(SNAPSHOT_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Err(error) = self.poll_once().await {
                tracing::debug!(%error, "media snapshot poll unavailable");
            }
        }
    }

    /// Refresh root-scoped source facts independently of placement traffic.
    /// Offers only read this memory; they never wake every candidate NAS.
    pub(crate) async fn root_readability_loop(
        self: Arc<Self>,
        store: Arc<dyn plurx_core::store::Store>,
    ) {
        let mut interval = tokio::time::interval(ROOT_REFRESH_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            self.refresh_root_readability(store.as_ref()).await;
        }
    }

    async fn refresh_root_readability(&self, store: &dyn plurx_core::store::Store) {
        let peers = match tokio::time::timeout(
            ROOT_PROBE_COLLECTION_DEADLINE,
            self.membership.activity_peers(),
        )
        .await
        {
            Ok(Ok(peers)) => peers,
            Ok(Err(error)) => {
                tracing::debug!(%error, "media root probe membership unavailable");
                return;
            }
            Err(_) => {
                tracing::debug!("media root probe membership timed out");
                return;
            }
        };
        if !has_reachable_media_peer(&peers) {
            self.root_readability.write().await.clear();
            return;
        }
        let libraries = match tokio::time::timeout(
            ROOT_PROBE_COLLECTION_DEADLINE,
            store.list_libraries(),
        )
        .await
        {
            Ok(Ok(libraries)) => libraries,
            Ok(Err(error)) => {
                tracing::debug!(%error, "media root readability inventory unavailable");
                return;
            }
            Err(_) => {
                tracing::debug!("media root readability inventory timed out");
                return;
            }
        };
        let roots = bounded_absolute_roots(libraries.into_iter().flat_map(|library| library.paths));
        self.root_readability
            .write()
            .await
            .retain(|root, _| roots.contains(root));
        let mut outcomes = self.reap_root_probes();
        outcomes.extend(self.start_root_probes(&roots));
        let deadline = deadline_after(ROOT_PROBE_COLLECTION_DEADLINE);
        while tokio::time::Instant::now() < deadline && self.has_running_root_probe(&roots) {
            tokio::time::sleep(Duration::from_millis(20)).await;
            outcomes.extend(self.reap_root_probes());
        }
        self.signal_timed_out_root_probes(&roots);
        outcomes.extend(self.reap_root_probes());
        let mut readability = self.root_readability.write().await;
        for (root, fact) in outcomes {
            if roots.contains(&root) {
                readability.insert(root, fact);
            }
        }
    }

    fn start_root_probes(&self, roots: &BTreeSet<PathBuf>) -> Vec<(PathBuf, RootReadability)> {
        let mut immediate = Vec::new();
        let mut registry = self
            .root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for root in roots {
            if registry.running.contains_key(root) || registry.running.len() >= MAX_LIBRARY_ROOTS {
                continue;
            }
            match spawn_library_root_probe(root) {
                Ok(child) => {
                    registry.running.insert(
                        root.clone(),
                        RootProbe {
                            child,
                            kill_sent: false,
                        },
                    );
                }
                Err(error) => {
                    tracing::debug!(%error, path = %root.display(), "could not start media root probe");
                    immediate.push((
                        root.clone(),
                        RootReadability {
                            readable: false,
                            observed_at: tokio::time::Instant::now(),
                        },
                    ));
                }
            }
        }
        immediate
    }

    fn reap_root_probes(&self) -> Vec<(PathBuf, RootReadability)> {
        let mut registry = self
            .root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let completed = registry
            .running
            .iter_mut()
            .filter_map(|(root, probe)| match probe.child.try_wait() {
                Ok(Some(status)) => {
                    // Once the common deadline fired this observation is
                    // permanently failed, even if the process happened to
                    // finish successfully before the later reap. A delayed
                    // success must never acquire a fresh TTL.
                    Some((root.clone(), status.success() && !probe.kill_sent))
                }
                Ok(None) => None,
                Err(error) => {
                    tracing::debug!(%error, path = %root.display(), "could not reap media root probe");
                    if !probe.kill_sent {
                        let _ = probe.child.start_kill();
                        probe.kill_sent = true;
                    }
                    None
                }
            })
            .collect::<Vec<_>>();
        for (root, _) in &completed {
            registry.running.remove(root);
        }
        let observed_at = tokio::time::Instant::now();
        completed
            .into_iter()
            .map(|(root, readable)| {
                (
                    root,
                    RootReadability {
                        readable,
                        observed_at,
                    },
                )
            })
            .collect()
    }

    fn has_running_root_probe(&self, roots: &BTreeSet<PathBuf>) -> bool {
        self.root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .running
            .iter()
            .any(|(root, probe)| roots.contains(root) && !probe.kill_sent)
    }

    fn signal_timed_out_root_probes(&self, roots: &BTreeSet<PathBuf>) {
        let mut registry = self
            .root_probes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (root, probe) in &mut registry.running {
            if roots.contains(root) && !probe.kill_sent {
                let _ = probe.child.start_kill();
                probe.kill_sent = true;
            }
        }
    }

    async fn root_likely_readable(&self, path: &Path) -> bool {
        let now = tokio::time::Instant::now();
        self.root_readability
            .read()
            .await
            .iter()
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.components().count())
            .is_some_and(|(_, fact)| {
                fact.readable && now.duration_since(fact.observed_at) <= ROOT_READABILITY_TTL
            })
    }

    async fn poll_once(&self) -> Result<(), MembershipError> {
        let peers = self
            .membership
            .activity_peers()
            .await?
            .into_iter()
            .filter(|peer| peer.reachable && peer.http_base.is_some())
            .take(MAX_PEERS)
            .collect::<Vec<_>>();
        if peers.is_empty() {
            self.expire().await;
            return Ok(());
        }
        let deadline = deadline_after(SNAPSHOT_DEADLINE);
        let transport = self.transport.clone();
        let outcomes = stream::iter(peers.into_iter().map(|peer| {
            let transport = transport.clone();
            async move {
                let node_id = peer.node_id.clone();
                let snapshot = fetch_snapshot(&transport, peer, deadline).await;
                (node_id, snapshot)
            }
        }))
        // Start every bounded peer immediately under the one shared deadline.
        // Eight blackholed peers must not prevent a healthy ninth from ever
        // getting a request.
        .buffer_unordered(snapshot_fanout_concurrency())
        .collect::<Vec<_>>()
        .await;
        let now = tokio::time::Instant::now();
        let mut snapshots = self.snapshots.write().await;
        snapshots.retain(|_, cached| now <= cached.expires_at);
        for (node_id, snapshot) in outcomes {
            if let Some(snapshot) = snapshot {
                snapshots.insert(node_id, snapshot);
            }
        }
        Ok(())
    }

    async fn expire(&self) {
        let now = tokio::time::Instant::now();
        self.snapshots
            .write()
            .await
            .retain(|_, cached| now <= cached.expires_at);
    }

    pub(crate) async fn diagnostics(&self, state: &AppState) -> MediaDirectoryDiagnostics {
        self.expire().await;
        let mut nodes = vec![local_snapshot(state).await];
        nodes.extend(
            self.snapshots
                .read()
                .await
                .values()
                .map(|cached| cached.snapshot.clone()),
        );
        nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));
        MediaDirectoryDiagnostics {
            protocol_version: PROTOCOL_VERSION,
            snapshot_interval_seconds: SNAPSHOT_INTERVAL.as_secs(),
            snapshot_expiry_seconds: SNAPSHOT_EXPIRY.as_secs(),
            nodes,
        }
    }

    pub(crate) async fn offers(
        &self,
        state: &AppState,
        request: MediaOfferRequest,
    ) -> PlacementDiagnostics {
        let deadline = deadline_after(OFFER_DEADLINE);
        if !request.is_valid() {
            return PlacementDiagnostics {
                request_fingerprint: request.request_fingerprint,
                budget_ms: u64::try_from(OFFER_DEADLINE.as_millis()).unwrap_or(u64::MAX),
                selected_node_id: None,
                offers: Vec::new(),
            };
        }
        self.expire().await;
        let snapshots = self
            .snapshots
            .read()
            .await
            .values()
            .map(|cached| (cached.snapshot.node_id.clone(), cached.snapshot.clone()))
            .collect::<HashMap<_, _>>();
        let (mut offers, local_snapshot) = match tokio::time::timeout_at(deadline, async {
            tokio::join!(local_offer(state, &request), local_snapshot(state))
        })
        .await
        {
            Ok((offer, snapshot)) => (vec![offer], Some(snapshot)),
            Err(_) => (Vec::new(), None),
        };
        let peers = tokio::time::timeout_at(deadline, self.membership.activity_peers())
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default()
            .into_iter()
            .filter(|peer| {
                peer.reachable
                    && peer.http_base.is_some()
                    && snapshots
                        .get(&peer.node_id)
                        .is_some_and(|snapshot| snapshot_can_answer(snapshot, &request))
            })
            .take(MAX_PEERS)
            .collect::<Vec<_>>();
        if !peers.is_empty() && tokio::time::Instant::now() < deadline {
            let body = serde_json::to_vec(&request).unwrap_or_default();
            let transport = self.transport.clone();
            let remote = stream::iter(peers.into_iter().map(|peer| {
                let transport = transport.clone();
                let body = body.clone();
                let fingerprint = request.request_fingerprint.clone();
                let scratch_bytes_required = request.scratch_bytes_required;
                async move {
                    fetch_offer(
                        &transport,
                        peer,
                        body,
                        &fingerprint,
                        scratch_bytes_required,
                        deadline,
                    )
                    .await
                }
            }))
            .buffer_unordered(MAX_PEERS)
            .filter_map(|offer| async move { offer })
            .collect::<Vec<_>>();
            let remote = tokio::time::timeout_at(deadline, remote)
                .await
                .unwrap_or_default();
            offers.extend(remote);
        }
        let mut ranking_snapshots = snapshots;
        if let Some(local_snapshot) = local_snapshot {
            ranking_snapshots.insert(state.node_id.clone(), local_snapshot);
        }
        rank_offers(
            &mut offers,
            &ranking_snapshots,
            &request.request_fingerprint,
            &state.node_id,
        );
        let selected_node_id = offers
            .first()
            .filter(|offer| offer.eligible)
            .map(|offer| offer.node_id.clone());
        PlacementDiagnostics {
            request_fingerprint: request.request_fingerprint,
            budget_ms: u64::try_from(OFFER_DEADLINE.as_millis()).unwrap_or(u64::MAX),
            selected_node_id,
            offers,
        }
    }
}

async fn fetch_snapshot(
    transport: &PeerTransport,
    peer: ActivityPeer,
    deadline: tokio::time::Instant,
) -> Option<CachedSnapshot> {
    let base = peer.http_base.as_deref()?;
    let response = transport
        .request(
            &peer.node_id,
            base,
            reqwest::Method::GET,
            SNAPSHOT_PATH,
            Vec::new(),
            deadline,
            MAX_SNAPSHOT_BYTES,
            PeerAuthMode::ExactRequest,
        )
        .await
        .ok()?;
    let snapshot = response
        .status
        .is_success()
        .then(|| serde_json::from_slice(&response.body).ok())
        .flatten()?;
    accepted_snapshot(snapshot, &peer.node_id)
}

fn bounded_absolute_roots(roots: impl IntoIterator<Item = PathBuf>) -> BTreeSet<PathBuf> {
    roots
        .into_iter()
        // An absolute root is both the storage contract and the command-line
        // grammar boundary: expression-like relative values such as
        // `-delete` must never become `find` arguments.
        .filter(|root| root.is_absolute())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .take(MAX_LIBRARY_ROOTS)
        .collect()
}

/// Spawn one tracked root probe. `-H` follows a command-line symlink root, so
/// success means the target directory was actually enumerated rather than
/// merely observing the symlink object. Callers retain the child until
/// `try_wait` confirms exit: SIGKILL cannot immediately release a process in
/// uninterruptible filesystem I/O, and dropping/resubmitting it would turn a
/// hard mount into unbounded PID growth.
fn spawn_library_root_probe(root: &Path) -> std::io::Result<tokio::process::Child> {
    debug_assert!(root.is_absolute());
    tokio::process::Command::new("find")
        .arg("-H")
        .arg(root)
        .args(["-mindepth", "1", "-maxdepth", "1", "-print", "-quit"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
}

async fn fetch_offer(
    transport: &PeerTransport,
    peer: ActivityPeer,
    body: Vec<u8>,
    fingerprint: &str,
    scratch_bytes_required: u64,
    deadline: tokio::time::Instant,
) -> Option<MediaOffer> {
    let base = peer.http_base.as_deref()?;
    let response = transport
        .request(
            &peer.node_id,
            base,
            reqwest::Method::POST,
            OFFERS_PATH,
            body,
            deadline,
            MAX_OFFER_BYTES,
            PeerAuthMode::ExactRequest,
        )
        .await
        .ok()?;
    if !response.status.is_success() {
        return None;
    }
    let offer = serde_json::from_slice::<MediaOffer>(&response.body).ok()?;
    offer_is_bounded(&offer, &peer.node_id, fingerprint, scratch_bytes_required).then_some(offer)
}

pub(crate) async fn local_snapshot(state: &AppState) -> MediaNodeSnapshot {
    let runtime = state.transcode.media_node_runtime().await;
    let scratch_pressure =
        capacity_pressure(runtime.scratch_bytes_free, runtime.scratch_target_bytes);
    let workload_pressure = count_pressure(runtime.active_sessions, runtime.session_pressure_limit);
    MediaNodeSnapshot {
        node_id: state.node_id.clone(),
        observed_at_unix_ms: unix_ms(),
        build: crate::version::BUILD.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        encoders: runtime
            .encoder_families
            .into_iter()
            .map(|family| EncoderOfferCapability {
                family,
                max_target_height: runtime.max_target_height,
                decoders: runtime.decoders.clone(),
            })
            .collect(),
        tone_map: runtime
            .tone_map_pipelines
            .into_iter()
            .map(|pipeline| ToneMapOfferCapability { pipeline })
            .collect(),
        hardware_slots_used: bounded_u32(runtime.hardware_slots_used),
        hardware_slots_max: bounded_u32(runtime.hardware_slots_max),
        software_threads_used: bounded_u32(runtime.software_threads_used),
        software_threads_max: bounded_u32(runtime.software_threads_max),
        scratch_bytes_free: runtime.scratch_bytes_free,
        scratch_pressure,
        source_io_pressure: workload_pressure,
        egress_pressure: workload_pressure,
        live_waiting: runtime.live_waiting,
        background_active: runtime.background_active,
    }
}

pub(crate) async fn local_offer(state: &AppState, request: &MediaOfferRequest) -> MediaOffer {
    let base = || MediaOffer {
        node_id: state.node_id.clone(),
        request_fingerprint: request.request_fingerprint.clone(),
        eligible: false,
        refusal_code: None,
        cache_hit: false,
        source_likely_readable: false,
        scratch_bytes_required: request.scratch_bytes_required,
        scratch_bytes_free: 0,
        source_io_pressure: PressureBand::Idle,
        egress_pressure: PressureBand::Idle,
        method: MediaDeliveryMethod::Transcode,
        encoder: String::new(),
        pipeline: String::new(),
        free_hardware_slots: 0,
        free_software_threads: 0,
        recent_speed: None,
    };
    if !request.is_valid() {
        return MediaOffer {
            refusal_code: Some("invalid_request".to_owned()),
            ..base()
        };
    }
    let file = match state.store.get_file(request.file_id).await {
        Ok(Some(file)) => file,
        _ => {
            return MediaOffer {
                refusal_code: Some("file_missing".to_owned()),
                ..base()
            }
        }
    };
    if file.size != request.source_size || file.mtime != request.source_mtime {
        return MediaOffer {
            refusal_code: Some("stale_source".to_owned()),
            ..base()
        };
    }
    if scratch_estimate(&file, request.target_height) != request.scratch_bytes_required {
        return MediaOffer {
            refusal_code: Some("scratch_contract_changed".to_owned()),
            ..base()
        };
    }
    if file
        .duration_ms
        .is_some_and(|duration| request.start_millis > duration.max(0))
    {
        return MediaOffer {
            refusal_code: Some("start_out_of_range".to_owned()),
            ..base()
        };
    }
    if decoder_contract(&file) != Some(request.decoder.as_str()) {
        return MediaOffer {
            refusal_code: Some("decoder_contract_changed".to_owned()),
            ..base()
        };
    }
    if !requested_tracks_exist(&file, request.audio_index, request.subtitle_index) {
        return MediaOffer {
            refusal_code: Some("track_missing".to_owned()),
            ..base()
        };
    }
    let source_likely_readable = state.availability.recently_present(file.id)
        || state.media_pool.root_likely_readable(&file.path).await;
    match state
        .transcode
        .media_offer_probe(
            &file,
            request.target_height,
            request.start_millis as f64 / 1_000.0,
            request.audio_index,
            request.subtitle_index,
            request.hdr10,
        )
        .await
    {
        Ok(probe) => {
            let source_io_pressure =
                count_pressure(probe.active_sessions, probe.session_pressure_limit);
            let egress_pressure = source_io_pressure;
            let scratch_ok = probe.scratch_bytes_free >= request.scratch_bytes_required;
            let capability_ok = probe.decoder_supported
                && probe.target_supported
                && request.output_contract == "hls-mpegts-v1";
            let pressure_ok = !matches!(egress_pressure, PressureBand::Unavailable);
            let eligible = capability_ok
                && pressure_ok
                && (probe.cache_hit || (source_likely_readable && scratch_ok));
            let refusal_code = (!eligible).then(|| {
                if !capability_ok {
                    "incapable"
                } else if !pressure_ok {
                    "egress_saturated"
                } else if !source_likely_readable && !probe.cache_hit {
                    "source_unverified"
                } else if !scratch_ok && !probe.cache_hit {
                    "scratch_full"
                } else {
                    "temporarily_busy"
                }
                .to_owned()
            });
            MediaOffer {
                eligible,
                refusal_code,
                cache_hit: probe.cache_hit,
                source_likely_readable,
                scratch_bytes_free: probe.scratch_bytes_free,
                source_io_pressure,
                egress_pressure,
                encoder: probe.encoder,
                pipeline: probe.pipeline,
                free_hardware_slots: bounded_u32(probe.free_hardware_slots),
                free_software_threads: bounded_u32(probe.free_software_threads),
                recent_speed: probe
                    .recent_speed
                    .filter(|speed| speed.is_finite() && *speed > 0.0),
                ..base()
            }
        }
        Err(code) => MediaOffer {
            refusal_code: Some(code.to_owned()),
            source_likely_readable,
            ..base()
        },
    }
}

fn has_reachable_media_peer(peers: &[ActivityPeer]) -> bool {
    peers
        .iter()
        .any(|peer| peer.reachable && peer.http_base.is_some())
}

const fn snapshot_fanout_concurrency() -> usize {
    MAX_PEERS
}

fn requested_tracks_exist(
    file: &MediaFile,
    audio_index: Option<i64>,
    subtitle_index: Option<i64>,
) -> bool {
    audio_index.is_none_or(|index| file.audio_streams.iter().any(|track| track.index == index))
        && subtitle_index.is_none_or(|index| {
            file.subtitle_streams
                .iter()
                .any(|track| track.index == index)
        })
}

fn snapshot_can_answer(snapshot: &MediaNodeSnapshot, request: &MediaOfferRequest) -> bool {
    snapshot.protocol_version == PROTOCOL_VERSION
        && snapshot.egress_pressure != PressureBand::Unavailable
        && snapshot.encoders.iter().any(|capability| {
            capability.max_target_height >= request.target_height
                && capability
                    .decoders
                    .iter()
                    .any(|decoder| decoder == &request.decoder)
        })
        && (!request.hdr10 || !snapshot.tone_map.is_empty())
}

fn snapshot_is_bounded(snapshot: &MediaNodeSnapshot, expected_node_id: &str) -> bool {
    snapshot.node_id == expected_node_id
        && !snapshot.node_id.is_empty()
        && snapshot.node_id.len() <= 256
        && snapshot.build.len() <= 256
        && snapshot.protocol_version == PROTOCOL_VERSION
        && snapshot_remaining_ttl(snapshot.observed_at_unix_ms).is_some()
        && snapshot.encoders.len() <= MAX_CAPABILITIES
        && snapshot.tone_map.len() <= MAX_CAPABILITIES
        && snapshot.encoders.iter().all(|capability| {
            !capability.family.is_empty()
                && capability.family.len() <= MAX_CAPABILITY_BYTES
                && capability.max_target_height > 0
                && capability.decoders.len() <= MAX_CAPABILITIES
                && capability
                    .decoders
                    .iter()
                    .all(|decoder| !decoder.is_empty() && decoder.len() <= MAX_CAPABILITY_BYTES)
        })
        && snapshot.tone_map.iter().all(|capability| {
            !capability.pipeline.is_empty() && capability.pipeline.len() <= MAX_CAPABILITY_BYTES
        })
}

fn accepted_snapshot(
    snapshot: MediaNodeSnapshot,
    expected_node_id: &str,
) -> Option<CachedSnapshot> {
    if !snapshot_is_bounded(&snapshot, expected_node_id) {
        return None;
    }
    let remaining = snapshot_remaining_ttl(snapshot.observed_at_unix_ms)?;
    Some(CachedSnapshot {
        snapshot,
        expires_at: tokio::time::Instant::now() + remaining,
    })
}

fn snapshot_remaining_ttl(observed_at_unix_ms: i64) -> Option<Duration> {
    let budget_ms = u64::try_from(SNAPSHOT_EXPIRY.as_millis()).unwrap_or(u64::MAX);
    let age_ms = unix_ms().abs_diff(observed_at_unix_ms);
    budget_ms.checked_sub(age_ms).map(Duration::from_millis)
}

fn offer_is_bounded(
    offer: &MediaOffer,
    expected_node_id: &str,
    fingerprint: &str,
    scratch_bytes_required: u64,
) -> bool {
    offer.node_id == expected_node_id
        && offer.node_id.len() <= 256
        && offer.request_fingerprint == fingerprint
        && offer.request_fingerprint.len() == 64
        && offer.scratch_bytes_required == scratch_bytes_required
        && offer.eligible == offer.refusal_code.is_none()
        && offer
            .refusal_code
            .as_ref()
            .is_none_or(|value| value.len() <= 64)
        && offer.encoder.len() <= MAX_CAPABILITY_BYTES
        && offer.pipeline.len() <= MAX_CAPABILITY_BYTES
        && offer
            .recent_speed
            .is_none_or(|speed| speed.is_finite() && speed > 0.0)
}

fn rank_offers(
    offers: &mut [MediaOffer],
    snapshots: &HashMap<String, MediaNodeSnapshot>,
    fingerprint: &str,
    local_node_id: &str,
) {
    offers.sort_by(|left, right| {
        compare_offer(right, left, snapshots, fingerprint, local_node_id)
            .then_with(|| left.node_id.cmp(&right.node_id))
    });
}

fn compare_offer(
    left: &MediaOffer,
    right: &MediaOffer,
    snapshots: &HashMap<String, MediaNodeSnapshot>,
    fingerprint: &str,
    local_node_id: &str,
) -> Ordering {
    let left_snapshot = snapshots.get(&left.node_id);
    let right_snapshot = snapshots.get(&right.node_id);
    let left_can_start = can_start_now(left, left_snapshot);
    let right_can_start = can_start_now(right, right_snapshot);
    left.eligible
        .cmp(&right.eligible)
        .then_with(|| left.cache_hit.cmp(&right.cache_hit))
        .then_with(|| left_can_start.cmp(&right_can_start))
        .then_with(|| pressure_score(left).cmp(&pressure_score(right)))
        .then_with(|| compare_speed(left.recent_speed, right.recent_speed))
        .then_with(|| compare_spare(left, right, left_snapshot, right_snapshot))
        .then_with(|| {
            rendezvous(fingerprint, &left.node_id).cmp(&rendezvous(fingerprint, &right.node_id))
        })
        .then_with(|| (left.node_id == local_node_id).cmp(&(right.node_id == local_node_id)))
}

fn can_start_now(offer: &MediaOffer, snapshot: Option<&MediaNodeSnapshot>) -> bool {
    offer.cache_hit
        || (offer.free_hardware_slots > 0 || offer.free_software_threads > 0)
            && snapshot.is_none_or(|snapshot| !snapshot.background_active)
}

fn pressure_score(offer: &MediaOffer) -> (u8, u8, u8) {
    (
        capacity_pressure(offer.scratch_bytes_free, offer.scratch_bytes_required).preference(),
        offer.source_io_pressure.preference(),
        offer.egress_pressure.preference(),
    )
}

fn compare_speed(left: Option<f64>, right: Option<f64>) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => left.partial_cmp(&right).unwrap_or(Ordering::Equal),
        (Some(_), None) => Ordering::Greater,
        (None, Some(_)) => Ordering::Less,
        (None, None) => Ordering::Equal,
    }
}

fn compare_spare(
    left: &MediaOffer,
    right: &MediaOffer,
    left_snapshot: Option<&MediaNodeSnapshot>,
    right_snapshot: Option<&MediaNodeSnapshot>,
) -> Ordering {
    let left_free = u64::from(left.free_hardware_slots) + u64::from(left.free_software_threads);
    let right_free = u64::from(right.free_hardware_slots) + u64::from(right.free_software_threads);
    let left_total = left_snapshot.map_or(left_free.max(1), |snapshot| {
        u64::from(snapshot.hardware_slots_max) + u64::from(snapshot.software_threads_max)
    });
    let right_total = right_snapshot.map_or(right_free.max(1), |snapshot| {
        u64::from(snapshot.hardware_slots_max) + u64::from(snapshot.software_threads_max)
    });
    left_free
        .saturating_mul(right_total.max(1))
        .cmp(&right_free.saturating_mul(left_total.max(1)))
}

fn rendezvous(fingerprint: &str, node_id: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(fingerprint.as_bytes());
    hasher.update([0]);
    hasher.update(node_id.as_bytes());
    let digest = hasher.finalize();
    u64::from_be_bytes(
        digest[..8]
            .try_into()
            .expect("SHA-256 prefix is eight bytes"),
    )
}

pub(crate) fn decoder_contract(file: &MediaFile) -> Option<&'static str> {
    match file.video_codec.as_deref()? {
        "h264" | "avc" => Some("h264"),
        "hevc" | "h265" | "hevc10" => Some("hevc"),
        "vp8" => Some("vp8"),
        "vp9" => Some("vp9"),
        "av1" => Some("av1"),
        "mpeg4" => Some("mpeg4"),
        "mpeg2video" => Some("mpeg2video"),
        _ => None,
    }
}

fn scratch_estimate(file: &MediaFile, target_height: i64) -> u64 {
    const OVERHEAD: u64 = 64 * 1024 * 1024;
    let duration_ms = u64::try_from(file.duration_ms.unwrap_or_default().max(0)).unwrap_or(0);
    let peak_kbps = match target_height {
        height if height <= 480 => 2_000_u64,
        height if height <= 720 => 4_000,
        height if height <= 1080 => 8_000,
        _ => 20_000,
    };
    duration_ms
        .saturating_mul(peak_kbps)
        .saturating_div(8)
        .saturating_mul(2)
        .saturating_add(OVERHEAD)
}

fn capacity_pressure(free: u64, target: u64) -> PressureBand {
    if free == 0 {
        PressureBand::Unavailable
    } else if target > 0 && free < target {
        PressureBand::High
    } else if target > 0 && free < target.saturating_mul(2) {
        PressureBand::Moderate
    } else {
        PressureBand::Idle
    }
}

fn count_pressure(used: usize, limit: usize) -> PressureBand {
    if limit == 0 || used >= limit {
        PressureBand::Unavailable
    } else if used.saturating_mul(4) >= limit.saturating_mul(3) {
        PressureBand::High
    } else if used.saturating_mul(2) >= limit {
        PressureBand::Moderate
    } else {
        PressureBand::Idle
    }
}

fn bounded_u32(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::domain::{AudioStream, SubtitleStream};

    fn snapshot(node: &str, decoders: &[&str], max_height: i64) -> MediaNodeSnapshot {
        MediaNodeSnapshot {
            node_id: node.to_owned(),
            observed_at_unix_ms: unix_ms(),
            build: "test".to_owned(),
            protocol_version: PROTOCOL_VERSION,
            encoders: vec![EncoderOfferCapability {
                family: "software".to_owned(),
                max_target_height: max_height,
                decoders: decoders.iter().map(|value| (*value).to_owned()).collect(),
            }],
            tone_map: Vec::new(),
            hardware_slots_used: 0,
            hardware_slots_max: 0,
            software_threads_used: 0,
            software_threads_max: 4,
            scratch_bytes_free: 1_000,
            scratch_pressure: PressureBand::Idle,
            source_io_pressure: PressureBand::Idle,
            egress_pressure: PressureBand::Idle,
            live_waiting: false,
            background_active: false,
        }
    }

    fn offer(node: &str) -> MediaOffer {
        MediaOffer {
            node_id: node.to_owned(),
            request_fingerprint: "a".repeat(64),
            eligible: true,
            refusal_code: None,
            cache_hit: false,
            source_likely_readable: true,
            scratch_bytes_required: 100,
            scratch_bytes_free: 1_000,
            source_io_pressure: PressureBand::Idle,
            egress_pressure: PressureBand::Idle,
            method: MediaDeliveryMethod::Transcode,
            encoder: "software".to_owned(),
            pipeline: "cpu".to_owned(),
            free_hardware_slots: 0,
            free_software_threads: 4,
            recent_speed: Some(2.0),
        }
    }

    fn media_file_with_tracks() -> MediaFile {
        MediaFile {
            id: 1,
            item_id: 1,
            path: PathBuf::from("/media/movie.mkv"),
            size: 1,
            mtime: 1,
            duration_ms: Some(1_000),
            container: Some("mkv".to_owned()),
            video_codec: Some("h264".to_owned()),
            video_profile: None,
            width: Some(1920),
            height: Some(1080),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            bitrate: None,
            audio_streams: vec![AudioStream {
                index: 4,
                ..AudioStream::default()
            }],
            subtitle_streams: vec![SubtitleStream {
                index: 7,
                ..SubtitleStream::default()
            }],
            scanned_at: 1,
            audio_offset_ms: 0,
            probed: true,
        }
    }

    #[test]
    fn snapshot_bounds_reject_wrong_identity_and_protocol() {
        let accepted = snapshot("peer-a", &["hevc"], 1080);
        assert!(snapshot_is_bounded(&accepted, "peer-a"));
        assert!(!snapshot_is_bounded(&accepted, "peer-b"));

        let mut incompatible = accepted;
        incompatible.protocol_version = PROTOCOL_VERSION.saturating_add(1);
        assert!(!snapshot_is_bounded(&incompatible, "peer-a"));
    }

    #[test]
    fn verified_cache_beats_source_and_pressure() {
        let mut cached = offer("cache");
        cached.cache_hit = true;
        cached.source_likely_readable = false;
        cached.source_io_pressure = PressureBand::High;
        let idle = offer("idle");
        let mut offers = vec![idle, cached];
        rank_offers(&mut offers, &HashMap::new(), &"a".repeat(64), "idle");
        assert_eq!(offers[0].node_id, "cache");
    }

    #[test]
    fn hard_refusals_and_saturated_egress_never_win() {
        let healthy = offer("healthy");
        let mut stale = offer("stale");
        stale.eligible = false;
        stale.refusal_code = Some("stale_source".to_owned());
        stale.cache_hit = true;
        let mut saturated = offer("saturated");
        saturated.eligible = false;
        saturated.egress_pressure = PressureBand::Unavailable;
        let mut offers = vec![saturated, stale, healthy];
        rank_offers(&mut offers, &HashMap::new(), &"a".repeat(64), "healthy");
        assert_eq!(offers[0].node_id, "healthy");
    }

    #[test]
    fn heterogeneous_idle_nodes_are_shortlisted_only_when_capable() {
        let request = MediaOfferRequest {
            protocol_version: PROTOCOL_VERSION,
            request_fingerprint: "a".repeat(64),
            file_id: 1,
            source_size: 1,
            source_mtime: 1,
            decoder: "hevc".to_owned(),
            target_height: 1080,
            start_millis: 0,
            audio_index: None,
            subtitle_index: None,
            hdr10: false,
            output_contract: "hls-mpegts-v1".to_owned(),
            scratch_bytes_required: 1,
        };
        assert!(snapshot_can_answer(
            &snapshot("capable", &["hevc"], 1080),
            &request
        ));
        assert!(!snapshot_can_answer(
            &snapshot("wrong-codec", &["h264"], 2160),
            &request
        ));
        assert!(!snapshot_can_answer(
            &snapshot("too-small", &["hevc"], 720),
            &request
        ));
    }

    #[test]
    fn scratch_full_stale_source_and_saturated_egress_cannot_outrank_healthy() {
        let healthy = offer("healthy");
        let mut scratch_full = offer("scratch-full");
        scratch_full.eligible = false;
        scratch_full.refusal_code = Some("scratch_full".to_owned());
        scratch_full.scratch_bytes_free = 0;
        scratch_full.recent_speed = Some(100.0);
        let mut stale = offer("stale");
        stale.eligible = false;
        stale.refusal_code = Some("stale_source".to_owned());
        stale.free_hardware_slots = 100;
        let mut saturated = offer("saturated");
        saturated.eligible = false;
        saturated.refusal_code = Some("egress_saturated".to_owned());
        saturated.egress_pressure = PressureBand::Unavailable;
        saturated.cache_hit = true;
        let mut offers = vec![scratch_full, stale, saturated, healthy];
        rank_offers(&mut offers, &HashMap::new(), &"a".repeat(64), "healthy");
        assert_eq!(offers[0].node_id, "healthy");
    }

    #[test]
    fn requested_tracks_must_exist_in_the_exact_file_snapshot() {
        let file = media_file_with_tracks();
        assert!(requested_tracks_exist(&file, Some(4), Some(7)));
        assert!(!requested_tracks_exist(&file, Some(3), Some(7)));
        assert!(!requested_tracks_exist(&file, Some(4), Some(6)));
    }

    #[test]
    fn root_probes_require_a_reachable_remote_media_consumer() {
        assert!(!has_reachable_media_peer(&[]));
        assert!(!has_reachable_media_peer(&[ActivityPeer {
            node_id: "offline".to_owned(),
            http_base: Some("http://offline:32400".to_owned()),
            reachable: false,
        }]));
        assert!(has_reachable_media_peer(&[ActivityPeer {
            node_id: "peer".to_owned(),
            http_base: Some("http://peer:32400".to_owned()),
            reachable: true,
        }]));
    }

    #[test]
    fn root_probe_command_accepts_only_bounded_absolute_paths() {
        let roots = bounded_absolute_roots([
            PathBuf::from("-delete"),
            PathBuf::from("relative/library"),
            PathBuf::from("/media/library"),
        ]);
        assert_eq!(roots, BTreeSet::from([PathBuf::from("/media/library")]));

        let bounded = bounded_absolute_roots(
            (0..=MAX_LIBRARY_ROOTS).map(|index| PathBuf::from(format!("/media/{index}"))),
        );
        assert_eq!(bounded.len(), MAX_LIBRARY_ROOTS);
    }

    #[tokio::test]
    async fn blackholed_first_eight_peers_do_not_starve_the_ninth() {
        let (started_tx, mut started_rx) = mpsc::unbounded_channel();
        let fanout = stream::iter((0..9).map(|index| {
            let started_tx = started_tx.clone();
            async move {
                if index < 8 {
                    std::future::pending::<()>().await;
                }
                let _ = started_tx.send(index);
            }
        }))
        .buffer_unordered(snapshot_fanout_concurrency())
        .collect::<Vec<_>>();
        let task = tokio::spawn(fanout);
        assert_eq!(
            tokio::time::timeout(Duration::from_millis(100), started_rx.recv())
                .await
                .expect("ninth peer starts without waiting for the first eight")
                .expect("fanout sender remains open"),
            8
        );
        task.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn snapshots_expire_after_fifteen_seconds() {
        let pool = MediaPool::new(MembershipManager::unavailable());
        pool.snapshots.write().await.insert(
            "peer".to_owned(),
            CachedSnapshot {
                snapshot: MediaNodeSnapshot {
                    node_id: "peer".to_owned(),
                    observed_at_unix_ms: 1,
                    build: "test".to_owned(),
                    protocol_version: PROTOCOL_VERSION,
                    encoders: Vec::new(),
                    tone_map: Vec::new(),
                    hardware_slots_used: 0,
                    hardware_slots_max: 0,
                    software_threads_used: 0,
                    software_threads_max: 0,
                    scratch_bytes_free: 0,
                    scratch_pressure: PressureBand::Unavailable,
                    source_io_pressure: PressureBand::Unavailable,
                    egress_pressure: PressureBand::Unavailable,
                    live_waiting: false,
                    background_active: false,
                },
                expires_at: tokio::time::Instant::now() + SNAPSHOT_EXPIRY,
            },
        );
        tokio::time::advance(SNAPSHOT_EXPIRY + Duration::from_millis(1)).await;
        pool.expire().await;
        assert!(pool.snapshots.read().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn receipt_cannot_restart_an_old_snapshots_full_ttl() {
        let pool = MediaPool::new(MembershipManager::unavailable());
        let mut old = snapshot("peer", &["h264"], 1080);
        old.observed_at_unix_ms = unix_ms() - 10_000;
        let cached = accepted_snapshot(old, "peer").expect("ten-second-old snapshot remains fresh");
        pool.snapshots
            .write()
            .await
            .insert("peer".to_owned(), cached);

        tokio::time::advance(Duration::from_millis(5_100)).await;
        pool.expire().await;
        assert!(pool.snapshots.read().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn root_readability_is_specific_and_age_bounded() {
        let pool = MediaPool::new(MembershipManager::unavailable());
        let observed_at = tokio::time::Instant::now();
        pool.root_readability.write().await.extend([
            (
                PathBuf::from("/media"),
                RootReadability {
                    readable: true,
                    observed_at,
                },
            ),
            (
                PathBuf::from("/media/offline"),
                RootReadability {
                    readable: false,
                    observed_at,
                },
            ),
        ]);

        assert!(
            pool.root_likely_readable(Path::new("/media/movie.mkv"))
                .await
        );
        assert!(
            !pool
                .root_likely_readable(Path::new("/media/offline/movie.mkv"))
                .await
        );
        tokio::time::advance(ROOT_READABILITY_TTL + Duration::from_millis(1)).await;
        assert!(
            !pool
                .root_likely_readable(Path::new("/media/movie.mkv"))
                .await
        );
    }

    #[tokio::test(start_paused = true)]
    async fn common_offer_deadline_is_exact() {
        let started = tokio::time::Instant::now();
        let deadline = started + OFFER_DEADLINE;
        let fanout = stream::iter((0..MAX_PEERS).map(|_| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }))
        .buffer_unordered(MAX_PEERS)
        .collect::<Vec<_>>();
        let outcome = tokio::time::timeout_at(deadline, fanout).await;
        assert!(outcome.is_err());
        assert_eq!(started.elapsed(), OFFER_DEADLINE);
    }

    #[tokio::test]
    async fn a_single_node_poll_performs_no_peer_http_work() {
        let pool = MediaPool::new(MembershipManager::unavailable());
        pool.poll_once().await.expect("empty membership is a no-op");
        assert!(pool.snapshots.read().await.is_empty());
    }
}
