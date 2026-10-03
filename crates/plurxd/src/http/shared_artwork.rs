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
/// Owned by accepted connection monitors and any still-running blocking read.
/// Body Drop cannot return capacity while DATA remains queued to the transport.
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
    let mut response = Response::new(Body::from(bytes.clone()));
    for (key, value) in [
        ("x-plurx-art-sha256", hex::encode(digest)),
        ("x-plurx-art-bytes", bytes.len().to_string()),
        ("x-plurx-art-variant", variant.label().to_owned()),
    ] {
        response.headers_mut().insert(
            key,
            HeaderValue::from_str(&value).map_err(|_| unavailable())?,
        );
    }
    response.headers_mut().insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&bytes.len().to_string()).map_err(|_| unavailable())?,
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
    response.extensions_mut().insert(lease);
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
        let Some(asset)=asset else {return Err(if reference.variant==ArtVariant::Original {missing()} else {fail(StatusCode::TOO_MANY_REQUESTS,"sharing_art_variant_busy")});};
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
        let (summary, asset) = state
            .sharing
            .read_artwork(&state, &reference)
            .await
            .map_err(|_| unavailable())?;
        key.verify_art(&resource, import_id, user.id, clock_ms())
            .map_err(|_| missing())?;
        let mut proof = Response::new(Body::empty());
        super::shared_library::attach_receiver_art_authority(
            &state, &token, user.id, &summary, &reference, &mut proof,
        )
        .await?;
        publish_disk_snapshot(&state, &reference, &asset, lease.clone()).await?;
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
/// Managed cache publication follows a fresh pinned Source byte/digest proof.
/// A detached writer retains its copy reservation until filesystem settlement.
struct OwnedArtWrite {
    bytes: axum::body::Bytes,
    _lease: Arc<ArtBodyLease>,
}
impl AsRef<[u8]> for OwnedArtWrite {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
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
    let context = serde_json::to_vec(&serde_json::json!([
        "receiver-art-disk-v1",
        reference.user_id,
        reference.lifecycle_generation,
        reference.item,
        source.grant_id,
        source.kind,
        source.variant,
        hex::encode(asset.digest)
    ]))
    .map_err(|_| unavailable())?;
    let filename = format!("{}.bin", hex::encode(Sha256::digest(context)));
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
            OwnedArtWrite {
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
        .map_err(|_| unavailable())
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
}
