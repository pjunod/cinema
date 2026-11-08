//! Bounded artwork operation and accepted-body memory ownership.
use crate::state::{clock_ms, AppState};
use axum::http::StatusCode;
use axum::{
    body::Body,
    extract::{Path, RawQuery, State},
    http::{header, HeaderMap, HeaderValue},
    response::Response,
};
use plurx_core::{
    sharing_artwork::{ArtVariant, SourceArtResource},
    sharing_catalogue_details::CatalogueRevisionKey,
    store::sharing_catalogue_artwork::SourceArtRead,
};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
const BYTE_KIB: usize = 64 * 1024;
// One extra byte detects a grown file without doubling Vec capacity.
const OBJECT_KIB: usize = plurx_core::sharing_artwork::MAX_ART_BYTES / 1024 + 1;
static SOURCE_OPERATIONS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));
static SOURCE_BYTES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(BYTE_KIB)));
static RECEIVER_OPERATIONS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(4)));
static RECEIVER_BYTES: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(BYTE_KIB)));
/// Owned by the response Bytes and any still-running blocking read or disk
/// writer. Body Drop alone cannot return capacity while DATA remains queued to
/// the transport; the queued Bytes keep their lease until the writer drops them.
pub(super) struct ArtBodyLease {
    _operation: OwnedSemaphorePermit,
    bytes: Mutex<OwnedSemaphorePermit>,
}
impl ArtBodyLease {
    pub(super) fn source(derivative: bool) -> Result<Arc<Self>, super::error::ApiError> {
        // Foreground derivatives may briefly own original plus variant. The
        // existing background worker retains its own CPU/memory settlement.
        Self::acquire(
            &SOURCE_OPERATIONS,
            &SOURCE_BYTES,
            OBJECT_KIB * if derivative { 2 } else { 1 },
        )
    }
    pub(super) fn source_collection_workspace(
    ) -> Result<OwnedSemaphorePermit, super::error::ApiError> {
        SOURCE_BYTES
            .clone()
            .try_acquire_many_owned(OBJECT_KIB as u32)
            .map_err(|_| {
                super::error::ApiError::typed(
                    StatusCode::TOO_MANY_REQUESTS,
                    "sharing_art_capacity",
                    "sharing art capacity",
                )
            })
    }
    pub(super) fn receiver() -> Result<Arc<Self>, super::error::ApiError> {
        Self::acquire(&RECEIVER_OPERATIONS, &RECEIVER_BYTES, OBJECT_KIB * 2)
    }
    fn acquire(
        operations: &Arc<Semaphore>,
        bytes: &Arc<Semaphore>,
        kib: usize,
    ) -> Result<Arc<Self>, super::error::ApiError> {
        let capacity = || {
            super::error::ApiError::typed(
                StatusCode::TOO_MANY_REQUESTS,
                "sharing_art_capacity",
                "sharing art capacity",
            )
        };
        let operation = operations
            .clone()
            .try_acquire_owned()
            .map_err(|_| capacity())?;
        let bytes = bytes
            .clone()
            .try_acquire_many_owned(kib as u32)
            .map_err(|_| capacity())?;
        Ok(Arc::new(Self {
            _operation: operation,
            bytes: Mutex::new(bytes),
        }))
    }
    pub(super) fn retain_body_bytes(&self, bytes: usize) -> Result<(), super::error::ApiError> {
        let invalid = || {
            super::error::ApiError::typed(
                StatusCode::SERVICE_UNAVAILABLE,
                "sharing_art_invalid_response",
                "sharing art invalid response",
            )
        };
        if bytes == 0 || bytes > plurx_core::sharing_artwork::MAX_ART_BYTES {
            return Err(invalid());
        }
        // Retain the bounded read sentinel as well as payload capacity.
        let keep = (bytes + 1).div_ceil(1024);
        let mut permit = self.bytes.lock().expect("art byte owner");
        let held = permit.num_permits();
        if keep > held {
            return Err(invalid());
        }
        if keep < held {
            drop(permit.split(held - keep).expect("bounded byte split"));
        }
        Ok(())
    }
}
/// How long a client is told to wait for a derivative the Source has queued
/// but not rendered. The durable artwork queue exposes no per-job completion
/// estimate, so this is a fixed bound: a W300/W500/W780 render of one poster is
/// well under a second of CPU on every node, and the worker is woken the moment
/// the demand is persisted, so two seconds covers the render plus the queue
/// pickup without parking a poster grid for long.
const VARIANT_RENDER_RETRY_AFTER_SECS: u64 = 2;
fn variant_busy() -> super::error::ApiError {
    super::error::ApiError::TypedRetry {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: "sharing_art_variant_busy",
        message: "sharing art variant busy".into(),
        retry_after_seconds: VARIANT_RENDER_RETRY_AFTER_SECS,
    }
}
/// The receiver relays a Source's typed wait to its own client, and records the
/// refusal; every other peer failure stays "unavailable".
fn peer_refusal(error: crate::sharing_client::PeerError) -> super::error::ApiError {
    tracing::warn!(
        target: "plurx::sharing",
        code = super::sharing::peer_error_code(&error),
        ?error,
        "shared artwork Source refused"
    );
    match error {
        crate::sharing_client::PeerError::Busy { retry_after_secs } => {
            super::error::ApiError::TypedRetry {
                status: StatusCode::TOO_MANY_REQUESTS,
                code: "sharing_art_source_busy",
                message: "sharing art source busy".into(),
                retry_after_seconds: retry_after_secs,
            }
        }
        _ => unavailable(),
    }
}
fn fail(status: StatusCode, code: &'static str) -> super::error::ApiError {
    super::error::ApiError::typed(status, code, code.replace('_', " "))
}
fn missing() -> super::error::ApiError {
    fail(StatusCode::NOT_FOUND, "sharing_art_not_found")
}
fn unavailable() -> super::error::ApiError {
    fail(StatusCode::SERVICE_UNAVAILABLE, "sharing_art_unavailable")
}
pub(super) fn asset_response(
    bytes: axum::body::Bytes,
    digest: [u8; 32],
    mime: &'static str,
    variant: ArtVariant,
    lease: Arc<ArtBodyLease>,
) -> Result<Response, super::error::ApiError> {
    lease.retain_body_bytes(bytes.len())?;
    let len = bytes.len();
    // The response bytes own the lease: Hyper and h2 hold these Bytes while
    // DATA is queued behind a blocked writer and drop them once written or
    // with the connection, so capacity returns exactly when they leave.
    let mut response = Response::new(Body::from(axum::body::Bytes::from_owner(LeasedArtBytes {
        bytes,
        _lease: lease,
    })));
    for (key, value) in [
        ("x-plurx-art-sha256", hex::encode(digest)),
        ("x-plurx-art-bytes", len.to_string()),
        ("x-plurx-art-variant", variant.label().to_owned()),
    ] {
        response.headers_mut().insert(
            key,
            HeaderValue::from_str(&value).map_err(|_| unavailable())?,
        );
    }
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&len.to_string()).map_err(|_| unavailable())?,
    );
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(mime));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response.headers_mut().insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store, no-transform"),
    );
    Ok(response)
}
pub(super) async fn source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(resource): Path<String>,
    RawQuery(raw): RawQuery,
) -> Result<Response, super::error::ApiError> {
    if raw.as_deref().is_some_and(|q| !q.is_empty()) {
        return Err(fail(StatusCode::BAD_REQUEST, "sharing_invalid_request"));
    }
    tokio::time::timeout(std::time::Duration::from_secs(5),async {
        if !crate::sharing::enabled(state.store.as_ref()).await.map_err(|_| unavailable())? {return Err(missing());}
        let (hash,grant)=super::shared_library::authority(&state,&headers).await?;
        let resource=SourceArtResource::parse(&resource).map_err(|_| missing())?;
        let reference=resource.reference_unverified().map_err(|_| missing())?;
        let SourceArtRead::Authorized(selection)=state.store.source_art_snapshot(&hash,grant,&reference).await.map_err(|_| unavailable())? else {return Err(missing());};
        let envelope=state.store.source_catalogue_revision_key(reference.server_id,reference.catalogue_epoch).await.map_err(|_| unavailable())?.ok_or_else(missing)?;
        let key=CatalogueRevisionKey::open(&state.sharing.key,selection.identity().clone(),&envelope).map_err(|_| unavailable())?;
        let reference=key.verify_art(&resource,grant,clock_ms()).map_err(|_| missing())?;
        let lease=ArtBodyLease::source(reference.variant!=ArtVariant::Original)?;
        let asset=super::images::shared_artwork_asset(&state,&selection,&hash,lease.clone()).await?;
        let Some(asset)=asset else {return Err(if reference.variant==ArtVariant::Original {missing()} else {variant_busy()});};
        let current=state.store.source_art_snapshot(&hash,grant,&reference).await.map_err(|_| unavailable())?;
        if !matches!(current,SourceArtRead::Authorized(ref snapshot) if selection.same_selection(snapshot)) || !asset.still_current().await
            || !super::shared_library::source_art_scope_current(&state,&reference).await
            || key.verify_art(&resource,grant,clock_ms()).is_err() {return Err(missing());}
        let mut response=asset_response(asset.bytes.clone(),asset.digest,asset.mime,reference.variant,lease)?;
        super::shared_library::attach_source_art_authority(&mut response,&reference);
        Ok(response)
    }).await.map_err(|_| unavailable())?
}

pub(super) async fn receiver(
    State(state): State<AppState>,
    super::extract::AuthUser(user): super::extract::AuthUser,
    super::extract::RawToken(token): super::extract::RawToken,
    Path((import, resource)): Path<(String, String)>,
    RawQuery(raw): RawQuery,
) -> Result<Response, super::error::ApiError> {
    if raw.as_deref().is_some_and(|q| !q.is_empty()) {
        return Err(fail(StatusCode::BAD_REQUEST, "sharing_invalid_request"));
    }
    let import_id = uuid::Uuid::parse_str(&import).map_err(|_| missing())?;
    if import_id.to_string() != import {
        return Err(missing());
    }
    tokio::time::timeout(std::time::Duration::from_secs(7), async {
        if !crate::sharing::enabled(state.store.as_ref())
            .await
            .map_err(|_| unavailable())?
        {
            return Err(missing());
        }
        let identity = state
            .store
            .sharing_identity(clock_ms())
            .await
            .map_err(|_| unavailable())?;
        let envelope = state
            .store
            .receiver_file_locator_key(identity.server_id, identity.catalogue_epoch)
            .await
            .map_err(|_| unavailable())?
            .ok_or_else(missing)?;
        let key = plurx_core::sharing_file_locators::FileLocatorKey::open(
            &state.sharing.key,
            &identity,
            &envelope,
        )
        .map_err(|_| unavailable())?;
        let reference = key
            .verify_art(&resource, import_id, user.id, clock_ms())
            .map_err(|_| missing())?;
        let lease = ArtBodyLease::receiver()?;
        // Authority first, with no peer call: the snapshot below may be served
        // only to a request that passes every check a live read would.
        let import = state
            .sharing
            .artwork_authority(&state, &reference)
            .await
            .map_err(peer_refusal)?;
        let summary = import.summary.clone();
        let (asset, from_peer) = match read_disk_snapshot(&state, &reference, lease.clone()).await {
            Some(asset) => (asset, false),
            None => (
                state
                    .sharing
                    .fetch_artwork(&state, &reference, &import)
                    .await
                    .map_err(peer_refusal)?,
                true,
            ),
        };
        key.verify_art(&resource, import_id, user.id, clock_ms())
            .map_err(|_| missing())?;
        let mut proof = Response::new(Body::empty());
        super::shared_library::attach_receiver_art_authority(
            &state, &token, user.id, &summary, &reference, &mut proof,
        )
        .await?;
        if from_peer {
            publish_disk_snapshot(&state, &reference, &asset, lease.clone()).await?;
        }
        let source = reference
            .source
            .reference_unverified()
            .map_err(|_| missing())?;
        let mut response =
            asset_response(asset.bytes, asset.digest, asset.mime, source.variant, lease)?;
        super::shared_library::attach_receiver_art_authority(
            &state,
            &token,
            user.id,
            &summary,
            &reference,
            &mut response,
        )
        .await?;
        Ok(response)
    })
    .await
    .map_err(|_| unavailable())?
}

pub(super) async fn source_records(
    state: &AppState,
    grant: uuid::Uuid,
    records: Vec<plurx_core::sharing_catalogue::SourceCatalogueRecord>,
) -> Result<Vec<plurx_core::sharing_catalogue::SourceCatalogueItem>, super::error::ApiError> {
    use plurx_core::sharing_artwork::{
        artwork_expiry, ArtKind, ArtVariant, SourceArtReference, SourceArtwork,
    };
    let identity = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(|_| unavailable())?;
    let key = state
        .store
        .source_catalogue_revision_key(identity.server_id, identity.catalogue_epoch)
        .await
        .map_err(|_| unavailable())?
        .map(|envelope| CatalogueRevisionKey::open(&state.sharing.key, identity.clone(), &envelope))
        .transpose()
        .map_err(|_| unavailable())?;
    let now = clock_ms();
    let mut items = Vec::with_capacity(records.len());
    for record in records {
        let mut item = record.item;
        item.art.clear();
        if let Some(key) = &key {
            for (kind, filename) in [
                (ArtKind::Poster, record.poster_filename),
                (ArtKind::Backdrop, record.backdrop_filename),
            ] {
                if filename.as_deref().is_none_or(|name| {
                    name.is_empty()
                        || name.starts_with('.')
                        || !name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                }) {
                    continue;
                }
                for variant in [
                    ArtVariant::Original,
                    ArtVariant::W300,
                    ArtVariant::W500,
                    ArtVariant::W780,
                ] {
                    let reference = SourceArtReference {
                        server_id: identity.server_id,
                        catalogue_epoch: identity.catalogue_epoch,
                        grant_id: grant,
                        library_id: item.library_id.clone(),
                        item_id: item.item_id.clone(),
                        kind,
                        variant,
                        expires_at_ms: artwork_expiry(now).map_err(|_| unavailable())?,
                    };
                    item.art.push(SourceArtwork {
                        kind,
                        variant,
                        resource: key.issue_art(&reference, now).map_err(|_| unavailable())?,
                    });
                }
            }
        }
        items.push(item);
    }
    Ok(items)
}
pub(super) async fn receiver_key(
    state: &AppState,
) -> Result<Option<plurx_core::sharing_file_locators::FileLocatorKey>, super::error::ApiError> {
    let identity = state
        .store
        .sharing_identity(clock_ms())
        .await
        .map_err(|_| unavailable())?;
    state
        .store
        .receiver_file_locator_key(identity.server_id, identity.catalogue_epoch)
        .await
        .map_err(|_| unavailable())?
        .map(|envelope| {
            plurx_core::sharing_file_locators::FileLocatorKey::open(
                &state.sharing.key,
                &identity,
                &envelope,
            )
        })
        .transpose()
        .map_err(|_| unavailable())
}

const DISK_BYTES: u64 = 256 * 1024 * 1024;
const DISK_ENTRIES: usize = 2048;
static DISK_WRITER: LazyLock<Arc<tokio::sync::Mutex<()>>> =
    LazyLock::new(|| Arc::new(tokio::sync::Mutex::new(())));
/// Bytes that own their admission lease. A response body hands these to the
/// writer, and managed cache publication (after a fresh pinned Source
/// byte/digest proof) to its detached writer; either way the reservation is
/// retained until the last owner of the actual bytes settles.
struct LeasedArtBytes {
    bytes: axum::body::Bytes,
    _lease: Arc<ArtBodyLease>,
}
impl AsRef<[u8]> for LeasedArtBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
fn disk_filename(
    reference: &plurx_core::sharing_artwork::ReceiverArtReference,
    source: &plurx_core::sharing_artwork::SourceArtReference,
    digest: [u8; 32],
) -> Result<String, super::error::ApiError> {
    use sha2::{Digest, Sha256};
    let context = serde_json::to_vec(&serde_json::json!([
        "receiver-art-disk-v1",
        reference.user_id,
        reference.lifecycle_generation,
        reference.item,
        source.grant_id,
        source.kind,
        source.variant,
        hex::encode(digest)
    ]))
    .map_err(|_| unavailable())?;
    Ok(format!("{}.bin", hex::encode(Sha256::digest(context))))
}
/// What the receiver last proved against the Source for one artwork view. The
/// disk file is named by its digest, so the digest has to be known before the
/// file can be found; it comes from here, never from the file itself.
#[derive(Clone, Copy)]
struct SnapshotRecord {
    digest: [u8; 32],
    mime: &'static str,
    used: u64,
}
static SNAPSHOT_INDEX: LazyLock<Mutex<std::collections::HashMap<[u8; 32], SnapshotRecord>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));
static SNAPSHOT_CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// The view a snapshot answers for: owner, lifecycle, item, grant, art slot,
/// and the Source's `expires_at_ms` on the resource the request carries. The
/// Source issues a fresh resource (new expiry) with every catalogue read, so a
/// snapshot is reused for exactly one catalogue issue: poster changes reach the
/// receiver on the next catalogue read, and never outlive the resource's own
/// five-minute lifetime, which `verify_art` already enforced for this request.
fn snapshot_view(
    reference: &plurx_core::sharing_artwork::ReceiverArtReference,
    source: &plurx_core::sharing_artwork::SourceArtReference,
) -> Result<[u8; 32], super::error::ApiError> {
    use sha2::{Digest, Sha256};
    let view = serde_json::to_vec(&serde_json::json!([
        "receiver-art-view-v1",
        reference.user_id,
        reference.lifecycle_generation,
        reference.item,
        source.grant_id,
        source.kind,
        source.variant,
        source.expires_at_ms
    ]))
    .map_err(|_| unavailable())?;
    Ok(Sha256::digest(view).into())
}
fn remember_snapshot(view: [u8; 32], digest: [u8; 32], mime: &'static str) {
    let mut index = SNAPSHOT_INDEX.lock().expect("art snapshot index");
    if index.len() >= DISK_ENTRIES && !index.contains_key(&view) {
        if let Some(oldest) = index.iter().min_by_key(|(_, r)| r.used).map(|(k, _)| *k) {
            index.remove(&oldest);
        }
    }
    let used = SNAPSHOT_CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    index.insert(view, SnapshotRecord { digest, mime, used });
}
/// The receiver's own verified snapshot for this view, or `None` (then the
/// caller dials the Source). Authority is the caller's: this runs only after
/// `artwork_authority`. The disk bytes are re-hashed against the digest the
/// receiver proved with the Source, so a missing, resized or corrupt file is a
/// miss, never a wrong poster.
async fn read_disk_snapshot(
    state: &AppState,
    reference: &plurx_core::sharing_artwork::ReceiverArtReference,
    lease: Arc<ArtBodyLease>,
) -> Option<crate::sharing_client::PeerArtwork> {
    let source = reference.source.reference_unverified().ok()?;
    let view = snapshot_view(reference, &source).ok()?;
    let record = *SNAPSHOT_INDEX
        .lock()
        .expect("art snapshot index")
        .get(&view)?;
    let filename = disk_filename(reference, &source, record.digest).ok()?;
    let root = state.runtime_cache_dir.join("sharing-art-v1");
    let read = async {
        use plurx_core::fs_secure;
        fs_secure::directory_identity_nofollow(&root).await?;
        let file = fs_secure::open_read_nofollow(&root.join(&filename)).await?;
        let file = file.into_std().await;
        tokio::task::spawn_blocking(move || {
            // The admission lease stays with the blocking read.
            let _owner = lease;
            use sha2::{Digest, Sha256};
            use std::io::Read;
            let mut bytes = Vec::new();
            file.take(plurx_core::sharing_artwork::MAX_ART_BYTES as u64 + 1)
                .read_to_end(&mut bytes)?;
            if bytes.is_empty() || bytes.len() > plurx_core::sharing_artwork::MAX_ART_BYTES {
                return Err(std::io::Error::other("art cache size"));
            }
            let digest: [u8; 32] = Sha256::digest(&bytes).into();
            Ok::<_, std::io::Error>((bytes, digest))
        })
        .await
        .map_err(std::io::Error::other)?
    };
    match tokio::time::timeout(std::time::Duration::from_secs(2), read).await {
        Ok(Ok((bytes, digest))) if digest == record.digest => {
            let used = SNAPSHOT_CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if let Some(entry) = SNAPSHOT_INDEX
                .lock()
                .expect("art snapshot index")
                .get_mut(&view)
            {
                entry.used = used;
            }
            Some(crate::sharing_client::PeerArtwork {
                bytes: axum::body::Bytes::from(bytes),
                digest,
                mime: record.mime,
            })
        }
        _ => {
            SNAPSHOT_INDEX
                .lock()
                .expect("art snapshot index")
                .remove(&view);
            None
        }
    }
}
async fn publish_disk_snapshot(
    state: &AppState,
    reference: &plurx_core::sharing_artwork::ReceiverArtReference,
    asset: &crate::sharing_client::PeerArtwork,
    lease: Arc<ArtBodyLease>,
) -> Result<(), super::error::ApiError> {
    use sha2::{Digest, Sha256};
    let source = reference
        .source
        .reference_unverified()
        .map_err(|_| unavailable())?;
    let filename = disk_filename(reference, &source, asset.digest)?;
    let view = snapshot_view(reference, &source)?;
    let mime = asset.mime;
    let root = state.runtime_cache_dir.join("sharing-art-v1");
    let bytes = asset.bytes.clone();
    let digest = asset.digest;
    let task = tokio::spawn(async move {
        let owner = lease;
        let _serial = DISK_WRITER.lock().await;
        use plurx_core::fs_secure;
        let parent = root
            .parent()
            .ok_or_else(|| std::io::Error::other("art cache"))?;
        if tokio::fs::symlink_metadata(&root).await.is_err() {
            fs_secure::create_directory_child(parent, "sharing-art-v1").await?;
        }
        fs_secure::directory_identity_nofollow(&root).await?;
        let mut reader = tokio::fs::read_dir(&root).await?;
        let mut entries = Vec::new();
        let mut total = 0u64;
        while let Some(entry) = reader.next_entry().await? {
            if entries.len() == DISK_ENTRIES {
                return Err(std::io::Error::other("art cache capacity"));
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| std::io::Error::other("art cache shape"))?;
            let key = name
                .strip_suffix(".bin")
                .ok_or_else(|| std::io::Error::other("art cache shape"))?;
            if key.len() != 64
                || !key
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(std::io::Error::other("art cache shape"));
            }
            let file = fs_secure::open_read_nofollow(&root.join(&name)).await?;
            let metadata = file.metadata().await?;
            if !metadata.is_file()
                || metadata.len() == 0
                || metadata.len() > plurx_core::sharing_artwork::MAX_ART_BYTES as u64
            {
                return Err(std::io::Error::other("art cache shape"));
            }
            total = total
                .checked_add(metadata.len())
                .ok_or_else(|| std::io::Error::other("art cache capacity"))?;
            entries.push((metadata.modified()?, name, metadata.len()));
        }
        if total > DISK_BYTES {
            return Err(std::io::Error::other("art cache capacity"));
        }
        // No stat digest cache: hash the current opened disk file on every reuse.
        if entries.iter().any(|(_, name, _)| name == &filename) {
            let file = fs_secure::open_read_nofollow(&root.join(&filename))
                .await?
                .into_std()
                .await;
            let hash_owner = owner.clone();
            let fresh = tokio::task::spawn_blocking(move || {
                let _owner = hash_owner;
                use std::io::Read;
                let mut file = file;
                let mut hash = Sha256::new();
                let mut buffer = [0u8; 64 * 1024];
                let mut size = 0usize;
                loop {
                    let n = file.read(&mut buffer)?;
                    if n == 0 {
                        break;
                    }
                    size += n;
                    if size > plurx_core::sharing_artwork::MAX_ART_BYTES {
                        return Err(std::io::Error::other("art cache size"));
                    }
                    hash.update(&buffer[..n]);
                }
                Ok::<_, std::io::Error>((<[u8; 32]>::from(hash.finalize()), size))
            })
            .await
            .map_err(std::io::Error::other)??;
            if fresh == (digest, bytes.len()) {
                #[cfg(not(windows))]
                {
                    let file = fs_secure::open_read_nofollow(&root.join(&filename))
                        .await?
                        .into_std()
                        .await;
                    file.set_times(
                        std::fs::FileTimes::new().set_modified(std::time::SystemTime::now()),
                    )?;
                    return Ok(());
                }
                // Windows read handles cannot mutate write attributes. Fall
                // through to the same bounded held-directory replacement;
                // remove the old entry before staging to preserve the disk cap.
            }
        }
        entries.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        if let Some(index) = entries.iter().position(|(_, name, _)| name == &filename) {
            let (_, name, len) = entries.remove(index);
            fs_secure::unlink_child(&root, &name).await?;
            total -= len;
        }
        while total.saturating_add(bytes.len() as u64) > DISK_BYTES || entries.len() >= DISK_ENTRIES
        {
            let (_, name, len) = entries
                .first()
                .cloned()
                .ok_or_else(|| std::io::Error::other("art cache capacity"))?;
            fs_secure::unlink_child(&root, &name).await?;
            entries.remove(0);
            total -= len;
        }
        // The actual blocking writer owns both Bytes and the admission lease.
        fs_secure::atomic_write_child_owned(
            &root,
            &filename,
            LeasedArtBytes {
                bytes,
                _lease: owner,
            },
        )
        .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?
        .map_err(|_| unavailable())?;
    remember_snapshot(view, digest, mime);
    Ok(())
}

#[cfg(test)]
pub(super) fn source_capacity() -> (usize, usize) {
    (
        SOURCE_OPERATIONS.available_permits(),
        SOURCE_BYTES.available_permits(),
    )
}

#[cfg(test)]
pub(super) static ART_FIXTURES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
#[cfg(test)]
pub(super) fn receiver_capacity() -> (usize, usize) {
    (
        RECEIVER_OPERATIONS.available_permits(),
        RECEIVER_BYTES.available_permits(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::{
        sharing::{SharingIdentity, SourceId},
        sharing_artwork::{artwork_expiry, ArtKind, ReceiverArtReference, SourceArtReference},
        sharing_catalogue::SharedReference,
        sharing_catalogue_details::CatalogueRevisionKey,
    };
    #[tokio::test]
    async fn sharing_art_nonqueued_byte_capacity_survives_held_owners() {
        let _serial = ART_FIXTURES.lock().await;
        let mut leases = Vec::new();
        for _ in 0..4 {
            leases.push(ArtBodyLease::source(false).expect("four bounded originals"));
        }
        assert!(ArtBodyLease::source(false).is_err());
        assert_eq!(SOURCE_OPERATIONS.available_permits(), 0);
        let held = leases.pop().expect("owner");
        let copy = held.clone();
        drop(held);
        assert_eq!(
            SOURCE_OPERATIONS.available_permits(),
            0,
            "another owner retains settled work"
        );
        drop(copy);
        assert_eq!(SOURCE_OPERATIONS.available_permits(), 1);
        drop(leases);
        let a = ArtBodyLease::source(true).expect("derivative original and variant");
        let b = ArtBodyLease::source(true).expect("second derivative");
        assert!(ArtBodyLease::source(true).is_err());
        assert_eq!(
            SOURCE_OPERATIONS.available_permits(),
            2,
            "byte refusal cannot leak an operation"
        );
        drop(a);
        drop(b);
        assert_eq!(source_capacity(), (4, BYTE_KIB));
        let a = ArtBodyLease::receiver().expect("two buffers");
        let b = ArtBodyLease::receiver().expect("two buffers");
        assert!(ArtBodyLease::receiver().is_err());
        drop(a);
        drop(b);
        assert_eq!(receiver_capacity(), (4, BYTE_KIB));
    }
    #[tokio::test]
    async fn sharing_art_disk_serializes_lru_rehashes_and_isolates_full_references() {
        let _serial = ART_FIXTURES.lock().await;
        let (_, mut state) = super::super::tests::test_app_with_state();
        let directory = crate::test_tempdir().expect("owned cache");
        state.runtime_cache_dir = directory.path().to_owned();
        let now = clock_ms();
        let identity = SharingIdentity {
            server_id: uuid::Uuid::new_v4(),
            catalogue_epoch: uuid::Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let envelope = CatalogueRevisionKey::generate_sealed(&state.sharing.key, identity.clone())
            .expect("key");
        let key = CatalogueRevisionKey::open(&state.sharing.key, identity.clone(), &envelope)
            .expect("open");
        let source = key
            .issue_art(
                &SourceArtReference {
                    server_id: identity.server_id,
                    catalogue_epoch: identity.catalogue_epoch,
                    grant_id: uuid::Uuid::new_v4(),
                    library_id: SourceId::parse("1").expect("lib"),
                    item_id: SourceId::parse("1").expect("item"),
                    kind: ArtKind::Poster,
                    variant: ArtVariant::Original,
                    expires_at_ms: artwork_expiry(now).expect("expiry"),
                },
                now,
            )
            .expect("resource");
        let reference = ReceiverArtReference {
            item: SharedReference {
                import_id: uuid::Uuid::new_v4(),
                server_id: identity.server_id,
                catalogue_epoch: identity.catalogue_epoch,
                library_id: SourceId::parse("1").expect("lib"),
                item_id: SourceId::parse("1").expect("item"),
            },
            user_id: 1,
            lifecycle_generation: 1,
            source,
        };
        use sha2::{Digest, Sha256};
        let bytes = axum::body::Bytes::from(vec![42u8; 2 * 1024 * 1024]);
        let asset = crate::sharing_client::PeerArtwork {
            digest: Sha256::digest(&bytes).into(),
            bytes,
            mime: "image/png",
        };
        publish_disk_snapshot(
            &state,
            &reference,
            &asset,
            ArtBodyLease::receiver().expect("permit"),
        )
        .await
        .expect("publish");
        let root = directory.path().join("sharing-art-v1");
        let original = std::fs::read_dir(&root)
            .expect("entries")
            .next()
            .expect("entry")
            .expect("entry")
            .path();
        std::fs::write(&original, vec![43u8; asset.bytes.len()])
            .expect("same-size stale disk bytes");
        publish_disk_snapshot(
            &state,
            &reference,
            &asset,
            ArtBodyLease::receiver().expect("permit"),
        )
        .await
        .expect("fresh proof repairs corrupt cached bytes");
        assert_eq!(
            std::fs::read(&original).expect("current bytes"),
            asset.bytes.as_ref()
        );
        let mut other = reference.clone();
        other.user_id = 2;
        publish_disk_snapshot(
            &state,
            &other,
            &asset,
            ArtBodyLease::receiver().expect("permit"),
        )
        .await
        .expect("other user");
        assert_eq!(
            std::fs::read_dir(&root).expect("entries").count(),
            2,
            "same numeric item must not alias another user"
        );
        // Sparse owned fixtures establish the actual byte cap without allocating
        // hundreds of MiB or touching unrelated artifacts.
        for entry in std::fs::read_dir(&root).expect("entries") {
            std::fs::remove_file(entry.expect("entry").path()).expect("fixture reset");
        }
        for n in 0..17 {
            let file =
                std::fs::File::create(root.join(format!("{n:064x}.bin"))).expect("sparse fixture");
            file.set_len(plurx_core::sharing_artwork::MAX_ART_BYTES as u64)
                .expect("bounded sparse length");
            file.set_times(
                std::fs::FileTimes::new()
                    .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(n + 1)),
            )
            .expect("deterministic age");
        }
        let mut tasks = Vec::new();
        for user in 1..=2 {
            let mut reference = reference.clone();
            reference.user_id = user;
            let state = state.clone();
            let asset = crate::sharing_client::PeerArtwork {
                bytes: asset.bytes.clone(),
                digest: asset.digest,
                mime: asset.mime,
            };
            let lease = ArtBodyLease::receiver().expect("bounded two copies");
            tasks.push(tokio::spawn(async move {
                publish_disk_snapshot(&state, &reference, &asset, lease).await
            }));
        }
        for task in tasks {
            task.await.expect("task").expect("serialized publication");
        }
        let total: u64 = std::fs::read_dir(&root)
            .expect("entries")
            .map(|entry| entry.expect("entry").metadata().expect("stat").len())
            .sum();
        assert!(total <= DISK_BYTES);
        assert!(
            !root.join(format!("{:064x}.bin", 0)).exists(),
            "oldest exact entry evicted"
        );
        assert_eq!(RECEIVER_OPERATIONS.available_permits(), 4);
        assert_eq!(RECEIVER_BYTES.available_permits(), BYTE_KIB);
        #[cfg(unix)]
        {
            let target = directory.path().join("outside");
            std::fs::write(&target, b"private").expect("outside fixture");
            std::os::unix::fs::symlink(&target, root.join(format!("{:064x}.bin", 99)))
                .expect("symlink");
            assert!(publish_disk_snapshot(
                &state,
                &reference,
                &asset,
                ArtBodyLease::receiver().expect("permit")
            )
            .await
            .is_err());
            assert_eq!(std::fs::read(target).expect("outside"), b"private");
        }
    }
    /// The receiver relays a Source's typed wait as 429 with `Retry-After`,
    /// and keeps every other peer failure a plain 503.
    #[test]
    fn sharing_art_receiver_relays_source_busy_as_429_with_retry_after() {
        use crate::sharing_client::PeerError;
        use axum::response::IntoResponse;
        let busy = peer_refusal(PeerError::Busy {
            retry_after_secs: 3,
        })
        .into_response();
        assert_eq!(busy.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(busy.headers()[header::RETRY_AFTER], "3");
        for other in [
            PeerError::Unavailable,
            PeerError::InvalidResponse,
            PeerError::Rejected(StatusCode::NOT_FOUND),
        ] {
            let response = peer_refusal(other).into_response();
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert!(response.headers().get(header::RETRY_AFTER).is_none());
        }
    }

    struct ReceiverArtFixture {
        _directory: tempfile::TempDir,
        state: AppState,
        key: plurx_core::sharing_file_locators::FileLocatorKey,
        source_key: CatalogueRevisionKey,
        import: uuid::Uuid,
        grant: uuid::Uuid,
        lifecycle: i64,
        viewer: i64,
        token: String,
        remote: SharingIdentity,
    }
    /// An active B import whose Source endpoint cannot be reached, so any
    /// request answered 200 was answered from B's own snapshot.
    async fn receiver_art_fixture() -> ReceiverArtFixture {
        use plurx_core::{
            secrets::SharingSecretPurpose,
            sharing::{Assignment, Endpoint, ImportOutcome, NewImport},
            sharing_file_locators::FileLocatorKey,
            store::SqliteStore,
        };
        let directory = tempfile::tempdir().expect("B fixture");
        let path = directory.path().join("receiver.sqlite");
        let store: Arc<dyn plurx_core::store::Store> =
            Arc::new(SqliteStore::open(&path).expect("B store"));
        let (_, mut state) = super::super::tests::test_app_with_state();
        state.store = store.clone();
        state.catalogue = plurx_core::store::CatalogueReader::authority(store);
        state.runtime_cache_dir = directory
            .path()
            .canonicalize()
            .expect("canonical cache")
            .join("runtime");
        std::fs::create_dir(&state.runtime_cache_dir).expect("runtime cache");
        state
            .store
            .put_setting(plurx_core::store::keys::SHARING_ENABLED, "true")
            .await
            .expect("enable B");
        let user = state
            .store
            .create_user("receiver-art-viewer", "synthetic-hash", false)
            .await
            .expect("viewer");
        let token = "receiver-art-login".to_owned();
        state
            .store
            .create_token(&plurx_core::auth::hash_token(&token), user.id, None)
            .await
            .expect("login");
        let local = state.store.sharing_identity(1000).await.expect("identity");
        let remote = SharingIdentity {
            server_id: uuid::Uuid::new_v4(),
            catalogue_epoch: uuid::Uuid::new_v4(),
            created_at_ms: 1000,
        };
        let import = uuid::Uuid::new_v4();
        let grant = uuid::Uuid::new_v4();
        let credential = crate::sharing::ImportCredential::encode(
            &plurx_core::sharing::new_secret().expect("grant"),
            uuid::Uuid::new_v4(),
            1000,
            "Fixture B",
            2000,
            None,
        )
        .expect("credential");
        let seal = |purpose, text: &str| {
            state
                .sharing
                .key
                .seal_sharing(purpose, local.server_id, import, text)
                .expect("seal")
        };
        assert_eq!(
            state
                .store
                .create_share_import(NewImport {
                    id: import,
                    source: remote.clone(),
                    source_name: "Fixture Source".into(),
                    claim_id: uuid::Uuid::new_v4(),
                    credential: seal(SharingSecretPurpose::Credential, credential.expose()),
                    claim_secret: seal(SharingSecretPurpose::Claim, "synthetic-claim"),
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
            .settle_share_claim(import, grant, true, 1001)
            .await
            .expect("active import");
        state
            .store
            .assign_share_viewers(
                import,
                1,
                vec![Assignment {
                    library_id: SourceId::parse("1").expect("lib"),
                    user_id: user.id,
                }],
                1002,
            )
            .await
            .expect("assignment");
        let envelope = FileLocatorKey::generate_sealed(&state.sharing.key, &local).expect("key");
        let writer = rusqlite::Connection::open(&path).expect("B writer");
        writer
            .execute_batch(
                plurx_core::store::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA,
            )
            .expect("key schema");
        writer
            .execute(
                "INSERT INTO sharing_file_locator_keys VALUES(1,?1,?2,?3)",
                rusqlite::params![
                    local.server_id.to_string(),
                    local.catalogue_epoch.to_string(),
                    envelope.as_stored()
                ],
            )
            .expect("key row");
        drop(writer);
        let key = FileLocatorKey::open(&state.sharing.key, &local, &envelope).expect("signer");
        let source_envelope =
            CatalogueRevisionKey::generate_sealed(&state.sharing.key, remote.clone())
                .expect("source key");
        let source_key =
            CatalogueRevisionKey::open(&state.sharing.key, remote.clone(), &source_envelope)
                .expect("open");
        let lifecycle = state
            .store
            .sharing_import(import)
            .await
            .expect("import")
            .expect("present")
            .summary
            .lifecycle_generation;
        ReceiverArtFixture {
            _directory: directory,
            state,
            key,
            source_key,
            import,
            grant,
            lifecycle,
            viewer: user.id,
            token,
            remote,
        }
    }
    impl ReceiverArtFixture {
        /// The reference B holds for one poster view, issued at `now_ms`.
        fn reference(&self, now_ms: i64) -> ReceiverArtReference {
            let library = SourceId::parse("1").expect("lib");
            let item = SourceId::parse("1").expect("item");
            ReceiverArtReference {
                item: SharedReference {
                    import_id: self.import,
                    server_id: self.remote.server_id,
                    catalogue_epoch: self.remote.catalogue_epoch,
                    library_id: library.clone(),
                    item_id: item.clone(),
                },
                user_id: self.viewer,
                lifecycle_generation: self.lifecycle,
                source: self
                    .source_key
                    .issue_art(
                        &SourceArtReference {
                            server_id: self.remote.server_id,
                            catalogue_epoch: self.remote.catalogue_epoch,
                            grant_id: self.grant,
                            library_id: library,
                            item_id: item,
                            kind: ArtKind::Poster,
                            variant: ArtVariant::W300,
                            expires_at_ms: artwork_expiry(now_ms).expect("expiry"),
                        },
                        now_ms,
                    )
                    .expect("Source resource"),
            }
        }
        async fn get(&self, reference: &ReceiverArtReference) -> axum::response::Response {
            use tower::ServiceExt;
            let url = self
                .key
                .issue_art(reference, clock_ms())
                .expect("B resource")
                .url();
            super::super::router(self.state.clone())
                .oneshot(
                    axum::http::Request::builder()
                        .uri(url)
                        .header(header::AUTHORIZATION, format!("Bearer {}", self.token))
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response")
        }
    }

    /// The second view of a poster comes from B's own verified snapshot and
    /// never dials the Source, yet every authority check still runs first.
    #[tokio::test]
    async fn sharing_art_receiver_serves_verified_snapshot_before_dialling_the_source() {
        use http_body_util::BodyExt;
        use plurx_core::sharing::Assignment;
        use sha2::{Digest, Sha256};
        let _serial = ART_FIXTURES.lock().await;
        let fixture = receiver_art_fixture().await;
        let now = clock_ms();
        let reference = fixture.reference(now);
        // Cold: nothing is held and the Source endpoint is unreachable.
        let cold = fixture.get(&reference).await;
        assert_eq!(cold.status(), StatusCode::SERVICE_UNAVAILABLE);
        // B proved these bytes against the Source once (the first view).
        let mut png = vec![7u8; 4096];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        let bytes = axum::body::Bytes::from(png);
        let asset = crate::sharing_client::PeerArtwork {
            digest: Sha256::digest(&bytes).into(),
            bytes: bytes.clone(),
            mime: "image/png",
        };
        publish_disk_snapshot(
            &fixture.state,
            &reference,
            &asset,
            ArtBodyLease::receiver().expect("permit"),
        )
        .await
        .expect("first view publishes");
        for _ in 0..2 {
            let again = fixture.get(&reference).await;
            assert_eq!(again.status(), StatusCode::OK, "served from the snapshot");
            assert_eq!(again.headers()[header::CONTENT_TYPE], "image/png");
            assert_eq!(
                again.headers()["x-plurx-art-sha256"],
                hex::encode(asset.digest)
            );
            assert_eq!(
                again.into_body().collect().await.expect("body").to_bytes(),
                bytes
            );
        }
        // Corrupt snapshot bytes are a miss, never a wrong poster.
        let root = fixture.state.runtime_cache_dir.join("sharing-art-v1");
        let file = std::fs::read_dir(&root)
            .expect("entries")
            .next()
            .expect("entry")
            .expect("entry")
            .path();
        std::fs::write(&file, vec![9u8; bytes.len()]).expect("same-size corruption");
        assert_eq!(
            fixture.get(&reference).await.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "digest mismatch falls through to the Source"
        );
        std::fs::write(&file, &bytes).expect("restore");
        publish_disk_snapshot(
            &fixture.state,
            &reference,
            &asset,
            ArtBodyLease::receiver().expect("permit"),
        )
        .await
        .expect("republish");
        assert_eq!(fixture.get(&reference).await.status(), StatusCode::OK);
        // A newer catalogue read issues a new Source resource: the old
        // snapshot does not answer for it.
        let next = fixture.reference(now + 60_000);
        assert_ne!(
            next.source.reference_unverified().unwrap().expires_at_ms,
            reference
                .source
                .reference_unverified()
                .unwrap()
                .expires_at_ms
        );
        assert_eq!(
            fixture.get(&next).await.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a new catalogue issue revalidates against the Source"
        );
        // Losing the assignment refuses even a held snapshot.
        fixture
            .state
            .store
            .assign_share_viewers(
                fixture.import,
                2,
                vec![Assignment {
                    library_id: SourceId::parse("2").expect("other library"),
                    user_id: fixture.viewer,
                }],
                1003,
            )
            .await
            .expect("assignment change");
        assert_eq!(
            fixture.get(&reference).await.status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "authority is checked before the snapshot"
        );
    }
}
