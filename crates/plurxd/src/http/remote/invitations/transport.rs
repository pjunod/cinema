//! Native-authorized capability issuance and immutable broker confirmation.
use super::broker_client::{Client, Failure};
use super::*;
use plurx_core::store::invitations::{
    BrokerReference, ConfirmInvitationTransport, StartInvitationTransport,
};

fn broker_error(error: Failure) -> ApiError {
    match error {
        Failure::Remediation => fail(503, "migration_remediation"),
        Failure::Busy => fail(429, "busy"),
        Failure::Unconfigured => fail(503, "provider_unconfigured"),
        Failure::Denied => fail(409, "transport_unavailable"),
        Failure::Unknown => fail(503, "unavailable"),
    }
}
fn observed_error(state: &AppState, e: Failure) -> ApiError {
    if e == Failure::Remediation {
        state
            .invitations
            .broker_remediation
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
    broker_error(e)
}
async fn client(state: &AppState) -> Result<Option<Client>, ApiError> {
    if state
        .invitations
        .broker_remediation
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(fail(503, "migration_remediation"));
    }
    let value = state
        .invitations
        .broker
        .get_or_init(Client::load)
        .clone()
        .map_err(broker_error)?;
    if let Some(c) = &value {
        if state.store.instance_id().await.map_err(store_error)? != c.instance() {
            return Err(fail(503, "migration_remediation"));
        }
        let health = state
            .store
            .invitation_broker_health(c.scope())
            .await
            .map_err(store_error)?;
        if health.invalid != 0 || health.mismatched != 0 {
            return Err(fail(503, "migration_remediation"));
        }
    }
    Ok(value)
}
pub(super) async fn readiness(
    state: &AppState,
    item: &InvitationConsent,
) -> Result<Option<&'static str>, ApiError> {
    if state
        .invitations
        .broker_remediation
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Ok(Some("migration_remediation"));
    }
    let configured = state.invitations.broker.get_or_init(Client::load);
    let c = match configured {
        Ok(Some(c)) => c,
        _ => return Ok(Some("provider_unconfigured")),
    };
    if state.store.instance_id().await.map_err(store_error)? != c.instance() {
        return Ok(Some("migration_remediation"));
    }
    let health = state
        .store
        .invitation_broker_health(c.scope())
        .await
        .map_err(store_error)?;
    if health.invalid != 0 || health.mismatched != 0 {
        return Ok(Some("migration_remediation"));
    }
    if let Some(raw) = &item.broker_ticket {
        let reference = match BrokerReference::decode(raw) {
            Ok(r) => r,
            Err(_) => return Ok(Some("migration_remediation")),
        };
        if !c.matches(&reference) {
            return Ok(Some("migration_remediation"));
        }
    } else if item.broker_enrollment.is_some() {
        return Ok(Some("migration_remediation"));
    }
    if state.invitations.provider_unavailable[usize::from(item.transport != "apns")]
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Ok(Some("provider_unconfigured"));
    }
    Ok(None)
}
async fn authority(
    state: &AppState,
    headers: &HeaderMap,
    phone_id: &str,
    receiver_id: &str,
    user: i64,
    digest: &str,
    generation: i64,
) -> Result<(String, String, InvitationConsent), ApiError> {
    if !valid_id(phone_id) || !valid_id(receiver_id) {
        return Err(fail(400, "invalid"));
    }
    let hash = proof(headers, "x-cinema-phone-secret")?.ok_or_else(|| fail(401, "unauthorized"))?;
    let phone = state
        .store
        .invitation_phone_authority(phone_id, user, &hash)
        .await
        .map_err(store_error)?
        .ok_or_else(|| fail(401, "unauthorized"))?;
    if phone.generation != generation {
        return Err(fail(409, "stale_generation"));
    }
    if state
        .store
        .invitation_phone_binding(phone_id, user, &hash)
        .await
        .map_err(store_error)?
        .as_deref()
        != Some(digest)
    {
        return Err(fail(409, "login_changed"));
    }
    let consent = state
        .store
        .invitation_consent(phone_id, receiver_id, user)
        .await
        .map_err(store_error)?
        .ok_or_else(|| fail(409, "stale_generation"))?;
    if !consent.enabled || !matches!(consent.transport.as_str(), "apns" | "fcm") {
        return Err(fail(409, "transport_unavailable"));
    }
    let grant_hash =
        proof(headers, "x-cinema-grant-secret")?.ok_or_else(|| fail(401, "unauthorized"))?;
    if state
        .store
        .remote_authority(
            receiver_id,
            user,
            RemoteProof::Grant {
                grant_id: consent
                    .grant_id
                    .clone()
                    .ok_or_else(|| fail(401, "unauthorized"))?,
                secret_hash: grant_hash.clone(),
            },
        )
        .await
        .map_err(store_error)?
        .is_none()
    {
        return Err(fail(401, "unauthorized"));
    }
    Ok((hash, grant_hash, consent))
}
pub(super) async fn dispatchable(
    state: &AppState,
    item: &InvitationConsent,
    digest: &str,
    phone_generation: i64,
) -> Result<bool, ApiError> {
    let live = state
        .store
        .invitation_login(digest, now_seconds()?)
        .await
        .map_err(store_error)?;
    if live.is_none_or(|login| login.user_id != item.user_id) {
        return Ok(false);
    }
    let scope = state
        .store
        .invitation_scope(&item.phone_id, &item.receiver_id, item.user_id)
        .await
        .map_err(store_error)?;
    if scope.is_none_or(|scope| {
        scope.phone_digest != digest
            || scope.phone_generation != phone_generation
            || !scope.permission_granted
            || !scope.consent.enabled
            || scope.consent.generation != item.generation
            || scope.consent.transport_generation != item.transport_generation
    }) {
        return Ok(false);
    }
    for key in ["cinema.remote_control", "cinema.remote_invitations"] {
        if state
            .store
            .get_setting(key)
            .await
            .map_err(store_error)?
            .as_deref()
            != Some("1")
        {
            return Ok(false);
        }
    }
    Ok(true)
}
async fn current(
    state: &AppState,
    phone: &str,
    receiver: &str,
    user: i64,
) -> Result<InvitationConsent, ApiError> {
    state
        .store
        .invitation_consent(phone, receiver, user)
        .await
        .map_err(store_error)?
        .ok_or_else(|| fail(409, "stale_generation"))
}
async fn cancel(
    state: &AppState,
    item: &InvitationConsent,
    reference: BrokerReference,
) -> Result<(), ApiError> {
    state
        .store
        .queue_invitation_reference(&item.id, item.user_id, reference, now_seconds()?)
        .await
        .map_err(store_error)?;
    state.invitations.changed.notify_waiters();
    Ok(())
}
pub(super) async fn start(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    user: i64,
    digest: &str,
) -> Result<Value, ApiError> {
    let r: TransportStart = decode(body)?;
    if !valid_id(&r.grant_id) {
        return Err(fail(400, "invalid"));
    }
    let (hash, grant_hash, prior) = authority(
        state,
        headers,
        &r.installation_id,
        &r.receiver_id,
        user,
        digest,
        r.expected_phone_generation,
    )
    .await?;
    if prior.grant_id.as_deref() != Some(&r.grant_id)
        || prior.generation != r.expected_consent_generation
    {
        return Err(fail(409, "stale_generation"));
    }
    let c = client(state).await?;
    let ticket_id = Uuid::new_v4().to_string();
    let (raw, secret_hash) = secret()?;
    let reference = c
        .as_ref()
        .map(|c| c.reference(ticket_id.clone()))
        .unwrap_or_else(|| BrokerReference {
            ticket_id: ticket_id.clone(),
            scope_hash: auth::hash_token("unconfigured"),
        });
    if !state
        .store
        .start_invitation_transport(StartInvitationTransport {
            phone_id: r.installation_id.clone(),
            receiver_id: r.receiver_id.clone(),
            user_id: user,
            phone_hash: hash,
            grant_id: r.grant_id,
            grant_hash,
            login_digest: digest.to_owned(),
            expected_phone_generation: r.expected_phone_generation,
            expected_consent_generation: r.expected_consent_generation,
            reference: reference.clone(),
            provider_available: c.is_some(),
            now: now_seconds()?,
        })
        .await
        .map_err(store_error)?
    {
        if state
            .store
            .invitation_cleanup_budget(user)
            .await
            .map_err(store_error)?
            >= 100000
        {
            return Err(fail(429, "retention_limit"));
        }
        return Err(fail(409, "stale_generation"));
    }
    let item = current(state, &r.installation_id, &r.receiver_id, user).await?;
    let ticket = if let Some(c) = c {
        if !dispatchable(state, &item, digest, r.expected_phone_generation).await? {
            cancel(state, &item, reference).await?;
            return Err(fail(409, "transport_unavailable"));
        }
        let instance = state.store.instance_id().await.map_err(store_error)?;
        let request = json!({"version":"cinema.invitation.v1","ticket_id":ticket_id,"ticket_secret_hash":secret_hash,"server_instance_id":instance,"installation_id":r.installation_id,"receiver_id":r.receiver_id,"consent_id":item.id,"phone_generation":r.expected_phone_generation,"consent_generation":item.generation,"transport_generation":item.transport_generation,"platform":if item.transport=="apns"{"apple"}else{"android"}});
        match c.issue(&instance, &request).await {
            Ok(reply)
                if reply.ticket_id == ticket_id
                    && reply.status == "pending"
                    && reply.expires_at > now_seconds()?
                    && reply.expires_at <= now_seconds()?.saturating_add(120) =>
            {
                let Version::V1 = reply.version;
                state.invitations.provider_unavailable[usize::from(item.transport != "apns")]
                    .store(false, std::sync::atomic::Ordering::Relaxed);
                if !dispatchable(state, &item, digest, r.expected_phone_generation).await? {
                    cancel(state, &item, reference).await?;
                    return Err(fail(409, "transport_unavailable"));
                }
                let latest = current(state, &r.installation_id, &r.receiver_id, user).await?;
                if latest.generation != item.generation
                    || latest.broker_ticket.as_deref()
                        != Some(&reference.encode().map_err(store_error)?)
                {
                    cancel(state, &item, reference).await?;
                    return Err(fail(409, "stale_generation"));
                }
                json!({"ticket_id":ticket_id,"ticket_secret":raw,"expires_at":reply.expires_at,"broker_origin":c.origin(),"broker_generation":c.generation()})
            }
            Ok(_) => {
                cancel(state, &item, reference).await?;
                return Err(fail(503, "unavailable"));
            }
            Err(Failure::Unconfigured) => {
                state.invitations.provider_unavailable[usize::from(item.transport != "apns")]
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                cancel(state, &item, reference.clone()).await?;
                Value::Null
            }
            Err(e) => {
                cancel(state, &item, reference).await?;
                return Err(observed_error(state, e));
            }
        }
    } else {
        Value::Null
    };
    let latest = current(state, &r.installation_id, &r.receiver_id, user).await?;
    if latest.generation != item.generation
        || latest.transport_generation != item.transport_generation
    {
        cancel(state, &item, reference).await?;
        return Err(fail(409, "stale_generation"));
    }
    state.invitations.changed.notify_waiters();
    Ok(
        json!({"version":"cinema.invitation.v1","consent":consent_value(state,latest,digest).await?,"ticket":ticket}),
    )
}
pub(super) async fn confirm(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    user: i64,
    digest: &str,
) -> Result<Value, ApiError> {
    let r: TransportConfirm = decode(body)?;
    if !valid_id(&r.ticket_id) {
        return Err(fail(400, "invalid"));
    }
    let (hash, grant_hash, item) = authority(
        state,
        headers,
        &r.installation_id,
        &r.receiver_id,
        user,
        digest,
        r.expected_phone_generation,
    )
    .await?;
    if item.generation != r.expected_consent_generation
        || item.transport_generation != r.expected_transport_generation
    {
        return Err(fail(409, "stale_generation"));
    }
    let reference = BrokerReference::decode(
        item.broker_ticket
            .as_deref()
            .ok_or_else(|| fail(409, "transport_unavailable"))?,
    )
    .map_err(|_| fail(503, "migration_remediation"))?;
    if reference.ticket_id != r.ticket_id {
        return Err(fail(409, "stale_generation"));
    }
    let c = client(state)
        .await?
        .ok_or_else(|| fail(503, "provider_unconfigured"))?;
    let instance = state.store.instance_id().await.map_err(store_error)?;
    let reply = c
        .status(&instance, &reference)
        .await
        .map_err(|e| observed_error(state, e))?;
    let Version::V1 = reply.version;
    if reply.ticket_id != r.ticket_id
        || reply.status != "claimed"
        || reply.enrollment_id.as_deref() != Some(&r.ticket_id)
        || reply.server_instance_id != instance
        || reply.installation_id != r.installation_id
        || reply.receiver_id != r.receiver_id
        || reply.consent_id != item.id
        || reply.phone_generation != r.expected_phone_generation
        || reply.consent_generation != item.generation
        || reply.transport_generation != item.transport_generation
        || reply.platform
            != if item.transport == "apns" {
                "apple"
            } else {
                "android"
            }
    {
        return Err(fail(409, "transport_unavailable"));
    }
    if !dispatchable(state, &item, digest, r.expected_phone_generation).await? {
        cancel(state, &item, reference).await?;
        return Err(fail(409, "transport_unavailable"));
    }
    let already_confirmed =
        item.transport_status == "ready" && item.broker_enrollment.as_deref() == Some(&r.ticket_id);
    if !already_confirmed
        && !state
            .store
            .confirm_invitation_transport(ConfirmInvitationTransport {
                phone_id: r.installation_id.clone(),
                receiver_id: r.receiver_id.clone(),
                user_id: user,
                phone_hash: hash,
                grant_id: item
                    .grant_id
                    .clone()
                    .ok_or_else(|| fail(401, "unauthorized"))?,
                grant_hash,
                login_digest: digest.to_owned(),
                phone_generation: r.expected_phone_generation,
                consent_generation: item.generation,
                transport_generation: item.transport_generation,
                reference,
            })
            .await
            .map_err(store_error)?
    {
        return Err(fail(409, "stale_generation"));
    }
    let latest = current(state, &r.installation_id, &r.receiver_id, user).await?;
    if latest.generation != item.generation
        || latest.transport_generation != item.transport_generation
    {
        return Err(fail(409, "stale_generation"));
    }
    state.invitations.changed.notify_waiters();
    Ok(
        json!({"version":"cinema.invitation.v1","consent":consent_value(state,latest,digest).await?}),
    )
}
