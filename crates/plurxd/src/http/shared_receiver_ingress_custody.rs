//! Receiver adapters bind common accepted-driver custody to actual B owners.
use super::*;
use crate::sharing_connection_custody::DriverClosureReceipt;
use forwarding::{ReceiverCustodyReply, ReceiverForwardIngress, ReceiverForwardTuple};
use plurx_core::{domain::MediaSessionRoute, sharing_ingress_custody::CustodyMutation};

async fn exact_route(
    state: &AppState,
    tuple: &ReceiverForwardTuple,
) -> Result<MediaSessionRoute, ReceiverStartError> {
    let route = state
        .store
        .media_session_route_by_incarnation(&tuple.incarnation_id.to_string())
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unavailable)?;
    if route.session_id != tuple.session_id.to_string()
        || route.owner_node_id != tuple.owner_node_id
        || route.owner_epoch != tuple.owner_epoch
    {
        return Err(ReceiverStartError::Conflict);
    }
    Ok(route)
}
/// Retained actor identity is required even for cleanup dispatch. This reader
/// never creates an actor or adopts a SQL assignment after process restart.
pub(super) async fn validate_owner(
    state: &AppState,
    tuple: &ReceiverForwardTuple,
) -> Result<ReceiverStartActor, ReceiverStartError> {
    if tuple.owner_node_id != state.node_id {
        return Err(ReceiverStartError::Unavailable);
    }
    let route = exact_route(state, tuple).await?;
    let actor = state
        .sharing
        .receiver_starts
        .by_session(tuple.session_id)
        .ok_or(ReceiverStartError::Unavailable)?;
    if actor.0.intent.recipe.source_request_id != tuple.incarnation_id {
        return Err(ReceiverStartError::Conflict);
    }
    let owner = actor
        .0
        .state
        .lock()
        .expect("receiver owner")
        .owner
        .clone()
        .ok_or(ReceiverStartError::Unavailable)?;
    if owner.incarnation_id != tuple.incarnation_id
        || owner.session_id != tuple.session_id
        || owner.owner_node_id != tuple.owner_node_id
        || owner.owner_epoch != tuple.owner_epoch
    {
        return Err(ReceiverStartError::Conflict);
    }
    let snapshot = state
        .store
        .receiver_ingress_snapshot(&route)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?;
    if let Some(snapshot) = snapshot {
        if snapshot.owner_identity != tuple.owner_identity {
            return Err(ReceiverStartError::Conflict);
        }
    } else {
        let proof = state
            .store
            .receiver_relay_read_authority(&route)
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?
            .ok_or(ReceiverStartError::Unavailable)?;
        if proof.owner_identity() != tuple.owner_identity {
            return Err(ReceiverStartError::Conflict);
        }
    }
    Ok(actor)
}
pub(super) async fn validate_forward_ingress(
    state: &AppState,
    tuple: &ReceiverForwardTuple,
    ingress: &ReceiverForwardIngress,
) -> Result<(), ReceiverStartError> {
    let actor = validate_owner(state, tuple).await?;
    actor.current_delivery_attachment(state).await?;
    let route = exact_route(state, tuple).await?;
    let proof = state
        .store
        .receiver_relay_read_authority(&route)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unavailable)?;
    if proof.owner_identity() != tuple.owner_identity {
        return Err(ReceiverStartError::Conflict);
    }
    let registration = ingress
        .registration
        .as_ref()
        .ok_or(ReceiverStartError::Unavailable)?;
    let snapshot = state
        .store
        .receiver_ingress_snapshot(&route)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unavailable)?;
    if snapshot.state.is_sealed()
        || snapshot.owner_identity != tuple.owner_identity
        || !snapshot
            .state
            .open()
            .any(|slot| slot.same_driver(registration))
    {
        return Err(ReceiverStartError::Unavailable);
    }
    Ok(())
}
pub(super) async fn register_owner(
    state: &AppState,
    tuple: &ReceiverForwardTuple,
    ingress: &ReceiverForwardIngress,
) -> Result<CustodyMutation, ReceiverStartError> {
    validate_owner(state, tuple).await?;
    let route = exact_route(state, tuple).await?;
    let proof = state
        .store
        .receiver_relay_read_authority(&route)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unavailable)?;
    if proof.owner_identity() != tuple.owner_identity {
        return Err(ReceiverStartError::Conflict);
    }
    let actor = state
        .sharing
        .receiver_starts
        .by_session(tuple.session_id)
        .ok_or(ReceiverStartError::Unavailable)?;
    {
        let mut owned = actor.0.state.lock().expect("receiver owner");
        if owned.retirement_started || owned.retired {
            return Err(ReceiverStartError::Unavailable);
        }
        if owned
            .ingress_owner_identity
            .as_ref()
            .is_some_and(|identity| identity != &tuple.owner_identity)
        {
            return Err(ReceiverStartError::Conflict);
        }
        // Before the first fallible Register await, retirement can always find
        // the identity of an already-dispatched or commit-unknown registration.
        owned.ingress_owner_identity = Some(tuple.owner_identity.clone());
    }
    let members = if state.membership.is_replicated() {
        Some(
            state
                .membership
                .observe_ingress_custody_members()
                .await
                .map_err(|_| ReceiverStartError::Unresolved)?
                .ok_or(ReceiverStartError::Unavailable)?,
        )
    } else {
        None
    };
    let registration = ingress
        .registration
        .as_ref()
        .ok_or(ReceiverStartError::Unavailable)?;
    let result = state
        .store
        .register_receiver_ingress(
            &proof,
            members.as_ref(),
            state.sharing.accepted_drivers.boot_id(),
            registration,
        )
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?;
    // A refused CAS may race another accepted write. It cannot discharge an
    // ingress reservation merely because this exchange did not observe it.
    if result == CustodyMutation::Refused {
        return Err(ReceiverStartError::Unresolved);
    }
    Ok(result)
}
pub(super) async fn ack_owner(
    state: &AppState,
    tuple: &ReceiverForwardTuple,
    ingress: &ReceiverForwardIngress,
    receipt: &DriverClosureReceipt,
) -> Result<ReceiverCustodyReply, ReceiverStartError> {
    let driver = ingress
        .driver
        .as_ref()
        .ok_or(ReceiverStartError::Unavailable)?;
    if !receipt.matches(driver) {
        return Err(ReceiverStartError::Unavailable);
    }
    let route = exact_route(state, tuple).await?;
    let snapshot = state
        .store
        .receiver_ingress_snapshot(&route)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unresolved)?;
    if snapshot.owner_identity != tuple.owner_identity {
        return Err(ReceiverStartError::Conflict);
    }
    let registration = ingress
        .registration
        .as_ref()
        .ok_or(ReceiverStartError::Unavailable)?;
    match state
        .store
        .acknowledge_receiver_ingress(&route, &snapshot, registration, receipt.confirmation())
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
    {
        CustodyMutation::Applied => Ok(ReceiverCustodyReply::Applied),
        CustodyMutation::Replay => Ok(ReceiverCustodyReply::ReconciledClosed),
        CustodyMutation::Refused => Ok(ReceiverCustodyReply::Unresolved),
    }
}
pub(super) async fn receiver_forward_cleanup_tuple(
    state: &AppState,
    route: &MediaSessionRoute,
) -> Result<ReceiverForwardTuple, ReceiverStartError> {
    let snapshot = state
        .store
        .receiver_ingress_snapshot(route)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unresolved)?;
    Ok(ReceiverForwardTuple {
        incarnation_id: Uuid::parse_str(&route.incarnation_id)
            .map_err(|_| ReceiverStartError::Unavailable)?,
        session_id: Uuid::parse_str(&route.session_id)
            .map_err(|_| ReceiverStartError::Unavailable)?,
        owner_node_id: route.owner_node_id.clone(),
        owner_epoch: route.owner_epoch,
        owner_identity: snapshot.owner_identity,
    })
}

async fn exchange_at_owner(
    state: &AppState,
    tuple: &ReceiverForwardTuple,
    ingress: &ReceiverForwardIngress,
    operation: forwarding::ReceiverCustodyOperation,
    deadline: Instant,
) -> Result<ReceiverCustodyReply, ReceiverStartError> {
    if tuple.owner_node_id != state.node_id {
        return forwarding::exchange_custody(state, tuple, ingress, operation, deadline).await;
    }
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
        match operation {
            forwarding::ReceiverCustodyOperation::Register => {
                match register_owner(state, tuple, ingress).await? {
                    CustodyMutation::Applied => Ok(ReceiverCustodyReply::Applied),
                    CustodyMutation::Replay => Ok(ReceiverCustodyReply::Replay),
                    CustodyMutation::Refused => Ok(ReceiverCustodyReply::Unresolved),
                }
            }
            forwarding::ReceiverCustodyOperation::Ack { receipt } => {
                ack_owner(state, tuple, ingress, &receipt).await
            }
        }
    })
    .await
    .map_err(|_| ReceiverStartError::Deadline)?
}

/// This cache owns metadata reservations, never sockets. The accepted-driver
/// monitor owns registration through cancellation and its finite closure ACK.
#[derive(Default)]
pub(super) struct ReceiverIngressCache(Mutex<Vec<Arc<ReceiverIngressEntry>>>);
struct ReceiverIngressEntry {
    tuple: ReceiverForwardTuple,
    ingress: ReceiverForwardIngress,
    obligation: crate::sharing_connection_custody::CapturedIngressObligation,
    admitted: Mutex<Option<Result<(), ReceiverStartError>>>,
    changed: tokio::sync::Notify,
    discharged: std::sync::atomic::AtomicBool,
}
pub(super) struct ReceiverIngressGuard {
    pub(super) ingress: ReceiverForwardIngress,
    _entry: Arc<ReceiverIngressEntry>,
}
async fn wait_admission(
    entry: &ReceiverIngressEntry,
    deadline: Instant,
) -> Result<(), ReceiverStartError> {
    loop {
        let changed = entry.changed.notified();
        if let Some(result) = *entry.admitted.lock().expect("receiver ingress admission") {
            return result;
        }
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed)
            .await
            .map_err(|_| ReceiverStartError::Deadline)?;
    }
}
pub(super) async fn receiver_forward_admit(
    state: Arc<AppState>,
    route: &MediaSessionRoute,
    proof: &plurx_core::sharing_receiver_delivery::ReceiverRelayReadAuthority,
    connection: &crate::SharingConnectionCancellation,
    deadline: Instant,
) -> Result<ReceiverIngressGuard, ReceiverStartError> {
    let tuple = ReceiverForwardTuple::from_read(proof);
    if route.incarnation_id != tuple.incarnation_id.to_string()
        || route.session_id != tuple.session_id.to_string()
        || route.owner_node_id != tuple.owner_node_id
        || route.owner_epoch != tuple.owner_epoch
    {
        return Err(ReceiverStartError::Conflict);
    }
    // Unrelated reconciliation runs only under bounded held-slot pressure.
    // Actual local closure is required; a missing ledger never frees a slot.
    let pressure = {
        let entries = state
            .sharing
            .receiver_starts
            .ingress
            .0
            .lock()
            .expect("receiver ingress cache");
        if entries.len() >= 512 {
            entries
                .iter()
                .filter(|entry| {
                    !entry.discharged.load(std::sync::atomic::Ordering::Acquire)
                        && entry.obligation.closed_receipt().is_some()
                })
                .take(8)
                .map(|entry| entry.tuple.clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        }
    };
    let pressure_deadline = deadline.min(Instant::now() + Duration::from_secs(1));
    for principal in pressure {
        let _ = reconcile_principal(state.clone(), connection, &principal, pressure_deadline).await;
    }
    let driver = state
        .sharing
        .accepted_drivers
        .capture(connection, &tuple.owner_node_id)
        .map_err(|_| ReceiverStartError::Capacity)?;
    let id = driver.id().clone();
    let existing = state
        .sharing
        .receiver_starts
        .ingress
        .0
        .lock()
        .expect("receiver ingress cache")
        .iter()
        .find(|entry| {
            entry.tuple.incarnation_id == tuple.incarnation_id
                && entry.tuple.owner_identity == tuple.owner_identity
                && entry.ingress.driver.as_ref() == Some(&id)
        })
        .cloned();
    if let Some(entry) = existing {
        let unresolved = matches!(
            *entry.admitted.lock().expect("receiver ingress admission"),
            Some(Err(_))
        );
        if unresolved {
            reconcile_principal(
                state.clone(),
                connection,
                &tuple,
                deadline.min(Instant::now() + Duration::from_secs(1)),
            )
            .await?;
        }
        wait_admission(&entry, deadline).await?;
        return Ok(ReceiverIngressGuard {
            ingress: entry.ingress.clone(),
            _entry: entry,
        });
    }
    reconcile_principal(
        state.clone(),
        connection,
        &tuple,
        deadline.min(Instant::now() + Duration::from_secs(1)),
    )
    .await?;
    let mut permit = state
        .sharing
        .accepted_drivers
        .registration_guard()
        .await
        .map_err(|_| ReceiverStartError::Capacity)?;
    let obligation = driver
        .prepare_obligation(
            &mut permit,
            "receiver",
            tuple.incarnation_id,
            &tuple.owner_identity,
        )
        .map_err(|_| ReceiverStartError::Unresolved)?;
    let registration = plurx_core::sharing_ingress_custody::IngressRegistration {
        node_id: state.node_id.clone(),
        boot_id: id.boot_id,
        connection_id: id.connection_id,
        driver_sequence: id.driver_sequence,
        registration_sequence: obligation.registration_sequence(),
        closed_confirmation: None,
    };
    let entry = Arc::new(ReceiverIngressEntry {
        tuple,
        ingress: ReceiverForwardIngress {
            driver: Some(id),
            registration: Some(registration),
        },
        obligation,
        admitted: Mutex::new(None),
        changed: Default::default(),
        discharged: Default::default(),
    });
    let raced = {
        let mut entries = state
            .sharing
            .receiver_starts
            .ingress
            .0
            .lock()
            .expect("receiver ingress cache");
        entries.retain(|entry| !entry.discharged.load(std::sync::atomic::Ordering::Acquire));
        let existing = entries
            .iter()
            .find(|old| {
                old.tuple.incarnation_id == entry.tuple.incarnation_id
                    && old.tuple.owner_identity == entry.tuple.owner_identity
                    && old.ingress.driver == entry.ingress.driver
            })
            .cloned();
        if existing.is_none() {
            if entries.len() >= 512 {
                entry
                    .obligation
                    .release_refused_registration()
                    .map_err(|_| ReceiverStartError::Unresolved)?;
                permit.complete();
                return Err(ReceiverStartError::Capacity);
            }
            entries.push(entry.clone());
        }
        existing
    };
    if let Some(existing) = raced {
        // Both callers prepared the SAME physical/principal ordinal. The
        // first accepted monitor owns its pending key; completing/releasing
        // this duplicate permit would remove the first caller's ambiguity.
        wait_admission(&existing, deadline).await?;
        return Ok(ReceiverIngressGuard {
            ingress: existing.ingress.clone(),
            _entry: existing,
        });
    }
    let monitored = entry.clone();
    let state_monitor = state.clone();
    let closed = connection.closed();
    // Attach BEFORE the first durable Register. Monitor-capacity refusal is a
    // definite never-sent registration; it may release this local reservation.
    if connection
        .monitor(async move {
            let outcome = exchange_at_owner(
                &state_monitor,
                &monitored.tuple,
                &monitored.ingress,
                forwarding::ReceiverCustodyOperation::Register,
                deadline,
            )
            .await;
            let admitted = match outcome {
                Ok(ReceiverCustodyReply::Applied | ReceiverCustodyReply::Replay) => {
                    permit.complete();
                    Ok(())
                }
                _ => Err(ReceiverStartError::Unresolved),
            };
            *monitored
                .admitted
                .lock()
                .expect("receiver ingress admission") = Some(admitted);
            monitored.changed.notify_waiters();
            // Existing B actor cancellation and the authenticated exact close
            // RPC own revocation/drain. This monitor adds no SQL poller.
            closed.wait().await;
            let receipt = monitored.obligation.joined().await;
            // Finite join aftermath, owned by this accepted-driver monitor. A
            // failed or lost ACK remains cached unresolved; no timer proves EOF.
            let ack = exchange_at_owner(
                &state_monitor,
                &monitored.tuple,
                &monitored.ingress,
                forwarding::ReceiverCustodyOperation::Ack {
                    receipt: receipt.clone(),
                },
                Instant::now() + Duration::from_secs(9),
            )
            .await;
            if matches!(
                ack,
                Ok(ReceiverCustodyReply::Applied
                    | ReceiverCustodyReply::Replay
                    | ReceiverCustodyReply::ReconciledClosed)
            ) && monitored.obligation.release_after_ack(&receipt).is_ok()
            {
                if let Ok(permit) = state_monitor
                    .sharing
                    .accepted_drivers
                    .reconcile_guard(&monitored.obligation)
                    .await
                {
                    permit.complete();
                }
                monitored
                    .discharged
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        })
        .is_err()
    {
        // No Register was sent; dropping the unpolled future retains its permit
        // until the exact local reservation is explicitly reconciled here.
        entry
            .obligation
            .release_refused_registration()
            .map_err(|_| ReceiverStartError::Unresolved)?;
        if let Ok(permit) = state
            .sharing
            .accepted_drivers
            .reconcile_guard(&entry.obligation)
            .await
        {
            permit.complete();
        }
        *entry.admitted.lock().expect("receiver ingress admission") =
            Some(Err(ReceiverStartError::Capacity));
        entry.changed.notify_waiters();
        entry
            .discharged
            .store(true, std::sync::atomic::Ordering::Release);
        return Err(ReceiverStartError::Capacity);
    }
    wait_admission(&entry, deadline).await?;
    Ok(ReceiverIngressGuard {
        ingress: entry.ingress.clone(),
        _entry: entry,
    })
}

/// Seal the exact principal before closing any transport or sending Source
/// End. One absolute driver deadline applies across sealing, closure and ACK.
pub(super) async fn close_receiver_ingress(
    state: Arc<AppState>,
    route: &MediaSessionRoute,
    original_identity: &str,
    mode: crate::sharing_connection_custody::DriverCloseMode,
    deadline: Instant,
) -> Result<(), ReceiverStartError> {
    if route.owner_node_id != state.node_id {
        return Err(ReceiverStartError::Conflict);
    }
    let mut sealed = None;
    for _ in 0..8 {
        if Instant::now() >= deadline {
            return Err(ReceiverStartError::Deadline);
        }
        if let Some(snapshot) = state
            .store
            .seal_receiver_ingress_route(route, original_identity)
            .await
            .map_err(|_| ReceiverStartError::Unresolved)?
        {
            sealed = Some(snapshot);
            break;
        }
    }
    let snapshot = sealed.ok_or(ReceiverStartError::Unresolved)?;
    let registrations = snapshot.state.open().cloned().collect::<Vec<_>>();
    let mut closes = tokio::task::JoinSet::new();
    for registration in registrations {
        let state = state.clone();
        let route = route.clone();
        let original_identity = original_identity.to_owned();
        closes.spawn(async move {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(ReceiverStartError::Deadline);
            }
            let driver = crate::sharing_connection_custody::AcceptedDriverId {
                boot_id: registration.boot_id,
                connection_id: registration.connection_id,
                driver_sequence: registration.driver_sequence,
            };
            let request = crate::sharing_connection_custody::DriverCloseRequest {
                driver: driver.clone(),
                registration_sequence: registration.registration_sequence,
                principal_kind: "receiver".into(),
                incarnation_id: Uuid::parse_str(&route.incarnation_id)
                    .map_err(|_| ReceiverStartError::Unavailable)?,
                owner_identity: original_identity.clone(),
                expected_owner_epoch: route.owner_epoch,
                mode,
                deadline_unix_ms: clock_ms()
                    .saturating_add(remaining.as_millis().min(315_000) as i64),
            };
            let receipt = if registration.node_id == state.node_id {
                state
                    .sharing
                    .accepted_drivers
                    .authorize_current_cleanup_owner(&route.owner_node_id, &request)
                    .map_err(|_| ReceiverStartError::Unresolved)?;
                state
                    .sharing
                    .accepted_drivers
                    .close(&route.owner_node_id, &request)
                    .await
                    .map_err(|_| ReceiverStartError::Unresolved)?
            } else {
                state
                    .media_sessions
                    .close_sharing_ingress(&registration.node_id, &request)
                    .await
                    .map_err(|_| ReceiverStartError::Unresolved)?
            };
            if !receipt.matches(&driver) {
                return Err(ReceiverStartError::Unresolved);
            }
            for _ in 0..8 {
                if Instant::now() >= deadline {
                    return Err(ReceiverStartError::Deadline);
                }
                let snapshot = state
                    .store
                    .receiver_ingress_snapshot(&route)
                    .await
                    .map_err(|_| ReceiverStartError::Unresolved)?
                    .ok_or(ReceiverStartError::Unresolved)?;
                if snapshot.owner_identity != original_identity || !snapshot.state.is_sealed() {
                    return Err(ReceiverStartError::Conflict);
                }
                match state
                    .store
                    .acknowledge_receiver_ingress(
                        &route,
                        &snapshot,
                        &registration,
                        receipt.confirmation(),
                    )
                    .await
                    .map_err(|_| ReceiverStartError::Unresolved)?
                {
                    CustodyMutation::Applied | CustodyMutation::Replay => return Ok(()),
                    CustodyMutation::Refused => {}
                }
            }
            Err(ReceiverStartError::Unresolved)
        });
    }
    let mut result = Ok(());
    while !closes.is_empty() {
        match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), closes.join_next())
            .await
        {
            Ok(Some(Ok(Ok(())))) => {}
            Ok(Some(Ok(Err(error)))) => {
                result = Err(error);
            }
            Ok(Some(Err(_))) => {
                result = Err(ReceiverStartError::Unresolved);
            }
            Ok(None) => break,
            Err(_) => {
                closes.shutdown().await;
                return Err(ReceiverStartError::Deadline);
            }
        }
    }
    result?;
    let snapshot = state
        .store
        .receiver_ingress_snapshot(route)
        .await
        .map_err(|_| ReceiverStartError::Unresolved)?
        .ok_or(ReceiverStartError::Unresolved)?;
    if snapshot.owner_identity != original_identity || !snapshot.state.settled() {
        return Err(ReceiverStartError::Unresolved);
    }
    Ok(())
}

/// Retry only an exact principal's retained ambiguity/actual closed receipts.
/// The new accepted driver owns this finite exchange if its HTTP waiter drops.
async fn reconcile_principal(
    state: Arc<AppState>,
    connection: &crate::SharingConnectionCancellation,
    tuple: &ReceiverForwardTuple,
    deadline: Instant,
) -> Result<(), ReceiverStartError> {
    let candidates = state
        .sharing
        .receiver_starts
        .ingress
        .0
        .lock()
        .expect("receiver ingress cache")
        .iter()
        .filter(|entry| {
            entry.tuple.incarnation_id == tuple.incarnation_id
                && entry.tuple.owner_identity == tuple.owner_identity
                && !entry.discharged.load(std::sync::atomic::Ordering::Acquire)
                && (entry.obligation.closed_receipt().is_some()
                    || matches!(
                        *entry.admitted.lock().expect("receiver ingress admission"),
                        Some(Err(_))
                    ))
        })
        .take(8)
        .cloned()
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(());
    }
    let (sent, received) = tokio::sync::oneshot::channel();
    connection
        .monitor(async move {
            let mut result = Ok(());
            for entry in candidates {
                if entry.discharged.load(std::sync::atomic::Ordering::Acquire) {
                    continue;
                }
                if Instant::now() >= deadline {
                    result = Err(ReceiverStartError::Deadline);
                    break;
                }
                let mut current = entry.tuple.clone();
                if let Ok(Some(route)) = state
                    .store
                    .media_session_route_by_incarnation(&current.incarnation_id.to_string())
                    .await
                {
                    if route.session_id != current.session_id.to_string() {
                        result = Err(ReceiverStartError::Conflict);
                        continue;
                    }
                    if let Ok(Some(snapshot)) = state.store.receiver_ingress_snapshot(&route).await
                    {
                        if snapshot.owner_identity == current.owner_identity {
                            current.owner_node_id = route.owner_node_id;
                            current.owner_epoch = route.owner_epoch;
                        }
                    }
                }
                if let Some(receipt) = entry.obligation.closed_receipt() {
                    let reply = exchange_at_owner(
                        &state,
                        &current,
                        &entry.ingress,
                        forwarding::ReceiverCustodyOperation::Ack {
                            receipt: receipt.clone(),
                        },
                        deadline,
                    )
                    .await;
                    if matches!(
                        reply,
                        Ok(ReceiverCustodyReply::Applied
                            | ReceiverCustodyReply::Replay
                            | ReceiverCustodyReply::ReconciledClosed)
                    ) && entry.obligation.release_after_ack(&receipt).is_ok()
                    {
                        if let Ok(permit) = state
                            .sharing
                            .accepted_drivers
                            .reconcile_guard(&entry.obligation)
                            .await
                        {
                            permit.complete();
                        }
                        entry
                            .discharged
                            .store(true, std::sync::atomic::Ordering::Release);
                    } else {
                        result = Err(ReceiverStartError::Unresolved);
                    }
                } else if !matches!(
                    *entry.admitted.lock().expect("receiver ingress admission"),
                    Some(Ok(()))
                ) {
                    let reply = exchange_at_owner(
                        &state,
                        &current,
                        &entry.ingress,
                        forwarding::ReceiverCustodyOperation::Register,
                        deadline,
                    )
                    .await;
                    if matches!(
                        reply,
                        Ok(ReceiverCustodyReply::Applied | ReceiverCustodyReply::Replay)
                    ) {
                        if let Ok(permit) = state
                            .sharing
                            .accepted_drivers
                            .reconcile_guard(&entry.obligation)
                            .await
                        {
                            permit.complete();
                        }
                        *entry.admitted.lock().expect("receiver ingress admission") = Some(Ok(()));
                        entry.changed.notify_waiters();
                    } else {
                        result = Err(ReceiverStartError::Unresolved);
                    }
                }
            }
            let _ = sent.send(result);
        })
        .map_err(|_| ReceiverStartError::Capacity)?;
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), received)
        .await
        .map_err(|_| ReceiverStartError::Deadline)?
        .map_err(|_| ReceiverStartError::Unresolved)?
}

pub(super) async fn receiver_forward_reconcile_cleanup(
    state: Arc<AppState>,
    route: &MediaSessionRoute,
    connection: &crate::SharingConnectionCancellation,
    deadline: Instant,
) -> Result<(), ReceiverStartError> {
    let tuple = receiver_forward_cleanup_tuple(&state, route).await?;
    reconcile_principal(
        state,
        connection,
        &tuple,
        deadline.min(Instant::now() + Duration::from_secs(1)),
    )
    .await
}

/// Real cached confirmation plus exact compact sealed ledger permits only End replay.
pub(super) async fn confirmed_terminal_end(state: &AppState, tuple: &ReceiverForwardTuple) -> bool {
    if !state
        .sharing
        .receiver_starts
        .confirmed_end(tuple.session_id)
    {
        return false;
    }
    let Ok(route) = exact_route(state, tuple).await else {
        return false;
    };
    if route.state != "ended" {
        return false;
    }
    matches!(state.store.receiver_ingress_snapshot(&route).await,
        Ok(Some(snapshot)) if snapshot.owner_identity == tuple.owner_identity
            && snapshot.state.is_sealed() && snapshot.state.settled())
}
