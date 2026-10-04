//! Receiver pre-session file assets — subtitle WebVTT, PGS overlay manifest
//! and objects, chapter thumbnails — and the typed closure for every shared
//! file suffix B does not serve.
//!
//! An asset is read fresh from the pinned Source on every request: no B cache,
//! `Cache-Control: no-store`. The caller is either the signed-in viewer
//! (header, cookie-less `?token=`) or an exact `session=` binding to a live
//! receiver session playing the same signed file revision. Either way the
//! body carries a file-scoped receiver authority that the content guard
//! re-observes before it leaves.
use super::{
    error::ApiError,
    extract::{AuthUser, RawToken},
    hls::SourcePlaybackTarget,
    shared_library,
};
use crate::{sharing_client::PeerError, sharing_client::PeerFileAsset, state::AppState};
use axum::{
    extract::{FromRequestParts, Path, State},
    http::{header, request::Parts, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::{any, get},
    Json, Router,
};
use plurx_core::sharing_resources::{SharingFileResource, SharingFileResourceKind};
use std::sync::{Arc, LazyLock};
use uuid::Uuid;

/// What B accepts from a Source for each closed asset kind.
pub(crate) const MAX_VTT_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_IMAGE_BYTES: usize = 1024 * 1024;
/// Asset relays in flight on this receiver. Each holds at most one complete
/// bounded body; a refusal is a 429, nothing queues behind it.
static ASSETS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(16)));

pub(crate) fn viewer_router(state: AppState) -> Router<AppState> {
    Router::new()
        .route(
            "/shared/imports/{import}/files/{locator}/subs/{subtitle}",
            get(subtitle),
        )
        .route(
            "/shared/imports/{import}/files/{locator}/subs/{index}/overlay.json",
            get(overlay_manifest),
        )
        .route(
            "/shared/imports/{import}/files/{locator}/subs/{index}/overlay/{generation}/objects/{object}",
            get(overlay_object),
        )
        .route(
            "/shared/imports/{import}/files/{locator}/chapters/{index}/thumb",
            get(chapter_thumb),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            shared_library::receiver_content_guard,
        ))
}

/// Every unserved suffix of the shared file grammar answers one typed 422
/// for every method, HEAD included, before any import, locator, account or
/// Local lookup: a Source file number is never a Local file ID here.
pub(crate) fn closure_router() -> Router<AppState> {
    Router::new()
        // Permanent: progressive is a live remux per seek, and shared Copy
        // HLS already delivers that representation.
        .route(
            "/shared/imports/{import}/files/{locator}/stream.mp4",
            any(resource_unsupported),
        )
}

pub(crate) async fn resource_unsupported() -> ApiError {
    ApiError::typed(
        StatusCode::UNPROCESSABLE_ENTITY,
        "sharing_resource_unsupported",
        "This shared file resource is not served",
    )
}

fn invalid() -> ApiError {
    ApiError::typed(
        StatusCode::BAD_REQUEST,
        "sharing_invalid_request",
        "Invalid shared asset request",
    )
}
fn unavailable() -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "sharing_asset_unavailable",
        "Shared asset is unavailable",
    )
}
fn binding_refused() -> ApiError {
    ApiError::typed(
        StatusCode::FORBIDDEN,
        "sharing_asset_binding_refused",
        "This playback session does not authorize the shared file",
    )
}
fn source_failure(error: PeerError) -> ApiError {
    match error {
        PeerError::Rejected(StatusCode::NOT_FOUND) => ApiError::typed(
            StatusCode::NOT_FOUND,
            "sharing_asset_not_found",
            "The Source has no such asset for this file",
        ),
        PeerError::Rejected(
            StatusCode::BAD_REQUEST
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
            | StatusCode::UNPROCESSABLE_ENTITY,
        ) => ApiError::typed(
            StatusCode::UNPROCESSABLE_ENTITY,
            "sharing_asset_unsupported",
            "The Source cannot provide this asset for this track",
        ),
        PeerError::Rejected(StatusCode::TOO_MANY_REQUESTS) => capacity(),
        _ => unavailable(),
    }
}
fn capacity() -> ApiError {
    ApiError::typed(
        StatusCode::TOO_MANY_REQUESTS,
        "sharing_asset_capacity",
        "Shared asset capacity is busy",
    )
}

async fn subtitle(
    State(state): State<AppState>,
    Path((import, locator, subtitle)): Path<(String, String, String)>,
    parts: Parts,
) -> Result<Response, ApiError> {
    serve(state, parts, &import, &locator, &format!("subs/{subtitle}")).await
}
async fn overlay_manifest(
    State(state): State<AppState>,
    Path((import, locator, index)): Path<(String, String, String)>,
    parts: Parts,
) -> Result<Response, ApiError> {
    serve(
        state,
        parts,
        &import,
        &locator,
        &format!("subs/{index}/overlay.json"),
    )
    .await
}
async fn overlay_object(
    State(state): State<AppState>,
    Path((import, locator, index, generation, object)): Path<(
        String,
        String,
        String,
        String,
        String,
    )>,
    parts: Parts,
) -> Result<Response, ApiError> {
    serve(
        state,
        parts,
        &import,
        &locator,
        &format!("subs/{index}/overlay/{generation}/objects/{object}"),
    )
    .await
}
async fn chapter_thumb(
    State(state): State<AppState>,
    Path((import, locator, index)): Path<(String, String, String)>,
    parts: Parts,
) -> Result<Response, ApiError> {
    serve(
        state,
        parts,
        &import,
        &locator,
        &format!("chapters/{index}/thumb"),
    )
    .await
}

/// The closed query: at most one `token` (an element that cannot set headers)
/// and at most one canonical `session`. Anything else is refused, never
/// ignored, so no other key can look like authority.
fn session_query(query: Option<&str>) -> Result<Option<Uuid>, ApiError> {
    let mut session = None;
    let mut token = false;
    for pair in query
        .unwrap_or("")
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        match pair.split_once('=') {
            Some(("token", _)) if !token => token = true,
            Some(("session", value)) if session.is_none() => {
                let id = Uuid::parse_str(value).map_err(|_| invalid())?;
                if id.is_nil() || id.to_string() != value {
                    return Err(invalid());
                }
                session = Some(id);
            }
            _ => return Err(invalid()),
        }
    }
    Ok(session)
}

/// The viewer and login a request reaches the file as: the session's own when
/// it carries an exact binding (an account, if also present, must be that
/// session's viewer), otherwise the signed-in account's.
async fn caller(
    state: &AppState,
    parts: &mut Parts,
    session: Option<Uuid>,
    reference: &plurx_core::sharing_file_locators::FileLocatorReference,
) -> Result<(i64, String), ApiError> {
    let account = match RawToken::from_request_parts(parts, state).await {
        Ok(RawToken(token)) => {
            let AuthUser(user) = AuthUser::from_request_parts(parts, state).await?;
            Some((user.id, plurx_core::auth::hash_token(&token)))
        }
        Err(_) => None,
    };
    match (session, account) {
        (Some(session), account) => {
            let actor = state
                .sharing
                .receiver_starts
                .by_session(session)
                .ok_or_else(binding_refused)?;
            let (user, login_hash) = actor
                .bound_file_viewer(state, reference)
                .await
                .map_err(|_| binding_refused())?;
            if account.is_some_and(|(id, _)| id != user) {
                return Err(binding_refused());
            }
            Ok((user, login_hash))
        }
        (None, Some(account)) => Ok(account),
        (None, None) => Err(ApiError::Unauthorized),
    }
}

async fn serve(
    state: AppState,
    mut parts: Parts,
    import: &str,
    locator: &str,
    suffix: &str,
) -> Result<Response, ApiError> {
    let resource = SharingFileResource::parse(suffix).map_err(|_| invalid())?;
    let manifest = match resource.kind() {
        SharingFileResourceKind::SubtitleManifest { .. } => true,
        SharingFileResourceKind::Subtitle { .. }
        | SharingFileResourceKind::SubtitleObject { .. }
        | SharingFileResourceKind::ChapterThumbnail { .. } => false,
        _ => return Err(resource_unsupported().await),
    };
    let session = session_query(parts.uri.query())?;
    let import_id = Uuid::parse_str(import).map_err(|_| invalid())?;
    if import_id.is_nil() || import_id.to_string() != import {
        return Err(invalid());
    }
    let stored = state
        .store
        .sharing_import(import_id)
        .await?
        .ok_or_else(unavailable)?;
    if stored.summary.state != "active" || !crate::sharing::enabled(state.store.as_ref()).await? {
        return Err(unavailable());
    }
    let key = super::shared_artwork::receiver_key(&state)
        .await?
        .ok_or_else(unavailable)?;
    let reference = key
        .verify(locator, import_id, stored.summary.lifecycle_generation)
        .map_err(|_| invalid())?;
    let (user, login_hash) = caller(&state, &mut parts, session, &reference).await?;
    let _permit = ASSETS.clone().try_acquire_owned().map_err(|_| capacity())?;
    let target = SourcePlaybackTarget {
        server_id: reference.item.server_id,
        catalogue_epoch: reference.item.catalogue_epoch,
        library_id: reference.item.library_id.clone(),
        item_id: reference.item.item_id.clone(),
        file_id: reference.file_id.clone(),
        revision: reference.revision.clone(),
    };
    let (summary, asset) = state
        .sharing
        .read_file_asset(&state, user, &reference, &target, &resource)
        .await
        .map_err(source_failure)?;
    let mut response = match asset {
        PeerFileAsset::Preparing => preparing(),
        PeerFileAsset::Ready { bytes, mime } => {
            let bytes = if manifest {
                project_manifest(&bytes, &reference, &key)?.into()
            } else {
                bytes
            };
            let mut response = (StatusCode::OK, bytes).into_response();
            response
                .headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
            response
        }
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    shared_library::attach_receiver_file_authority(
        &state,
        &login_hash,
        user,
        &summary,
        &reference,
        &mut response,
    )
    .await?;
    Ok(response)
}

/// The Source manifest keeps its generation, cue geometry, source-time
/// intervals and relative object names (beneath this B file base); its
/// numeric file ID becomes the canonical string with the full reference.
pub(crate) fn project_manifest(
    bytes: &[u8],
    reference: &plurx_core::sharing_file_locators::FileLocatorReference,
    key: &plurx_core::sharing_file_locators::FileLocatorKey,
) -> Result<Vec<u8>, ApiError> {
    let manifest: crate::pgs_overlay::OverlayManifest =
        serde_json::from_slice(bytes).map_err(|_| unavailable())?;
    let projected = super::sharing_playback_wire::project_shared_overlay(manifest, reference, key)
        .map_err(|_| unavailable())?;
    serde_json::to_vec(&projected).map_err(|_| unavailable())
}

fn preparing() -> Response {
    let mut response = (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({"state": "preparing", "retry_after_ms": 1000})),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use plurx_core::{
        secrets::SharingSecretPurpose,
        sharing::*,
        sharing_catalogue::SharedReference,
        sharing_catalogue_details::FileRevision,
        sharing_file_locators::{FileLocatorKey, FileLocatorReference},
        sharing_receiver_sessions::{
            ReceiverProducerKind, ReceiverSessionIntent, RemoteSourceRecipe,
        },
        store::{sharing_catalogue::ReceiverCatalogueScope, SqliteStore},
    };
    use tower::ServiceExt;

    const LOCAL_VTT: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nLocal caption\n";

    struct ReceiverFixture {
        _directory: tempfile::TempDir,
        state: AppState,
        token: String,
        key: FileLocatorKey,
        reference: FileLocatorReference,
        base: String,
    }

    /// An active B import of a Source whose endpoint is never dialled here,
    /// a viewer login not assigned to the shared library, a signed locator
    /// for Source file 0, and a Local file that is also numbered 0 and has
    /// its own text track.
    async fn receiver_fixture() -> ReceiverFixture {
        let directory = tempfile::tempdir().expect("B fixture");
        let path = directory.path().join("receiver.sqlite");
        let store: Arc<dyn plurx_core::store::Store> =
            Arc::new(SqliteStore::open(&path).expect("B store"));
        let (_, mut state) = super::super::tests::test_app_with_state();
        state.store = store.clone();
        state.catalogue = plurx_core::store::CatalogueReader::authority(store);
        state
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
            .await
            .expect("enable B");
        let user = state
            .store
            .create_user("receiver-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let token = "receiver-asset-login".to_owned();
        state
            .store
            .create_token(&plurx_core::auth::hash_token(&token), user.id, None)
            .await
            .expect("login");
        let local = state
            .store
            .sharing_identity(1000)
            .await
            .expect("B identity");
        let remote = SharingIdentity {
            server_id: Uuid::new_v4(),
            catalogue_epoch: Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let import = Uuid::new_v4();
        let credential = crate::sharing::ImportCredential::encode(
            &new_secret().expect("grant"),
            Uuid::new_v4(),
            1000,
            "Fixture B",
            2000,
            None,
        )
        .expect("credential");
        assert_eq!(
            state
                .store
                .create_share_import(NewImport {
                    id: import,
                    source: remote.clone(),
                    source_name: "Fixture Source".into(),
                    claim_id: Uuid::new_v4(),
                    credential: state
                        .sharing
                        .key
                        .seal_sharing(
                            SharingSecretPurpose::Credential,
                            local.server_id,
                            import,
                            credential.expose()
                        )
                        .expect("seal credential"),
                    claim_secret: state
                        .sharing
                        .key
                        .seal_sharing(
                            SharingSecretPurpose::Claim,
                            local.server_id,
                            import,
                            "synthetic-claim"
                        )
                        .expect("seal claim"),
                    endpoints: vec![Endpoint {
                        ipv4: "100.64.0.1".parse().expect("address"),
                        ipv6: None,
                        ts_fqdn: "source.fixture.ts.net".into(),
                        port: 32443,
                        spki_sha256: "a".repeat(64),
                    }],
                    now_ms: 1000,
                })
                .await
                .expect("B import"),
            ImportOutcome::Created
        );
        state
            .store
            .settle_share_claim(import, Uuid::new_v4(), true, 1001)
            .await
            .expect("active import");
        let envelope = FileLocatorKey::generate_sealed(&state.sharing.key, &local).expect("B key");
        let writer = rusqlite::Connection::open(&path).expect("B writer");
        writer
            .execute_batch(
                plurx_core::store::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA,
            )
            .expect("B key schema");
        writer
            .execute(
                "INSERT INTO sharing_file_locator_keys VALUES(1,?1,?2,?3)",
                rusqlite::params![
                    local.server_id.to_string(),
                    local.catalogue_epoch.to_string(),
                    envelope.as_stored()
                ],
            )
            .expect("B key row");
        // The colliding Local file: numbered 0, with a downloaded text track.
        let library = state
            .store
            .create_library(&plurx_core::domain::NewLibrary {
                name: "Local".into(),
                kind: plurx_core::domain::LibraryKind::Movies,
                paths: vec![directory.path().join("local")],
                anime: false,
            })
            .await
            .expect("Local library")
            .id;
        let item = state
            .store
            .insert_item(&plurx_core::domain::NewItem {
                library_id: library,
                kind: plurx_core::domain::ItemKind::Movie,
                parent_id: None,
                title: "Local film".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("Local item");
        let downloaded = serde_json::json!([{
            "source_size":5,"source_mtime":1000,"provider_file_id":1,"language":"en",
            "title":"Local","hearing_impaired":false,"forced":false,"vtt":LOCAL_VTT
        }]);
        writer.execute("INSERT INTO files(id,item_id,path,size,mtime,duration_ms,container,subtitle_streams,downloaded_subtitles) VALUES(0,?1,'/nonexistent/local.mkv',5,1000,60000,'matroska','[]',?2)",rusqlite::params![item,downloaded.to_string()]).expect("Local file zero");
        drop(writer);
        let key = FileLocatorKey::open(&state.sharing.key, &local, &envelope).expect("B signer");
        let reference = FileLocatorReference {
            item: SharedReference {
                import_id: import,
                server_id: remote.server_id,
                catalogue_epoch: remote.catalogue_epoch,
                library_id: SourceId::parse("1").expect("library"),
                item_id: SourceId::parse("1").expect("item"),
            },
            lifecycle_generation: 1,
            file_id: SourceId::parse("0").expect("file"),
            revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
        };
        let base = key.issue(&reference).expect("locator").file_base();
        ReceiverFixture {
            _directory: directory,
            state,
            token,
            key,
            reference,
            base,
        }
    }

    async fn call(
        state: &AppState,
        method: &str,
        uri: &str,
        token: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut request = axum::http::Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = super::super::router(state.clone())
            .oneshot(request.body(Body::empty()).expect("request"))
            .await
            .expect("response");
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024)
            .await
            .expect("bounded body");
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                serde_json::Value::String(String::from_utf8_lossy(&bytes).into_owned())
            }),
        )
    }

    #[tokio::test]
    async fn sharing_receiver_unserved_file_suffixes_refuse_typed_without_local_lookup() {
        let fixture = receiver_fixture().await;
        let unknown = format!("/api/v1/shared/imports/{}/files/unsigned", Uuid::new_v4());
        for base in [fixture.base.as_str(), unknown.as_str()] {
            for suffix in ["stream.mp4", "stream.mp4?audio=1"] {
                for method in ["GET", "HEAD", "POST"] {
                    let uri = format!("{base}/{suffix}");
                    let (status, body) = call(&fixture.state, method, &uri, None).await;
                    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{method} {uri}");
                    if method != "HEAD" {
                        assert_eq!(body["code"], "sharing_resource_unsupported", "{uri}");
                    }
                    let (status, _) =
                        call(&fixture.state, method, &uri, Some(&fixture.token)).await;
                    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{method} {uri}");
                }
            }
        }
        // The same number names a real Local file that only a login reaches.
        assert_eq!(
            call(&fixture.state, "GET", "/api/v1/files/0/stream.mp4", None)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn sharing_receiver_presession_asset_requires_authuser_or_exact_session() {
        let fixture = receiver_fixture().await;
        let vtt = format!("{}/subs/0.vtt", fixture.base);
        let thumb = format!("{}/chapters/1/thumb", fixture.base);
        for uri in [&vtt, &thumb] {
            assert_eq!(
                call(&fixture.state, "GET", uri, None).await.0,
                StatusCode::UNAUTHORIZED,
                "{uri}"
            );
            assert_eq!(
                call(&fixture.state, "GET", uri, Some("not-a-login"))
                    .await
                    .0,
                StatusCode::UNAUTHORIZED,
                "{uri}"
            );
            let session = Uuid::new_v4();
            for (query, token, expected) in [
                (format!("session={session}"), None, StatusCode::FORBIDDEN),
                (
                    format!("session={session}"),
                    Some(fixture.token.as_str()),
                    StatusCode::FORBIDDEN,
                ),
                (
                    format!("session={}", session.to_string().to_uppercase()),
                    None,
                    StatusCode::BAD_REQUEST,
                ),
                (
                    format!("session={session}&session={session}"),
                    None,
                    StatusCode::BAD_REQUEST,
                ),
                (
                    "v=1".to_owned(),
                    Some(fixture.token.as_str()),
                    StatusCode::BAD_REQUEST,
                ),
            ] {
                let (status, body) =
                    call(&fixture.state, "GET", &format!("{uri}?{query}"), token).await;
                assert_eq!(status, expected, "{uri}?{query}");
                if expected == StatusCode::FORBIDDEN {
                    assert_eq!(body["code"], "sharing_asset_binding_refused");
                }
            }
            // A signed-in viewer passes the gate; this one is not assigned to
            // the shared library, so the Source read is refused before any
            // dial and nothing is served.
            for (uri, token) in [
                (uri.clone(), Some(fixture.token.as_str())),
                (format!("{uri}?token={}", fixture.token), None),
            ] {
                let (status, body) = call(&fixture.state, "GET", &uri, token).await;
                assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{uri}");
                assert_eq!(body["code"], "sharing_asset_unavailable", "{uri}");
            }
        }
        let tampered = format!("{}x/subs/0.vtt", fixture.base);
        assert_eq!(
            call(&fixture.state, "GET", &tampered, Some(&fixture.token))
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
        // Asset routes serve only the closed asset grammar.
        for suffix in ["subs/4096.vtt", "subs/x", "chapters/1/thumb.jpg"] {
            let uri = format!("{}/{suffix}", fixture.base);
            assert_ne!(
                call(&fixture.state, "GET", &uri, Some(&fixture.token))
                    .await
                    .0,
                StatusCode::OK,
                "{uri}"
            );
        }
    }

    fn intent(reference: &FileLocatorReference) -> ReceiverSessionIntent {
        ReceiverSessionIntent {
            scope: ReceiverCatalogueScope {
                import_id: reference.item.import_id,
                source_server_id: reference.item.server_id,
                catalogue_epoch: reference.item.catalogue_epoch,
                lifecycle_generation: reference.lifecycle_generation,
                assignment_generation: 1,
                endpoint_generation: 1,
                claim_id: Uuid::new_v4(),
                remote_grant_id: Uuid::new_v4(),
                libraries: vec![reference.item.library_id.clone()],
            },
            user_id: 7,
            login_hash: "b".repeat(64),
            recipe: RemoteSourceRecipe {
                kind: ReceiverProducerKind::RemoteSource,
                version: 1,
                reference: reference.item.clone(),
                lifecycle_generation: reference.lifecycle_generation,
                file_id: reference.file_id.clone(),
                file_revision: reference.revision.clone(),
                source_request_id: Uuid::new_v4(),
                parent_login_hash: "b".repeat(64),
                request_json: "{}".into(),
            },
            source_position_ms: 0,
        }
    }

    #[tokio::test]
    async fn sharing_receiver_asset_session_for_other_file_refused() {
        use super::super::shared_receiver_playback::recipe_binds_file;
        let fixture = receiver_fixture().await;
        let reference = fixture.reference.clone();
        assert!(recipe_binds_file(&intent(&reference), &reference));
        let mut others = Vec::new();
        let mut other = reference.clone();
        other.file_id = SourceId::parse("1").expect("other file");
        others.push(other);
        let mut other = reference.clone();
        other.revision = FileRevision::parse(&"c".repeat(64)).expect("other revision");
        others.push(other);
        let mut other = reference.clone();
        other.lifecycle_generation = 2;
        others.push(other);
        let mut other = reference.clone();
        other.item.item_id = SourceId::parse("2").expect("other item");
        others.push(other);
        let mut other = reference.clone();
        other.item.import_id = Uuid::new_v4();
        others.push(other);
        for other in &others {
            // A session playing another file binds none of this file's assets,
            // and this file's session binds none of another's.
            assert!(!recipe_binds_file(&intent(other), &reference));
            assert!(!recipe_binds_file(&intent(&reference), other));
        }
        // On the router, an unknown or foreign session is the same refusal.
        let uri = format!("{}/subs/0.vtt?session={}", fixture.base, Uuid::new_v4());
        assert_eq!(
            call(&fixture.state, "GET", &uri, Some(&fixture.token))
                .await
                .0,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn sharing_receiver_asset_numeric_collision_reaches_source() {
        use super::super::shared_source_assets::tests::{source_fixture, vtt_suffix, SOURCE_VTT};
        let fixture = receiver_fixture().await;
        // The Local file 0 is real and has its own caption...
        let local = format!("/api/v1/files/0/subs/0.vtt?token={}", fixture.token);
        let (status, body) = call(&fixture.state, "GET", &local, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, serde_json::Value::String(LOCAL_VTT.into()));
        // ...and the shared path naming Source file 0 never serves it.
        let shared = format!("{}/subs/0.vtt", fixture.base);
        let (status, body) = call(&fixture.state, "GET", &shared, Some(&fixture.token)).await;
        assert_ne!(status, StatusCode::OK);
        assert!(!body.to_string().contains("Local caption"));
        // The file number goes to the Source only: the real Source router
        // answers it from its own catalogue under the full reference.
        let source = source_fixture().await;
        let app = super::super::sharing::peer_router(source.state.clone());
        let (client, server) = tokio::io::duplex(64 * 1024);
        let server = tokio::spawn(async move {
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(
                    hyper_util::rt::TokioIo::new(server),
                    hyper_util::service::TowerToHyperService::new(app),
                )
                .await;
        });
        let mut peer = crate::sharing_client::PeerConnection::over_test_stream(client)
            .await
            .expect("fixture connection");
        let resource = SharingFileResource::parse(&vtt_suffix()).expect("resource");
        let Ok(PeerFileAsset::Ready { bytes, mime }) = peer
            .file_asset(&source.secret, &source.target, &resource)
            .await
        else {
            panic!("the Source answered its own file")
        };
        assert_eq!(mime, "text/vtt; charset=utf-8");
        assert_eq!(bytes, SOURCE_VTT.as_bytes());
        let mut stale = source.target.clone();
        stale.revision = FileRevision::parse(&"0".repeat(64)).expect("revision");
        assert!(matches!(
            peer.file_asset(&source.secret, &stale, &resource).await,
            Err(PeerError::Rejected(StatusCode::SERVICE_UNAVAILABLE))
        ));
        drop(peer);
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn sharing_receiver_pgs_manifest_generation_preserved() {
        use crate::pgs_overlay::{OverlayCue, OverlayManifest, OverlayObject};
        let fixture = receiver_fixture().await;
        let generation = "e".repeat(64);
        let image = format!("overlay/{generation}/objects/{}.png", "f".repeat(64));
        let manifest = OverlayManifest {
            schema: crate::pgs_overlay::SCHEMA,
            generation: generation.clone(),
            file_id: 0,
            track_index: 2,
            kind: "pgs".into(),
            timebase: "source_ms".into(),
            duration_ms: 60_000,
            cues: vec![OverlayCue {
                id: "cue-1".into(),
                start_ms: 1000,
                end_ms: 2500,
                canvas_width: 1920,
                canvas_height: 1080,
                objects: vec![OverlayObject {
                    image: image.clone(),
                    x: 100,
                    y: 900,
                    width: 400,
                    height: 80,
                }],
            }],
        };
        let source_bytes = serde_json::to_vec(&manifest).expect("Source manifest");
        let projected: serde_json::Value = serde_json::from_slice(
            &project_manifest(&source_bytes, &fixture.reference, &fixture.key).expect("projection"),
        )
        .expect("B manifest");
        assert_eq!(projected["generation"], generation);
        assert_eq!(projected["track_index"], 2);
        assert_eq!(projected["file_id"], "0");
        assert_eq!(projected["cues"][0]["objects"][0]["image"], image);
        assert_eq!(projected["cues"][0]["start_ms"], 1000);
        assert_eq!(projected["cues"][0]["end_ms"], 2500);
        assert_eq!(
            projected["reference"]["revision"],
            serde_json::to_value(&fixture.reference.revision).expect("revision")
        );
        // The relative object resolves beneath this B file base, inside the
        // asset grammar B serves for that same track.
        let object = format!("{}/subs/2/{image}", fixture.base);
        let suffix = object
            .strip_prefix(&format!("{}/", fixture.base))
            .expect("beneath base");
        assert!(matches!(
            SharingFileResource::parse(suffix).expect("grammar").kind(),
            SharingFileResourceKind::SubtitleObject { index: 2 }
        ));
        let mut other_file = manifest.clone();
        other_file.file_id = 1;
        let mut other_generation = manifest.clone();
        other_generation.cues[0].objects[0].image =
            format!("overlay/{}/objects/{}.png", "c".repeat(64), "f".repeat(64));
        for wrong in [other_file, other_generation] {
            assert!(project_manifest(
                &serde_json::to_vec(&wrong).expect("wire"),
                &fixture.reference,
                &fixture.key
            )
            .is_err());
        }
        assert!(project_manifest(b"not json", &fixture.reference, &fixture.key).is_err());
    }
}
