//! Actual Source owner adapters. Transport observations never become factory authority.
use super::forwarding::{
    CustodyAction, CustodyReply, ForwardAuthority, ForwardCustody, ForwardIngress, ForwardRoute,
    FreshAuthority,
};
use super::*;
use plurx_core::{
    playback_principal::PlaybackPrincipal, sharing_ingress_custody::IngressRegistration,
    sharing_source_sessions::SourceDispatchAssignment,
    store::sharing_source_ingress_custody::SourceCustodyWrite,
};
use std::{sync::Arc, time::Instant};
#[derive(Clone)]
pub(super) struct LocalCustody {
    registration: IngressRegistration,
    obligation: crate::sharing_connection_custody::CapturedIngressObligation,
}
async fn repair_local(
    state: &crate::state::AppState,
    entry: &SourceStartEntry,
    value: &SourceDispatchAssignment,
) -> Result<(), ApiError> {
    use futures_util::FutureExt;
    let entries = entry
        .local_custody
        .lock()
        .expect("actual local Source custody")
        .clone();
    for held in entries {
        let Some(receipt) = held.obligation.joined().now_or_never() else {
            continue;
        };
        let permit = state
            .sharing
            .accepted_drivers
            .reconcile_guard(&held.obligation)
            .await;
        let result = state
            .store
            .acknowledge_source_ingress_custody(value, &held.registration, receipt.confirmation())
            .await
            .map_err(|_| unavailable())?;
        if matches!(
            result,
            SourceCustodyWrite::Applied
                | SourceCustodyWrite::ExactReplay
                | SourceCustodyWrite::ReconciledClosed
        ) {
            held.obligation
                .release_after_ack(&receipt)
                .map_err(|_| unavailable())?;
            if let Ok(permit) = permit {
                permit.complete();
            }
            entry
                .local_custody
                .lock()
                .expect("actual local Source custody")
                .retain(|row| row.registration != held.registration);
        }
    }
    Ok(())
}

fn input(path: &str, bytes: &[u8]) -> Result<SourceStartInput, ApiError> {
    let parts: Vec<_> = path.split('/').collect();
    match parts.as_slice() {
        ["", "sharing", "v1", "items", item, "files", file, "sessions"] => {
            parse_start_request(bytes, item, file)
        }
        ["", "sharing", "v1", "items", item, "files", file, "sessions", request, action] => {
            match *action {
                "control" => Ok(parse_live_operation(bytes, item, file, request, true)?
                    .0
                    .start),
                "vod-status" => Ok(parse_live_operation(bytes, item, file, request, false)?
                    .0
                    .start),
                "resources" => Ok(parse_resource_request(bytes, item, file, request)?
                    .operation
                    .start),
                "direct" => Ok(direct::parse_direct_request(bytes, item, file, request)?
                    .0
                    .start),
                "status" | "end" => Ok(parse_operation_request(bytes, item, file, request)?.start),
                _ => Err(invalid()),
            }
        }
        _ => Err(invalid()),
    }
}
fn fingerprint(input: &SourceStartInput) -> String {
    use sha2::Digest;
    let mut hash = sha2::Sha256::new();
    hash.update(b"plurx.sharing-source-invocation.v2\0");
    hash.update(&input.canonical_recipe);
    format!("{:x}", hash.finalize())
}
pub(super) fn actual_assignment(entry: &SourceStartEntry) -> Option<SourceDispatchAssignment> {
    let stage = entry
        .task
        .stage
        .lock()
        .expect("actual Source invocation stage");
    match &*stage {
        SourceStartTaskStage::Assigned(value)
        | SourceStartTaskStage::Activating(value)
        | SourceStartTaskStage::InvokingFactory(value) => Some(value.clone()),
        SourceStartTaskStage::FactoryRefused(value) => Some(value.assignment().clone()),
        _ => None,
    }
}
fn retained(
    state: &crate::state::AppState,
    wire: &ForwardAuthority,
) -> Result<(Arc<SourceStartEntry>, SourceDispatchAssignment), ApiError> {
    let entries = state
        .transcode
        .source_http_starts
        .entries
        .lock()
        .expect("actual Source owners");
    let settled = state
        .transcode
        .source_http_starts
        .settled
        .lock()
        .expect("actual retained Source cleanup owners");
    for entry in entries.iter().chain(settled.iter()) {
        if entry.identity.request_id != wire.request_id
            || entry.identity.reference != wire.reference
            || !entry
                .authenticated_hashes
                .lock()
                .expect("actual owner hashes")
                .iter()
                .any(|hash| hash == &wire.credential_hash)
        {
            continue;
        }
        let Some(assignment) = actual_assignment(entry) else {
            continue;
        };
        if assignment.binding().principal() == &wire.principal
            && assignment.binding().incarnation_id() == wire.incarnation_id
            && assignment.binding().request_fingerprint() == wire.request_fingerprint
            && assignment.binding().playback_id() == wire.playback_id
            && assignment.owner_node_id() == wire.owner_node_id
            && assignment.dispatch_generation() == wire.dispatch_generation
            && assignment.custody_identity() == wire.owner_identity
            && wire.expected_registry_boot == state.sharing.accepted_drivers.boot_id()
        {
            return Ok((Arc::clone(entry), assignment));
        }
    }
    Err(unavailable())
}
fn authority(
    state: &crate::state::AppState,
    entry: &SourceStartEntry,
    value: &SourceDispatchAssignment,
    hash: &str,
) -> ForwardAuthority {
    ForwardAuthority {
        credential_hash: hash.to_owned(),
        principal: value.binding().principal().clone(),
        owner_node_id: value.owner_node_id().to_owned(),
        incarnation_id: value.binding().incarnation_id(),
        dispatch_generation: value.dispatch_generation(),
        expected_registry_boot: state.sharing.accepted_drivers.boot_id(),
        reference: entry.identity.reference.clone(),
        request_id: entry.identity.request_id,
        request_fingerprint: value.binding().request_fingerprint().to_owned(),
        playback_id: value.binding().playback_id().to_owned(),
        owner_identity: value.custody_identity(),
        cleanup_only: false,
    }
}
pub(super) async fn assigned(
    state: &crate::state::AppState,
    entry: &Arc<SourceStartEntry>,
    deadline: Instant,
) -> Result<SourceDispatchAssignment, ApiError> {
    loop {
        let changed = entry.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        // A completed refusal is retained invocation evidence, not a dispatch
        // assignment or negative-admission proof. Preserve its typed response
        // before the caller can register a protected writer on this entry.
        if let Some(failure) = entry
            .result
            .lock()
            .expect("owned Source result")
            .as_ref()
            .and_then(|result| result.as_ref().err())
            .copied()
        {
            return Err(failure.response());
        }
        if let Some(value) = actual_assignment(entry) {
            return Ok(value);
        }
        // Completion can race the assignment observation. Match this final
        // snapshot too, so a newly observed refusal never becomes generic503.
        match entry.result.lock().expect("owned Source result").as_ref() {
            Some(Err(failure)) => return Err(failure.response()),
            Some(Ok(_)) => return Err(unavailable()),
            None => {}
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed)
            .await
            .map_err(|_| unavailable())?;
        if state.sharing.accepted_drivers.boot_id().is_nil() {
            return Err(unavailable());
        }
    }
}
pub(super) async fn resolve_forward_route(
    state: &crate::state::AppState,
    headers: &SourceHeaders,
    path: &str,
    bytes: &[u8],
    connection: &crate::SharingConnectionCancellation,
    deadline: Instant,
) -> Result<ForwardRoute, ApiError> {
    let input = input(path, bytes)?;
    let hash = source_credential_hash(headers)?;
    let viewer = viewer_hash(headers)?;
    if state
        .transcode
        .source_http_starts
        .cleanup_entry(&hash, &viewer, &input)
        .is_ok()
    {
        return Ok(ForwardRoute::Local);
    }
    let target = &input.reference;
    if let Some(route) = state
        .store
        .lookup_source_ingress_route(
            &hash,
            &viewer,
            &input.request_id.to_string(),
            target.server_id,
            target.catalogue_epoch,
            target.library_id.as_str(),
            target.item_id.as_str(),
            target.file_id.as_str(),
            target.revision.as_str(),
        )
        .await
        .map_err(|_| unavailable())?
    {
        if route.request_fingerprint != fingerprint(&input) {
            return Err(invalid());
        }
        if route.owner_node_id == state.node_id {
            return Ok(ForwardRoute::Local);
        }
        let principal = PlaybackPrincipal::sharing(route.grant_id, &route.viewer_key)
            .map_err(|_| unavailable())?;
        if route.dispatch_generation == 0 {
            return Ok(ForwardRoute::Pending(Box::new(FreshAuthority {
                credential_hash: hash,
                principal,
                candidate_node_id: route.owner_node_id,
                expected_registry_boot: Some(route.registry_boot_id),
                reference: input.reference,
                request_id: input.request_id,
            })));
        }
        if route.dispatch_generation != 1 {
            return Err(unavailable());
        }
        let cleanup_only = path.ends_with("/end")
            || (path.ends_with("/status")
                && (route.sealed || current_reference(state, headers, target).await.is_err()));
        return Ok(ForwardRoute::Retained(Box::new(ForwardAuthority {
            credential_hash: hash,
            principal,
            owner_node_id: route.owner_node_id,
            incarnation_id: route.incarnation_id,
            dispatch_generation: 1,
            expected_registry_boot: route.registry_boot_id,
            reference: input.reference,
            request_id: input.request_id,
            request_fingerprint: route.request_fingerprint,
            playback_id: route.playback_id,
            owner_identity: route.owner_identity,
            cleanup_only,
        })));
    }
    if let Some((principal, node, boot)) = state
        .transcode
        .source_http_starts
        .forwarding
        .pending_cleanup_route(
            &hash,
            &viewer,
            input.request_id,
            target,
            &input.canonical_recipe,
        )
    {
        if node == state.node_id {
            return Ok(ForwardRoute::Local);
        }
        return Ok(ForwardRoute::Pending(Box::new(FreshAuthority {
            credential_hash: hash,
            principal,
            candidate_node_id: node,
            expected_registry_boot: Some(boot),
            reference: input.reference,
            request_id: input.request_id,
        })));
    }
    // NULL/absent assignment never permits another candidate after dispatch.
    let (_, grant) = current_reference(state, headers, target).await?;
    let principal = PlaybackPrincipal::sharing(grant, &viewer).map_err(|_| unavailable())?;
    if let Some((node, boot)) = state
        .transcode
        .source_http_starts
        .forwarding
        .pending_candidate(
            &principal,
            input.request_id,
            target,
            &input.canonical_recipe,
        )
    {
        if node == state.node_id {
            return Ok(ForwardRoute::Local);
        }
        let boot = boot.ok_or_else(unavailable)?;
        return Ok(ForwardRoute::Pending(Box::new(FreshAuthority {
            credential_hash: hash,
            principal,
            candidate_node_id: node,
            expected_registry_boot: Some(boot),
            reference: input.reference,
            request_id: input.request_id,
        })));
    }
    if !path.ends_with("/sessions") {
        return Err(unavailable());
    }
    let local = FreshAuthority {
        credential_hash: hash.clone(),
        principal: principal.clone(),
        candidate_node_id: state.node_id.clone(),
        expected_registry_boot: Some(state.sharing.accepted_drivers.boot_id()),
        reference: input.reference.clone(),
        request_id: input.request_id,
    };
    let (local, remote) = tokio::join!(
        source_forward_locality(state, &local, connection, deadline),
        forwarding::collect_fresh_location(
            state,
            &principal,
            &hash,
            &input.reference,
            input.request_id,
            deadline
        )
    );
    let mut candidates = Vec::new();
    if local?.is_some() {
        candidates.push((
            state.node_id.clone(),
            state.sharing.accepted_drivers.boot_id(),
        ));
    }
    if let Some(remote) = remote? {
        candidates.push(remote);
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0));
    let (node, boot) = candidates.into_iter().next().ok_or_else(unavailable)?;
    if node == state.node_id {
        return Ok(ForwardRoute::Local);
    }
    Ok(ForwardRoute::Fresh(Box::new(FreshAuthority {
        credential_hash: hash,
        principal,
        candidate_node_id: node,
        expected_registry_boot: Some(boot),
        reference: input.reference,
        request_id: input.request_id,
    })))
}
pub(super) async fn prepare_forward_start(
    state: &crate::state::AppState,
    wire: &FreshAuthority,
    bytes: &[u8],
    deadline: Instant,
) -> Result<ForwardAuthority, ApiError> {
    if wire.candidate_node_id != state.node_id
        || wire
            .expected_registry_boot
            .is_some_and(|boot| boot != state.sharing.accepted_drivers.boot_id())
    {
        return Err(unavailable());
    }
    let input = parse_start_request(
        bytes,
        wire.reference.item_id.as_str(),
        wire.reference.file_id.as_str(),
    )?;
    if input.reference != wire.reference || input.request_id != wire.request_id {
        return Err(invalid());
    }
    let headers = SourceHeaders::verified_fresh(
        wire.credential_hash.clone(),
        wire.principal.clone(),
        deadline,
    );
    let (hash, grant) = current_reference(state, &headers, &input.reference).await?;
    let viewer = viewer_hash(&headers)?;
    let (entry, new) = state
        .transcode
        .source_http_starts
        .register(grant, &viewer, &input, &hash)
        .map_err(SourceStartFailure::response)?;
    if new {
        entry.start_owned_task(Arc::new(state.clone()), headers, input, deadline);
    }
    let value = assigned(state, &entry, deadline).await?;
    let ledger = state
        .store
        .source_ingress_custody(&value)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    if ledger.state.source_routing().is_none() || ledger.state.is_sealed() {
        return Err(unavailable());
    }
    Ok(authority(state, &entry, &value, &hash))
}
fn registration(ingress: &ForwardIngress) -> Result<IngressRegistration, ApiError> {
    Ok(IngressRegistration {
        node_id: ingress.node_id.clone(),
        boot_id: ingress.driver.boot_id,
        connection_id: ingress.driver.connection_id,
        driver_sequence: ingress.driver.driver_sequence,
        registration_sequence: ingress.registration_sequence.ok_or_else(unavailable)?,
        closed_confirmation: None,
    })
}
pub(super) async fn validate_retained_forward(
    state: &crate::state::AppState,
    wire: &ForwardAuthority,
    ingress: &ForwardIngress,
    path: &str,
    bytes: &[u8],
    deadline: Instant,
) -> Result<(), ApiError> {
    let (entry, value) = retained(state, wire)?;
    let input = input(path, bytes)?;
    if input.reference != entry.identity.reference
        || input.request_id != entry.identity.request_id
        || fingerprint(&input) != value.binding().request_fingerprint()
    {
        return Err(invalid());
    }
    if path.ends_with("/end") || (path.ends_with("/status") && wire.cleanup_only) {
        return Ok(());
    }
    if wire.cleanup_only {
        return Err(unavailable());
    }
    let headers = SourceHeaders::verified_fresh(
        wire.credential_hash.clone(),
        wire.principal.clone(),
        deadline,
    );
    current_reference(state, &headers, &wire.reference).await?;
    let ledger = state
        .store
        .source_ingress_custody(&value)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    let reg = registration(ingress)?;
    if ledger.state.is_sealed() || !ledger.state.open().any(|slot| slot == &reg) {
        return Err(unavailable());
    }
    Ok(())
}
pub(super) async fn apply_forward_custody(
    state: &crate::state::AppState,
    wire: &ForwardAuthority,
    custody: &ForwardCustody,
    deadline: Instant,
) -> Result<CustodyReply, ApiError> {
    let (entry, value) = retained(state, wire)?;
    let reg = registration(&custody.ingress)?;
    let result = match &custody.action {
        CustodyAction::Register => {
            let headers = SourceHeaders::verified_fresh(
                wire.credential_hash.clone(),
                wire.principal.clone(),
                deadline,
            );
            current_reference(state, &headers, &wire.reference).await?;
            let members = state
                .membership
                .observe_source_admission_members()
                .await
                .map_err(|_| unavailable())?
                .ok_or_else(unavailable)?;
            state
                .store
                .register_source_ingress_custody(&value, &reg, &members)
                .await
        }
        CustodyAction::Ack { receipt } => {
            if !receipt.matches(&custody.ingress.driver) {
                return Err(unavailable());
            }
            state
                .store
                .acknowledge_source_ingress_custody(&value, &reg, receipt.confirmation())
                .await
        }
    }
    .map_err(|_| unavailable())?;
    entry.changed.notify_waiters();
    Ok(match result {
        SourceCustodyWrite::Applied | SourceCustodyWrite::ExactReplay => {
            if matches!(custody.action, CustodyAction::Register) {
                CustodyReply::Registered
            } else {
                CustodyReply::Acknowledged
            }
        }
        SourceCustodyWrite::ReconciledClosed => CustodyReply::ReconciledClosed,
        SourceCustodyWrite::Refused => CustodyReply::Unresolved,
    })
}
pub(super) async fn execute_forward_unassigned_cleanup(
    state: &crate::state::AppState,
    wire: &FreshAuthority,
    ingress: &ForwardIngress,
    path: &str,
    bytes: &[u8],
    deadline: Instant,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    if wire.candidate_node_id != state.node_id
        || wire.expected_registry_boot != Some(state.sharing.accepted_drivers.boot_id())
    {
        return Err(unavailable());
    }
    let input = input(path, bytes)?;
    let PlaybackPrincipal::Sharing { viewer_key, .. } = &wire.principal else {
        return Err(unavailable());
    };
    let entry = state
        .transcode
        .source_http_starts
        .cleanup_entry(&wire.credential_hash, viewer_key.as_str(), &input)
        .map_err(SourceStartFailure::response)?;
    if path.ends_with("/end") {
        let owner = entry.end(Arc::new(state.clone()));
        if end_uses_registered_driver(state, &entry, &ingress.driver).await? {
            return Err(unavailable());
        }
        return owner
            .wait(deadline)
            .await
            .map(|receipt| axum::Json(receipt).into_response())
            .map_err(SourceStartFailure::response);
    }
    if !path.ends_with("/status") {
        return Err(unavailable());
    }
    // Actual retained invocation facts only; no reconstructed g1/playable envelope.
    Ok(
        axum::Json(
            serde_json::json!({"state":"unresolved","request_id":entry.identity.request_id}),
        )
        .into_response(),
    )
}

pub(super) async fn register_local(
    state: &crate::state::AppState,
    connection: &crate::SharingConnectionCancellation,
    entry: &Arc<SourceStartEntry>,
    value: &SourceDispatchAssignment,
) -> Result<(), ApiError> {
    repair_local(state, entry, value).await?;
    // Definite never-sent refusals occur before reserving an ordinal.
    let existing = state
        .store
        .source_ingress_custody(value)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    if existing.state.is_sealed() {
        return Err(unavailable());
    }
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    let captured = state
        .sharing
        .accepted_drivers
        .capture(connection, &state.node_id)
        .map_err(|_| unavailable())?;
    let driver = captured.id().clone();
    let mut permit = state
        .sharing
        .accepted_drivers
        .registration_guard()
        .await
        .map_err(|_| unavailable())?;
    let obligation = captured
        .prepare_obligation(
            &mut permit,
            "source",
            value.binding().incarnation_id(),
            &value.custody_identity(),
        )
        .map_err(|_| unavailable())?;
    let reg = IngressRegistration {
        node_id: state.node_id.clone(),
        boot_id: driver.boot_id,
        connection_id: driver.connection_id,
        driver_sequence: driver.driver_sequence,
        registration_sequence: obligation.registration_sequence(),
        closed_confirmation: None,
    };
    if existing.state.open().any(|slot| slot == &reg) {
        permit.complete();
        return Ok(());
    }
    if entry
        .local_custody
        .lock()
        .expect("actual local custody")
        .len()
        >= 32
    {
        permit.complete();
        let _ = obligation.release_refused_registration();
        return Err(unavailable());
    }
    let (completed, completion) = tokio::sync::oneshot::channel::<()>();
    let ack_state = state.clone();
    let ack_assignment = value.clone();
    let ack_reg = reg.clone();
    let ack_obligation = obligation.clone();
    let monitor = connection.monitor(async move {
        // Even physical closure cannot race a later Register send by this owner.
        // Cancellation drops the sender; ambiguity then requires guarded ACK fencing.
        let _ = completion.await;
        let receipt = ack_obligation.joined().await;
        let reconciliation = ack_state
            .sharing
            .accepted_drivers
            .reconcile_guard(&ack_obligation)
            .await;
        let future = ack_state.store.acknowledge_source_ingress_custody(
            &ack_assignment,
            &ack_reg,
            receipt.confirmation(),
        );
        if let Ok(Ok(
            SourceCustodyWrite::Applied
            | SourceCustodyWrite::ExactReplay
            | SourceCustodyWrite::ReconciledClosed,
        )) = tokio::time::timeout(std::time::Duration::from_secs(60), future).await
        {
            if ack_obligation.release_after_ack(&receipt).is_ok() {
                if let Ok(permit) = reconciliation {
                    permit.complete();
                }
            }
        }
    });
    if monitor.is_err() {
        permit.complete();
        let _ = obligation.release_refused_registration();
        return Err(unavailable());
    }
    entry
        .local_custody
        .lock()
        .expect("actual local custody")
        .push(LocalCustody {
            registration: reg.clone(),
            obligation: obligation.clone(),
        });
    let result = state
        .store
        .register_source_ingress_custody(value, &reg, &members)
        .await;
    let _ = completed.send(());
    match result.map_err(|_| unavailable())? {
        SourceCustodyWrite::Applied | SourceCustodyWrite::ExactReplay => {
            permit.complete();
            entry.changed.notify_waiters();
            Ok(())
        }
        SourceCustodyWrite::Refused | SourceCustodyWrite::ReconciledClosed => Err(unavailable()),
    }
}

pub(super) async fn retire_custody_with_mode(
    state: &crate::state::AppState,
    value: &SourceDispatchAssignment,
    deadline: Instant,
    mode: crate::sharing_connection_custody::DriverCloseMode,
) -> Result<(), ApiError> {
    use crate::sharing_connection_custody::{AcceptedDriverId, DriverCloseRequest};
    match state
        .store
        .seal_source_ingress_custody(value)
        .await
        .map_err(|_| unavailable())?
    {
        SourceCustodyWrite::Applied | SourceCustodyWrite::ExactReplay => {}
        SourceCustodyWrite::Refused | SourceCustodyWrite::ReconciledClosed => {
            return Err(unavailable())
        }
    }
    let ledger = state
        .store
        .source_ingress_custody(value)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    use futures_util::{stream::FuturesUnordered, StreamExt};
    let mut closures = FuturesUnordered::new();
    for slot in ledger.state.open().cloned() {
        closures.push(async move {
            let left = deadline
                .saturating_duration_since(Instant::now())
                .as_millis()
                .min(305_000);
            if left == 0 {
                return Err(unavailable());
            }
            let request = DriverCloseRequest {
                driver: AcceptedDriverId {
                    boot_id: slot.boot_id,
                    connection_id: slot.connection_id,
                    driver_sequence: slot.driver_sequence,
                },
                registration_sequence: slot.registration_sequence,
                principal_kind: "source".into(),
                incarnation_id: value.binding().incarnation_id(),
                owner_identity: value.custody_identity(),
                expected_owner_epoch: value.dispatch_generation(),
                mode,
                deadline_unix_ms: crate::state::clock_ms()
                    + i64::try_from(left).map_err(|_| unavailable())?,
            };
            let close = async {
                if slot.node_id == state.node_id {
                    state
                        .sharing
                        .accepted_drivers
                        .close(value.owner_node_id(), &request)
                        .await
                        .map_err(|_| unavailable())
                } else {
                    state
                        .media_sessions
                        .close_sharing_ingress(&slot.node_id, &request)
                        .await
                        .map_err(|_| unavailable())
                }
            };
            let receipt = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), close)
                .await
                .map_err(|_| unavailable())??;
            if !receipt.matches(&request.driver) {
                return Err(unavailable());
            }
            Ok((slot, receipt))
        });
    }
    // Poll every bounded close exchange under the same inherited deadline.
    // One unreachable ingress cannot prevent the other actual drivers from
    // receiving their drain/revoke signal. No independently spawned work.
    let mut closed = Vec::new();
    let mut failed = false;
    while let Some(outcome) = closures.next().await {
        match outcome {
            Ok(receipt) => closed.push(receipt),
            Err(_) => failed = true,
        }
    }
    // Serialize ledger acknowledgements after actual closure aggregation so
    // this fanout cannot create optimistic CAS contention against itself.
    for (slot, receipt) in closed {
        let ack = tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline),
            state
                .store
                .acknowledge_source_ingress_custody(value, &slot, receipt.confirmation()),
        )
        .await;
        if !matches!(
            ack,
            Ok(Ok(SourceCustodyWrite::Applied
                | SourceCustodyWrite::ExactReplay
                | SourceCustodyWrite::ReconciledClosed))
        ) {
            failed = true;
        }
    }
    if failed {
        return Err(unavailable());
    }
    let ledger = tokio::time::timeout_at(
        tokio::time::Instant::from_std(deadline),
        state.store.source_ingress_custody(value),
    )
    .await
    .map_err(|_| unavailable())?
    .map_err(|_| unavailable())?
    .ok_or_else(unavailable)?;
    if !ledger.state.is_sealed() || !ledger.state.settled() {
        return Err(unavailable());
    }
    Ok(())
}

pub(super) async fn retire_custody(
    state: &crate::state::AppState,
    value: &SourceDispatchAssignment,
    deadline: Instant,
) -> Result<(), ApiError> {
    retire_custody_with_mode(
        state,
        value,
        deadline,
        crate::sharing_connection_custody::DriverCloseMode::Drain,
    )
    .await
}

pub(super) async fn end_uses_registered_driver(
    state: &crate::state::AppState,
    entry: &SourceStartEntry,
    driver: &crate::sharing_connection_custody::AcceptedDriverId,
) -> Result<bool, ApiError> {
    let Some(value) = actual_assignment(entry) else {
        return Ok(false);
    };
    let ledger = state
        .store
        .source_ingress_custody(&value)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    let same_driver = ledger.state.open().any(|slot| {
        slot.boot_id == driver.boot_id
            && slot.connection_id == driver.connection_id
            && slot.driver_sequence == driver.driver_sequence
    });
    Ok(same_driver)
}

pub(super) async fn publication_allowed(
    state: &crate::state::AppState,
    value: &SourceDispatchAssignment,
) -> Result<(), ApiError> {
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    state
        .store
        .prepare_source_ingress_admission(value, state.sharing.accepted_drivers.boot_id(), &members)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    Ok(())
}

static PLACEMENT_IO: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(32)));
/// Read-only locality observation. The actual accepted driver owns the existing
/// scanner-identity inspection even when the request waiter disconnects.
pub(super) async fn source_forward_locality(
    state: &crate::state::AppState,
    wire: &FreshAuthority,
    connection: &crate::SharingConnectionCancellation,
    deadline: Instant,
) -> Result<Option<(i64, i64)>, ApiError> {
    if wire.candidate_node_id != state.node_id
        || wire
            .expected_registry_boot
            .is_some_and(|boot| boot != state.sharing.accepted_drivers.boot_id())
    {
        return Err(unavailable());
    }
    let headers = SourceHeaders::verified_fresh(
        wire.credential_hash.clone(),
        wire.principal.clone(),
        deadline,
    );
    current_reference(state, &headers, &wire.reference)
        .await
        .inspect_err(|_error| {
            #[cfg(test)]
            eprintln!("Source locate actual owner refused current reference");
        })?;
    #[cfg(test)]
    eprintln!("Source locate actual owner current reference accepted");
    state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_error| {
            #[cfg(test)]
            eprintln!("Source locate actual owner member observation error");
            unavailable()
        })?
        .ok_or_else(|| {
            #[cfg(test)]
            eprintln!("Source locate actual owner member floor unavailable");
            unavailable()
        })?;
    #[cfg(test)]
    eprintln!("Source locate actual owner member floor accepted");
    if !state.serving.accepting_new_media().await {
        #[cfg(test)]
        eprintln!("Source locate actual owner not serving media");
        return Ok(None);
    }
    let file_id = wire
        .reference
        .file_id
        .as_str()
        .parse::<i64>()
        .map_err(|_| unavailable())?;
    let snapshot = state
        .store
        .playback_planning_snapshot(file_id, &crate::transcode::QUALITY_PLANNING_KEYS)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    let file = snapshot.file;
    let size = file.size;
    let mtime = file.mtime;
    let permit = Arc::clone(&PLACEMENT_IO)
        .try_acquire_owned()
        .map_err(|_| unavailable())?;
    let (result, reply) = tokio::sync::oneshot::channel();
    connection
        .monitor(async move {
            let observed = crate::fragment_index_cluster::inspect_source(&file).await;
            let _ = result.send(observed.is_ok());
            drop(permit);
        })
        .map_err(|_| unavailable())?;
    let present = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), reply)
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    if !present {
        return Ok(None);
    }
    current_reference(state, &headers, &wire.reference).await?;
    Ok(Some((size, mtime)))
}
