//! Settlement of a Source incarnation whose owner process is gone.
//!
//! A Source owner keeps an invocation's authority in its process: the
//! retained entry and the accepted-driver registry whose boot the custody
//! ledger routes to. When that process dies, nothing in the new process can
//! answer for the incarnation, yet its durable rows remain: a `held` binding
//! that counts against the grant's occupancy, its request, its media session
//! and an unsealed custody ledger. Without this module every End for it is
//! refused as retryable forever, and the binding never releases.
//!
//! The new process proves the old one is gone from durable facts only: the
//! binding's request names this node, the ledger routes to this node under a
//! registry boot, and this node's live membership now publishes a different
//! boot (the data-directory lock admits one process per node, and each boot
//! is one process lifetime). From that it may do exactly what the dead
//! process's End would have done, through the same guarded store writes:
//! seal the ledger, take real closure receipts from ingress drivers that are
//! still alive, record slots whose own boot is superseded as supersession
//! (never as a closure receipt), end the media session, release the binding.
//! Start, media and Register never come here and stay fail-closed, and a
//! request this node never assigned finds no row and stays unresolved.
use super::*;
use plurx_core::sharing_source_sessions::SupersededSourceIncarnation;
use plurx_core::store::sharing_source_ingress_custody::SourceCustodyWrite;
use std::time::{Duration, Instant};

/// Settlement attempts from the reconciliation loop get this long each.
const RECONCILE_BUDGET: Duration = Duration::from_secs(60);
const RECONCILE_INTERVAL: Duration = Duration::from_secs(60);
const RECONCILE_BATCH: usize = 16;

/// Dispatched incarnations this node routed under a superseded boot.
pub(super) async fn lookup(
    state: &crate::state::AppState,
    incarnation: Option<Uuid>,
    after: Option<Uuid>,
    limit: usize,
) -> Result<Vec<SupersededSourceIncarnation>, ApiError> {
    let members = state
        .cluster
        .source_members()
        .observe()
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    state
        .store
        .superseded_source_incarnations(
            &state.node_id,
            state.sharing.accepted_drivers.boot_id(),
            incarnation,
            after,
            &members,
            limit,
        )
        .await
        .map_err(|_| unavailable())
}

/// The one superseded incarnation a forwarded authority names, when every
/// signed identity field agrees with the durable rows. Never from a missing
/// entry alone: the wire must name this node and the exact dead boot.
pub(super) async fn forwarded(
    state: &crate::state::AppState,
    wire: &forwarding::ForwardAuthority,
) -> Option<SupersededSourceIncarnation> {
    if wire.owner_node_id != state.node_id
        || wire.dispatch_generation != 1
        || wire.expected_registry_boot == state.sharing.accepted_drivers.boot_id()
    {
        return None;
    }
    let found = lookup(state, Some(wire.incarnation_id), None, 1)
        .await
        .ok()?;
    let [found] = found.as_slice() else {
        return None;
    };
    let assignment = found.assignment();
    let binding = assignment.binding();
    let target = &wire.reference;
    (found.dead_boot() == wire.expected_registry_boot
        && found.credential_matches(&wire.credential_hash)
        && assignment.custody_identity() == wire.owner_identity
        && binding.principal() == &wire.principal
        && binding.request_id() == wire.request_id.to_string()
        && binding.request_fingerprint() == wire.request_fingerprint
        && binding.playback_id() == wire.playback_id
        && binding.source_server_id() == target.server_id
        && binding.catalogue_epoch() == target.catalogue_epoch
        && binding.library_id() == &target.library_id
        && binding.item_id() == &target.item_id
        && binding.file_id() == &target.file_id
        && binding.file_revision() == &target.revision)
        .then(|| found.clone())
}

/// The superseded incarnation an End or cleanup Status names, found the way
/// the ingress routes it: by credential, viewer, request and exact target.
async fn named(
    state: &crate::state::AppState,
    hash: &str,
    viewer: &str,
    input: &SourceStartInput,
) -> Option<SupersededSourceIncarnation> {
    let target = &input.reference;
    let route = state
        .store
        .lookup_source_ingress_route(
            hash,
            viewer,
            &input.request_id.to_string(),
            target.server_id,
            target.catalogue_epoch,
            target.library_id.as_str(),
            target.item_id.as_str(),
            target.file_id.as_str(),
            target.revision.as_str(),
        )
        .await
        .ok()??;
    if route.owner_node_id != state.node_id
        || route.dispatch_generation != 1
        || route.registry_boot_id == state.sharing.accepted_drivers.boot_id()
        || (route.request_fingerprint != ingress::fingerprint(input)
            && route.request_fingerprint
                != ingress::fingerprint_recipe(&input.canonical_recipe, false))
    {
        return None;
    }
    let found = lookup(state, Some(route.incarnation_id), None, 1)
        .await
        .ok()?;
    let [found] = found.as_slice() else {
        return None;
    };
    let assignment = found.assignment();
    (found.dead_boot() == route.registry_boot_id
        && assignment.custody_identity() == route.owner_identity
        && assignment.binding().request_fingerprint() == route.request_fingerprint
        && assignment.binding().playback_id() == route.playback_id)
        .then(|| found.clone())
}

/// One receipt per incarnation, the same on every replay. Derived, so an End
/// answered after a lost reply, or by another attempt, carries the same id.
fn confirmation(found: &SupersededSourceIncarnation) -> Uuid {
    use sha2::Digest;
    let mut hash = sha2::Sha256::new();
    hash.update(b"plurx.sharing-source-superseded-end.v1\0");
    hash.update(found.assignment().custody_identity().as_bytes());
    let bytes: [u8; 32] = hash.finalize().into();
    let mut random = [0_u8; 16];
    random.copy_from_slice(&bytes[..16]);
    uuid::Builder::from_random_bytes(random).into_uuid()
}

/// Bring the dead incarnation to the state the guarded release accepts and
/// release it. Every step is an exact, replayable store write, so concurrent
/// attempts (an End and the reconciliation loop) converge or one retries.
pub(super) async fn settle(
    state: &crate::state::AppState,
    found: &SupersededSourceIncarnation,
    deadline: Instant,
) -> Result<Uuid, SourceStartFailure> {
    if found.released() {
        return Ok(confirmation(found));
    }
    let assignment = found.assignment();
    // Seal, then take actual closure receipts from every ingress driver that
    // is still alive. Drivers of a superseded boot refuse here (no registry
    // closes another boot's driver) and are recorded as superseded below.
    let _ = ingress::retire_custody_with_mode(
        state,
        assignment,
        deadline,
        crate::sharing_connection_custody::DriverCloseMode::Revoke,
    )
    .await;
    let unresolved = |_| SourceStartFailure::Unresolved;
    let ledger = state
        .store
        .source_ingress_custody(assignment)
        .await
        .map_err(unresolved)?
        .ok_or(SourceStartFailure::Unresolved)?;
    if !ledger.state.is_sealed() {
        return Err(SourceStartFailure::Unresolved);
    }
    for slot in ledger.state.open().cloned().collect::<Vec<_>>() {
        // A slot whose node still publishes its boot is refused by the
        // store's guard and keeps the ledger open: that custody is real.
        let _ = state
            .store
            .supersede_source_ingress_custody(assignment, &slot)
            .await;
    }
    let ledger = state
        .store
        .source_ingress_custody(assignment)
        .await
        .map_err(unresolved)?
        .ok_or(SourceStartFailure::Unresolved)?;
    if !ledger.state.settled() {
        return Err(SourceStartFailure::Unresolved);
    }
    let incarnation = assignment.binding().incarnation_id().to_string();
    let route = state
        .store
        .media_session_route_by_incarnation(&incarnation)
        .await
        .map_err(unresolved)?;
    let outcome = match route {
        Some(route) => {
            if route.owner_epoch != 1
                || route.owner_node_id != assignment.owner_node_id()
                || route.principal != *assignment.binding().principal()
            {
                return Err(SourceStartFailure::Unresolved);
            }
            let terminal = if route.state == "ended" {
                route
            } else {
                // The process that owned this session is gone; this is the
                // end it would have written, guarded on the exact route.
                state
                    .store
                    .end_media_session_if_owner(&plurx_core::domain::MediaSessionEnd {
                        incarnation_id: route.incarnation_id,
                        session_id: route.session_id,
                        expected_owner_node_id: route.owner_node_id,
                        expected_owner_epoch: route.owner_epoch,
                        expected_lease_expires_at_ms: route.lease_expires_at_ms,
                        terminal_reason: "deleted".into(),
                        now_ms: crate::fragment_index_cluster::unix_ms(),
                    })
                    .await
                    .map_err(unresolved)?
                    .ok_or(SourceStartFailure::Unresolved)?
            };
            state
                .store
                .settle_source_terminal_worker(assignment, &terminal)
                .await
        }
        None => {
            state
                .store
                .settle_source_assigned_without_activation(assignment)
                .await
        }
    }
    .map_err(unresolved)?;
    match outcome {
        plurx_core::sharing_source_sessions::SourceReleaseOutcome::Released
        | plurx_core::sharing_source_sessions::SourceReleaseOutcome::ExactReplay => {
            tracing::info!(
                target: "plurx::sharing",
                stage = "source_superseded.settled",
                incarnation_id = %assignment.binding().incarnation_id(),
                dead_boot = %found.dead_boot(),
                "Source incarnation of a superseded owner boot settled"
            );
            Ok(confirmation(found))
        }
        plurx_core::sharing_source_sessions::SourceReleaseOutcome::Refused => {
            Err(SourceStartFailure::Unresolved)
        }
    }
}

/// End for an incarnation this process has no entry for: settle it when a
/// superseded boot of this node owned it, otherwise the original refusal.
pub(super) async fn end(
    state: &crate::state::AppState,
    headers: &SourceHeaders,
    hash: &str,
    viewer: &str,
    input: &SourceStartInput,
    known: Option<&SourcePublishedLineage>,
    missing: SourceStartFailure,
) -> Result<SourceEndReceipt, ApiError> {
    let Some(found) = named(state, hash, viewer, input).await else {
        return Err(missing.response());
    };
    // The same lineage check the live owner applies: an End naming another
    // publication is not this incarnation's End.
    let published = found.published();
    if let Some(known) = known {
        if known.incarnation_id != found.assignment().binding().incarnation_id()
            || published != Some((known.session_id, known.control_epoch))
        {
            return Err(SourceStartFailure::Conflict.response());
        }
    }
    let confirmation_id = settle(state, &found, headers.deadline(Duration::from_secs(9)))
        .await
        .map_err(SourceStartFailure::response)?;
    Ok(SourceEndReceipt {
        reference: input.reference.clone(),
        request_id: input.request_id,
        incarnation_id: found.assignment().binding().incarnation_id(),
        session_id: published.map(|(session, _)| session),
        control_epoch: published.map(|(_, epoch)| epoch),
        state: "settled",
        confirmation_id,
        settled: true,
    })
}

/// Cleanup-only Status for such an incarnation observes; it never settles.
pub(super) async fn status(
    state: &crate::state::AppState,
    hash: &str,
    viewer: &str,
    input: &SourceStartInput,
) -> Option<serde_json::Value> {
    let found = named(state, hash, viewer, input).await?;
    Some(serde_json::json!({
        "reference": input.reference,
        "request_id": input.request_id,
        "incarnation_id": found.assignment().binding().incarnation_id(),
        "state": if found.released() { "settled" } else { "unresolved" },
    }))
}

/// Acknowledge an actual ingress closure receipt against the durable ledger
/// of a superseded incarnation. The receipt is the ingress registry's own;
/// only the retained entry that used to verify the owner is gone.
pub(super) async fn acknowledge(
    state: &crate::state::AppState,
    found: &SupersededSourceIncarnation,
    registration: &plurx_core::sharing_ingress_custody::IngressRegistration,
    confirmation: &str,
) -> Result<SourceCustodyWrite, ApiError> {
    state
        .store
        .acknowledge_source_ingress_custody(found.assignment(), registration, confirmation)
        .await
        .map_err(|_| unavailable())
}

/// Reconcile what earlier boots of this node left behind. New ones appear
/// only at a restart, or when an ingress that held a slot restarts later.
pub(crate) async fn reconcile_loop(
    state: crate::state::AppState,
    shutdown: tokio_util::sync::CancellationToken,
) {
    // Page past incarnations that cannot settle yet, so a batch of them
    // never hides one that can.
    let mut after = None;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(RECONCILE_INTERVAL) => {}
        }
        let Ok(found) = lookup(&state, None, after, RECONCILE_BATCH).await else {
            continue;
        };
        after = (found.len() == RECONCILE_BATCH)
            .then(|| {
                found
                    .last()
                    .map(|found| found.assignment().binding().incarnation_id())
            })
            .flatten();
        for found in found {
            if let Err(failure) = settle(&state, &found, Instant::now() + RECONCILE_BUDGET).await {
                tracing::debug!(
                    target: "plurx::sharing",
                    stage = "source_superseded.pending",
                    incarnation_id = %found.assignment().binding().incarnation_id(),
                    ?failure,
                    "Source incarnation of a superseded owner boot still has open custody"
                );
            }
        }
    }
}
