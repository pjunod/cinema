//! Bounded no-touch invitation admission and at-most-once provider attempts.
use super::*;
use super::{
    broker_client::{Client, Failure},
    snapshots,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use futures_util::{stream, StreamExt};
use plurx_core::store::invitations::{
    AdmitInvitation, BrokerReference, InvitationAdmission, InvitationDispatchOutcome,
    InvitationScope,
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
fn identity(phone: &str, event: &str) -> Result<String, ApiError> {
    let phone = Uuid::parse_str(phone).map_err(|_| fail(503, "unavailable"))?;
    let event = Uuid::parse_str(event).map_err(|_| fail(503, "unavailable"))?;
    let mut bytes = [0_u8; 32];
    bytes[..16].copy_from_slice(phone.as_bytes());
    bytes[16..].copy_from_slice(event.as_bytes());
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}
async fn configured(state: &AppState) -> Option<Client> {
    state
        .invitations
        .broker
        .get_or_init(Client::load)
        .as_ref()
        .ok()?
        .clone()
}
pub(super) async fn cleanup(state: &AppState, after: &mut String) -> Result<(), ApiError> {
    let rows = state
        .store
        .invitation_revocation_page(after)
        .await
        .map_err(store_error)?;
    if let Some(last) = rows.last() {
        *after = last.id.clone();
    } else {
        after.clear();
    }
    let Some(client) = configured(state).await else {
        return Ok(());
    };
    let instance = state.store.instance_id().await.map_err(store_error)?;
    let outcomes = stream::iter(rows.into_iter().map(|row| {
        let client = client.clone();
        let instance = instance.clone();
        async move {
            let Ok(reference) = BrokerReference::decode(&row.id) else {
                return Ok::<(), ApiError>(());
            };
            if reference.ticket_id != row.enrollment_id || !client.matches(&reference) {
                return Ok(());
            };
            let outcome = client.revoke(&instance, &reference).await;
            if outcome == Err(Failure::Remediation) {
                state
                    .invitations
                    .broker_remediation
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            state
                .store
                .record_invitation_revocation(&row.id, outcome.is_ok())
                .await
                .map_err(store_error)
        }
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;
    for outcome in outcomes {
        outcome?;
    }
    Ok(())
}
pub(super) async fn audit(state: &AppState, after: &mut String) -> Result<(), ApiError> {
    let rows = state
        .store
        .invitation_retained_consents(after)
        .await
        .map_err(store_error)?;
    if let Some(last) = rows.last() {
        *after = last.id.clone();
    } else {
        after.clear();
    }
    let results = stream::iter(rows.into_iter().map(|item| async move {
        let reference = BrokerReference::decode(
            item.broker_ticket
                .as_deref()
                .ok_or_else(|| fail(503, "migration_remediation"))?,
        )
        .map_err(|_| fail(503, "migration_remediation"))?;
        if item
            .broker_enrollment
            .as_ref()
            .is_some_and(|id| id != &reference.ticket_id)
        {
            return Err(fail(503, "migration_remediation"));
        }
        let scope = state
            .store
            .invitation_scope(&item.phone_id, &item.receiver_id, item.user_id)
            .await
            .map_err(store_error)?;
        let mut invalid = !item.enabled;
        if let Some(scope) = scope {
            invalid |= !scope.permission_granted
                || item.transport_phone_generation != scope.phone_generation
                || state
                    .store
                    .invitation_login(&scope.phone_digest, now_seconds()?)
                    .await
                    .map_err(store_error)?
                    .is_none_or(|login| login.user_id != item.user_id);
        } else {
            invalid = true;
        }
        if invalid {
            state
                .store
                .queue_invitation_reference(&item.id, item.user_id, reference, now_seconds()?)
                .await
                .map_err(store_error)?;
            state.invitations.changed.notify_waiters();
        }
        Ok::<(), ApiError>(())
    }))
    .buffer_unordered(4)
    .collect::<Vec<_>>()
    .await;
    for result in results {
        result?;
    }
    Ok(())
}
pub(super) async fn dispatch(state: &AppState, scope: InvitationScope) -> Result<(), ApiError> {
    let item = &scope.consent;
    let now = now_seconds()?;
    let Some(phone_login) = state
        .store
        .invitation_login(&scope.phone_digest, now)
        .await
        .map_err(store_error)?
    else {
        return Ok(());
    };
    if phone_login.user_id != item.user_id
        || !scope.permission_granted
        || (item.transport == "android_resident" && !scope.resident_active)
    {
        return Ok(());
    };
    let summaries = snapshots::collect(state, item.user_id, &scope.phone_digest).await?;
    let Some(receiver) = snapshots::unique(&summaries, &item.receiver_id) else {
        return Ok(());
    };
    if receiver.receiver_hash != scope.receiver_hash {
        return Ok(());
    };
    let Some(receiver_login) = state
        .store
        .invitation_login(&receiver.token_digest, now_seconds()?)
        .await
        .map_err(store_error)?
    else {
        return Ok(());
    };
    if receiver_login.user_id != item.user_id {
        return Ok(());
    };
    if item.transport != "android_resident"
        && (item.transport_status != "ready"
            || super::transport::readiness(state, item).await?.is_some())
    {
        return Ok(());
    };
    let admission = AdmitInvitation {
        id: Uuid::new_v4().to_string(),
        enrollment_id: item.id.clone(),
        foreground_id: receiver.foreground_id.to_string(),
        receiver_hash: scope.receiver_hash.clone(),
        phone_generation: scope.phone_generation,
        consent_generation: item.generation,
        transport_generation: item.transport_generation,
        phone_login,
        receiver_login,
        now: now_seconds()?,
    };
    if state
        .store
        .admit_invitation(admission.clone())
        .await
        .map_err(store_error)?
        != InvitationAdmission::Admitted
    {
        return Ok(());
    };
    // An admitted event is not yet a provider attempt. Re-read both login verdicts
    // and unique owner immediately before the durable single-use transition.
    let phone_login = state
        .store
        .invitation_login(&scope.phone_digest, now_seconds()?)
        .await
        .map_err(store_error)?;
    let receiver_login = state
        .store
        .invitation_login(&receiver.token_digest, now_seconds()?)
        .await
        .map_err(store_error)?;
    let latest = match snapshots::collect(state, item.user_id, &scope.phone_digest).await {
        Ok(rows) => rows,
        Err(error) => {
            state
                .store
                .finish_invitation(&admission.id, InvitationDispatchOutcome::Unknown)
                .await
                .map_err(store_error)?;
            state.invitations.changed.notify_waiters();
            return Err(error);
        }
    };
    let same = snapshots::unique(&latest, &item.receiver_id).is_some_and(|s| {
        s.target == receiver.target
            && s.foreground_id == receiver.foreground_id
            && s.token_digest == receiver.token_digest
            && s.receiver_hash == receiver.receiver_hash
    });
    let (Some(phone_login), Some(receiver_login)) = (phone_login, receiver_login) else {
        state
            .store
            .finish_invitation(&admission.id, InvitationDispatchOutcome::Revoked)
            .await
            .map_err(store_error)?;
        state.invitations.changed.notify_waiters();
        return Ok(());
    };
    if !same {
        state
            .store
            .finish_invitation(&admission.id, InvitationDispatchOutcome::Revoked)
            .await
            .map_err(store_error)?;
        state.invitations.changed.notify_waiters();
        return Ok(());
    };
    let attempt = AdmitInvitation {
        phone_login,
        receiver_login,
        now: now_seconds()?,
        ..admission.clone()
    };
    if !state
        .store
        .attempt_invitation(attempt)
        .await
        .map_err(store_error)?
    {
        state
            .store
            .finish_invitation(&admission.id, InvitationDispatchOutcome::Revoked)
            .await
            .map_err(store_error)?;
        state.invitations.changed.notify_waiters();
        return Ok(());
    };
    if !super::transport::dispatchable(state, item, &scope.phone_digest, scope.phone_generation)
        .await?
        || state
            .store
            .invitation_login(&receiver.token_digest, now_seconds()?)
            .await
            .map_err(store_error)?
            .is_none_or(|login| login.user_id != item.user_id)
    {
        state
            .store
            .finish_invitation(&admission.id, InvitationDispatchOutcome::Revoked)
            .await
            .map_err(store_error)?;
        state.invitations.changed.notify_waiters();
        return Ok(());
    }
    let outcome = if item.transport == "android_resident" {
        InvitationDispatchOutcome::ResidentReady
    } else {
        let Some(client) = configured(state).await else {
            state
                .store
                .finish_invitation(&admission.id, InvitationDispatchOutcome::Unknown)
                .await
                .map_err(store_error)?;
            return Ok(());
        };
        let reference = BrokerReference::decode(
            item.broker_ticket
                .as_deref()
                .ok_or_else(|| fail(503, "migration_remediation"))?,
        )
        .map_err(|_| fail(503, "migration_remediation"))?;
        let instance = state.store.instance_id().await.map_err(store_error)?;
        let body = json!({"version":"cinema.invitation.v1","enrollment_id":reference.ticket_id,"installation_id":item.phone_id,"phone_generation":scope.phone_generation,"consent_generation":item.generation,"transport_generation":item.transport_generation,"invitation_id":identity(&item.phone_id,&admission.id)?,"expires_at":admission.now.saturating_add(120)});
        match client.deliver(&instance, &reference, &body).await {
            Ok(status) if matches!(status.as_str(), "accepted" | "duplicate") => {
                InvitationDispatchOutcome::Accepted
            }
            Ok(status) if status == "denied" => InvitationDispatchOutcome::Denied,
            Err(Failure::Remediation) => {
                state
                    .invitations
                    .broker_remediation
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                InvitationDispatchOutcome::Unknown
            }
            _ => InvitationDispatchOutcome::Unknown,
        }
    };
    state
        .store
        .finish_invitation(&admission.id, outcome)
        .await
        .map_err(store_error)?;
    state.invitations.changed.notify_waiters();
    Ok(())
}
pub(crate) async fn run(state: AppState, stop: CancellationToken) {
    let mut after = String::new();
    let mut cleanup_after = String::new();
    let mut audit_after = String::new();
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {_ = stop.cancelled()=>return,_=interval.tick()=>{}}
        let work = async {
            let audited = audit(&state, &mut audit_after).await;
            cleanup(&state, &mut cleanup_after).await?;
            audited?;
            for key in ["cinema.remote_control", "cinema.remote_invitations"] {
                if state
                    .store
                    .get_setting(key)
                    .await
                    .map_err(store_error)?
                    .as_deref()
                    != Some("1")
                {
                    return Ok::<(), ApiError>(());
                };
            }
            let rows = state
                .store
                .invitation_dispatch_candidates(&after)
                .await
                .map_err(store_error)?;
            if let Some(last) = rows.last() {
                after = last.consent.id.clone();
            } else {
                after.clear();
            }
            let results = stream::iter(rows.into_iter().map(|row| dispatch(&state, row)))
                .buffer_unordered(4)
                .collect::<Vec<_>>()
                .await;
            for result in results {
                result?;
            }
            Ok(())
        };
        tokio::select! {_ = stop.cancelled()=>return,result=work=>{if result.is_err(){tracing::debug!("invitation worker deferred unavailable authority");}}}
    }
}
