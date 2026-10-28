//! Existing-caption translation is independent of speech inference. Its jobs
//! use the shared durable queue, CPU pool and acquired-caption delivery path.
pub(crate) mod api;
pub(crate) mod artifacts;
pub(crate) mod config;
pub(crate) mod engine;
pub(crate) mod manager;
pub(crate) mod text;
mod tokenizer;
mod weights;

use crate::{http::error::ApiError, state::AppState};
use plurx_core::{
    domain::MediaFile,
    fs_secure::{std_file_identity, FileIdentity},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    sync::{Arc, LazyLock},
};
use tokio_util::sync::CancellationToken;
pub(crate) const MODEL_ID: &str = "opus-mt-mul-en";
pub(crate) const ADAPTER_CONTRACT: &str = "candle-marian-0.11-v1";
pub(crate) const CATALOG_REVISION: &str = "plurx-marian-mul-en-v1";
const MAX_INPUT: usize = 1024 * 1024;
// Release images are much smaller; retain room for instrumented debug/test
// images while giving every cold-cache hash a finite regular-file read budget.
const MAX_ENGINE_IDENTITY_BYTES: u64 = 2 * 1024 * 1024 * 1024;
type EngineIdentityCache = Arc<tokio::sync::Mutex<Option<(FileIdentity, String)>>>;
static ENGINE: LazyLock<EngineIdentityCache> =
    LazyLock::new(|| Arc::new(tokio::sync::Mutex::new(None)));

fn engine_file_identity(held: &std::fs::File) -> Result<FileIdentity, String> {
    if !held
        .metadata()
        .map_err(|_| "engine_unavailable".to_owned())?
        .is_file()
    {
        return Err("engine_unavailable".to_owned());
    }
    let identity = std_file_identity(held).map_err(|_| "engine_unavailable".to_owned())?;
    if identity.size == 0 || identity.size > MAX_ENGINE_IDENTITY_BYTES {
        return Err("engine_unavailable".to_owned());
    }
    Ok(identity)
}

fn read_engine_digest(reader: impl Read, expected_bytes: u64) -> Result<String, String> {
    if expected_bytes == 0 || expected_bytes > MAX_ENGINE_IDENTITY_BYTES {
        return Err("engine_unavailable".to_owned());
    }
    let mut hash = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 128 * 1024];
    // A concurrently growing file cannot postpone EOF indefinitely. One extra
    // byte distinguishes growth from the exact immutable length we accepted.
    let mut input = reader.take(expected_bytes + 1);
    loop {
        let count = input
            .read(&mut buffer)
            .map_err(|_| "engine_unavailable".to_owned())?;
        if count == 0 {
            break;
        }
        total += count as u64;
        hash.update(&buffer[..count]);
    }
    if total != expected_bytes {
        return Err("engine_changed".to_owned());
    }
    Ok(hex::encode(hash.finalize()))
}

fn hash_engine_file(mut held: std::fs::File, identity: FileIdentity) -> Result<String, String> {
    if engine_file_identity(&held)? != identity {
        return Err("engine_changed".to_owned());
    }
    let digest = read_engine_digest(&mut held, identity.size)?;
    if std_file_identity(&held).ok() != Some(identity) {
        return Err("engine_changed".to_owned());
    }
    Ok(digest)
}

async fn cached_engine_identity(
    held: std::fs::File,
    cache: EngineIdentityCache,
    hasher: impl FnOnce(std::fs::File, FileIdentity) -> Result<String, String> + Send + 'static,
) -> Result<(FileIdentity, String), String> {
    let mut cache = cache.lock_owned().await;
    // Waiting for another hash may span an artifact change; inspect the held
    // inode only after acquiring serialization, before considering a cache hit.
    let identity = engine_file_identity(&held)?;
    if let Some((_, digest)) = cache.as_ref().filter(|(prior, _)| *prior == identity) {
        return Ok((identity, digest.clone()));
    }
    tokio::task::spawn_blocking(move || {
        // The worker owns both serialization and publication. Cancelling its
        // requester abandons only the wait, never releases the permit early.
        let digest = hasher(held, identity)?;
        *cache = Some((identity, digest.clone()));
        Ok((identity, digest))
    })
    .await
    .map_err(|_| "engine_unavailable".to_owned())?
}

pub(crate) async fn engine_identity() -> Result<(String, String), String> {
    let path = std::env::current_exe()
        .map_err(|_| "engine_unavailable".to_owned())?
        .canonicalize()
        .map_err(|_| "engine_unavailable".to_owned())?;
    let held = plurx_core::fs_secure::open_read_nofollow(&path)
        .await
        .map_err(|_| "engine_unavailable".to_owned())?
        .into_std()
        .await;
    let (identity, digest) = cached_engine_identity(held, ENGINE.clone(), hash_engine_file).await?;
    Ok((
        digest,
        crate::subtitle_models::artifacts::identity_key(identity),
    ))
}
pub(crate) fn normalize_language(value: &str) -> Result<String, ApiError> {
    let code = crate::subtitle_transcription::normalized_language(value);
    if !artifacts::supports_language(&code) || matches!(code.as_str(), "en" | "und") {
        return Err(ApiError::typed(
            axum::http::StatusCode::BAD_REQUEST,
            "source_language_unsupported",
            "Choose an advertised source language",
        ));
    }
    Ok(code)
}
pub(crate) async fn input_bytes(
    state: &AppState,
    file: &MediaFile,
    index: i64,
) -> Result<Vec<u8>, String> {
    if index < 0
        || file
            .subtitle_streams
            .get(index as usize)
            .is_none_or(|track| plurx_core::tracks::is_bitmap_subtitle(&track.codec))
    {
        return Err("subtitle_not_text".into());
    }
    let bytes = crate::subtitles::ensure_vtt_bytes_with_store(
        &state.subs_dir,
        file,
        index,
        &state.subtitle_source_access(),
        crate::process_control::ChildWork::background("existing subtitle translation input"),
    )
    .await?;
    if bytes.len() > MAX_INPUT {
        return Err("subtitle_too_large".into());
    }
    text::parse(&bytes)?;
    Ok(bytes)
}
pub(crate) async fn workers(state: &AppState) -> Result<Vec<Value>, ApiError> {
    let cancel = CancellationToken::new();
    let bundle = artifacts::inspect(&state.subtitle_models.root, &cancel).await;
    let engine = engine_identity().await;
    let receipts = state.store.subtitle_model_receipts(&state.node_id).await?;
    let ready = match (&bundle, &engine) {
        (Ok(bundle), Ok((digest, identity))) => receipts.iter().any(|r| {
            r.node_id == state.node_id
                && r.boot_id == state.subtitle_models.boot_id
                && r.model_id == MODEL_ID
                && r.model_sha256 == bundle.bundle_digest
                && r.model_file_identity == bundle.identity()
                && r.adapter_contract == ADAPTER_CONTRACT
                && r.engine_sha256 == *digest
                && r.engine_file_identity == *identity
        }),
        _ => false,
    };
    let status = if ready {
        "verified"
    } else if bundle.is_ok() {
        "unverified_cache"
    } else if bundle
        .as_ref()
        .is_err_and(|e| e.contains("missing") || e.contains("not_installed"))
    {
        "missing"
    } else {
        "unknown"
    };
    Ok(vec![
        json!({"node_id":state.node_id,"label":state.server_name,"boot_id":state.subtitle_models.boot_id,"model_id":MODEL_ID,"status":status,"reason":if ready{None}else{Some(if bundle.is_err(){"model_missing"}else{"model_unverified"})}}),
    ])
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        io::{Seek, Write},
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };

    #[tokio::test]
    async fn cancelled_engine_identity_request_retains_single_flight_and_publishes_cache() {
        let directory = tempfile::tempdir().expect("owned test directory");
        let path = directory.path().join("engine");
        std::fs::write(&path, b"hello").expect("engine bytes");
        let cache: EngineIdentityCache = Arc::new(tokio::sync::Mutex::new(None));
        let (entered, started) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let first = tokio::spawn(cached_engine_identity(
            std::fs::File::open(&path).expect("first handle"),
            cache.clone(),
            move |held, identity| {
                entered.send(()).expect("signal actual worker entry");
                blocked
                    .recv_timeout(Duration::from_secs(5))
                    .expect("bounded worker release");
                hash_engine_file(held, identity)
            },
        ));
        tokio::time::timeout(Duration::from_secs(5), started)
            .await
            .expect("worker entry deadline")
            .expect("worker entered");
        first.abort();
        assert!(first.await.expect_err("requester cancelled").is_cancelled());
        assert!(
            cache.try_lock().is_err(),
            "worker must retain serialization after requester abort"
        );
        release.send(()).expect("release owned worker");
        let called_again = Arc::new(AtomicBool::new(false));
        let observed = called_again.clone();
        let (_, digest) = tokio::time::timeout(
            Duration::from_secs(5),
            cached_engine_identity(
                std::fs::File::open(&path).expect("next handle"),
                cache.clone(),
                move |held, identity| {
                    observed.store(true, Ordering::SeqCst);
                    hash_engine_file(held, identity)
                },
            ),
        )
        .await
        .expect("cache completion deadline")
        .expect("cache result");
        assert!(
            !called_again.load(Ordering::SeqCst),
            "cancelled requester cannot discard worker cache publication"
        );
        assert_eq!(
            digest,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
    }

    #[test]
    fn engine_identity_growing_reader_stops_after_one_extra_byte() {
        struct GrowingReader {
            consumed: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Read for GrowingReader {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let count = self.consumed.fetch_add(bytes.len(), Ordering::SeqCst) + bytes.len();
                // Never return EOF; fail quickly if the production read budget
                // is removed, rather than hanging this regression forever.
                if count > 4 {
                    return Err(std::io::Error::other("reader exceeded regression budget"));
                }
                bytes.fill(0);
                Ok(bytes.len())
            }
        }
        let consumed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        assert_eq!(
            read_engine_digest(
                GrowingReader {
                    consumed: consumed.clone()
                },
                1
            ),
            Err("engine_changed".into())
        );
        assert_eq!(
            consumed.load(Ordering::SeqCst),
            2,
            "accepted length plus exactly one growth byte"
        );
    }

    #[test]
    fn engine_identity_rejects_mutation_before_hashing() {
        let directory = tempfile::tempdir().expect("owned test directory");
        let path = directory.path().join("engine");
        std::fs::write(&path, b"x").expect("initial bytes");
        let held = std::fs::File::open(&path).expect("held engine");
        let mut witness = held.try_clone().expect("shared cursor witness");
        let identity = engine_file_identity(&held).expect("initial identity");
        // Grow after the initial identity is accepted. The hash rejects the
        // changed identity before reading, rather than consuming the new tail.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("append")
            .write_all(&vec![0; 256 * 1024])
            .expect("grow");
        assert_eq!(
            hash_engine_file(held, identity),
            Err("engine_changed".into())
        );
        assert_eq!(witness.stream_position().expect("cursor"), 0);
    }

    #[tokio::test]
    async fn engine_identity_refuses_nonregular_empty_and_oversized_files_before_hashing() {
        let directory = tempfile::tempdir().expect("owned test directory");
        let cache: EngineIdentityCache = Arc::new(tokio::sync::Mutex::new(None));
        let path = directory.path().join("engine");
        let file = std::fs::File::create(&path).expect("sparse engine");
        for size in [0, MAX_ENGINE_IDENTITY_BYTES + 1] {
            file.set_len(size).expect("sparse logical length");
            assert!(cached_engine_identity(
                std::fs::File::open(&path).expect("held engine"),
                cache.clone(),
                |_, _| panic!("invalid engine must never start hashing")
            )
            .await
            .is_err());
        }
        assert!(cached_engine_identity(
            std::fs::File::open(directory.path()).expect("directory handle"),
            cache,
            |_, _| panic!("nonregular engine must never start hashing")
        )
        .await
        .is_err());
    }
}
