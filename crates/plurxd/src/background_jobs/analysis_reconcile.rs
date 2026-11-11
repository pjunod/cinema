//! Reconciliation of pre-worker analysis requests whose engine, source or
//! target changed. The admin page previews and applies it; the fragment-index
//! worker applies the retired-engine half of it on every pass
//! (`AppState::reconcile_retired_analysis_requests`).

use std::collections::BTreeMap;

use plurx_core::cluster::membership::MembershipManager;
use plurx_core::store::{
    AnalysisRequest, LiveAnalysisEngines, NewAnalysisRequest, Store, StrandedRemedy,
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::http::error::ApiError;
use crate::http::internal_activity::{PeerActivityClient, PeerActivityOutcome};
use crate::state::AppState;

/// What reconciliation reads: the Store, this node's id, and — on a
/// replicated node — the peer activity inventory that carries each worker's
/// engine digest. The admin handler builds it from `AppState`; the
/// fragment-index worker from its `JobManager`.
#[derive(Clone, Copy)]
pub(crate) struct ReconcileContext<'a> {
    pub(crate) store: &'a dyn Store,
    pub(crate) node_id: &'a str,
    pub(crate) membership: Option<&'a MembershipManager>,
    pub(crate) peers: Option<&'a PeerActivityClient>,
}

impl<'a> ReconcileContext<'a> {
    pub(crate) fn of_state(state: &'a AppState) -> Self {
        Self {
            store: &*state.store,
            node_id: &state.node_id,
            membership: Some(&state.membership),
            peers: Some(&state.peer_activity),
        }
    }
}

/// The candidate reasons the background reconciler acts on, one per
/// [`StrandedRemedy`] it can apply: the request's target reports, fresh, a
/// different engine digest (`engine_changed`), or the target is not a
/// committed member and the row is re-homed here (`target_changed`). Source
/// changes stay with the operator's page.
pub(crate) const RETIRED_ENGINE_REASON: &str = "engine_changed";
pub(crate) const ABSENT_TARGET_REASON: &str = "target_changed";

#[derive(Serialize)]
pub(crate) struct Candidate {
    pub(crate) request_id: String,
    pub(crate) candidate_id: String,
    pub(crate) file_id: String,
    pub(crate) title: String,
    pub(crate) target_node_id: String,
    pub(crate) replacement_node_id: String,
    pub(crate) reason: &'static str,
    pub(crate) eligible: bool,
    #[serde(skip)]
    pub(crate) replacement: Option<NewAnalysisRequest>,
}

/// Every live worker's fragment-index engine digest by node: this node's,
/// when its engine is current, and each voter peer whose activity snapshot
/// reported one inside the last 30 seconds. A node absent here retires
/// nothing: an unreported or stale worker is not evidence of a new engine.
pub(crate) async fn engines(state: &AppState) -> Result<BTreeMap<String, String>, ApiError> {
    engines_in(ReconcileContext::of_state(state)).await
}

/// The live engine set as last read, for callers that must not wait on a
/// peer fan-out (an enqueue on the playback path). Every successful
/// inventory read refreshes it; older than [`LIVE_ENGINES_TTL`] it is
/// unknown.
static LIVE_ENGINES: std::sync::RwLock<
    Option<(std::time::Instant, std::sync::Arc<LiveAnalysisEngines>)>,
> = std::sync::RwLock::new(None);
const LIVE_ENGINES_TTL: std::time::Duration = std::time::Duration::from_secs(90);

/// The engines live workers ran at the last inventory read, or `None` when
/// there has been none recently. Feeds the enqueue caps
/// (`enqueue_analysis_request_with`, `enqueue_or_promote_subtitle_source_with`),
/// which treat `None` as "count every active row". Carries no membership:
/// the caps' stranded rule needs only the digests.
pub(crate) fn live_engines_snapshot() -> Option<std::sync::Arc<LiveAnalysisEngines>> {
    let guard = LIVE_ENGINES.read().ok()?;
    let (at, live) = guard.as_ref()?;
    (at.elapsed() <= LIVE_ENGINES_TTL && !live.engines.is_empty()).then(|| live.clone())
}

fn publish_live_engines(engines: &BTreeMap<String, String>) {
    let live = std::sync::Arc::new(LiveAnalysisEngines {
        engines: engines.clone(),
        members: None,
    });
    if let Ok(mut guard) = LIVE_ENGINES.write() {
        *guard = Some((std::time::Instant::now(), live));
    }
}

pub(crate) async fn engines_in(
    ctx: ReconcileContext<'_>,
) -> Result<BTreeMap<String, String>, ApiError> {
    let engines = read_engines(ctx).await?;
    publish_live_engines(&engines);
    Ok(engines)
}

/// The committed members, this node included, for the absent-target remedy:
/// `{this node}` on a single node, `None` when replicated membership could
/// not be read (which disables that remedy rather than guessing).
async fn read_members(ctx: ReconcileContext<'_>) -> Option<std::collections::BTreeSet<String>> {
    let membership = ctx.membership?;
    let mut members = std::collections::BTreeSet::from([ctx.node_id.to_owned()]);
    if membership.is_replicated() {
        members.extend(
            membership
                .operations_peers()
                .await
                .ok()?
                .into_iter()
                .map(|peer| peer.node_id),
        );
    }
    Some(members)
}

async fn read_engines(ctx: ReconcileContext<'_>) -> Result<BTreeMap<String, String>, ApiError> {
    let mut engines = BTreeMap::new();
    if crate::ffmpeg::fragment_index_engine_is_current().await {
        engines.insert(
            ctx.node_id.to_owned(),
            crate::ffmpeg::fragment_index_engine_digest().await,
        );
    }
    if let (Some(membership), Some(peers)) = (ctx.membership, ctx.peers) {
        if !membership.is_replicated() {
            return Ok(engines);
        }
        let peers = peers.snapshots().await.map_err(|_| {
            ApiError::Conflict("Worker inventory is unavailable; try again.".into())
        })?;
        let now = crate::state::clock_ms();
        for (id, outcome) in peers.iter() {
            if let PeerActivityOutcome::Answered(snapshot) = outcome {
                if let Some(worker) = &snapshot.workers {
                    if (-30_000..30_000).contains(&now.saturating_sub(worker.observed_at_ms)) {
                        if let Some(engine) = &worker.analysis_engine {
                            if engine.len() == 64 && engine.bytes().all(|b| b.is_ascii_hexdigit()) {
                                engines.insert(id.clone(), engine.clone());
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(engines)
}

/// Whether `old` should be replaced, and by what — the same decision for an
/// operator's previewed apply and for the background reconciler.
pub(crate) async fn candidate(
    state: &AppState,
    old: &AnalysisRequest,
    engines: &BTreeMap<String, String>,
    reassign: bool,
) -> Result<Candidate, ApiError> {
    candidate_in(ReconcileContext::of_state(state), old, engines, reassign).await
}

pub(crate) async fn candidate_in(
    ctx: ReconcileContext<'_>,
    old: &AnalysisRequest,
    engines: &BTreeMap<String, String>,
    reassign: bool,
) -> Result<Candidate, ApiError> {
    let mut row = Candidate {
        request_id: old.request_id.clone(),
        candidate_id: String::new(),
        file_id: old.file_id.to_string(),
        title: format!("File {}", old.file_id),
        target_node_id: old.target_node_id.clone(),
        replacement_node_id: old.target_node_id.clone(),
        reason: "current",
        eligible: false,
        replacement: None,
    };
    if old.state != "queued" || !old.result_cache_key.is_empty() || old.cancel_requested {
        row.reason = "worker_owned";
        return Ok(row);
    }
    if old.component != "fragment_index" {
        row.reason = "other_component";
        return Ok(row);
    }
    if old.requested_generation.starts_with("predict:") {
        row.reason = "producer_owned";
        return Ok(row);
    }
    let Some(file) = ctx.store.get_file(old.file_id).await? else {
        row.reason = "source_missing";
        return Ok(row);
    };
    if let Some(item) = ctx.store.get_item(file.item_id).await? {
        row.title = item.title;
    }
    let engine = if let Some(engine) = engines.get(&old.target_node_id) {
        engine
    } else if reassign {
        let Some(engine) = engines.get(ctx.node_id) else {
            row.reason = "worker_unknown";
            return Ok(row);
        };
        // A redirected request must have a source on its new node. The normal
        // worker still owns full attestation; this bounded check is only admission.
        if !matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(2),
                crate::fragment_index_cluster::inspect_copy_source(&file)
            )
            .await,
            Ok(Ok(_))
        ) {
            row.reason = "source_unavailable";
            return Ok(row);
        }
        ctx.node_id.clone_into(&mut row.replacement_node_id);
        engine
    } else {
        row.reason = "worker_unknown";
        return Ok(row);
    };
    // The forced-successor uniqueness key deliberately spans target nodes.
    // A same-engine rebuild cannot be replaced while its old slot is active.
    if old.force_rebuild
        && engine == &old.pipeline_version
        && file.size == old.source_size
        && file.mtime == old.source_mtime
        && row.replacement_node_id != old.target_node_id
    {
        row.reason = "rebuild_target";
        return Ok(row);
    }
    let generation = crate::state::analysis_request_generation(
        &file,
        &old.component,
        engine,
        &old.video_identity,
        false,
    );
    row.reason = if file.size != old.source_size || file.mtime != old.source_mtime {
        "source_changed"
    } else if row.replacement_node_id != old.target_node_id {
        "target_changed"
    } else if engine != &old.pipeline_version {
        "engine_changed"
    } else {
        "current"
    };
    if row.reason == "current" {
        return Ok(row);
    }
    // Stable identity makes an uncertain response safe to retry. A forced
    // predecessor keeps its explicit rebuild intent without generating another
    // forced generation on every preview.
    let identity = serde_json::to_vec(&(
        old,
        engine,
        &row.replacement_node_id,
        file.size,
        file.mtime,
        &generation,
    ))
    .map_err(|error| ApiError::Internal(error.to_string()))?;
    row.candidate_id = hex::encode(Sha256::digest(identity));
    let id = row.candidate_id.clone();
    row.replacement = Some(NewAnalysisRequest {
        request_id: id.clone(),
        file_id: old.file_id,
        source_size: file.size,
        source_mtime: file.mtime,
        component: old.component.clone(),
        pipeline_version: engine.clone(),
        video_identity: old.video_identity.clone(),
        requested_generation: if old.force_rebuild { id } else { generation },
        priority: old.priority.clone(),
        trigger: old.trigger.clone(),
        force_rebuild: old.force_rebuild,
        target_node_id: row.replacement_node_id.clone(),
        not_before_ms: 0,
        created_at_ms: 0,
    });
    if old.force_rebuild {
        if let Some(occupied) = ctx
            .store
            .analysis_reconciliation_forced_slot(
                row.replacement.as_ref().expect("replacement derived above"),
            )
            .await?
        {
            if occupied.target_node_id != row.replacement_node_id {
                row.reason = "rebuild_target";
                row.replacement = None;
                return Ok(row);
            }
        }
    }
    row.eligible = true;
    Ok(row)
}

const RECONCILE_PAGE: i64 = 100;

/// Requests the background reconciler replaced, by candidate reason.
static RECONCILED_ENGINE_CHANGED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
static RECONCILED_TARGET_CHANGED: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(crate) fn prometheus() -> String {
    format!(
        "# HELP plurx_analysis_reconciled_total Pre-worker analysis requests the fragment-index worker replaced onto a live engine, by reason.\n\
         # TYPE plurx_analysis_reconciled_total counter\n\
         plurx_analysis_reconciled_total{{reason=\"{RETIRED_ENGINE_REASON}\"}} {}\n\
         plurx_analysis_reconciled_total{{reason=\"{ABSENT_TARGET_REASON}\"}} {}\n",
        RECONCILED_ENGINE_CHANGED.load(std::sync::atomic::Ordering::Relaxed),
        RECONCILED_TARGET_CHANGED.load(std::sync::atomic::Ordering::Relaxed)
    )
}

/// What one background reconciliation pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ReconcileReport {
    /// Active rows read this pass (bounded by the pass limit).
    pub(crate) scanned: usize,
    /// Stranded rows (no live worker runs their engine) the pass acted on:
    /// the target reports a different engine, or the target is not a
    /// committed member and the row was re-homed here.
    pub(crate) retired: usize,
    /// Of those, the ones whose successor this pass committed. A retired row
    /// not replaced lost a race to another reconciler, or is held by a live
    /// playback waiter until that waiter's deadline.
    pub(crate) replaced: usize,
    /// Of `retired`, the rows whose target is absent from committed
    /// membership (re-homed through the reassign path).
    pub(crate) absent_target: usize,
    /// Stranded rows whose target is a member with no fresh engine report:
    /// nothing is retired on the absence of evidence.
    pub(crate) skipped_unfresh: usize,
    /// The cursor reached the end of the table and starts over next pass.
    pub(crate) wrapped: bool,
}

/// One bounded pass of the stranded-request half of reconciliation.
///
/// The root cause it removes: a request names the engine digest of the node
/// that asked, and the claim filters on an exact digest, so after a deploy
/// changes the engine every queued request under the old digest is
/// unclaimable by anyone, forever — it is never charged an attempt, so the
/// attempt sweep never retires it, and it counts against the active cap.
///
/// Which rows: exactly the ones [`LiveAnalysisEngines`] calls stranded, the
/// rule the queue health's `incompatible` and the enqueue caps also use, so
/// the pass and the metric agree. A row some live worker can still claim
/// (its own target, or another idle member through the claim's cross-node
/// arm) is left alone. Of the stranded rows: a target that reports, fresh, a
/// different engine gets the row replaced onto that engine; a target absent
/// from committed membership gets it re-homed here through the reassign
/// path (source checked on this node); a removed target's row is never
/// touched — removal retires it and the reconcile statements refuse it; a
/// silent member's row stays (absence of evidence) and stays counted.
/// Replacement goes through the same `candidate` decision and the same
/// atomic insert-then-cancel the operator's page uses, so two voters racing
/// on one row commit exactly one successor.
///
/// `cursor` persists across passes so a large table is walked, not re-read
/// from its first page; `own_targets_only` keeps a learner to the rows that
/// name it.
pub(crate) async fn reconcile_retired(
    ctx: ReconcileContext<'_>,
    cursor: &mut String,
    limit: usize,
    own_targets_only: bool,
) -> Result<ReconcileReport, ApiError> {
    let live = LiveAnalysisEngines {
        engines: engines_in(ctx).await?,
        members: read_members(ctx).await,
    };
    reconcile_retired_against(ctx, &live, cursor, limit, own_targets_only).await
}

/// [`reconcile_retired`] against an inventory already read.
pub(crate) async fn reconcile_retired_against(
    ctx: ReconcileContext<'_>,
    live: &LiveAnalysisEngines,
    cursor: &mut String,
    limit: usize,
    own_targets_only: bool,
) -> Result<ReconcileReport, ApiError> {
    let mut report = ReconcileReport::default();
    if live.engines.is_empty() {
        return Ok(report);
    }
    while report.scanned < limit {
        let page = ctx
            .store
            .analysis_reconciliation_page(cursor, RECONCILE_PAGE)
            .await?;
        let full = page.len() == RECONCILE_PAGE as usize;
        match page.last() {
            Some(last) => last.request_id.clone_into(cursor),
            None => cursor.clear(),
        }
        for old in &page {
            report.scanned += 1;
            // Cheap filters first: only queued, unowned fragment-index rows
            // can be candidates, and a row some live worker can claim is not
            // stranded. Neither needs a Store read.
            if (own_targets_only && old.target_node_id != ctx.node_id)
                || old.state != "queued"
                || old.component != "fragment_index"
                || !old.result_cache_key.is_empty()
                || old.cancel_requested
            {
                continue;
            }
            let (reassign, reason, counter) = match live.remedy(old) {
                None => continue,
                Some(StrandedRemedy::TargetSilent) => {
                    report.skipped_unfresh += 1;
                    continue;
                }
                Some(StrandedRemedy::TargetEngine(_)) => {
                    (false, RETIRED_ENGINE_REASON, &RECONCILED_ENGINE_CHANGED)
                }
                Some(StrandedRemedy::TargetAbsent) => {
                    (true, ABSENT_TARGET_REASON, &RECONCILED_TARGET_CHANGED)
                }
            };
            let candidate = candidate_in(ctx, old, &live.engines, reassign).await?;
            if candidate.reason != reason || !candidate.eligible {
                continue;
            }
            report.retired += 1;
            report.absent_target += usize::from(reassign);
            if let Some(replacement) = candidate.replacement {
                if ctx
                    .store
                    .reconcile_analysis_request(old, &replacement, crate::state::clock_ms())
                    .await?
                {
                    report.replaced += 1;
                    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        if !full {
            cursor.clear();
            report.wrapped = true;
            break;
        }
    }
    Ok(report)
}
