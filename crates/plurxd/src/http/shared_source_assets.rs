//! Private Source pre-session file assets: one text track as WebVTT, the PGS
//! overlay manifest and its immutable objects, and chapter thumbnails.
//!
//! Each read checks the same current grant/item/file/revision authority as a
//! Source decision and allocates no media session. The work behind it is the
//! Local per-file work, unchanged: subtitle extraction and overlay preparation
//! run in their own owned flights and publish into the Local caches, so a
//! request that times out leaves a result the retry reads; a chapter
//! extraction is bounded by its permit wait plus extraction timeout, inside
//! this route's deadline. Bodies are complete in memory when the handler
//! returns, so `source_content_guard` is their authority point.
use super::{
    chapter_thumbs, error::ApiError, hls::SourcePlaybackTarget, pgs_overlay, shared_library,
    shared_playback, stream,
};
use crate::state::AppState;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
    routing::get,
    Router,
};
use plurx_core::{
    sharing::SourceId,
    sharing_resources::{SharingFileResource, SharingFileResourceKind},
};
use std::sync::{Arc, LazyLock};

/// The complete expected Source file identity travels in this one bounded
/// request header; the path names only the file-relative resource.
pub(crate) const REFERENCE_HEADER: &str = "cinemashare-reference";
pub(crate) const SUBTITLE_REVISION_HEADER: &str = "cinemashare-subtitle-revision";
pub(crate) const MAX_REFERENCE_BYTES: usize = 1024;
/// Peer asset reads in flight on this Source. A refusal is a 429 the receiver
/// reports as capacity; nothing queues behind it.
static ASSETS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(16)));

/// Every asset route; the peer guard bounds each at the 30 s resource deadline.
pub(crate) const ROUTES: [&str; 4] = [
    "/sharing/v1/items/{item}/files/{file}/subs/{subtitle}",
    "/sharing/v1/items/{item}/files/{file}/subs/{index}/overlay.json",
    "/sharing/v1/items/{item}/files/{file}/subs/{index}/overlay/{generation}/objects/{object}",
    "/sharing/v1/items/{item}/files/{file}/chapters/{index}/thumb",
];

pub(crate) fn peer_router(state: AppState) -> Router<AppState> {
    let [subs, manifest, object, chapter] = ROUTES;
    Router::new()
        .route(subs, get(subtitle))
        .route(manifest, get(overlay_manifest))
        .route(object, get(overlay_object))
        .route(chapter, get(chapter_thumb))
        .route_layer(axum::middleware::from_fn_with_state(
            state,
            shared_library::source_content_guard,
        ))
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
        "Shared asset authority is unavailable",
    )
}

/// The exact single header value a receiver sends for `target`.
pub(crate) fn encode_reference(target: &SourcePlaybackTarget) -> Option<String> {
    serde_json::to_string(target)
        .ok()
        .filter(|value| value.len() <= MAX_REFERENCE_BYTES)
}

fn reference(headers: &HeaderMap) -> Result<SourcePlaybackTarget, ApiError> {
    let mut values = headers.get_all(REFERENCE_HEADER).iter();
    let (Some(value), None) = (values.next(), values.next()) else {
        return Err(invalid());
    };
    if value.len() > MAX_REFERENCE_BYTES {
        return Err(invalid());
    }
    serde_json::from_slice(value.as_bytes()).map_err(|_| invalid())
}

async fn subtitle(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((item, file, subtitle)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    let mut values = headers.get_all(SUBTITLE_REVISION_HEADER).iter();
    let revision = match (values.next(), values.next()) {
        (None, None) => None,
        (Some(value), None) if value.len() <= 192 => Some(value.to_str().map_err(|_| invalid())?),
        _ => return Err(invalid()),
    };
    serve_with_revision(
        &state,
        &headers,
        &item,
        &file,
        &format!("subs/{subtitle}"),
        revision,
    )
    .await
}
async fn overlay_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((item, file, index)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    serve(
        &state,
        &headers,
        &item,
        &file,
        &format!("subs/{index}/overlay.json"),
    )
    .await
}
async fn overlay_object(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((item, file, index, generation, object)): Path<(String, String, String, String, String)>,
) -> Result<Response, ApiError> {
    serve(
        &state,
        &headers,
        &item,
        &file,
        &format!("subs/{index}/overlay/{generation}/objects/{object}"),
    )
    .await
}
async fn chapter_thumb(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((item, file, index)): Path<(String, String, String)>,
) -> Result<Response, ApiError> {
    serve(
        &state,
        &headers,
        &item,
        &file,
        &format!("chapters/{index}/thumb"),
    )
    .await
}

/// Authorize the complete reference, run the Local per-file work, then
/// re-observe authority before the response leaves. No receiver identifier is
/// ever looked up as a Local file before the witness admits it.
pub(super) async fn serve(
    state: &AppState,
    headers: &HeaderMap,
    item: &str,
    file: &str,
    suffix: &str,
) -> Result<Response, ApiError> {
    serve_with_revision(state, headers, item, file, suffix, None).await
}

async fn serve_with_revision(
    state: &AppState,
    headers: &HeaderMap,
    item: &str,
    file: &str,
    suffix: &str,
    revision: Option<&str>,
) -> Result<Response, ApiError> {
    let resource = SharingFileResource::parse(suffix).map_err(|_| invalid())?;
    let target = reference(headers)?;
    if SourceId::parse(item).map_err(|_| invalid())? != target.item_id
        || SourceId::parse(file).map_err(|_| invalid())? != target.file_id
    {
        return Err(invalid());
    }
    let _permit = ASSETS.clone().try_acquire_owned().map_err(|_| {
        ApiError::typed(
            StatusCode::TOO_MANY_REQUESTS,
            "sharing_asset_capacity",
            "Shared asset capacity is busy",
        )
    })?;
    let authority = shared_playback::source_file_authority(state, headers, &target).await?;
    let file_id = target
        .file_id
        .as_str()
        .parse::<i64>()
        .map_err(|_| invalid())?;
    let media = state
        .store
        .get_file(file_id)
        .await?
        .ok_or_else(unavailable)?;
    let mut response = match resource.kind() {
        SharingFileResourceKind::Subtitle { index } => {
            stream::subtitle_vtt_for_file_revision(state, &media, index.into(), revision).await?
        }
        SharingFileResourceKind::SubtitleManifest { index } => {
            pgs_overlay::manifest_for_file(state, &media, index.into()).await?
        }
        SharingFileResourceKind::SubtitleObject { index } => {
            let [_, _, _, generation, _, object] = suffix.split('/').collect::<Vec<_>>()[..] else {
                return Err(invalid());
            };
            pgs_overlay::object_for_file(state, &media, index.into(), generation, object).await?
        }
        SharingFileResourceKind::ChapterThumbnail { index } => {
            chapter_thumbs::serve_for_file(state, &media, index.into()).await?
        }
        SharingFileResourceKind::Decision
        | SharingFileResourceKind::Start
        | SharingFileResourceKind::Direct
        | SharingFileResourceKind::Progressive => return Err(invalid()),
    };
    authority.still_current(state, headers, &target).await?;
    shared_library::attach_source_file_authority(&mut response, authority.grant, &target);
    Ok(response)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        response::IntoResponse,
    };
    use plurx_core::{
        domain::{ItemKind, LibraryKind, NewItem, NewLibrary},
        sharing::*,
        sharing_catalogue_details::{CatalogueRevisionKey, FileRevision},
        store::SqliteStore,
    };

    /// The Source's own text track: a downloaded caption, appended after the
    /// three embedded tracks (text, PGS, PGS), so no FFmpeg is needed.
    pub(crate) const SOURCE_VTT: &str = "WEBVTT\n\n00:00:01.000 --> 00:00:03.000\nSource caption\n";
    pub(crate) const TEXT_TRACK: i64 = 3;
    pub(crate) const JPEG: &[u8] = b"\xff\xd8\xff\xe0source-thumb";
    pub(crate) const PNG: &[u8] = b"\x89PNG\r\n\x1a\nsource-object";

    pub(crate) struct SourceFixture {
        pub(crate) _directory: tempfile::TempDir,
        pub(crate) state: AppState,
        pub(crate) path: std::path::PathBuf,
        pub(crate) secret: plurx_core::secrets::Secret,
        pub(crate) private: i64,
        pub(crate) item: i64,
        pub(crate) target: SourcePlaybackTarget,
    }

    impl SourceFixture {
        pub(crate) fn headers(&self) -> HeaderMap {
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::AUTHORIZATION,
                format!("CinemaShare {}", self.secret.expose())
                    .parse()
                    .expect("credential header"),
            );
            headers.insert(
                REFERENCE_HEADER,
                encode_reference(&self.target)
                    .expect("bounded reference")
                    .parse()
                    .expect("reference header"),
            );
            headers
        }
        pub(crate) fn writer(&self) -> rusqlite::Connection {
            rusqlite::Connection::open(&self.path).expect("Source writer")
        }
        pub(crate) async fn file(&self) -> plurx_core::domain::MediaFile {
            self.state
                .store
                .get_file(0)
                .await
                .expect("file read")
                .expect("Source file zero")
        }
    }

    /// A Source with one granted and one private library, one shared item
    /// whose file is numbered 0, a sealed revision key and an approved grant.
    pub(crate) async fn source_fixture() -> SourceFixture {
        let directory = crate::test_tempdir().expect("Source fixture directory");
        let path = directory.path().join("source.sqlite");
        let store: Arc<dyn plurx_core::store::Store> =
            Arc::new(SqliteStore::open(&path).expect("Source store"));
        let (_, mut state) = super::super::tests::test_app_with_state();
        state.store = store.clone();
        state.catalogue = plurx_core::store::CatalogueReader::authority(store);
        state.subs_dir = directory.path().join("subs");
        state.runtime_cache_dir = directory.path().join("runtime");
        state
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
            .await
            .expect("enable sharing");
        let identity = state
            .store
            .sharing_identity(1000)
            .await
            .expect("Source identity");
        let mut libraries = Vec::new();
        for name in ["Shared", "Private"] {
            libraries.push(
                state
                    .store
                    .create_library(&NewLibrary {
                        name: name.into(),
                        kind: LibraryKind::Movies,
                        paths: vec![directory.path().join(name)],
                        anime: false,
                    })
                    .await
                    .expect("library")
                    .id,
            );
        }
        let writer = rusqlite::Connection::open(&path).expect("candidate writer");
        for schema in [
            plurx_core::store::sharing_catalogue_source::CANDIDATE_ITEM_IDENTITY_SCHEMA,
            plurx_core::store::sharing_catalogue_source::CANDIDATE_SCHEMA,
            plurx_core::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
        ] {
            writer.execute_batch(schema).expect("candidate schema");
        }
        let item = state
            .store
            .insert_item(&NewItem {
                library_id: libraries[0],
                kind: ItemKind::Movie,
                parent_id: None,
                title: "Shared film".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let invitation = uuid::Uuid::new_v4();
        let grant = uuid::Uuid::new_v4();
        let secret = new_secret().expect("grant secret");
        state
            .store
            .create_share_invitation(InvitationRecord {
                id: invitation,
                token_hash: "a".repeat(64),
                library_ids: vec![libraries[0]],
                created_at_ms: 1000,
                expires_at_ms: 2000,
            })
            .await
            .expect("invitation");
        state
            .store
            .claim_share(ShareClaim {
                invitation_id: invitation,
                invitation_hash: "a".repeat(64),
                claim_id: uuid::Uuid::new_v4(),
                grant_id: grant,
                recipient_server_id: uuid::Uuid::new_v4(),
                recipient_name: "Recipient".into(),
                credential_hash: secret_hash(SecretDomain::Grant, &secret),
                now_ms: 1001,
            })
            .await
            .expect("claim");
        state
            .store
            .approve_share(grant, 1, 1002)
            .await
            .expect("approval");
        let envelope = CatalogueRevisionKey::generate_sealed(&state.sharing.key, identity.clone())
            .expect("revision key");
        let key = CatalogueRevisionKey::open(&state.sharing.key, identity.clone(), &envelope)
            .expect("revision signer");
        writer
            .execute(
                "INSERT INTO sharing_catalogue_keys VALUES(1,?1,?2,?3)",
                rusqlite::params![
                    identity.server_id.to_string(),
                    identity.catalogue_epoch.to_string(),
                    envelope.as_stored()
                ],
            )
            .expect("revision key row");
        let media = directory.path().join("film.mkv");
        std::fs::write(&media, b"Source presence").expect("Source file");
        let embedded = serde_json::json!([
            {"index":0,"codec":"subrip","default":false,"forced":false},
            {"index":1,"codec":"hdmv_pgs_subtitle","default":false,"forced":false},
            {"index":2,"codec":"hdmv_pgs_subtitle","default":false,"forced":false}
        ]);
        let downloaded = serde_json::json!([{
            "source_size":15,"source_mtime":1000,"provider_file_id":1,"language":"en",
            "title":"Downloaded","hearing_impaired":false,"forced":false,"vtt":SOURCE_VTT
        }]);
        let probe = serde_json::json!({"chapters":[
            {"id":0,"start_time":"0.000000","end_time":"30.000000"},
            {"id":1,"start_time":"30.000000","end_time":"60.000000"}
        ]});
        writer.execute("INSERT INTO files(id,item_id,path,size,mtime,duration_ms,container,video_codec,width,height,bit_depth,bitrate,subtitle_streams,downloaded_subtitles,probe_json) VALUES(0,?1,?2,15,1000,60000,'matroska','h264',1920,1080,8,1000000,?3,?4,?5)",rusqlite::params![item,media.to_str().expect("path"),embedded.to_string(),downloaded.to_string(),probe.to_string()]).expect("Source file zero");
        drop(writer);
        let hash = secret_hash(SecretDomain::Grant, &secret);
        let plurx_core::store::sharing_catalogue_details::SourceDetailsRead::Authorized(witness) =
            state
                .store
                .source_item_file_witness(
                    &hash,
                    grant,
                    SourceId::parse(&item.to_string()).expect("item"),
                    SourceId::parse("0").expect("file"),
                )
                .await
                .expect("witness")
        else {
            panic!("authorized witness")
        };
        let target = SourcePlaybackTarget {
            server_id: identity.server_id,
            catalogue_epoch: identity.catalogue_epoch,
            library_id: SourceId::parse(&libraries[0].to_string()).expect("library"),
            item_id: SourceId::parse(&item.to_string()).expect("item"),
            file_id: SourceId::parse("0").expect("file"),
            revision: key.file_revision(&witness).expect("revision"),
        };
        SourceFixture {
            _directory: directory,
            state,
            path,
            secret,
            private: libraries[1],
            item,
            target,
        }
    }

    pub(crate) fn vtt_suffix() -> String {
        format!("subs/{TEXT_TRACK}.vtt")
    }

    async fn call(fixture: &SourceFixture, headers: &HeaderMap, suffix: &str) -> Response {
        let item = fixture.target.item_id.as_str().to_owned();
        let file = fixture.target.file_id.as_str().to_owned();
        match serve(&fixture.state, headers, &item, &file, suffix).await {
            Ok(response) => response,
            Err(error) => error.into_response(),
        }
    }
    async fn body(response: Response) -> Vec<u8> {
        to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .expect("bounded body")
            .to_vec()
    }
    /// The guard is the response's authority point: an in-memory asset body
    /// leaves only while its attached grant/item/file scope is still current.
    async fn guarded(fixture: &SourceFixture, response: Response) -> Response {
        shared_library::guard_source_response(fixture.state.clone(), None, response).await
    }

    #[tokio::test]
    async fn sharing_source_asset_requires_current_grant_item_file_revision() {
        let fixture = source_fixture().await;
        let response = guarded(
            &fixture,
            call(&fixture, &fixture.headers(), &vtt_suffix()).await,
        )
        .await;
        let status = response.status();
        if status != StatusCode::OK {
            panic!(
                "asset response {status}: {}",
                String::from_utf8_lossy(&body(response).await)
            );
        }
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "text/vtt; charset=utf-8"
        );
        assert_eq!(body(response).await, SOURCE_VTT.as_bytes());

        let mut no_grant = fixture.headers();
        no_grant.remove(axum::http::header::AUTHORIZATION);
        assert_eq!(
            call(&fixture, &no_grant, &vtt_suffix()).await.status(),
            StatusCode::UNAUTHORIZED
        );
        let mut unknown_grant = fixture.headers();
        unknown_grant.insert(
            axum::http::header::AUTHORIZATION,
            format!("CinemaShare {}", new_secret().expect("other").expose())
                .parse()
                .expect("header"),
        );
        assert_eq!(
            call(&fixture, &unknown_grant, &vtt_suffix()).await.status(),
            StatusCode::NOT_FOUND
        );
        let mut missing = fixture.headers();
        missing.remove(REFERENCE_HEADER);
        assert_eq!(
            call(&fixture, &missing, &vtt_suffix()).await.status(),
            StatusCode::BAD_REQUEST
        );
        let mut duplicated = fixture.headers();
        duplicated.append(
            REFERENCE_HEADER,
            encode_reference(&fixture.target)
                .expect("reference")
                .parse()
                .expect("header"),
        );
        assert_eq!(
            call(&fixture, &duplicated, &vtt_suffix()).await.status(),
            StatusCode::BAD_REQUEST
        );
        // The path file must be the referenced file.
        let other = serve(
            &fixture.state,
            &fixture.headers(),
            fixture.target.item_id.as_str(),
            "1",
            &vtt_suffix(),
        )
        .await;
        assert_eq!(
            other
                .expect_err("path/reference mismatch")
                .into_response()
                .status(),
            StatusCode::BAD_REQUEST
        );
        for changed in [
            SourcePlaybackTarget {
                server_id: uuid::Uuid::new_v4(),
                ..fixture.target.clone()
            },
            SourcePlaybackTarget {
                catalogue_epoch: uuid::Uuid::new_v4(),
                ..fixture.target.clone()
            },
            SourcePlaybackTarget {
                library_id: SourceId::parse(&fixture.private.to_string()).expect("private"),
                ..fixture.target.clone()
            },
            SourcePlaybackTarget {
                revision: FileRevision::parse(&"0".repeat(64)).expect("revision"),
                ..fixture.target.clone()
            },
        ] {
            let mut headers = fixture.headers();
            headers.insert(
                REFERENCE_HEADER,
                encode_reference(&changed)
                    .expect("reference")
                    .parse()
                    .expect("header"),
            );
            assert_eq!(
                call(&fixture, &headers, &vtt_suffix()).await.status(),
                StatusCode::SERVICE_UNAVAILABLE
            );
        }
        // A changed file is a new revision; the old reference names nothing.
        fixture
            .writer()
            .execute("UPDATE files SET mtime=2000 WHERE id=0", [])
            .expect("change revision");
        assert_eq!(
            call(&fixture, &fixture.headers(), &vtt_suffix())
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        fixture
            .writer()
            .execute("UPDATE files SET mtime=1000 WHERE id=0", [])
            .expect("restore revision");
        fixture
            .state
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "false")
            .await
            .expect("disable sharing");
        assert_eq!(
            call(&fixture, &fixture.headers(), &vtt_suffix())
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn sharing_source_asset_library_removed_mid_request_refuses() {
        let fixture = source_fixture().await;
        let file = fixture.file().await;
        let thumbs = fixture
            .state
            .runtime_cache_dir
            .join("chapter-thumbs")
            .join(format!("0-{}-{}", file.size, file.mtime));
        std::fs::create_dir_all(&thumbs).expect("thumbnail cache");
        std::fs::write(thumbs.join("1.jpg"), JPEG).expect("cached thumbnail");
        for suffix in [vtt_suffix(), "chapters/1/thumb".to_owned()] {
            let response = call(&fixture, &fixture.headers(), &suffix).await;
            assert_eq!(response.status(), StatusCode::OK, "{suffix}");
            // The item leaves the granted library after the work and before
            // the body leaves: the attached file scope refuses it.
            fixture
                .writer()
                .execute(
                    "UPDATE items SET library_id=?1 WHERE id=?2",
                    rusqlite::params![fixture.private, fixture.item],
                )
                .expect("move outside grant");
            let refused = guarded(&fixture, response).await;
            assert_eq!(
                refused.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{suffix}"
            );
            assert!(!String::from_utf8_lossy(&body(refused).await).contains("caption"));
            assert_eq!(
                call(&fixture, &fixture.headers(), &suffix).await.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{suffix}: the next request is refused before any work"
            );
            let library = fixture
                .target
                .library_id
                .as_str()
                .parse::<i64>()
                .expect("id");
            fixture
                .writer()
                .execute(
                    "UPDATE items SET library_id=?1 WHERE id=?2",
                    rusqlite::params![library, fixture.item],
                )
                .expect("restore grant");
        }
        let chapter = guarded(
            &fixture,
            call(&fixture, &fixture.headers(), "chapters/1/thumb").await,
        )
        .await;
        assert_eq!(chapter.status(), StatusCode::OK);
        assert_eq!(
            chapter.headers()[axum::http::header::CONTENT_TYPE],
            "image/jpeg"
        );
        assert_eq!(body(chapter).await, JPEG);
    }

    #[tokio::test]
    async fn sharing_source_asset_refuses_bitmap_track() {
        let fixture = source_fixture().await;
        for track in [1, 2] {
            let response = call(&fixture, &fixture.headers(), &format!("subs/{track}.vtt")).await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "bitmap track {track}"
            );
        }
        assert!(
            !fixture.state.subs_dir.exists()
                || std::fs::read_dir(&fixture.state.subs_dir)
                    .expect("subs dir")
                    .next()
                    .is_none(),
            "a refused bitmap track starts no extraction"
        );
        assert_eq!(
            call(&fixture, &fixture.headers(), "subs/9.vtt")
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        // A text track is not a pgs-v1 overlay.
        fixture
            .state
            .store
            .put_setting(plurx_core::store::keys::PGS_OVERLAY, "true")
            .await
            .expect("overlay on");
        assert_eq!(
            call(&fixture, &fixture.headers(), "subs/0/overlay.json")
                .await
                .status(),
            StatusCode::UNSUPPORTED_MEDIA_TYPE
        );
        // Non-asset file suffixes are never served by this route family.
        for suffix in ["direct", "stream.mp4", "decision", "hls/sessions"] {
            assert_eq!(
                call(&fixture, &fixture.headers(), suffix).await.status(),
                StatusCode::BAD_REQUEST,
                "{suffix}"
            );
        }
    }

    #[tokio::test]
    async fn sharing_receiver_pgs_object_cross_track_refused() {
        let fixture = source_fixture().await;
        fixture
            .state
            .store
            .put_setting(plurx_core::store::keys::PGS_OVERLAY, "true")
            .await
            .expect("overlay on");
        let file = fixture.file().await;
        let hash = "b".repeat(64);
        let manifest = crate::pgs_overlay::manifest_path(&fixture.state.subs_dir, &file, 1);
        let object = crate::pgs_overlay::object_path(&fixture.state.subs_dir, &file, 1, &hash)
            .expect("object path");
        std::fs::create_dir_all(object.parent().expect("objects")).expect("generation");
        std::fs::write(&manifest, b"{}").expect("published manifest");
        std::fs::write(&object, PNG).expect("published object");
        let own = crate::pgs_overlay::generation(&file, 1);
        let other = crate::pgs_overlay::generation(&file, 2);
        assert_ne!(own, other);
        let response = guarded(
            &fixture,
            call(
                &fixture,
                &fixture.headers(),
                &format!("subs/1/overlay/{own}/objects/{hash}.png"),
            )
            .await,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[axum::http::header::CONTENT_TYPE],
            "image/png"
        );
        assert_eq!(body(response).await, PNG);
        // Track 1's generation named under track 2, and track 2's generation
        // under track 1, both name no object.
        for suffix in [
            format!("subs/2/overlay/{own}/objects/{hash}.png"),
            format!("subs/1/overlay/{other}/objects/{hash}.png"),
        ] {
            assert_eq!(
                call(&fixture, &fixture.headers(), &suffix).await.status(),
                StatusCode::NOT_FOUND,
                "{suffix}"
            );
        }
    }

    #[tokio::test]
    async fn sharing_source_asset_routes_through_the_peer_router_with_its_deadline_group() {
        use tower::ServiceExt;
        let fixture = source_fixture().await;
        let app = super::super::sharing::peer_router(fixture.state.clone());
        let mut request = axum::http::Request::builder()
            .uri(format!(
                "/sharing/v1/items/{}/files/0/{}",
                fixture.item,
                vtt_suffix()
            ))
            .body(Body::empty())
            .expect("request");
        *request.headers_mut() = fixture.headers();
        let response = app.clone().oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[axum::http::header::CACHE_CONTROL],
            "no-store"
        );
        assert_eq!(body(response).await, SOURCE_VTT.as_bytes());
        let mut head = axum::http::Request::builder()
            .method("HEAD")
            .uri(format!(
                "/sharing/v1/items/{}/files/0/chapters/5/thumb",
                fixture.item
            ))
            .body(Body::empty())
            .expect("request");
        *head.headers_mut() = fixture.headers();
        // HEAD is served by the GET route; a chapter the file lacks is a Local 404.
        assert_eq!(
            app.oneshot(head).await.expect("response").status(),
            StatusCode::NOT_FOUND
        );
    }
}
