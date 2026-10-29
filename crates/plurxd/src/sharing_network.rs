//! External connectivity belongs to the Cinema invitation lifecycle.
//! Only user-owned API authorization reaches the fixed Tailscale control API.
use crate::{
    sharing_host,
    state::{clock_ms, AppState},
};
use plurx_core::{
    secrets::{SealedSecret, Secret, SharingSecretPurpose},
    sharing::{ConnectionInvitation, Endpoint, Invitation, NetworkShare},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{path::PathBuf, time::Duration};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub(crate) type Result<T> = std::result::Result<T, &'static str>;
const API: &str = "https://api.tailscale.com/api/v2/";
const AUTH_ID: Uuid = Uuid::from_u128(0x8ab154b2_8186_4e80_9a04_7eacb3425a18);
const LIMIT: usize = 2 * 1024 * 1024;

fn directory(state: &AppState) -> Result<PathBuf> {
    let p = state
        .sharing
        .host_directory()
        .with_file_name("sharing-network");
    sharing_host::private_directory(&p).map_err(|_| "sharing_network_storage_unavailable")?;
    Ok(p)
}
fn good_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}
fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| "sharing_network_unavailable")
}
async fn request(
    token: &Secret,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<Value> {
    let mut req = client()?
        .request(method, format!("{API}{path}"))
        .bearer_auth(token.expose());
    if let Some(body) = body {
        req = req.json(&body);
    }
    let mut reply = req
        .send()
        .await
        .map_err(|_| "sharing_network_outcome_unknown")?;
    if !reply.status().is_success() {
        return Err(match reply.status().as_u16() {
            401 | 403 => "sharing_network_authorization_required",
            429 => "sharing_network_rate_limited",
            404 => "sharing_network_not_found",
            _ => "sharing_network_unavailable",
        });
    }
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    while let Some(chunk) = reply
        .chunk()
        .await
        .map_err(|_| "sharing_network_outcome_unknown")?
    {
        if bytes.len().saturating_add(chunk.len()) > LIMIT {
            return Err("sharing_network_invalid_response");
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&bytes).map_err(|_| "sharing_network_invalid_response")
}
async fn token(state: &AppState) -> Result<Secret> {
    let p = directory(state)?.join("authorization.json");
    let raw = sharing_host::read_json(&p).map_err(|_| "sharing_network_authorization_required")?;
    let sealed = SealedSecret::from_persisted(
        raw.get("sealed")
            .and_then(Value::as_str)
            .ok_or("sharing_network_authorization_required")?
            .to_owned(),
    )
    .map_err(|_| "sharing_network_authorization_required")?;
    let identity = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(|_| "sharing_authority_unavailable")?;
    state
        .sharing
        .key
        .open_sharing(
            SharingSecretPurpose::NetworkAuthorization,
            identity.server_id,
            AUTH_ID,
            &sealed,
        )
        .map_err(|_| "sharing_network_authorization_required")
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    id: String,
    name: String,
    addresses: Vec<String>,
    #[serde(default)]
    is_external: bool,
}
async fn devices(token: &Secret) -> Result<Vec<Device>> {
    let raw = request(token, reqwest::Method::GET, "tailnet/-/devices", None).await?;
    let rows: Vec<Device> = serde_json::from_value(
        raw.get("devices")
            .cloned()
            .ok_or("sharing_network_invalid_response")?,
    )
    .map_err(|_| "sharing_network_invalid_response")?;
    if rows.len() > 10000 {
        return Err("sharing_network_invalid_response");
    }
    Ok(rows)
}
fn select_devices(rows: &[Device], endpoints: &[Endpoint]) -> Result<Vec<String>> {
    let mut ids = Vec::new();
    for endpoint in endpoints {
        let matches: Vec<_> = rows
            .iter()
            .filter(|d| {
                !d.is_external
                    && d.name == endpoint.ts_fqdn
                    && d.addresses.contains(&endpoint.ipv4.to_string())
                    && good_id(&d.id)
            })
            .collect();
        if matches.len() != 1 {
            return Err("sharing_network_device_mismatch");
        }
        ids.push(matches[0].id.clone());
    }
    Ok(ids)
}
pub(crate) async fn authorization(state: &AppState, value: Secret) -> Result<()> {
    if !value.expose().starts_with("tskey-api-")
        || value.expose().len() > 512
        || value
            .expose()
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
    {
        return Err("sharing_network_user_token_required");
    }
    // Never persist an unverified key, or broaden an OAuth application's scope.
    let _guard = state.sharing.network_invite_lock.lock().await;
    let owned = devices(&value).await?;
    let mut pending = jobs(state)?;
    for job in &pending {
        for share in &job.shares {
            if !owned.iter().any(|d| d.id == share.device && !d.is_external) {
                return Err("sharing_network_account_mismatch");
            }
            let record = request(
                &value,
                reqwest::Method::GET,
                &format!("device-invites/{}", share.id),
                None,
            )
            .await?;
            if record.get("id").and_then(Value::as_str) != Some(share.id.as_str()) {
                return Err("sharing_network_account_mismatch");
            }
        }
        if job
            .attempting
            .as_ref()
            .is_some_and(|id| !owned.iter().any(|d| &d.id == id && !d.is_external))
        {
            return Err("sharing_network_account_mismatch");
        }
    }
    let identity = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(|_| "sharing_authority_unavailable")?;
    let sealed = state
        .sharing
        .key
        .seal_sharing(
            SharingSecretPurpose::NetworkAuthorization,
            identity.server_id,
            AUTH_ID,
            value.expose(),
        )
        .map_err(|_| "sharing_network_storage_unavailable")?;
    for job in &mut pending {
        job.authorization = sealed
            .to_persist()
            .map_err(|_| "sharing_network_storage_unavailable")?
            .to_owned();
        save(state, job)?;
    }
    sharing_host::write_json(
        &directory(state)?.join("authorization.json"),
        &json!({"sealed":sealed.to_persist().map_err(|_| "sharing_network_storage_unavailable")?}),
    )
    .map_err(|_| "sharing_network_storage_unavailable")
}
pub(crate) async fn status(state: &AppState) -> Value {
    let jobs = jobs(state).unwrap_or_default();
    json!({"configured":token(state).await.is_ok(),"node_id":state.node_id,
        "pending_cleanup":jobs.iter().filter(|j| j.cleanup).count(),
        "unknown_outcomes":jobs.iter().filter(|j| j.attempting.is_some()).count(),
        "last_error":*state.sharing.network_invite_error.lock().expect("network cleanup observation")})
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Job {
    invitation: Uuid,
    created_at_ms: i64,
    cleanup: bool,
    completed: bool,
    authorization: String,
    attempting: Option<String>,
    shares: Vec<Share>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Share {
    device: String,
    id: String,
}
fn job_path(state: &AppState, id: Uuid) -> Result<PathBuf> {
    Ok(directory(state)?.join(format!("{id}.json")))
}
fn save(state: &AppState, job: &Job) -> Result<()> {
    sharing_host::write_json(&job_path(state, job.invitation)?, job)
        .map_err(|_| "sharing_network_storage_unavailable")
}
fn jobs(state: &AppState) -> Result<Vec<Job>> {
    let directory = directory(state)?;
    let mut jobs = Vec::new();
    for entry in std::fs::read_dir(&directory).map_err(|_| "sharing_network_storage_unavailable")? {
        let entry = entry.map_err(|_| "sharing_network_storage_unavailable")?;
        let name = entry.file_name();
        let Some(id) = name
            .to_str()
            .and_then(|n| n.strip_suffix(".json"))
            .and_then(|n| Uuid::parse_str(n).ok())
        else {
            continue;
        };
        let raw = sharing_host::read_json(&entry.path())
            .map_err(|_| "sharing_network_storage_unavailable")?;
        let job: Job =
            serde_json::from_value(raw).map_err(|_| "sharing_network_storage_unavailable")?;
        if job.invitation != id
            || job.shares.len() > 4
            || job
                .shares
                .iter()
                .any(|s| !good_id(&s.id) || !good_id(&s.device))
        {
            return Err("sharing_network_storage_unavailable");
        }
        jobs.push(job);
        if jobs.len() > 128 {
            return Err("sharing_network_capacity");
        }
    }
    Ok(jobs)
}
async fn issue_inner(state: &AppState, invite: &Invitation) -> Result<Secret> {
    let _guard = state.sharing.network_invite_lock.lock().await;
    let token = token(state).await?;
    if jobs(state)?.len() >= 128 {
        return Err("sharing_network_capacity");
    }
    let ids = select_devices(&devices(&token).await?, &invite.endpoints)?;
    let mut job = Job {
        invitation: invite.id,
        created_at_ms: clock_ms(),
        cleanup: false,
        completed: false,
        authorization: state
            .sharing
            .key
            .seal_sharing(
                SharingSecretPurpose::NetworkAuthorization,
                invite.identity.server_id,
                AUTH_ID,
                token.expose(),
            )
            .map_err(|_| "sharing_network_storage_unavailable")?
            .to_persist()
            .map_err(|_| "sharing_network_storage_unavailable")?
            .to_owned(),
        attempting: None,
        shares: Vec::new(),
    };
    save(state, &job)?;
    let mut network = Vec::new();
    for (endpoint, device) in invite.endpoints.iter().zip(ids) {
        if network
            .iter()
            .any(|s: &NetworkShare| s.ts_fqdn == endpoint.ts_fqdn)
        {
            continue;
        }
        // Write intent before calling a provider without idempotency keys.
        // An unknown outcome is retained and never blindly re-created.
        job.attempting = Some(device.clone());
        save(state, &job)?;
        let raw = request(
            &token,
            reqwest::Method::POST,
            &format!("device/{device}/device-invites"),
            Some(json!([{"multiUse":false,"allowExitNode":false}])),
        )
        .await;
        let result = raw.and_then(|v| {
            let rows = v.as_array().ok_or("sharing_network_invalid_response")?;
            if rows.len() != 1 {
                return Err("sharing_network_invalid_response");
            }
            let id = rows[0]
                .get("id")
                .and_then(Value::as_str)
                .filter(|v| good_id(v))
                .ok_or("sharing_network_invalid_response")?
                .to_owned();
            let url = rows[0]
                .get("inviteUrl")
                .and_then(Value::as_str)
                .ok_or("sharing_network_invalid_response")?
                .to_owned();
            plurx_core::sharing::validate_network_invite_url(&url)
                .map_err(|_| "sharing_network_invalid_response")?;
            if rows[0].get("multiUse").and_then(Value::as_bool) != Some(false)
                || rows[0].get("allowExitNode").and_then(Value::as_bool) != Some(false)
            {
                return Err("sharing_network_invalid_response");
            }
            Ok((id, url))
        });
        match result {
            Ok((id, url)) => {
                job.shares.push(Share { device, id });
                job.attempting = None;
                save(state, &job)?;
                network.push(NetworkShare {
                    ts_fqdn: endpoint.ts_fqdn.clone(),
                    invite_url: url,
                });
            }
            Err(error) => {
                job.cleanup = true;
                // Definitive refusals did not create a share.
                if matches!(
                    error,
                    "sharing_network_authorization_required"
                        | "sharing_network_rate_limited"
                        | "sharing_network_not_found"
                ) {
                    job.attempting = None;
                }
                save(state, &job)?;
                return Err(error);
            }
        }
    }
    let combined = ConnectionInvitation {
        invitation: invite.encode().map_err(|_| "sharing_invalid_request")?,
        network,
    }
    .encode()
    .map_err(|_| "sharing_invalid_request")?;
    job.completed = true;
    save(state, &job)?;
    Ok(combined)
}
pub(crate) async fn cleanup(state: &AppState) -> Result<()> {
    let _guard = state.sharing.network_invite_lock.lock().await;
    for mut job in jobs(state)? {
        if !job.completed {
            state
                .store
                .cancel_share_invitation(job.invitation)
                .await
                .map_err(|_| "sharing_authority_unavailable")?;
            job.cleanup = true;
            save(state, &job)?;
        }
        let retained = state
            .store
            .sharing_network_retained(job.invitation, clock_ms())
            .await
            .map_err(|_| "sharing_authority_unavailable")?;
        if !job.cleanup && retained {
            continue;
        }
        job.cleanup = true;
        save(state, &job)?;
        let sealed = SealedSecret::from_persisted(job.authorization.clone())
            .map_err(|_| "sharing_network_storage_unavailable")?;
        let identity = state
            .store
            .sharing_identity(clock_ms())
            .await
            .map_err(|_| "sharing_authority_unavailable")?;
        let token = state
            .sharing
            .key
            .open_sharing(
                SharingSecretPurpose::NetworkAuthorization,
                identity.server_id,
                AUTH_ID,
                &sealed,
            )
            .map_err(|_| "sharing_network_authorization_required")?;
        while let Some(share) = job.shares.last() {
            match request(
                &token,
                reqwest::Method::DELETE,
                &format!("device-invites/{}", share.id),
                None,
            )
            .await
            {
                Ok(_) | Err("sharing_network_not_found") => {
                    job.shares.pop();
                    save(state, &job)?;
                }
                Err(error) => return Err(error),
            }
        }
        if job.attempting.is_none() {
            std::fs::remove_file(job_path(state, job.invitation)?)
                .map_err(|_| "sharing_network_storage_unavailable")?;
        }
    }
    Ok(())
}
pub(crate) async fn run(state: AppState, stop: CancellationToken) {
    loop {
        let result = cleanup(&state).await;
        *state
            .sharing
            .network_invite_error
            .lock()
            .expect("network cleanup observation") = result.err();
        tokio::select! { _ = stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(30)) => {} }
    }
}

pub(crate) async fn issue(state: &AppState, invite: Invitation) -> Result<Secret> {
    let state = state.clone();
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = tokio::time::timeout(Duration::from_secs(22), issue_inner(&state, &invite))
            .await
            .unwrap_or(Err("sharing_network_outcome_unknown"));
        let failed = result.is_err();
        let delivered = send.send(result).is_ok();
        if failed || !delivered {
            let _ = state.store.cancel_share_invitation(invite.id).await;
            let result = cleanup(&state).await;
            *state
                .sharing
                .network_invite_error
                .lock()
                .expect("network cleanup observation") = result.err();
        }
    });
    receive
        .await
        .map_err(|_| "sharing_network_outcome_unknown")?
}
