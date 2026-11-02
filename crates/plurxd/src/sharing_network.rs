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
use std::{
    path::{Path, PathBuf},
    time::Duration,
};
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
    let observation = state
        .sharing
        .network_observation
        .lock()
        .expect("network share observation")
        .clone();
    json!({"configured":token(state).await.is_ok(),"node_id":state.node_id,
        "pending_cleanup":jobs.iter().filter(|j| j.cleanup).count(),
        "unknown_outcomes":jobs.iter().filter(|j| j.attempting.is_some()).count(),
        "last_error":*state.sharing.network_invite_error.lock().expect("network cleanup observation"),
        "tailnet":observation.tailnet,
        "observed_at_ms":observation.checked_at_ms,
        "observation_error":observation.error,
        "shares":observation.invites})
}

/// What the Tailscale control API says about the device shares this node
/// issued: whether each was accepted, and by which account. Advisory only.
#[derive(Clone, Default, Serialize)]
pub(crate) struct NetworkObservation {
    pub checked_at_ms: Option<i64>,
    /// The tailnet the saved API key belongs to, derived from its devices.
    pub tailnet: Option<String>,
    pub error: Option<&'static str>,
    pub invites: Vec<InviteObservation>,
}
#[derive(Clone, Serialize)]
pub(crate) struct InviteObservation {
    pub invitation_id: Uuid,
    pub created_at_ms: i64,
    pub shares: Vec<ShareObservation>,
}
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ShareObservation {
    /// The shared machine's Tailscale name.
    pub device: String,
    /// The provider's device ID, the stable key for carrying an observation.
    #[serde(skip)]
    pub device_id: String,
    pub accepted: Option<bool>,
    pub accepted_by: Option<String>,
    pub error: Option<&'static str>,
}
const OBSERVED_INVITES: usize = 16;
fn tailnet_of(name: &str) -> Option<String> {
    let (_, rest) = name.split_once('.')?;
    rest.ends_with(".ts.net").then(|| rest.to_owned())
}
fn accepted_login(record: &Value) -> Option<String> {
    let login = record.get("acceptedBy")?.get("loginName")?.as_str()?.trim();
    (!login.is_empty() && login.len() <= 256 && !login.chars().any(char::is_control))
        .then(|| login.to_owned())
}
/// How often the account itself is re-read when no share awaits acceptance:
/// enough to notice an expired key without a steady stream of API calls.
const ACCOUNT_RECHECK_MS: i64 = 10 * 60 * 1000;
async fn observe(state: &AppState) {
    let token = match token(state).await {
        Ok(token) => token,
        Err(_) => {
            *state
                .sharing
                .network_observation
                .lock()
                .expect("network share observation") = NetworkObservation::default();
            return;
        }
    };
    let previous = state
        .sharing
        .network_observation
        .lock()
        .expect("network share observation")
        .clone();
    let mut open: Vec<Job> = {
        let _guard = state.sharing.network_invite_lock.lock().await;
        jobs(state)
            .unwrap_or_default()
            .into_iter()
            .filter(|j| j.completed && !j.cleanup && !j.shares.is_empty())
            .collect()
    };
    open.sort_by_key(|j| std::cmp::Reverse(j.created_at_ms));
    open.truncate(OBSERVED_INVITES);
    // Acceptance is final: a share once seen accepted is never asked again.
    let settled = |invitation: Uuid, device: &str| {
        previous
            .invites
            .iter()
            .find(|i| i.invitation_id == invitation)
            .and_then(|i| i.shares.iter().find(|s| s.device_id == device))
            .filter(|s| s.accepted == Some(true))
            .cloned()
    };
    let pending = open.iter().any(|job| {
        job.shares
            .iter()
            .any(|share| settled(job.invitation, &share.device).is_none())
    });
    let now = clock_ms();
    let account_due = previous
        .checked_at_ms
        .is_none_or(|at| now.saturating_sub(at) >= ACCOUNT_RECHECK_MS)
        || previous.error.is_some();
    let mut current = NetworkObservation {
        checked_at_ms: previous.checked_at_ms,
        tailnet: previous.tailnet.clone(),
        ..Default::default()
    };
    let mut names: std::collections::HashMap<String, String> = previous
        .invites
        .iter()
        .flat_map(|i| i.shares.iter())
        .map(|s| (s.device_id.clone(), s.device.clone()))
        .collect();
    if pending || account_due {
        current.checked_at_ms = Some(now);
        match devices(&token).await {
            Ok(rows) => {
                current.tailnet = rows
                    .iter()
                    .filter(|d| !d.is_external)
                    .find_map(|d| tailnet_of(&d.name));
                names.extend(
                    rows.into_iter()
                        .filter(|d| !d.is_external)
                        .map(|d| (d.id, d.name)),
                );
            }
            Err(error) => current.error = Some(error),
        }
    }
    for job in open {
        let mut shares = Vec::with_capacity(job.shares.len());
        for share in &job.shares {
            if let Some(done) = settled(job.invitation, &share.device) {
                shares.push(done);
                continue;
            }
            let device = names
                .get(&share.device)
                .cloned()
                .unwrap_or_else(|| format!("device {}", share.device));
            let device_id = share.device.clone();
            shares.push(
                match request(
                    &token,
                    reqwest::Method::GET,
                    &format!("device-invites/{}", share.id),
                    None,
                )
                .await
                {
                    Ok(record) => ShareObservation {
                        device,
                        device_id,
                        accepted: record.get("accepted").and_then(Value::as_bool),
                        accepted_by: accepted_login(&record),
                        error: None,
                    },
                    Err(error) => ShareObservation {
                        device,
                        device_id,
                        accepted: None,
                        accepted_by: None,
                        error: Some(error),
                    },
                },
            );
        }
        current.invites.push(InviteObservation {
            invitation_id: job.invitation,
            created_at_ms: job.created_at_ms,
            shares,
        });
    }
    for invite in &current.invites {
        let before = previous
            .invites
            .iter()
            .find(|old| old.invitation_id == invite.invitation_id);
        for share in &invite.shares {
            let earlier =
                before.and_then(|b| b.shares.iter().find(|s| s.device_id == share.device_id));
            if earlier.map(|p| (p.accepted, &p.accepted_by))
                == Some((share.accepted, &share.accepted_by))
            {
                continue;
            }
            match (share.accepted, share.accepted_by.as_deref()) {
                (Some(true), by) => tracing::info!(
                    invitation_id = %invite.invitation_id,
                    device = %share.device,
                    accepted_by = by.unwrap_or("unknown account"),
                    "Tailscale device share accepted"
                ),
                (Some(false), _) => tracing::info!(
                    invitation_id = %invite.invitation_id,
                    device = %share.device,
                    "Tailscale device share is waiting for the recipient to accept it"
                ),
                _ => {}
            }
        }
    }
    if current.error != previous.error {
        match current.error {
            Some(error) => tracing::warn!(code = error, "Tailscale account check failed"),
            None => tracing::info!("Tailscale account check recovered"),
        }
    }
    *state
        .sharing
        .network_observation
        .lock()
        .expect("network share observation") = current;
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
/// Tailscale omits false-valued flags in DeviceInvite responses.
/// Missing means the requested default, while explicit broader permissions
/// and malformed flag values remain refusals.
fn created_invite(value: Value) -> Result<(String, String)> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct CreatedInvite {
        id: String,
        invite_url: String,
        #[serde(default)]
        multi_use: bool,
        #[serde(default)]
        allow_exit_node: bool,
    }
    let mut rows: Vec<CreatedInvite> =
        serde_json::from_value(value).map_err(|_| "sharing_network_invalid_response")?;
    if rows.len() != 1 {
        return Err("sharing_network_invalid_response");
    }
    let row = rows.remove(0);
    if !good_id(&row.id) || row.multi_use || row.allow_exit_node {
        return Err("sharing_network_invalid_response");
    }
    plurx_core::sharing::validate_network_invite_url(&row.invite_url)
        .map_err(|_| "sharing_network_invalid_response")?;
    Ok((row.id, row.invite_url))
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
        let result = raw.and_then(created_invite);
        match result {
            Ok((id, url)) => {
                job.shares.push(Share {
                    device,
                    id: id.clone(),
                });
                job.attempting = None;
                save(state, &job)?;
                // Non-secret cluster metadata lets another publishing node
                // retain a shared user's other live Cinema grants.
                state
                    .store
                    .put_setting(
                        &format!("sharing.network.invite.{id}"),
                        &invite.id.to_string(),
                    )
                    .await
                    .map_err(|_| "sharing_authority_unavailable")?;
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
    tracing::info!(
        invitation_id = %invite.id,
        shares = job.shares.len(),
        "invitation created with single-use Tailscale device shares"
    );
    Ok(combined)
}
pub(crate) async fn cleanup(state: &AppState) -> Result<()> {
    let _guard = state.sharing.network_invite_lock.lock().await;
    for mut job in jobs(state)? {
        if !job.completed || job.cleanup {
            job.cleanup = true;
            save(state, &job)?;
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
        let mut index = 0;
        while index < job.shares.len() {
            let share = &job.shares[index];
            if retain_other_authority(state, &token, share).await? {
                index += 1;
                continue;
            }
            match request(
                &token,
                reqwest::Method::DELETE,
                &format!("device-invites/{}", share.id),
                None,
            )
            .await
            {
                Ok(_) | Err("sharing_network_not_found") => {
                    job.shares.remove(index);
                    save(state, &job)?;
                }
                Err(error) => return Err(error),
            }
        }
        if job.attempting.is_none() && job.shares.is_empty() {
            std::fs::remove_file(job_path(state, job.invitation)?)
                .map_err(|_| "sharing_network_storage_unavailable")?;
        }
    }
    Ok(())
}
pub(crate) async fn run(state: AppState, stop: CancellationToken) {
    loop {
        let result = cleanup(&state).await;
        let error = result.err();
        let previous = std::mem::replace(
            &mut *state
                .sharing
                .network_invite_error
                .lock()
                .expect("network cleanup observation"),
            error,
        );
        if previous != error {
            match error {
                Some(code) => {
                    tracing::warn!(code, "Tailscale share cleanup failed; retrying every 30 s")
                }
                None => tracing::info!(
                    previous = previous.unwrap_or(""),
                    "Tailscale share cleanup recovered"
                ),
            }
        }
        observe(&state).await;
        tokio::select! { _ = stop.cancelled() => break, _ = tokio::time::sleep(Duration::from_secs(30)) => {} }
    }
}

pub(crate) async fn issue(state: &AppState, invite: Invitation) -> Result<Secret> {
    let invitation_id = invite.id;
    let result = issue_owned(state, invite).await;
    if let Err(code) = result {
        tracing::warn!(%invitation_id, code, "invitation with Tailscale device shares could not be created");
    }
    result
}

async fn issue_owned(state: &AppState, invite: Invitation) -> Result<Secret> {
    let state = state.clone();
    let (send, receive) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let result = tokio::time::timeout(Duration::from_secs(22), issue_inner(&state, &invite))
            .await
            .unwrap_or(Err("sharing_network_outcome_unknown"));
        let failed = result.is_err();
        let delivered = send.send(result).is_ok();
        if failed || !delivered {
            {
                let _guard = state.sharing.network_invite_lock.lock().await;
                if let Ok(path) = job_path(&state, invite.id) {
                    if path.exists() {
                        let _ = abandon_file(&path);
                    }
                }
            }
            observe_failed_issue_cleanup(&state, invite.id).await;
        }
    });
    receive
        .await
        .map_err(|_| "sharing_network_outcome_unknown")?
}

async fn observe_failed_issue_cleanup(state: &AppState, invitation: Uuid) {
    let cancellation = state
        .store
        .cancel_share_invitation(invitation)
        .await
        .map_err(|error| {
            crate::store_result::observe(
                crate::store_result::Operation::CancelUnpublishedShareInvitation,
                crate::store_result::Discard::LostWork,
                Err::<(), _>(error),
            );
            "sharing_authority_unavailable"
        });
    // Always attempt cleanup. A failed issue may have no saved job, so an
    // empty successful sweep must not hide its failed authority cancellation.
    let cleanup = cleanup(state).await;
    *state
        .sharing
        .network_invite_error
        .lock()
        .expect("network cleanup observation") = cancellation.and(cleanup).err();
}

fn abandon_file(path: &Path) -> Result<()> {
    let raw = sharing_host::read_json(path).map_err(|_| "sharing_network_storage_unavailable")?;
    let mut job: Job =
        serde_json::from_value(raw).map_err(|_| "sharing_network_storage_unavailable")?;
    job.cleanup = true;
    sharing_host::write_json(path, &job).map_err(|_| "sharing_network_storage_unavailable")
}

fn accepted_user(record: &Value) -> Option<String> {
    if record.get("accepted").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let value = record.get("acceptedBy")?.get("id")?;
    value
        .as_str()
        .map(str::to_owned)
        .or_else(|| value.as_u64().map(|n| n.to_string()))
}
async fn retain_other_authority(state: &AppState, token: &Secret, share: &Share) -> Result<bool> {
    let record = match request(
        token,
        reqwest::Method::GET,
        &format!("device-invites/{}", share.id),
        None,
    )
    .await
    {
        Ok(value) => value,
        Err("sharing_network_not_found") => return Ok(false),
        Err(error) => return Err(error),
    };
    let Some(user) = accepted_user(&record) else {
        return Ok(false);
    };
    let raw = request(
        token,
        reqwest::Method::GET,
        &format!("device/{}/device-invites", share.device),
        None,
    )
    .await?;
    let rows = raw.as_array().ok_or("sharing_network_invalid_response")?;
    for other in rows {
        let Some(id) = other
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| good_id(id))
        else {
            return Err("sharing_network_invalid_response");
        };
        if id == share.id || accepted_user(other).as_ref() != Some(&user) {
            continue;
        }
        let owner = state
            .store
            .get_setting(&format!("sharing.network.invite.{id}"))
            .await
            .map_err(|_| "sharing_authority_unavailable")?;
        // A manual/external share is independent authority. Never revoke its
        // relationship merely because this Cinema grant ended.
        let Some(invitation) = owner.and_then(|s| Uuid::parse_str(&s).ok()) else {
            return Ok(true);
        };
        if state
            .store
            .sharing_network_retained(invitation, clock_ms())
            .await
            .map_err(|_| "sharing_authority_unavailable")?
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn share_acceptance_reads_the_accepting_login_and_the_keys_tailnet() {
        let accepted = json!({"id":"1","accepted":true,"acceptedBy":{"id":42,"loginName":"owner-b@example.com"}});
        assert_eq!(
            accepted_login(&accepted).as_deref(),
            Some("owner-b@example.com")
        );
        assert_eq!(accepted_login(&json!({"id":"1","accepted":false})), None);
        assert_eq!(
            accepted_login(&json!({"acceptedBy":{"loginName":"a\u{7}b"}})),
            None,
            "control characters never reach a page or log"
        );
        assert_eq!(
            tailnet_of("cinema.tail111.ts.net").as_deref(),
            Some("tail111.ts.net")
        );
        assert_eq!(tailnet_of("cinema.example.com"), None);
        assert_eq!(tailnet_of("cinema"), None);
    }
    #[test]
    fn provider_invite_accepts_omitted_false_flags() {
        for fields in [
            json!({"id":"123","inviteUrl":"https://login.tailscale.com/admin/invite/fixture"}),
            json!({"id":"123","inviteUrl":"https://login.tailscale.com/admin/invite/fixture","multiUse":false,"allowExitNode":false}),
        ] {
            let result = created_invite(json!([fields]))
                .expect("single-use response uses default false flags");
            assert_eq!(result.0, "123");
        }
    }
    #[test]
    fn provider_invite_refuses_reusable_exit_node_and_malformed_flags() {
        for (key, value) in [
            ("multiUse", json!(true)),
            ("allowExitNode", json!(true)),
            ("multiUse", json!("false")),
            ("allowExitNode", Value::Null),
        ] {
            let mut row =
                json!({"id":"123","inviteUrl":"https://login.tailscale.com/admin/invite/fixture"});
            row[key] = value;
            assert!(created_invite(json!([row])).is_err());
        }
        assert!(created_invite(json!([])).is_err());
        assert!(created_invite(
            json!([{"id":"123","inviteUrl":"https://evil.invalid/invite/fixture"}])
        )
        .is_err());
    }
    fn endpoint() -> Endpoint {
        Endpoint {
            ipv4: "100.64.1.2".parse().expect("fixture IP"),
            ipv6: None,
            ts_fqdn: "cinema.tail123.ts.net".into(),
            port: 32443,
            spki_sha256: "a".repeat(64),
        }
    }
    #[test]
    fn external_invite_targets_only_exact_owned_manifest_devices() {
        let endpoint = endpoint();
        let mut rows = vec![Device {
            id: "12345".into(),
            name: endpoint.ts_fqdn.clone(),
            addresses: vec![endpoint.ipv4.to_string()],
            is_external: false,
        }];
        assert_eq!(
            select_devices(&rows, std::slice::from_ref(&endpoint)).expect("owned device"),
            vec!["12345"]
        );
        rows[0].is_external = true;
        assert!(select_devices(&rows, std::slice::from_ref(&endpoint)).is_err());
        rows[0].is_external = false;
        rows[0].addresses = vec!["100.64.9.9".into()];
        assert!(select_devices(&rows, std::slice::from_ref(&endpoint)).is_err());
        rows[0].addresses = vec![endpoint.ipv4.to_string()];
        rows[0].id = "../other".into();
        assert!(select_devices(&rows, std::slice::from_ref(&endpoint)).is_err());
    }
    #[test]
    fn external_invite_ids_cannot_escape_fixed_control_paths() {
        for id in [
            "",
            "../admin",
            "123?x=y",
            "https://other.invalid",
            "123/456",
            "123#x",
        ] {
            assert!(!good_id(id));
        }
        assert!(good_id("n123_CNTRL-456"));
    }
}

#[cfg(test)]
mod abandonment_test {
    use super::*;
    #[tokio::test]
    async fn failed_issue_reports_cancellation_failure_and_preserves_cleanup_intent() {
        use std::sync::Arc;
        let root = tempfile::tempdir().expect("owned failed issue fixture");
        let database = root.path().join("store.sqlite");
        let store = plurx_core::store::SqliteStore::open(&database).expect("fixture store");
        let dirs = crate::state::Dirs {
            artwork: root.path().join("artwork"),
            transcode: root.path().join("transcode"),
            cache: root.path().join("cache"),
            subs: root.path().join("subs"),
            runtime_cache: root.path().join("runtime"),
            renditions: root.path().join("renditions"),
        };
        let mut state = AppState::new(
            "test".into(),
            Arc::new(store),
            dirs,
            "test-node".into(),
            Default::default(),
            Default::default(),
            Arc::new(crate::logbuf::LogBuffer::new(64)),
        );
        state.sharing = Arc::new(crate::sharing::SharingManager::new(
            Arc::clone(&state.sharing.key),
            root.path().join("sharing-tls"),
            Default::default(),
        ));
        assert!(directory(&state)
            .expect("owned journal directory")
            .starts_with(root.path()));
        let connection = rusqlite::Connection::open(&database).expect("owned database fault");
        connection
            .execute_batch("DROP TABLE sharing_invitations;")
            .expect("inject cancellation storage failure");
        let invitation = Uuid::new_v4();
        assert!(jobs(&state)
            .expect("no job saved before failed issuance")
            .is_empty());
        observe_failed_issue_cleanup(&state, invitation).await;
        assert_eq!(
            *state
                .sharing
                .network_invite_error
                .lock()
                .expect("failure observation"),
            Some("sharing_authority_unavailable")
        );
        let job = Job {
            invitation,
            created_at_ms: 1,
            completed: false,
            cleanup: false,
            authorization: "synthetic sealed credential".into(),
            attempting: None,
            shares: Vec::new(),
        };
        save(&state, &job).expect("owned cleanup journal");
        observe_failed_issue_cleanup(&state, invitation).await;
        let recovered: Job = serde_json::from_value(
            sharing_host::read_json(&job_path(&state, invitation).expect("job path"))
                .expect("retained journal"),
        )
        .expect("retained job");
        assert!(
            recovered.cleanup,
            "cleanup is attempted despite the first cancellation failure"
        );
        assert_eq!(recovered.authorization, job.authorization);
        assert_eq!(
            *state
                .sharing
                .network_invite_error
                .lock()
                .expect("retained failure"),
            Some("sharing_authority_unavailable")
        );
    }

    #[test]
    fn abandoned_completed_network_job_keeps_durable_cleanup_intent() {
        let root = tempfile::tempdir().expect("network journal directory");
        let id = Uuid::new_v4();
        let path = root.path().join(format!("{id}.json"));
        let job = Job {
            invitation: id,
            created_at_ms: 1,
            completed: true,
            cleanup: false,
            authorization: "synthetic sealed credential".into(),
            attempting: None,
            shares: vec![Share {
                device: "123".into(),
                id: "456".into(),
            }],
        };
        sharing_host::write_json(&path, &job).expect("completed job persisted before delivery");
        abandon_file(&path).expect("lost recipient recorded before authority cancellation");
        let recovered: Job =
            serde_json::from_value(sharing_host::read_json(&path).expect("durable journal"))
                .expect("decode job");
        assert!(recovered.completed);
        assert!(
            recovered.cleanup,
            "a still-open Cinema invitation cannot hide abandonment after restart"
        );
        assert_eq!(recovered.shares[0].id, "456");
        assert_eq!(recovered.authorization, job.authorization);
    }
}
