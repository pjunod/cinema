//! Separately bounded resident polls and human notification discovery.
use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use plurx_core::store::invitations::InvitationEvent;
use std::{collections::HashSet, sync::Mutex, time::Duration};
struct Permit {
    hub: Arc<Hub>,
    key: (i64, String),
}
impl Drop for Permit {
    fn drop(&mut self) {
        if let Ok(mut active) = self.hub.poll_phones.lock() {
            active.remove(&self.key);
        }
    }
}
fn permit(hub: Arc<Hub>, user: i64, phone: &str) -> Result<Permit, ApiError> {
    let key = (user, phone.to_owned());
    let mut active = hub
        .poll_phones
        .lock()
        .map_err(|_| fail(503, "unavailable"))?;
    if !active.insert(key.clone()) {
        return Err(fail(429, "busy"));
    }
    drop(active);
    Ok(Permit { hub, key })
}
pub(super) fn pools() -> (Arc<Semaphore>, Mutex<HashSet<(i64, String)>>) {
    (Arc::new(Semaphore::new(64)), Mutex::new(HashSet::new()))
}
fn event_id(raw: &str, phone: &str) -> Result<String, ApiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| fail(400, "invalid"))?;
    let phone = Uuid::parse_str(phone).map_err(|_| fail(400, "invalid"))?;
    if bytes.len() != 32
        || raw.len() != 43
        || URL_SAFE_NO_PAD.encode(&bytes) != raw
        || &bytes[..16] != phone.as_bytes()
    {
        return Err(fail(400, "invalid"));
    }
    Uuid::from_slice(&bytes[16..])
        .map(|id| id.to_string())
        .map_err(|_| fail(400, "invalid"))
}
async fn enabled(state: &AppState) -> Result<bool, ApiError> {
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
async fn phone(
    state: &AppState,
    id: &str,
    user: i64,
    digest: &str,
    hash: &str,
    resident: bool,
) -> Result<InvitationPhone, ApiError> {
    let login = state
        .store
        .invitation_login(digest, now_seconds()?)
        .await
        .map_err(store_error)?
        .ok_or_else(|| fail(401, "unauthorized"))?;
    if login.user_id != user {
        return Err(fail(401, "unauthorized"));
    }
    let phone = state
        .store
        .invitation_phone_authority(id, user, hash)
        .await
        .map_err(store_error)?
        .ok_or_else(|| fail(401, "unauthorized"))?;
    if state
        .store
        .invitation_phone_binding(id, user, hash)
        .await
        .map_err(store_error)?
        .as_deref()
        != Some(digest)
    {
        return Err(fail(409, "login_changed"));
    }
    if resident && !enabled(state).await? {
        return Err(fail(409, "global_disabled"));
    }
    if resident
        && (phone.platform != "android" || !phone.permission_granted || !phone.resident_active)
    {
        return Err(fail(409, "transport_unavailable"));
    }
    Ok(phone)
}
async fn scope(state: &AppState, event: &InvitationEvent, digest: &str) -> Result<bool, ApiError> {
    if event.expires_at <= now_seconds()? || !enabled(state).await? {
        return Ok(false);
    }
    let Some(scope) = state
        .store
        .invitation_scope(&event.phone_id, &event.receiver_id, event.user_id)
        .await
        .map_err(store_error)?
    else {
        return Ok(false);
    };
    Ok(scope.phone_digest == digest
        && scope.phone_generation == event.phone_generation
        && scope.consent.enabled
        && scope.consent.generation == event.consent_generation
        && scope.consent.transport_generation == event.transport_generation
        && scope.consent.grant_id.as_deref() == Some(&event.grant_id))
}
pub(super) async fn lookup(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    user: i64,
    digest: &str,
) -> Result<Value, ApiError> {
    let r: Lookup = decode(body)?;
    if !valid_id(&r.installation_id) {
        return Err(fail(400, "invalid"));
    }
    let id = event_id(&r.invitation_id, &r.installation_id)?;
    if !enabled(state).await? {
        return Err(fail(409, "global_disabled"));
    }
    let hash = proof(headers, "x-cinema-phone-secret")?.ok_or_else(|| fail(401, "unauthorized"))?;
    phone(state, &r.installation_id, user, digest, &hash, false).await?;
    let event = state
        .store
        .invitation_event(&r.installation_id, user, &id, now_seconds()?)
        .await
        .map_err(store_error)?
        .ok_or_else(|| fail(409, "stale_generation"))?;
    if event.phase != "attempted"
        || !matches!(
            event.outcome.as_deref(),
            Some("accepted" | "resident_ready" | "unknown")
        )
        || !scope(state, &event, digest).await?
    {
        return Err(fail(409, "stale_generation"));
    }
    let rows = super::snapshots::collect(state, user, digest).await?;
    let live = super::snapshots::unique(&rows, &event.receiver_id)
        .filter(|s| s.foreground_id.to_string() == event.foreground_id)
        .ok_or_else(|| fail(409, "stale_target"))?;
    if state
        .store
        .invitation_login(&live.token_digest, now_seconds()?)
        .await
        .map_err(store_error)?
        .is_none_or(|login| login.user_id != user)
    {
        return Err(fail(409, "stale_target"));
    }
    phone(state, &r.installation_id, user, digest, &hash, false).await?;
    if !scope(state, &event, digest).await? {
        return Err(fail(409, "stale_generation"));
    }
    Ok(
        json!({"version":"cinema.invitation.v1","receiver_id":event.receiver_id,"foreground_id":event.foreground_id,"target":live.target,"expires_at":event.expires_at}),
    )
}
pub(super) async fn poll(
    state: &AppState,
    headers: &HeaderMap,
    body: &[u8],
    user: i64,
    digest: &str,
) -> Result<Value, ApiError> {
    let r: Poll = decode(body)?;
    if !valid_id(&r.installation_id) || r.wait_ms > 20000 {
        return Err(fail(400, "invalid"));
    }
    let hash = proof(headers, "x-cinema-phone-secret")?.ok_or_else(|| fail(401, "unauthorized"))?;
    phone(state, &r.installation_id, user, digest, &hash, true).await?;
    let _permit = permit(state.invitations.clone(), user, &r.installation_id)?;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(u64::from(r.wait_ms));
    loop {
        let changed = state.invitations.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        phone(state, &r.installation_id, user, digest, &hash, true).await?;
        let high = state
            .store
            .invitation_revision(&r.installation_id, user)
            .await
            .map_err(store_error)?
            .ok_or_else(|| fail(401, "unauthorized"))?;
        if r.after_revision > high {
            return Err(fail(409, "stale_generation"));
        }
        let rows = state
            .store
            .invitation_events(&r.installation_id, user, r.after_revision, now_seconds()?)
            .await
            .map_err(store_error)?;
        let count = rows.len();
        let mut revision = r.after_revision;
        let mut events = Vec::new();
        let mut eligible = Vec::new();
        let mut barrier = false;
        let mut snapshots = None;
        for event in rows {
            if event.phase == "admitted" || event.outcome.is_none() {
                barrier = true;
                revision = event.revision.saturating_sub(1);
                break;
            }
            revision = event.revision;
            if event.outcome.as_deref() != Some("resident_ready")
                || !scope(state, &event, digest).await?
            {
                continue;
            }
            if snapshots.is_none() {
                snapshots = Some(super::snapshots::collect(state, user, digest).await?);
            }
            let Some(live) =
                super::snapshots::unique(snapshots.as_deref().unwrap_or(&[]), &event.receiver_id)
                    .filter(|s| s.foreground_id.to_string() == event.foreground_id)
            else {
                continue;
            };
            if state
                .store
                .invitation_login(&live.token_digest, now_seconds()?)
                .await
                .map_err(store_error)?
                .is_none_or(|login| login.user_id != user)
            {
                continue;
            }
            let mut bytes = [0_u8; 32];
            bytes[..16].copy_from_slice(
                Uuid::parse_str(&event.phone_id)
                    .map_err(|_| fail(503, "unavailable"))?
                    .as_bytes(),
            );
            bytes[16..].copy_from_slice(
                Uuid::parse_str(&event.id)
                    .map_err(|_| fail(503, "unavailable"))?
                    .as_bytes(),
            );
            if !scope(state, &event, digest).await? {
                continue;
            }
            eligible.push(event.clone());
            events.push(json!({"invitation_id":URL_SAFE_NO_PAD.encode(bytes),"expires_at":event.expires_at}));
        }
        if !barrier && count < 16 {
            revision = revision.max(high);
        }
        if revision != r.after_revision
            || !events.is_empty()
            || tokio::time::Instant::now() >= deadline
        {
            phone(state, &r.installation_id, user, digest, &hash, true).await?;
            let mut current = Vec::new();
            for (event, value) in eligible.iter().zip(events) {
                if scope(state, event, digest).await? {
                    current.push(value);
                }
            }
            if !enabled(state).await? {
                return Err(fail(409, "global_disabled"));
            }
            return Ok(
                json!({"version":"cinema.invitation.v1","revision":revision,"invitations":current}),
            );
        }
        let next = (tokio::time::Instant::now() + Duration::from_secs(1)).min(deadline);
        tokio::select! {_=changed=>{},_=tokio::time::sleep_until(next)=>{}}
    }
}
