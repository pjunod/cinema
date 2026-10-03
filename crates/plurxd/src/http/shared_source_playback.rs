//! Private Source HLS start coordination. Wire data is never producer authority.
use super::{
    error::ApiError,
    hls::{CreateSession, SourcePlaybackTarget},
};
use axum::http::{HeaderMap, StatusCode};
use serde_json::Value;
use uuid::Uuid;

struct SourceStartInput {
    reference: SourcePlaybackTarget,
    session: CreateSession,
    request_id: Uuid,
    // The canonical complete client recipe is retained for exact retry identity;
    // the actual engine's normalized fingerprint is separately bound at prepare.
    canonical_recipe: Vec<u8>,
}
#[derive(Default)]
pub(crate) struct SourceStartRegistry {
    entries: std::sync::Mutex<Vec<std::sync::Arc<SourceStartEntry>>>,
}
struct SourceStartEntry {
    identity: SourceStartIdentity,
    changed: tokio::sync::Notify,
    result: std::sync::Mutex<Option<Result<SourceStartOwned, SourceStartFailure>>>,
}
struct SourceStartIdentity {
    owner_key: String,
    request_id: Uuid,
    reference: SourcePlaybackTarget,
    recipe_hash: [u8; 32],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceStartFailure {
    Unavailable,
    Capacity,
    Conflict,
    Unresolved,
    Unsupported,
    Deadline,
}
impl SourceStartRegistry {
    // This object is installed on one actual manager. No global pointer map,
    // rotating credential hash or user-supplied authority keys a retained task.
    fn register(
        &self,
        grant: Uuid,
        viewer: &str,
        input: &SourceStartInput,
    ) -> Result<(std::sync::Arc<SourceStartEntry>, bool), SourceStartFailure> {
        use sha2::Digest;
        let principal = plurx_core::playback_principal::PlaybackPrincipal::sharing(grant, viewer)
            .map_err(|_| SourceStartFailure::Unavailable)?;
        let identity = SourceStartIdentity {
            owner_key: principal.owner_key(),
            request_id: input.request_id,
            reference: input.reference.clone(),
            recipe_hash: sha2::Sha256::digest(&input.canonical_recipe).into(),
        };
        let mut entries = self.entries.lock().expect("Source HTTP starts");
        // Only the actual actor's terminal physical/body/SQL settlement can
        // release bookkeeping capacity. Unknown/failed outcomes stay retained.
        entries.retain(|entry| {
            !entry
                .result
                .lock()
                .expect("Source HTTP outcome")
                .as_ref()
                .is_some_and(|result| {
                    result
                        .as_ref()
                        .is_ok_and(|owned| owned.actor.settlement_status() == Some(Ok(())))
                })
        });
        if let Some(entry) = entries.iter().find(|entry| {
            entry.identity.owner_key == identity.owner_key
                && entry.identity.request_id == identity.request_id
        }) {
            if entry.identity.reference != identity.reference
                || entry.identity.recipe_hash != identity.recipe_hash
            {
                return Err(SourceStartFailure::Conflict);
            }
            return Ok((std::sync::Arc::clone(entry), false));
        }
        if entries.len() >= 8 {
            return Err(SourceStartFailure::Capacity);
        }
        let entry = std::sync::Arc::new(SourceStartEntry {
            identity,
            changed: tokio::sync::Notify::new(),
            result: std::sync::Mutex::new(None),
        });
        entries.push(std::sync::Arc::clone(&entry));
        Ok((entry, true))
    }
}

#[derive(Clone)]
struct SourceStartOwned {
    actor: crate::transcode::source_actor::SourceViewerActor,
    assignment: plurx_core::sharing_source_sessions::SourceDispatchAssignment,
}
impl From<crate::transcode::source_actor::SourceWorkerError> for SourceStartFailure {
    fn from(value: crate::transcode::source_actor::SourceWorkerError) -> Self {
        use crate::transcode::source_actor::SourceWorkerError as E;
        match value {
            E::Unavailable => Self::Unavailable,
            E::Capacity => Self::Capacity,
            E::Conflict => Self::Conflict,
            E::Deadline => Self::Deadline,
            E::Unresolved => Self::Unresolved,
            E::Unsupported => Self::Unsupported,
        }
    }
}
impl SourceStartFailure {
    fn response(self) -> ApiError {
        let (status, code) = match self {
            Self::Conflict => (StatusCode::CONFLICT, "sharing_start_conflict"),
            Self::Capacity => (StatusCode::TOO_MANY_REQUESTS, "sharing_start_capacity"),
            Self::Unsupported => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "sharing_start_unsupported",
            ),
            Self::Unresolved => (StatusCode::SERVICE_UNAVAILABLE, "sharing_start_unresolved"),
            Self::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "sharing_start_unavailable"),
            Self::Deadline => (StatusCode::SERVICE_UNAVAILABLE, "sharing_start_deadline"),
        };
        ApiError::typed(status, code, "Shared start is unavailable")
    }
}
impl SourceStartEntry {
    async fn wait(
        &self,
        deadline: std::time::Instant,
    ) -> Result<SourceStartOwned, SourceStartFailure> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(result) = self.result.lock().expect("Source HTTP outcome").clone() {
                return result;
            }
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed)
                .await
                .map_err(|_| SourceStartFailure::Deadline)?;
        }
    }
}
pub(crate) fn peer_router(state: crate::state::AppState) -> axum::Router<crate::state::AppState> {
    axum::Router::new()
        .route(
            "/sharing/v1/items/{item}/files/{file}/sessions",
            axum::routing::post(start),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            super::shared_library::source_content_guard,
        ))
}
async fn start(
    axum::extract::State(state): axum::extract::State<crate::state::AppState>,
    headers: HeaderMap,
    axum::extract::Path((item, file)): axum::extract::Path<(String, String)>,
    body: axum::body::Body,
) -> Result<axum::response::Response, ApiError> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(305);
    let bytes = axum::body::to_bytes(body, 128 * 1024)
        .await
        .map_err(|_| invalid())?;
    let input = parse_start_request(&bytes, &item, &file)?;
    let viewer = viewer_hash(&headers)?;
    let (_, grant) = current_reference(&state, &headers, &input.reference).await?;
    let target = input.reference.clone();
    let (entry, new) = state
        .transcode
        .source_http_starts
        .register(grant, &viewer, &input)
        .map_err(SourceStartFailure::response)?;
    if new {
        // No await separates insertion and spawning the owned task. Disconnect
        // drops only a waiter; durable/physical work stays owned by this entry.
        let state = std::sync::Arc::new(state.clone());
        let headers = headers.clone();
        let entry = std::sync::Arc::clone(&entry);
        tokio::spawn(async move {
            let result = Box::pin(own_start(state, headers, input, grant, deadline)).await;
            *entry.result.lock().expect("Source HTTP outcome") = Some(result);
            entry.changed.notify_waiters();
        });
    }
    let owned = entry
        .wait(deadline)
        .await
        .map_err(SourceStartFailure::response)?;
    let (response, guard) = owned
        .actor
        .open_start_response(deadline)
        .await
        .map_err(|e| SourceStartFailure::from(e).response())?;
    let (_, current_grant) = current_reference(&state, &headers, &target).await?;
    if current_grant != grant {
        return Err(unavailable());
    }
    let response = super::shared_library::source_file_json(
        grant,
        &target,
        serde_json::json!({"reference":target,"incarnation_id":owned.assignment.binding().incarnation_id(),"response":response}),
    )?;
    Ok(hold_start_body(response, guard))
}
fn hold_start_body(
    response: axum::response::Response,
    guard: crate::transcode::source_actor::SourceResponseGuard,
) -> axum::response::Response {
    use futures_util::StreamExt;
    let (parts, body) = response.into_parts();
    let stream = futures_util::stream::try_unfold(
        (body.into_data_stream(), guard),
        |(mut stream, guard)| async move {
            let next = tokio::select! {biased;
                ()=guard.cancelled()=>return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,"Source body retired")),
                next=stream.next()=>next,
            };
            match next {
                Some(bytes) => Ok(Some((
                    bytes.map_err(std::io::Error::other)?,
                    (stream, guard),
                ))),
                None => Ok(None),
            }
        },
    );
    let body = axum::body::Body::from_stream(stream);
    axum::response::Response::from_parts(parts, body)
}
async fn own_start(
    state: std::sync::Arc<crate::state::AppState>,
    headers: HeaderMap,
    input: SourceStartInput,
    grant: Uuid,
    deadline: std::time::Instant,
) -> Result<SourceStartOwned, SourceStartFailure> {
    use plurx_core::sharing_source_sessions::{
        SourceClaimOutcome, SourceIntentRead, SourceSessionRequest, SourceWriteAuthorityRead,
    };
    let reference = input.reference.clone();
    let prepared = Box::pin(super::hls::prepare_source_playback(
        &state,
        &headers,
        reference.clone(),
        input.session,
    ))
    .await
    .map_err(|_| SourceStartFailure::Unavailable)?;
    let (hash, current_grant) = current_reference(&state, &headers, &reference)
        .await
        .map_err(|_| SourceStartFailure::Unavailable)?;
    if grant != current_grant || std::time::Instant::now() >= deadline {
        return Err(SourceStartFailure::Unavailable);
    }
    let now = crate::state::clock_ms();
    let remaining = deadline
        .saturating_duration_since(std::time::Instant::now())
        .as_millis()
        .min(305000) as i64;
    let intent = state
        .store
        .prepare_source_session_intent(
            SourceSessionRequest {
                principal: prepared.principal().clone(),
                request_id: input.request_id.to_string(),
                request_fingerprint: prepared.fingerprint().into(),
                playback_id: prepared.request().playback_id.clone(),
                incarnation_id: Uuid::new_v4(),
                now_ms: now,
                claim_expires_at_ms: now + remaining,
                credential_hash: hash,
                item_id: reference.item_id.clone(),
                file_id: reference.file_id.clone(),
                file_revision: reference.revision.clone(),
            },
            &state.sharing.key,
        )
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?;
    let intent = match intent {
        SourceIntentRead::Ready(value) => value,
        SourceIntentRead::Unavailable => return Err(SourceStartFailure::Unavailable),
        SourceIntentRead::Capacity => return Err(SourceStartFailure::Capacity),
    };
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
        .ok_or(SourceStartFailure::Unavailable)?;
    let binding = match state
        .store
        .claim_source_media_session(&intent, &members)
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
    {
        SourceClaimOutcome::Acquired(binding) => binding,
        // A committed old claim/replay without this retained HTTP owner is an
        // unresolved restart outcome, never evidence of no physical activation.
        SourceClaimOutcome::InFlight(_)
        | SourceClaimOutcome::Resolved { .. }
        | SourceClaimOutcome::Retired(_) => return Err(SourceStartFailure::Unresolved),
        SourceClaimOutcome::Conflict => return Err(SourceStartFailure::Conflict),
        SourceClaimOutcome::Unavailable => return Err(SourceStartFailure::Unavailable),
        SourceClaimOutcome::Capacity(_) => return Err(SourceStartFailure::Capacity),
    };
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
        .ok_or(SourceStartFailure::Unresolved)?;
    let assignment = state
        .store
        .assign_source_dispatch(&binding, &state.sharing.key, &members)
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
        .ok_or(SourceStartFailure::Unresolved)?;
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
        .ok_or(SourceStartFailure::Unresolved)?;
    let activation = match state
        .store
        .prepare_source_activation_authority(&assignment, &state.sharing.key, &members)
        .await
        .map_err(|_| SourceStartFailure::Unresolved)?
    {
        SourceWriteAuthorityRead::Ready(value) => *value,
        SourceWriteAuthorityRead::Unavailable => return Err(SourceStartFailure::Unresolved),
        SourceWriteAuthorityRead::Capacity => return Err(SourceStartFailure::Capacity),
    };
    let actor = state
        .transcode
        .start_source_worker(
            std::sync::Arc::clone(&state),
            assignment.clone(),
            activation,
            prepared,
            deadline,
        )
        .await
        .map_err(SourceStartFailure::from)?;
    Ok(SourceStartOwned { actor, assignment })
}

// Read-only current authentication can precede owner insertion. It owns no
// durable or physical obligation and binds a stable grant across rotation.
async fn current_reference(
    state: &crate::state::AppState,
    headers: &HeaderMap,
    target: &SourcePlaybackTarget,
) -> Result<(String, Uuid), ApiError> {
    use plurx_core::{
        sharing::SharingIdentity, sharing_catalogue_details::CatalogueRevisionKey,
        store::sharing_catalogue_details::SourceDetailsRead,
    };
    let (hash, grant) = super::shared_library::authority(state, headers).await?;
    let SourceDetailsRead::Authorized(witness) = state
        .store
        .source_item_file_witness(&hash, grant, target.item_id.clone(), target.file_id.clone())
        .await
        .map_err(|_| unavailable())?
    else {
        return Err(unavailable());
    };
    if !witness.matches_source_file(
        target.server_id,
        target.catalogue_epoch,
        &target.library_id,
        &target.item_id,
        &target.file_id,
    ) {
        return Err(unavailable());
    }
    let envelope = state
        .store
        .source_catalogue_revision_key(target.server_id, target.catalogue_epoch)
        .await
        .map_err(|_| unavailable())?
        .ok_or_else(unavailable)?;
    let key = CatalogueRevisionKey::open(
        &state.sharing.key,
        SharingIdentity {
            server_id: target.server_id,
            catalogue_epoch: target.catalogue_epoch,
            created_at_ms: 0,
        },
        &envelope,
    )
    .map_err(|_| unavailable())?;
    if key.file_revision(&witness).map_err(|_| unavailable())? != target.revision {
        return Err(unavailable());
    }
    Ok((hash, grant))
}
fn unavailable() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "sharing_playback_authority_unavailable",
        "Shared playback authority is unavailable",
    )
}

fn invalid() -> ApiError {
    ApiError::typed(
        StatusCode::BAD_REQUEST,
        "sharing_invalid_request",
        "Invalid shared start request",
    )
}
fn parse_start_request(bytes: &[u8], item: &str, file: &str) -> Result<SourceStartInput, ApiError> {
    if bytes.is_empty() || bytes.len() > 128 * 1024 {
        return Err(invalid());
    }
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = super::sharing_decision_decode::bounded_decision_value(&mut decoder)
        .map_err(|_| invalid())?;
    decoder.end().map_err(|_| invalid())?;
    let reference: SourcePlaybackTarget =
        serde_json::from_value(value.get("reference").cloned().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
    super::validate_source_start_request(bytes, &reference).map_err(|_| invalid())?;
    if plurx_core::sharing::SourceId::parse(item).map_err(|_| invalid())? != reference.item_id
        || plurx_core::sharing::SourceId::parse(file).map_err(|_| invalid())? != reference.file_id
    {
        return Err(invalid());
    }
    let session_value = value.get("session").ok_or_else(invalid)?;
    let session: CreateSession =
        serde_json::from_value(session_value.clone()).map_err(|_| invalid())?;
    let typed = serde_json::to_value(&session).map_err(|_| invalid())?;
    if !closed_provided_fields(session_value, &typed) {
        return Err(invalid());
    }
    let request_id = Uuid::parse_str(session.request_id.as_deref().ok_or_else(invalid)?)
        .map_err(|_| invalid())?;
    Ok(SourceStartInput {
        reference,
        session,
        request_id,
        canonical_recipe: bytes.to_vec(),
    })
}
// Ordinary CreateSession supports omitted optional fields and explicit numeric
// JSON integers for floating time fields. Close every provided field against
// the actual complete serializer without changing that existing contract.
fn closed_provided_fields(input: &Value, typed: &Value) -> bool {
    match (input, typed) {
        (Value::Object(input), Value::Object(typed)) => input.iter().all(|(name, value)| {
            typed
                .get(name)
                .is_some_and(|field| closed_provided_fields(value, field))
        }),
        (Value::Array(input), Value::Array(typed)) => {
            input.len() == typed.len()
                && input
                    .iter()
                    .zip(typed)
                    .all(|(a, b)| closed_provided_fields(a, b))
        }
        (Value::Number(input), Value::Number(typed)) if typed.is_f64() => {
            input.as_f64() == typed.as_f64()
        }
        _ => input == typed,
    }
}
fn viewer_hash(headers: &HeaderMap) -> Result<String, ApiError> {
    let mut values = headers.get_all("cinemashare-viewer").iter();
    let value = values
        .next()
        .and_then(|v| v.to_str().ok())
        .ok_or_else(invalid)?;
    if values.next().is_some()
        || value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(invalid());
    }
    Ok(value.to_owned())
}

#[cfg(test)]
pub(crate) struct RealSourceStartFixture {
    pub state: std::sync::Arc<crate::state::AppState>,
    pub reference: SourcePlaybackTarget,
    pub headers: HeaderMap,
    pub request: Vec<u8>,
    pub grant: Uuid,
    selected: plurx_core::cluster::migration::SelectedStore,
    _directory: tempfile::TempDir,
}
#[cfg(test)]
impl RealSourceStartFixture {
    pub async fn shutdown(self) {
        self.selected
            .shutdown()
            .await
            .expect("actual voter shutdown");
    }
}
#[cfg(test)]
pub(crate) fn real_source_start_fixture(
) -> std::pin::Pin<Box<dyn std::future::Future<Output = RealSourceStartFixture> + Send>> {
    Box::pin(build_real_source_start_fixture())
}
#[cfg(test)]
async fn build_real_source_start_fixture() -> RealSourceStartFixture {
    use plurx_core::{
        cluster::migration::select_daemon_store,
        config::Config,
        domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
        sharing::{new_secret, secret_hash, InvitationRecord, SecretDomain, ShareClaim, SourceId},
        sharing_catalogue_details::CatalogueRevisionKey,
        store::{keys, sharing_catalogue_details::SourceDetailsRead},
    };
    use std::sync::Arc;
    let directory = crate::test_tempdir().expect("real Source fixture");
    let mut config = Config::default();
    config.storage.data_dir = directory.path().join("database");
    let raft = std::net::TcpListener::bind("127.0.0.1:0").expect("Raft port");
    let api = std::net::TcpListener::bind("127.0.0.1:0").expect("API port");
    config.cluster.raft_bind = raft.local_addr().expect("Raft address");
    config.cluster.api_bind = api.local_addr().expect("API address");
    config.cluster.advertise_host = "localhost".into();
    drop((raft, api));
    let mut selected = Box::pin(select_daemon_store(&config))
        .await
        .expect("actual one voter");
    selected
        .store
        .put_setting(keys::SHARING_ENABLED, "true")
        .await
        .expect("saved choice");
    // Run the real pre-serving factory before constructing State/listeners.
    // No manual candidate DDL or synthetic capability rows admit this fixture.
    assert!(Box::pin(selected.prepare_source_schema_before_serving())
        .await
        .expect("actual startup factory"));
    let client = selected.local_client().expect("local voter client");
    let mut state = Arc::new(crate::http::source_actor_test_state());
    let state_mut = Arc::get_mut(&mut state).expect("sole initial State");
    state_mut.store = Arc::clone(&selected.store);
    state_mut.membership = selected.membership_manager();
    state_mut.sharing = Arc::new(crate::sharing::SharingManager::new(
        Arc::clone(&selected.credential_key),
        config.storage.data_dir.clone(),
        config.sharing.clone(),
    ));
    let store = Arc::clone(&state.store);
    store
        .put_setting(keys::SW_POOL_THREADS, "4")
        .await
        .expect("actual software budget");
    let identity = store
        .sharing_identity(crate::state::clock_ms())
        .await
        .expect("identity");
    let library = store
        .create_library(&NewLibrary {
            name: "Actual HTTP Source".into(),
            kind: LibraryKind::Movies,
            paths: vec![directory.path().to_owned()],
            anime: false,
        })
        .await
        .expect("library")
        .id;
    let item = store
        .insert_item(&NewItem {
            library_id: library,
            kind: ItemKind::Movie,
            parent_id: None,
            title: "Actual HTTP Source".into(),
            year: None,
            season_number: None,
            episode_number: None,
        })
        .await
        .expect("item");
    let file = directory.path().join("source.mp4");
    let generated = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
        .args([
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=s=128x72:r=24",
            "-t",
            "2",
            "-c:v",
            "libx264",
            "-pix_fmt",
            "yuv420p",
            "-y",
        ])
        .arg(&file)
        .output()
        .await
        .expect("actual FFmpeg");
    assert!(
        generated.status.success(),
        "{}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let metadata = std::fs::metadata(&file).expect("actual file");
    let mtime = metadata
        .modified()
        .expect("mtime")
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let probe = tokio::process::Command::new(crate::ffmpeg::ffprobe_bin())
        .args([
            "-v",
            "error",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            "-of",
            "json",
        ])
        .arg(&file)
        .output()
        .await
        .expect("actual probe");
    assert!(probe.status.success());
    client.execute("INSERT INTO files(id,item_id,path,size,mtime,duration_ms,container,video_codec,width,height,bit_depth,bitrate,probe_json,scanned_at) VALUES(1,$1,$2,$3,$4,2000,'mp4','h264',128,72,8,100000,$5,$6)",hiqlite::params!(item,file.to_string_lossy().to_string(),metadata.len() as i64,mtime,String::from_utf8(probe.stdout).expect("scan JSON"),crate::state::clock_ms()/1000)).await.expect("actual scanned file facts");
    let now = crate::state::clock_ms();
    let grant = Uuid::new_v4();
    let invitation = Uuid::new_v4();
    let secret = new_secret().expect("credential");
    let hash = secret_hash(SecretDomain::Grant, &secret);
    store
        .create_share_invitation(InvitationRecord {
            id: invitation,
            token_hash: "a".repeat(64),
            library_ids: vec![library],
            created_at_ms: now,
            expires_at_ms: now + 60000,
        })
        .await
        .expect("invite");
    store
        .claim_share(ShareClaim {
            invitation_id: invitation,
            invitation_hash: "a".repeat(64),
            claim_id: Uuid::new_v4(),
            grant_id: grant,
            recipient_server_id: Uuid::new_v4(),
            recipient_name: "Actual B".into(),
            credential_hash: hash.clone(),
            now_ms: now + 1,
        })
        .await
        .expect("claim");
    store
        .approve_share(grant, 1, now + 2)
        .await
        .expect("explicit approval");
    let SourceDetailsRead::Authorized(witness) = store
        .source_item_file_witness(
            &hash,
            grant,
            SourceId::parse(&item.to_string()).expect("item"),
            SourceId::parse("1").expect("file"),
        )
        .await
        .expect("actual witness")
    else {
        panic!("authorized Source witness")
    };
    let envelope = store
        .source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
        .await
        .expect("stored key")
        .expect("factory key");
    let key = CatalogueRevisionKey::open(&state.sharing.key, identity.clone(), &envelope)
        .expect("actual factory key");
    let reference = SourcePlaybackTarget {
        server_id: identity.server_id,
        catalogue_epoch: identity.catalogue_epoch,
        library_id: SourceId::parse(&library.to_string()).expect("library"),
        item_id: SourceId::parse(&item.to_string()).expect("item"),
        file_id: SourceId::parse("1").expect("file"),
        revision: key.file_revision(&witness).expect("actual revision"),
    };
    let media = store.get_file(1).await.expect("file read").expect("file");
    let crate::fragindex::IndexOutcome::Built(index) = Box::pin(crate::fragindex::build(
        &media,
        plurx_core::transcode::CopyVideoOptions::new(false, false),
        directory.path(),
        std::time::Duration::from_secs(30),
    ))
    .await
    else {
        panic!("actual fragment scan")
    };
    store
        .put_fragment_index(1, &index)
        .await
        .expect("actual fragment scan committed");
    Arc::get_mut(&mut state)
        .expect("sole State before listeners")
        .transcode = Arc::new(crate::transcode::TranscodeManager::new(
        Arc::clone(&store),
        directory.path().join("workers"),
        plurx_core::transcode::EncoderCaps::default(),
        plurx_core::transcode::Pipeline::Cpu,
    ));
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("CinemaShare {}", secret.expose())
            .parse()
            .expect("auth"),
    );
    headers.insert(
        "cinemashare-viewer",
        "c".repeat(64).parse().expect("viewer"),
    );
    let request=serde_json::to_vec(&serde_json::json!({"reference":reference,"session":{"playback_id":"actual-http-client","request_id":Uuid::new_v4(),"copy":true,"height":72,"quality_auto":false,"presentation":"vod","caps":{"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]}}})).expect("canonical complete recipe");
    RealSourceStartFixture {
        state,
        reference,
        headers,
        request,
        grant,
        selected,
        _directory: directory,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn sharing_source_http_fixture_uses_actual_startup_factory_and_full_current_file_reference(
    ) {
        let fixture = real_source_start_fixture().await;
        let (_, grant) = current_reference(&fixture.state, &fixture.headers, &fixture.reference)
            .await
            .expect("actual authenticated tuple");
        assert_eq!(grant, fixture.grant);
        let input = parse_start_request(
            &fixture.request,
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str(),
        )
        .expect("complete canonical actual request");
        assert!(input.reference == fixture.reference);
        let mut stale = fixture.reference.clone();
        stale.revision =
            plurx_core::sharing_catalogue_details::FileRevision::parse(&"f".repeat(64))
                .expect("revision");
        assert!(current_reference(&fixture.state, &fixture.headers, &stale)
            .await
            .is_err());
        fixture.shutdown().await;
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn sharing_source_http_start_disconnect_joins_actual_actor_and_fresh_exact_replay() {
        Box::pin(actual_source_http_start()).await;
    }
    async fn actual_source_http_start() {
        use std::time::{Duration, Instant};
        use tokio::io::AsyncWriteExt;
        let fixture = real_source_start_fixture().await;
        let hold = fixture
            .state
            .transcode
            .test_hold_source_software_capacity()
            .expect("actual occupied software capacity");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("private fixture listener");
        let address = listener.local_addr().expect("private address");
        let router = super::super::sharing::peer_router((*fixture.state).clone());
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(crate::serve_http(
            listener,
            router,
            async move {
                let _ = stopped.await;
            },
            crate::HTTP_TIMEOUTS,
        ));
        let path = format!(
            "http://{address}/sharing/v1/items/{}/files/{}/sessions",
            fixture.reference.item_id.as_str(),
            fixture.reference.file_id.as_str()
        );
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("actual HTTP client");
        let mut initial = tokio::net::TcpStream::connect(address)
            .await
            .expect("actual initial TCP connection");
        let wire=format!("POST /sharing/v1/items/{}/files/{}/sessions HTTP/1.1\r\nHost: {address}\r\nAuthorization: {}\r\nCinemaShare-Viewer: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",fixture.reference.item_id.as_str(),fixture.reference.file_id.as_str(),fixture.headers.get("authorization").expect("auth").to_str().expect("auth text"),fixture.headers.get("cinemashare-viewer").expect("viewer").to_str().expect("viewer text"),fixture.request.len());
        initial
            .write_all(wire.as_bytes())
            .await
            .expect("actual start headers");
        initial
            .write_all(&fixture.request)
            .await
            .expect("actual start body");
        initial.flush().await.expect("actual start sent");
        let deadline = Instant::now() + Duration::from_secs(10);
        let owned = loop {
            let entry = fixture
                .state
                .transcode
                .source_http_starts
                .entries
                .lock()
                .expect("registry")
                .first()
                .cloned();
            if let Some(entry) = entry {
                let outcome = entry.result.lock().expect("actual owner outcome").clone();
                if let Some(result) = outcome {
                    break result.expect("actual Source actor inserted");
                }
            }
            assert!(
                Instant::now() < deadline,
                "actual HTTP owner must be retained before cancelled waiter"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert_eq!(fixture.state.transcode.test_software_threads_in_use(), 4);
        assert!(
            fixture
                .state
                .store
                .media_session_route_by_incarnation(
                    &owned.assignment.binding().incarnation_id().to_string()
                )
                .await
                .expect("actual route read")
                .is_none(),
            "occupied admission has no published Source route"
        );
        drop(initial); // Actual TCP EOF drops only the HTTP waiter.
        assert_eq!(
            fixture
                .state
                .transcode
                .source_http_starts
                .entries
                .lock()
                .expect("registry")
                .len(),
            1
        );
        drop(hold);
        let response = tokio::time::timeout(
            Duration::from_secs(20),
            client
                .post(&path)
                .headers(fixture.headers.clone())
                .body(fixture.request.clone())
                .send(),
        )
        .await
        .expect("actual retry deadline")
        .expect("actual HTTP retry");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("cache-control")
                .expect("private response"),
            "no-store"
        );
        let bytes = response.bytes().await.expect("guarded full Source body");
        let decoded = super::super::decode_source_start_response(&bytes, &fixture.reference)
            .expect("actual complete strict Source envelope");
        assert_eq!(
            decoded.incarnation_id(),
            owned.assignment.binding().incarnation_id()
        );
        let decoded_start = decoded.response();
        assert_eq!(
            decoded_start
                .control
                .as_ref()
                .expect("actual controller")
                .generation,
            decoded.incarnation_id().to_string()
        );
        let route = fixture
            .state
            .store
            .media_session_route_by_incarnation(&decoded.incarnation_id().to_string())
            .await
            .expect("actual route read")
            .expect("actual producer publication");
        assert_eq!(route.session_id, decoded_start.session_id);
        assert_eq!(route.publication_ready_at_ms, 0);
        let retry = client
            .post(&path)
            .headers(fixture.headers.clone())
            .body(fixture.request.clone())
            .send()
            .await
            .expect("exact metadata replay");
        assert_eq!(retry.status(), StatusCode::OK);
        assert_eq!(retry.bytes().await.expect("replay body"), bytes);
        let mut different: Value =
            serde_json::from_slice(&fixture.request).expect("canonical recipe");
        different["session"]["start"] = serde_json::json!(1);
        let conflict = client
            .post(&path)
            .headers(fixture.headers.clone())
            .body(serde_json::to_vec(&different).expect("changed recipe"))
            .send()
            .await
            .expect("conflict HTTP");
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        // A benign rotation authenticates the same stable grant and joins the
        // same actual Source owner; a rotating hash never keys its identity.
        let replacement = plurx_core::sharing::new_secret().expect("rotation credential");
        let new_hash = plurx_core::sharing::secret_hash(
            plurx_core::sharing::SecretDomain::Grant,
            &replacement,
        );
        let (old_hash, _) = current_reference(&fixture.state, &fixture.headers, &fixture.reference)
            .await
            .expect("old current credential");
        assert_eq!(
            fixture
                .state
                .store
                .rotate_share(
                    fixture.grant,
                    Uuid::new_v4(),
                    &old_hash,
                    &new_hash,
                    crate::state::clock_ms()
                )
                .await
                .expect("actual rotation"),
            plurx_core::sharing::MutationOutcome::Applied
        );
        let mut rotated = fixture.headers.clone();
        rotated.insert(
            "authorization",
            format!("CinemaShare {}", replacement.expose())
                .parse()
                .expect("new auth"),
        );
        let after_rotation = client
            .post(&path)
            .headers(rotated.clone())
            .body(fixture.request.clone())
            .send()
            .await
            .expect("rotated exact retry");
        assert_eq!(after_rotation.status(), StatusCode::OK);
        assert_eq!(after_rotation.bytes().await.expect("rotated body"), bytes);
        // An empty manager registry is not readiness or no-activation proof.
        // Keep the original real producer alive, but model lost in-process
        // HTTP/worker ownership with a new actual manager over the same Store.
        let mut untracked = (*fixture.state).clone();
        untracked.transcode = std::sync::Arc::new(crate::transcode::TranscodeManager::new(
            std::sync::Arc::clone(&untracked.store),
            fixture._directory.path().join("untracked-workers"),
            plurx_core::transcode::EncoderCaps::default(),
            plurx_core::transcode::Pipeline::Cpu,
        ));
        let unknown = Box::pin(start(
            axum::extract::State(untracked.clone()),
            rotated.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        ))
        .await
        .expect_err("stored response without actual owner must refuse");
        use axum::response::IntoResponse;
        assert_eq!(
            unknown.into_response().status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(untracked.transcode.test_software_threads_in_use(), 0);
        assert_eq!(owned.actor.settlement_status(), None);
        assert_eq!(
            untracked
                .transcode
                .source_http_starts
                .entries
                .lock()
                .expect("unresolved registry")
                .len(),
            1
        );
        // Hold the actual returned HTTP Body, not an invented publication
        // flag. The actor cannot settle physical/body/SQL obligations while
        // the complete Start transport still owns this response guard.
        let held = Box::pin(start(
            axum::extract::State((*fixture.state).clone()),
            rotated.clone(),
            axum::extract::Path((
                fixture.reference.item_id.as_str().to_owned(),
                fixture.reference.file_id.as_str().to_owned(),
            )),
            axum::body::Body::from(fixture.request.clone()),
        ))
        .await
        .expect("actual guarded HTTP response");
        let retiring = owned.actor.clone();
        let retirement = tokio::spawn(async move { retiring.retire().await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !retirement.is_finished(),
            "actual returned Start Body keeps retirement barrier"
        );
        assert_eq!(owned.actor.settlement_status(), None);
        fixture
            .state
            .store
            .revoke_share(fixture.grant, crate::state::clock_ms())
            .await
            .expect("actual revoke");
        let revoked = client
            .post(&path)
            .headers(rotated)
            .body(fixture.request.clone())
            .send()
            .await
            .expect("revoked retry");
        assert!(!revoked.status().is_success());
        drop(held);
        tokio::time::timeout(Duration::from_secs(15), retirement)
            .await
            .expect("physical settlement deadline")
            .expect("detached retirement task")
            .expect("actual physical/body/SQL settlement");
        assert_eq!(owned.actor.settlement_status(), Some(Ok(())));
        assert_eq!(fixture.state.transcode.test_software_threads_in_use(), 0);
        let _ = stop.send(());
        server
            .await
            .expect("server join")
            .expect("actual server shutdown");
        drop((owned, client));
        fixture.shutdown().await;
    }
    fn fixture() -> Value {
        json!({"reference":{"server_id":Uuid::new_v4(),"catalogue_epoch":Uuid::new_v4(),"library_id":"0","item_id":"9007199254740993","file_id":"9223372036854775807","revision":"a".repeat(64)},"session":{"playback_id":"actual-client-instance","request_id":Uuid::new_v4(),"copy":true,"presentation":"vod","start":0,"caps":{"v":2,"video":[{"codec":"h264","max_height":2160,"present":["sdr"]}],"audio":["aac"],"containers":["mp4"],"transports":["hls","progressive"]}}})
    }
    fn parse(value: &Value) -> Result<SourceStartInput, ApiError> {
        parse_start_request(
            &serde_json::to_vec(value).expect("canonical JSON"),
            "9007199254740993",
            "9223372036854775807",
        )
    }
    #[test]
    fn sharing_source_start_request_preserves_complete_actual_create_session_and_exact_source_ids()
    {
        let value = fixture();
        let input = parse(&value).expect("actual Source start body");
        assert_eq!(input.reference.library_id.as_str(), "0");
        assert_eq!(input.reference.item_id.as_str(), "9007199254740993");
        assert_eq!(input.reference.file_id.as_str(), "9223372036854775807");
        assert_eq!(
            input.session.request_id.as_deref(),
            Some(input.request_id.to_string().as_str())
        );
        assert_eq!(
            input.canonical_recipe,
            serde_json::to_vec(&value).expect("recipe")
        );
    }
    #[test]
    fn sharing_source_start_request_refuses_unknown_nested_fields_duplicates_and_path_drift() {
        let value = fixture();
        for path in ["", "/session", "/session/caps", "/session/caps/video/0"] {
            let mut bad = value.clone();
            let field = if path.is_empty() {
                &mut bad
            } else {
                bad.pointer_mut(path).expect("path")
            };
            field["foreign_authority"] = json!(true);
            assert!(parse(&bad).is_err(), "unknown {path}");
        }
        assert!(parse_start_request(
            &serde_json::to_vec(&value).expect("JSON"),
            "1",
            "9223372036854775807"
        )
        .is_err());
        let duplicate = format!(
            "{{\"reference\":{},\"session\":{},\"session\":{}}}",
            value["reference"], value["session"], value["session"]
        );
        assert!(parse_start_request(
            duplicate.as_bytes(),
            "9007199254740993",
            "9223372036854775807"
        )
        .is_err());
        let mut bad = value;
        bad["session"]["request_id"] = json!(Uuid::nil());
        assert!(parse(&bad).is_err());
    }
    #[test]
    fn sharing_source_start_viewer_requires_single_sensitive_hash_shape() {
        let mut headers = HeaderMap::new();
        headers.insert("cinemashare-viewer", "a".repeat(64).parse().expect("hash"));
        assert_eq!(viewer_hash(&headers).expect("viewer"), "a".repeat(64));
        headers.append(
            "cinemashare-viewer",
            "b".repeat(64).parse().expect("duplicate"),
        );
        assert!(viewer_hash(&headers).is_err());
        for invalid in ["A".repeat(64), "g".repeat(64), "a".repeat(63)] {
            headers.remove("cinemashare-viewer");
            headers.insert("cinemashare-viewer", invalid.parse().expect("header"));
            assert!(viewer_hash(&headers).is_err());
        }
    }
    #[test]
    fn sharing_source_start_registry_binds_full_identity_and_keeps_owner_after_waiter_drop() {
        let value = fixture();
        let input = parse(&value).expect("request");
        let grant = Uuid::new_v4();
        let viewer = "a".repeat(64);
        let registry = SourceStartRegistry::default();
        let (first, new) = registry.register(grant, &viewer, &input).expect("first");
        assert!(new);
        let pointer = std::sync::Arc::as_ptr(&first);
        drop(first);
        let (retry, new) = registry.register(grant, &viewer, &input).expect("retry");
        assert!(!new);
        assert_eq!(std::sync::Arc::as_ptr(&retry), pointer);
        let mut changed = value.clone();
        changed["session"]["quality_auto"] = json!(true);
        assert!(matches!(
            registry.register(grant, &viewer, &parse(&changed).expect("changed body")),
            Err(SourceStartFailure::Conflict)
        ));
        let mut changed = value;
        changed["reference"]["revision"] = json!("b".repeat(64));
        assert!(matches!(
            registry.register(grant, &viewer, &parse(&changed).expect("changed reference")),
            Err(SourceStartFailure::Conflict)
        ));
        assert!(
            registry
                .register(Uuid::new_v4(), &viewer, &input)
                .expect("other grant")
                .1
        );
        assert!(
            registry
                .register(grant, &"b".repeat(64), &input)
                .expect("other viewer")
                .1
        );
        let second = SourceStartRegistry::default();
        let (other, new) = second
            .register(grant, &viewer, &input)
            .expect("independent registry");
        assert!(new);
        assert!(!std::sync::Arc::ptr_eq(&retry, &other));
    }
    #[test]
    fn sharing_source_start_registry_refuses_ninth_retained_owner_without_evicting_unknown_requests(
    ) {
        let registry = SourceStartRegistry::default();
        let grant = Uuid::new_v4();
        let viewer = "a".repeat(64);
        let mut value = fixture();
        for _ in 0..8 {
            value["session"]["request_id"] = json!(Uuid::new_v4());
            let input = parse(&value).expect("request");
            assert!(
                registry
                    .register(grant, &viewer, &input)
                    .expect("bounded entry")
                    .1
            );
        }
        value["session"]["request_id"] = json!(Uuid::new_v4());
        assert!(matches!(
            registry.register(grant, &viewer, &parse(&value).expect("ninth")),
            Err(SourceStartFailure::Capacity)
        ));
        assert_eq!(registry.entries.lock().expect("entries").len(), 8);
    }
}
