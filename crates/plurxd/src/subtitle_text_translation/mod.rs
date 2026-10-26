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
use std::{io::Read, sync::LazyLock};
use tokio_util::sync::CancellationToken;
pub(crate) const MODEL_ID: &str = "opus-mt-mul-en";
pub(crate) const ADAPTER_CONTRACT: &str = "candle-marian-0.11-v1";
pub(crate) const CATALOG_REVISION: &str = "plurx-marian-mul-en-v1";
const MAX_INPUT: usize = 1024 * 1024;
static ENGINE: LazyLock<tokio::sync::Mutex<Option<(FileIdentity, String)>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(None));

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
    let identity = std_file_identity(&held).map_err(|_| "engine_unavailable".to_owned())?;
    let mut cache = ENGINE.lock().await;
    let digest =
        if let Some((prior, digest)) = cache.as_ref().filter(|(prior, _)| *prior == identity) {
            let _ = prior;
            digest.clone()
        } else {
            let digest = tokio::task::spawn_blocking(move || {
                let mut input = held;
                let mut hash = Sha256::new();
                let mut buffer = [0_u8; 128 * 1024];
                loop {
                    let count = input
                        .read(&mut buffer)
                        .map_err(|_| "engine_unavailable".to_owned())?;
                    if count == 0 {
                        break;
                    }
                    hash.update(&buffer[..count]);
                }
                if std_file_identity(&input).ok() != Some(identity) {
                    return Err("engine_changed".to_owned());
                }
                Ok(hex::encode(hash.finalize()))
            })
            .await
            .map_err(|_| "engine_unavailable".to_owned())??;
            *cache = Some((identity, digest.clone()));
            digest
        };
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
