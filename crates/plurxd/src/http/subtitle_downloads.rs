//! Search and acquire captions without rewriting the original media.

use super::error::ApiError;
use super::extract::{AdminUser, AuthUser};
use crate::online_subtitles::{Candidate, Provider, Search};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use plurx_core::domain::{DownloadedSubtitle, ItemKind, MediaFile};
use serde::Deserialize;
use serde_json::{json, Value};

const KEY: &str = "subtitles.opensubtitles.api_key";
const USERNAME: &str = "subtitles.opensubtitles.username";
const PASSWORD: &str = "subtitles.opensubtitles.password";
const AUTO: &str = "subtitles.automatic";
const LANGUAGES: &str = "subtitles.languages";
const CURSOR: &str = "subtitles.auto_cursor";
const NEXT_RUN: &str = "subtitles.auto_next_run";

async fn setting(state: &AppState, key: &str) -> Result<String, ApiError> {
    Ok(state.store.get_setting(key).await?.unwrap_or_default())
}

async fn provider(state: &AppState) -> Result<Provider, ApiError> {
    Provider::new(
        setting(state, KEY).await?,
        setting(state, USERNAME).await?,
        setting(state, PASSWORD).await?,
    )
    .map_err(provider_error)
}

fn provider_error(error: crate::online_subtitles::Error) -> ApiError {
    let status = if error.code == "subtitle_provider_quota" {
        StatusCode::TOO_MANY_REQUESTS
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    ApiError::TypedRetry {
        status,
        code: error.code,
        message: error.message.into(),
        retry_after_seconds: error.retry_after,
    }
}

pub async fn settings(
    _admin: AdminUser,
    State(state): State<AppState>,
) -> Result<Json<Value>, ApiError> {
    Ok(Json(json!({
        "configured": !setting(&state, KEY).await?.is_empty(),
        "username": setting(&state, USERNAME).await?,
        "password_configured": !setting(&state, PASSWORD).await?.is_empty(),
        "automatic": setting(&state,AUTO).await? == "true",
        "languages": languages(&state).await?,
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsUpdate {
    api_key: Option<String>,
    username: Option<String>,
    password: Option<String>,
    automatic: Option<bool>,
    languages: Option<Vec<String>>,
}

pub async fn update_settings(
    admin: AdminUser,
    State(state): State<AppState>,
    Json(update): Json<SettingsUpdate>,
) -> Result<Json<Value>, ApiError> {
    let mut values = Vec::new();
    let language_text = update.languages.as_ref().map(|values| values.join(","));
    if let Some(languages) = &update.languages {
        if languages.is_empty()
            || languages.len() > 3
            || languages
                .iter()
                .any(|v| !crate::online_subtitles::valid_language(v))
        {
            return Err(ApiError::BadRequest(
                "Choose one to three subtitle language codes".into(),
            ));
        }
    }
    if let Some(text) = &language_text {
        values.push((LANGUAGES, text.as_str()));
    }
    if let Some(enabled) = update.automatic {
        values.push((AUTO, if enabled { "true" } else { "false" }));
    }
    if update.automatic == Some(true)
        && update
            .api_key
            .as_deref()
            .unwrap_or(&setting(&state, KEY).await?)
            .trim()
            .is_empty()
    {
        return Err(ApiError::BadRequest(
            "Configure an OpenSubtitles API key before enabling automatic downloads".into(),
        ));
    }
    if update.automatic == Some(true) || update.languages.is_some() {
        values.push((NEXT_RUN, "0"));
        values.push((CURSOR, "0"));
    }
    for (key, value) in [
        (KEY, &update.api_key),
        (USERNAME, &update.username),
        (PASSWORD, &update.password),
    ] {
        if let Some(value) = value {
            if value.len() > 1024 || value.contains(['\r', '\n', '\0']) {
                return Err(ApiError::BadRequest(
                    "Invalid OpenSubtitles credential".into(),
                ));
            }
            values.push((key, value.as_str()));
        }
    }
    state.store.put_settings(&values).await?;
    settings(admin, State(state)).await
}

async fn languages(state: &AppState) -> Result<Vec<String>, ApiError> {
    let raw = setting(state, LANGUAGES).await?;
    Ok(if raw.is_empty() {
        vec!["en".into()]
    } else {
        raw.split(',')
            .filter(|v| crate::online_subtitles::valid_language(v))
            .take(3)
            .map(str::to_owned)
            .collect()
    })
}

/// One cluster lease owns the persisted cursor; each download also takes the
/// same per-file lease used by manual requests. Shutdown or lease loss cancels
/// the provider request, and every publication checks its current lease.
pub(crate) async fn automatic_loop(state: AppState, shutdown: tokio_util::sync::CancellationToken) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {_ = shutdown.cancelled()=>return,_ = interval.tick()=>{}}
        if setting(&state, AUTO).await.ok().as_deref() != Some("true") {
            continue;
        }
        let Ok(Some(lease)) = state.jobs.acquire_job("subtitles:automatic".into()).await else {
            continue;
        };
        let lost = lease.loss_token();
        let result = tokio::select! {
            _ = shutdown.cancelled()=>Ok(()),
            _ = lost.cancelled()=>Ok(()),
            result = tokio::time::timeout(std::time::Duration::from_secs(120),automatic_page(&state,&lease))=>result.unwrap_or(Ok(())),
        };
        if let Err(error) = result {
            tracing::debug!(error=?error,"automatic subtitle pass stopped");
        }
        let _ = lease.release().await;
    }
}

async fn automatic_page(
    state: &AppState,
    lease: &crate::job_lease::ActiveJobLease,
) -> Result<(), ApiError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| ApiError::Internal("system clock predates Unix epoch".into()))?
        .as_secs() as i64;
    let next = setting(state, NEXT_RUN).await?.parse::<i64>().unwrap_or(0);
    if next > now {
        return Ok(());
    }
    let provider = provider(state).await?;
    let languages = languages(state).await?;
    let cursor = setting(state, CURSOR).await?.parse::<i64>().unwrap_or(0);
    let files = state.store.subtitle_candidate_file_ids(cursor, 16).await?;
    let publisher = lease.publisher(state.store.as_ref());
    // Persist the cooldown before making requests so a restart cannot cause
    // an immediate repeat against the provider.
    publisher
        .put_setting(NEXT_RUN, &(now + 180).to_string())
        .await?;
    if files.is_empty() {
        publisher.put_setting(CURSOR, "0").await?;
        publisher
            .put_setting(NEXT_RUN, &(now + 86400).to_string())
            .await?;
        return Ok(());
    }
    for id in files {
        if setting(state, AUTO).await? != "true" {
            return Ok(());
        }
        let Some(file_lease) = state
            .jobs
            .acquire_job(format!("subtitles:file:{id}"))
            .await?
        else {
            return Ok(());
        };
        let lost = file_lease.loss_token();
        let result = tokio::select! {
            _=lost.cancelled()=>Err(ApiError::ServiceUnavailable("Subtitle ownership changed".into())),
            result=automatic_file(state,id,&languages,&provider,&file_lease)=>result,
        };
        file_lease.release().await?;
        if let Err(error) = result {
            let retry = match &error {
                ApiError::TypedRetry {
                    retry_after_seconds,
                    ..
                } => *retry_after_seconds,
                _ => 3600,
            };
            publisher
                .put_setting(NEXT_RUN, &(now + retry.max(180) as i64).to_string())
                .await?;
            return Err(error);
        }
        publisher.put_setting(CURSOR, &id.to_string()).await?;
    }
    Ok(())
}

fn missing_language(tracks: &[plurx_core::domain::SubtitleStream], language: &str) -> bool {
    !tracks
        .iter()
        .any(|s| !s.forced && plurx_core::tracks::lang_matches(&s.language, language))
}

async fn automatic_file(
    state: &AppState,
    id: i64,
    languages: &[String],
    provider: &Provider,
    lease: &crate::job_lease::ActiveJobLease,
) -> Result<(), ApiError> {
    for language in languages {
        let Some(file) = state.store.get_file(id).await? else {
            return Ok(());
        };
        if !missing_language(&file.subtitle_streams, language)
            || file.downloaded_subtitles.len() >= plurx_core::store::MAX_DOWNLOADED_SUBTITLES
        {
            continue;
        }
        let Ok((file, search)) = search_request(state, id, language).await else {
            continue;
        };
        if search.moviehash.is_none() {
            continue;
        }
        let results = provider.search(&search).await.map_err(provider_error)?;
        if let Some(candidate) = results.iter().find(|c| automatic_candidate(c, language)) {
            save_candidate(state, &file, provider, candidate, lease).await?;
        }
    }
    Ok(())
}

fn automatic_candidate(candidate: &Candidate, language: &str) -> bool {
    candidate.hash_match
        && !candidate.forced
        && !candidate.ai_translated
        && !candidate.machine_translated
        && candidate.language.eq_ignore_ascii_case(language)
}

#[derive(Deserialize)]
pub struct SearchQuery {
    pub language: String,
}

pub async fn search(
    _user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Value>, ApiError> {
    let provider = provider(&state).await?;
    let (file, search) = search_request(&state, id, &query.language).await?;
    let results = provider.search(&search).await.map_err(provider_error)?;
    Ok(Json(
        json!({"results": results, "downloaded": file.downloaded_subtitles.iter().map(|s|s.provider_file_id).collect::<Vec<_>>()}),
    ))
}

pub(crate) async fn search_request(
    state: &AppState,
    id: i64,
    language: &str,
) -> Result<(MediaFile, Search), ApiError> {
    if !crate::online_subtitles::valid_language(language) {
        return Err(ApiError::BadRequest("Choose a subtitle language".into()));
    }
    let file = state
        .store
        .get_file(id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    if !file.probed {
        return Err(ApiError::Conflict(
            "Wait for this media file to finish scanning before downloading subtitles".into(),
        ));
    }
    let item = state
        .store
        .get_item(file.item_id)
        .await?
        .ok_or(ApiError::NotFound("item"))?;
    if !matches!(item.kind, ItemKind::Movie | ItemKind::Episode) {
        return Err(ApiError::BadRequest(
            "Online subtitle search is available for movies and episodes".into(),
        ));
    }
    let query = if item.kind == ItemKind::Episode {
        // An episode title alone often matches a completely different movie.
        // The filename retains its show and season/episode identity.
        file.path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&item.title)
            .to_owned()
    } else {
        format!(
            "{} {}",
            item.title,
            item.year.map(|v| v.to_string()).unwrap_or_default()
        )
    };
    let hash = movie_hash(&file).await;
    Ok((
        file,
        Search {
            language: language.into(),
            imdb_id: item.imdb_id,
            tmdb_id: item.tmdb_id,
            query: Some(query),
            moviehash: hash,
        },
    ))
}

// OpenSubtitles' 64-bit little-endian sum of file size and the first/last
// 64 KiB. Read only those blocks from the same revision-checked handle.
async fn movie_hash(file: &MediaFile) -> Option<String> {
    let source = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        crate::fragment_index_cluster::open_source_fence(file, None),
    )
    .await
    .ok()?
    .ok()?;
    let size = u64::try_from(file.size).ok()?;
    if size < 128 * 1024 {
        return None;
    }
    let mtime = file.mtime;
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::task::spawn_blocking(move || hash_held_source(source.handle, size, mtime)),
    )
    .await
    .ok()?
    .ok()?
}

fn hash_held_source(mut handle: std::fs::File, size: u64, mtime: i64) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let before = handle.metadata().ok()?;
    let stamp = crate::fragment_index_cluster::source_stamp(&before);
    if !before.is_file() || size < 128 * 1024 || stamp.size != size || stamp.mtime != mtime {
        return None;
    }
    let mut sum = size;
    let mut block = [0u8; 65536];
    for offset in [0, size - 65536] {
        handle.seek(SeekFrom::Start(offset)).ok()?;
        handle.read_exact(&mut block).ok()?;
        for word in block.chunks_exact(8) {
            sum = sum.wrapping_add(u64::from_le_bytes(word.try_into().ok()?));
        }
    }
    let after = handle.metadata().ok()?;
    if crate::fragment_index_cluster::source_stamp(&after) != stamp
        || after.modified().ok()? != before.modified().ok()?
    {
        return None;
    }
    Some(format!("{sum:016x}"))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DownloadRequest {
    language: String,
    provider_file_id: i64,
}

pub async fn download(
    _user: AuthUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
    Json(request): Json<DownloadRequest>,
) -> Result<Json<Value>, ApiError> {
    let lease = state
        .jobs
        .acquire_job(format!("subtitles:file:{id}"))
        .await?
        .ok_or_else(|| {
            ApiError::Conflict(
                "Another subtitle download is already running for this file. Try again shortly."
                    .into(),
            )
        })?;
    let lost = lease.loss_token();
    let result = tokio::select! {
        _ = lost.cancelled() => Err(ApiError::ServiceUnavailable("Subtitle download ownership changed. Try again.".into())),
        result = download_selected(&state, id, &request, &lease) => result,
    };
    lease.release().await?;
    result.map(Json)
}

async fn download_selected(
    state: &AppState,
    id: i64,
    request: &DownloadRequest,
    lease: &crate::job_lease::ActiveJobLease,
) -> Result<Value, ApiError> {
    let (file, search) = search_request(state, id, &request.language).await?;
    if let Some(index) = downloaded_index(&file, request.provider_file_id) {
        return Ok(json!({"subtitle_index":index,"already_downloaded":true}));
    }
    if file.downloaded_subtitles.len() >= plurx_core::store::MAX_DOWNLOADED_SUBTITLES {
        return Err(ApiError::Conflict(
            "This file already has eight downloaded subtitle tracks".into(),
        ));
    }
    let provider = provider(state).await?;
    let result = provider.search(&search).await.map_err(provider_error)?;
    let candidate = result
        .into_iter()
        .find(|c| {
            c.file_id == request.provider_file_id
                && c.language.eq_ignore_ascii_case(&request.language)
        })
        .ok_or_else(|| {
            ApiError::Conflict(
                "That subtitle is no longer in the search results. Search again.".into(),
            )
        })?;
    save_candidate(state, &file, &provider, &candidate, lease).await
}

fn downloaded_index(file: &MediaFile, provider_id: i64) -> Option<usize> {
    file.downloaded_subtitles
        .iter()
        .position(|t| t.provider_file_id == provider_id)
        .map(|n| file.subtitle_streams.len() - file.downloaded_subtitles.len() + n)
}

async fn save_candidate(
    state: &AppState,
    file: &MediaFile,
    provider: &Provider,
    candidate: &Candidate,
    lease: &crate::job_lease::ActiveJobLease,
) -> Result<Value, ApiError> {
    let vtt = provider
        .download(candidate.file_id)
        .await
        .map_err(provider_error)?;
    let track = DownloadedSubtitle {
        source_size: file.size,
        source_mtime: file.mtime,
        provider_file_id: candidate.file_id,
        language: candidate.language.clone(),
        title: format!("OpenSubtitles · {}", candidate.release),
        forced: candidate.forced,
        hearing_impaired: candidate.hearing_impaired,
        vtt,
    };
    if !lease
        .publisher(state.store.as_ref())
        .add_downloaded_subtitle(file.id, &track)
        .await?
    {
        return Err(ApiError::Conflict("The media changed or this subtitle was already added. Refresh the title before trying again.".into()));
    }
    let current = state
        .store
        .get_file(file.id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let index = downloaded_index(&current, candidate.file_id).ok_or_else(|| {
        ApiError::Conflict(
            "The media changed after the subtitle was downloaded. Refresh the title.".into(),
        )
    })?;
    Ok(json!({"subtitle_index":index,"already_downloaded":false}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitle_hash_rejects_a_replacement_before_catalog_rescan() {
        let source = tempfile::tempfile().expect("source");
        source.set_len(128 * 1024).expect("size");
        let stamp =
            crate::fragment_index_cluster::source_stamp(&source.metadata().expect("metadata"));
        assert_eq!(
            hash_held_source(source.try_clone().expect("clone"), stamp.size, stamp.mtime),
            Some("0000000000020000".into())
        );
        source.set_len(stamp.size + 1).expect("replace size");
        assert!(
            hash_held_source(source.try_clone().expect("clone"), stamp.size, stamp.mtime).is_none()
        );
        source.set_len(stamp.size).expect("restore size");
        source
            .set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_secs((stamp.mtime + 5) as u64),
            )
            .expect("replace mtime");
        assert!(hash_held_source(source, stamp.size, stamp.mtime).is_none());
    }

    #[test]
    fn automatic_acquisition_requires_complete_untranslated_file_matches() {
        let mut candidate = Candidate {
            file_id: 1,
            language: "en".into(),
            release: "Example".into(),
            hearing_impaired: false,
            forced: false,
            hash_match: false,
            downloads: 1,
            ai_translated: false,
            machine_translated: false,
        };
        assert!(!automatic_candidate(&candidate, "en"));
        candidate.hash_match = true;
        assert!(automatic_candidate(&candidate, "en"));
        assert!(!automatic_candidate(&candidate, "fr"));
        candidate.forced = true;
        assert!(!automatic_candidate(&candidate, "en"));
        candidate.forced = false;
        candidate.machine_translated = true;
        assert!(!automatic_candidate(&candidate, "en"));
        candidate.machine_translated = false;
        candidate.ai_translated = true;
        assert!(!automatic_candidate(&candidate, "en"));
    }

    #[test]
    fn forced_tracks_do_not_satisfy_a_missing_full_language() {
        let mut track = plurx_core::domain::SubtitleStream {
            language: Some("eng".into()),
            forced: true,
            ..Default::default()
        };
        assert!(missing_language(&[track.clone()], "en"));
        track.forced = false;
        assert!(!missing_language(&[track.clone()], "en"));
        assert!(missing_language(&[track], "fr"));
    }
}
