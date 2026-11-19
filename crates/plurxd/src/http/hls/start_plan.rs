//! The create's one planning read (playback-start D3b,
//! `docs/streaming/PLAYBACK-START-D3-BUILD.md` §4.2–4.4).
//!
//! A create reads its planning inputs once — the file row, its probe, every
//! key of [`crate::transcode::QUALITY_PLANNING_KEYS`] and the planning
//! generation, in one statement — as a [`StartPlan`] from the node's applied
//! view behind the settings and files floors, and hands that plan down every
//! chain that used to read them again. A helper on such a chain takes
//! `plan: Option<&StartPlan>`: `Some` answers from the plan, `None` reads the
//! Store exactly as before D3b. Whether a call may use plan values is a
//! property of the call, not of the function: only the ingress create
//! (`create_with_purpose_attempt`) and the remote owner's start
//! (`internal_media_sessions::start`) ever pass `Some`, and a source test
//! pins that.
//!
//! **The binding.** Since D3t every planning key moves
//! `playback_input_generation`, so two plans with one [`PlanningBinding`]
//! hold the same values. A remote owner plans again from its own view at the
//! binding the ingress sent ([`owner_plan`]): equal, it builds at that
//! revision; different, it asks Authority once, and a still-different answer
//! refuses the start (`409`, the answer it already gave for a changed
//! source). The refusal closes the ingress's view ([`close_after_refusal`]):
//! the ingress may be the stale side, so its next plan is a quorum read.
//! No wire change: the binding was already on the start request.
//!
//! **A stale ingress view** (review B1) is caught earlier, by the catalog's
//! own quorum read, before the create holds its claim: the create closes its
//! view and plans once more, from Authority, in the same request
//! ([`replan_once_after_stale`]). A remote owner that will receive no binding
//! (a legacy route) plans from Authority, so the ingress confirms its view
//! plan at Authority before handing it such a start
//! ([`confirm_unbound_remote`]).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use plurx_core::domain::MediaFile;
use plurx_core::error::StoreError;
use plurx_core::store::{
    AppliedTable, AppliedView, PlaybackPlanningSnapshot, ViewFallback, ViewSource,
};

use crate::media_pool::PlanningBinding;

/// One create's planning inputs, read once.
#[derive(Clone, Debug)]
pub(crate) struct StartPlan {
    /// The file, its probe, the planning keys, the generation and the title.
    pub(crate) snapshot: Arc<PlaybackPlanningSnapshot>,
    /// The binding a peer checks its own plan against (unchanged wire type).
    pub(crate) binding: PlanningBinding,
    /// Where the snapshot came from: the replica, or Authority.
    pub(crate) source: ViewSource,
}

impl StartPlan {
    pub(crate) fn new(snapshot: PlaybackPlanningSnapshot, source: ViewSource) -> Self {
        let binding = PlanningBinding::from_snapshot(&snapshot);
        Self {
            snapshot: Arc::new(snapshot),
            binding,
            source,
        }
    }

    /// One planning setting as stored; `None` when unset.
    pub(crate) fn setting(&self, key: &str) -> Option<&str> {
        self.snapshot.settings.get(key).map(String::as_str)
    }

    /// A boolean planning setting with the store's own parsing and default.
    pub(crate) fn switch(&self, key: &str, default: bool) -> bool {
        plurx_core::store::stored_switch(self.setting(key), default)
    }

    /// Whether the plan describes exactly this file revision: id, size and
    /// modification time.
    pub(crate) fn describes(&self, file: &MediaFile) -> bool {
        let planned = &self.snapshot.file;
        planned.id == file.id && planned.size == file.size && planned.mtime == file.mtime
    }

    /// The stored probe the plan holds for `file`, when it describes exactly
    /// that revision; `None` asks the caller to read it.
    pub(crate) fn probe_for(&self, file: &MediaFile) -> Option<Option<&str>> {
        self.describes(file)
            .then(|| self.snapshot.probe_json.as_deref())
    }
}

/// The ingress create's plan: one read through the applied view (§4.2).
/// A create that re-planned after a stale view ([`replan_once_after_stale`])
/// reads from Authority.
pub(crate) async fn ingress_plan(
    applied: &AppliedView,
    file_id: i64,
) -> Result<Option<StartPlan>, StoreError> {
    let keys = &crate::transcode::QUALITY_PLANNING_KEYS;
    let plan = if crate::media_pool::create_replanned() {
        applied
            .planning_from_authority(file_id, keys, ViewFallback::Floor)
            .await?
    } else {
        applied.planning(file_id, keys).await?
    };
    Ok(plan.map(|(snapshot, source)| StartPlan::new(snapshot, source)))
}

/// Run a create attempt, and once more if its view plan was refuted by a
/// catalog's quorum read before the claim (review B1): the view closes, the
/// second attempt plans from Authority (`ingress_plan`), and its answer is
/// the create's. One re-plan per request, never a loop.
pub(crate) async fn replan_once_after_stale<T, F, Fut>(
    applied: &AppliedView,
    mut attempt: F,
) -> Result<T, crate::http::error::ApiError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, crate::http::error::ApiError>>,
{
    match attempt().await {
        Err(_) if crate::media_pool::take_create_replan() => {
            close_planning_view(applied);
            STALE_REPLANS.fetch_add(1, Ordering::Relaxed);
            attempt().await
        }
        result => result,
    }
}

/// Views closed and re-planned because a catalog refuted this node's plan.
static STALE_REPLANS: AtomicU64 = AtomicU64::new(0);

/// [`replan_once_after_stale`]'s count since start, for Settings → Developer.
pub(crate) fn stale_replans() -> u64 {
    STALE_REPLANS.load(Ordering::Relaxed)
}

/// Why a remote owner refused to plan at the ingress's binding.
#[derive(Debug)]
pub(crate) enum OwnerPlanError {
    /// The Store could not answer.
    Store(StoreError),
    /// Authority itself holds another revision of the source or settings.
    Changed,
}

impl From<StoreError> for OwnerPlanError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// A remote owner's plan (§4.3). With the ingress's binding: one view read,
/// and Authority once only when the view answers another binding. Without
/// one (a legacy route): Authority, once, for the whole start. `Ok(None)`
/// when the source is gone; the chain then reads and refuses as before.
pub(crate) async fn owner_plan(
    applied: &AppliedView,
    file_id: i64,
    expected: Option<&PlanningBinding>,
) -> Result<Option<StartPlan>, OwnerPlanError> {
    let keys = &crate::transcode::QUALITY_PLANNING_KEYS;
    let Some(expected) = expected else {
        return Ok(applied
            .planning_from_authority(file_id, keys, ViewFallback::Unbound)
            .await?
            .map(|(snapshot, source)| StartPlan::new(snapshot, source)));
    };
    // One plan, one count: a view answer at another binding is not counted
    // as a view read; Authority answers it (`binding_mismatch`).
    match applied
        .planning_matching(file_id, keys, |snapshot| {
            snapshot.is_some_and(|snapshot| PlanningBinding::from_snapshot(snapshot) == *expected)
        })
        .await?
    {
        Some((snapshot, source)) => {
            let plan = StartPlan::new(snapshot, source);
            if plan.binding == *expected {
                Ok(Some(plan))
            } else {
                Err(OwnerPlanError::Changed)
            }
        }
        None => Ok(None),
    }
}

/// A peer's catalog request at the ingress's binding (D3d, §4.6): the same
/// one view read and at most one authority read as an owner's start
/// ([`owner_plan`]), answered as the catalog's own causes. A binding the
/// view holds is the plan; one only Authority can answer differently is
/// `StaleSource`, the cause that closes the ingress's view and re-plans its
/// create ([`replan_once_after_stale`]); a source that is gone is
/// `StaleSource` too, as the quorum read always answered it.
pub(crate) async fn peer_catalog_plan(
    applied: &AppliedView,
    file_id: i64,
    expected: &PlanningBinding,
) -> Result<StartPlan, crate::media_pool::CatalogCause> {
    use crate::media_pool::CatalogCause;
    match owner_plan(applied, file_id, Some(expected)).await {
        Ok(Some(plan)) => {
            PEER_CATALOG_PLANS.fetch_add(1, Ordering::Relaxed);
            Ok(plan)
        }
        Ok(None) => Err(CatalogCause::StaleSource),
        Err(OwnerPlanError::Changed) => {
            PEER_CATALOG_STALE.fetch_add(1, Ordering::Relaxed);
            Err(CatalogCause::StaleSource)
        }
        Err(OwnerPlanError::Store(_)) => Err(CatalogCause::SnapshotUnavailable),
    }
}

/// Peer catalog requests planned at the sender's binding.
static PEER_CATALOG_PLANS: AtomicU64 = AtomicU64::new(0);
/// Peer catalog requests refused `StaleSource` because Authority held
/// another binding than the sender planned at.
static PEER_CATALOG_STALE: AtomicU64 = AtomicU64::new(0);

/// (planned, refused stale) since start, for Settings → Developer.
pub(crate) fn peer_catalog_counts() -> (u64, u64) {
    (
        PEER_CATALOG_PLANS.load(Ordering::Relaxed),
        PEER_CATALOG_STALE.load(Ordering::Relaxed),
    )
}

/// Views closed because an owner refused this node's plan.
static REFUSAL_CLOSES: AtomicU64 = AtomicU64::new(0);

/// An owner refused a start planned here as changed (§4.3): this node may be
/// the stale side. A quorum fact closes the view — both planning floors —
/// so the next plan here is Authority's until the replica is proven to
/// contain what the owner saw. No retry, no loop: the create answers the
/// typed `quality_catalog_unavailable` and the viewer's retry re-plans.
pub(crate) fn close_after_refusal(applied: &AppliedView) {
    close_planning_view(applied);
    REFUSAL_CLOSES.fetch_add(1, Ordering::Relaxed);
}

/// A quorum fact refutes this node's planning view: close both its floors.
fn close_planning_view(applied: &AppliedView) {
    applied.note_fact(AppliedTable::Settings);
    applied.note_fact(AppliedTable::Files);
}

/// The ingress's answer when `candidate` refused the start as planned at
/// another revision (`RemoteStartFailure::PlanChanged`): the view closes and
/// the create answers the typed retryable `quality_catalog_unavailable`,
/// offering the refuted plan to no further candidate (review S1).
pub(crate) fn remote_plan_refused(
    applied: &AppliedView,
    candidate: &str,
) -> crate::http::error::ApiError {
    close_after_refusal(applied);
    crate::http::error::ApiError::typed(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "quality_catalog_unavailable",
        format!("media worker {candidate} holds another revision of this source or its settings"),
    )
}

/// Whether a remote start failure ends placement: a refused plan is refuted
/// for every candidate, this node included (review S1).
pub(crate) fn ends_placement(failure: &crate::media_sessions::RemoteStartFailure) -> bool {
    matches!(
        failure,
        crate::media_sessions::RemoteStartFailure::PlanChanged
    )
}

/// Before a start that will carry no binding goes to a remote owner (a
/// legacy route; the owner plans from Authority, `unbound`), the ingress
/// plans from Authority too (review S2): a view plan is confirmed by one
/// authority read at the same binding, or the view closes and the create
/// answers `quality_catalog_unavailable` (the viewer's retry plans from
/// Authority). An Authority plan needs nothing.
pub(crate) async fn confirm_unbound_remote(
    applied: &AppliedView,
    plan: &StartPlan,
) -> Result<(), crate::http::error::ApiError> {
    if plan.source != ViewSource::View {
        return Ok(());
    }
    let confirmed = applied
        .planning_from_authority(
            plan.snapshot.file.id,
            &crate::transcode::QUALITY_PLANNING_KEYS,
            ViewFallback::Unbound,
        )
        .await
        .map_err(|error| {
            crate::http::error::ApiError::ServiceUnavailable(format!(
                "confirming the plan for a remote start: {error}"
            ))
        })?
        .is_some_and(|(snapshot, _)| PlanningBinding::from_snapshot(&snapshot) == plan.binding);
    if confirmed {
        return Ok(());
    }
    close_planning_view(applied);
    Err(crate::http::error::ApiError::typed(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "quality_catalog_unavailable",
        "this node planned another revision of this source or its settings than Authority holds",
    ))
}

/// [`close_after_refusal`]'s count since start, for Settings → Developer.
pub(crate) fn refusal_closes() -> u64 {
    REFUSAL_CLOSES.load(Ordering::Relaxed)
}

#[cfg(test)]
#[path = "start_plan_tests.rs"]
mod tests;
