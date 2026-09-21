//! Resumable subject matching; matching never runs on the playback path.
pub(crate) mod local;

use crate::{
    http::{error::ApiError, extract::AuthUser},
    state::AppState,
};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use plurx_core::{
    channel_subjects::*,
    library_channels::{
        subject_candidates, ChannelCandidate, ChannelMatch, LibraryChannel, LibraryChannelRecipe,
        CHANNEL_CANDIDATE_ROWS_MAX,
    },
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{OnceLock, RwLock},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const ENABLE_KEY: &str = "library_channel_subject_matching.enabled";
fn now() -> i64 {
    crate::media_sessions::unix_ms()
}
fn unavailable(e: impl std::fmt::Display) -> ApiError {
    ApiError::typed(
        StatusCode::SERVICE_UNAVAILABLE,
        "subject_unavailable",
        e.to_string(),
    )
}
fn invalid(e: impl std::fmt::Display) -> ApiError {
    ApiError::typed(
        StatusCode::BAD_REQUEST,
        "invalid_subject_request",
        e.to_string(),
    )
}
#[derive(Clone, Default, Serialize)]
pub struct Observation {
    pub profile: Option<String>,
    pub error: Option<String>,
    pub pending: usize,
    pub metadata_total: usize,
    pub missing_overviews: usize,
    pub truncated: usize,
}
fn observations() -> &'static RwLock<Observation> {
    static CELL: OnceLock<RwLock<Observation>> = OnceLock::new();
    CELL.get_or_init(|| RwLock::new(Observation::default()))
}
pub fn observation() -> Observation {
    observations()
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}
fn observe(f: impl FnOnce(&mut Observation)) {
    f(&mut observations()
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner));
}
pub fn router() -> Router<AppState> {
    Router::new()
        .route("/subject-previews", post(create_preview))
        .route(
            "/subject-previews/{id}",
            get(read_preview).delete(cancel_preview),
        )
}

#[derive(Deserialize)]
struct PreviewRequest {
    recipe: LibraryChannelRecipe,
    request_id: String,
    preview_seed: Option<String>,
}
#[derive(Deserialize, Default)]
struct Page {
    cursor: Option<String>,
    verdict: Option<Verdict>,
}
#[derive(Serialize)]
pub struct Summary {
    pub job_id: String,
    pub state: String,
    #[serde(flatten)]
    pub counts: Counts,
    pub complete: bool,
    pub error: Option<String>,
    pub result_revision: u64,
}
impl From<&SubjectJob> for Summary {
    fn from(j: &SubjectJob) -> Self {
        Self {
            job_id: j.id.clone(),
            state: j.state.clone(),
            counts: j.counts.clone(),
            complete: j.state == "complete",
            error: j.error.clone(),
            result_revision: j.result_revision,
        }
    }
}
#[derive(Serialize)]
struct Row {
    item_id: String,
    title: String,
    #[serde(flatten)]
    decision: SubjectDecision,
}
#[derive(Serialize)]
struct Envelope {
    #[serde(flatten)]
    summary: Summary,
    rows: Vec<Row>,
    next_cursor: Option<String>,
    preview_seed: String,
}
async fn create_preview(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Json(body): Json<PreviewRequest>,
) -> Result<(StatusCode, Json<Envelope>), ApiError> {
    if body.request_id.trim().is_empty() || body.request_id.len() > 128 {
        return Err(invalid("request_id must contain 1–128 characters"));
    }
    let recipe = body.recipe.normalize().map_err(invalid)?;
    if recipe.subject.is_none() {
        return Err(invalid("provide a subject"));
    }
    let seed = body
        .preview_seed
        .as_deref()
        .map(|s| {
            hex::decode(s)
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b).ok())
                .ok_or_else(|| invalid("invalid preview seed"))
        })
        .transpose()?
        .unwrap_or_else(|| {
            // Retry-stable, account-private preview seed.
            let hash = digest(&(user.id, &body.request_id));
            let mut seed = [0; 32];
            hex::decode_to_slice(hash, &mut seed).expect("sha256 hex");
            seed
        });
    let mut candidate = SubjectJob::new(user.id, recipe, seed, None, now());
    candidate.identity = digest(&(candidate.identity, &body.request_id));
    let job = enqueue(&state, candidate).await?;
    // The creation acknowledgement performs no catalogue scan or provider call.
    Ok((
        StatusCode::ACCEPTED,
        Json(Envelope {
            summary: Summary::from(&job),
            rows: Vec::new(),
            next_cursor: None,
            preview_seed: hex::encode(job.seed),
        }),
    ))
}
async fn owned_job(
    state: &AppState,
    user: &plurx_core::domain::User,
    id: String,
) -> Result<SubjectJob, ApiError> {
    state
        .store
        .subject_job(JobQuery::Id {
            owner: user.id,
            admin: user.is_admin,
            id,
            now: now(),
        })
        .await
        .map_err(unavailable)?
        .ok_or_else(|| {
            ApiError::typed(
                StatusCode::GONE,
                "subject_preview_gone",
                "This preview expired or is unavailable; start a new preview.",
            )
        })
}
async fn read_preview(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(page): Query<Page>,
) -> Result<Json<Envelope>, ApiError> {
    let mut job = owned_job(&state, &user, id).await?;
    let (candidates, snapshot) = catalogue(&state, &job.recipe).await?;
    if job
        .catalogue_digest
        .as_ref()
        .is_some_and(|previous| previous != &snapshot)
    {
        job.state = "superseded".into();
        job.error = Some("Catalogue changed; start a new preview.".into());
        return Ok(Json(Envelope {
            summary: Summary::from(&job),
            rows: vec![],
            next_cursor: None,
            preview_seed: hex::encode(job.seed),
        }));
    }
    let cursor_identity = digest(&(
        job.id.as_str(),
        job.result_revision,
        &snapshot,
        page.verdict,
    ));
    let offset = if let Some(cursor) = page.cursor {
        let (identity, offset) = cursor
            .split_once(':')
            .ok_or_else(|| invalid("invalid cursor"))?;
        if identity != cursor_identity {
            return Err(ApiError::typed(
                StatusCode::CONFLICT,
                "subject_cursor_restart",
                "Results changed; restart from the first page.",
            ));
        }
        offset.parse::<usize>().map_err(invalid)?
    } else {
        0
    };
    let decisions = cached(&state, &job, &candidates).await?;
    let rows = candidates
        .iter()
        .filter_map(|c| decisions.get(&c.candidate.item_id).map(|d| (c, d)))
        .filter(|(_, d)| page.verdict.is_none_or(|v| d.verdict == v))
        .map(|(c, d)| Row {
            item_id: c.candidate.item_id.to_string(),
            title: c.candidate.title.clone(),
            decision: d.clone(),
        })
        .collect::<Vec<_>>();
    let next_cursor = (offset.saturating_add(50) < rows.len())
        .then(|| format!("{cursor_identity}:{}", offset + 50));
    // Read again to fence cache commits or cancellation during page assembly.
    let current = owned_job(&state, &user, job.id.clone()).await?;
    if current.result_revision != job.result_revision || current.state != job.state {
        return Err(ApiError::typed(
            StatusCode::CONFLICT,
            "subject_cursor_restart",
            "Results changed; restart from the first page.",
        ));
    }
    Ok(Json(Envelope {
        summary: Summary::from(&job),
        rows: rows.into_iter().skip(offset).take(50).collect(),
        next_cursor,
        preview_seed: hex::encode(job.seed),
    }))
}
async fn cancel_preview(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    owned_job(&state, &user, id.clone()).await?;
    state
        .store
        .subject_write(JobWrite::Cancel {
            id,
            owner: user.id,
            admin: user.is_admin,
            now: now(),
        })
        .await
        .map_err(unavailable)?;
    Ok(StatusCode::NO_CONTENT)
}
async fn enqueue(state: &AppState, mut job: SubjectJob) -> Result<SubjectJob, ApiError> {
    if job.classifier_profile.is_none() {
        if let Some(previous) = state
            .store
            .subject_job(JobQuery::Profile {
                owner: job.owner_user_id,
                subject: job.subject_digest.clone(),
                now: now(),
            })
            .await
            .map_err(unavailable)?
        {
            job.classifier_profile = previous.classifier_profile;
        }
    }
    state
        .store
        .subject_write(JobWrite::Enqueue(job.clone()))
        .await
        .map_err(unavailable)?;
    state
        .store
        .subject_job(JobQuery::Identity {
            owner: job.owner_user_id,
            identity: job.identity,
            now: now(),
        })
        .await
        .map_err(unavailable)?
        .ok_or_else(|| {
            ApiError::typed(
                StatusCode::TOO_MANY_REQUESTS,
                "subject_job_limit",
                "Too many retained previews; cancel unused previews or wait for expiry.",
            )
        })
}
pub async fn request_channel(state: &AppState, channel: &LibraryChannel) -> Result<(), ApiError> {
    request_channel_activation(state, channel, false).await
}
pub async fn request_channel_activation(
    state: &AppState,
    channel: &LibraryChannel,
    next_programme: bool,
) -> Result<(), ApiError> {
    if channel.recipe.subject.is_none() {
        return Ok(());
    }
    let mut candidate = SubjectJob::new(
        channel.owner_user_id,
        channel.recipe.clone(),
        channel.seed,
        Some((channel.id.clone(), channel.revision)),
        now(),
    );
    candidate.activate_next_programme = next_programme;
    let preview_identity = SubjectJob::new(
        channel.owner_user_id,
        channel.recipe.clone(),
        channel.seed,
        None,
        now(),
    )
    .identity;
    if let Some(preview) = state
        .store
        .subject_job(JobQuery::Identity {
            owner: channel.owner_user_id,
            identity: preview_identity,
            now: now(),
        })
        .await
        .map_err(unavailable)?
    {
        candidate.classifier_profile = preview.classifier_profile;
    }
    if let Some(previous) = state
        .store
        .subject_job(JobQuery::Identity {
            owner: channel.owner_user_id,
            identity: candidate.identity.clone(),
            now: now(),
        })
        .await
        .map_err(unavailable)?
    {
        if channel.recipe.auto_refresh && previous.state == "complete" {
            let (_, snapshot) = catalogue(state, &channel.recipe).await?;
            if previous.catalogue_digest.as_ref() != Some(&snapshot)
                || observation()
                    .profile
                    .as_ref()
                    .is_some_and(|profile| previous.classifier_profile.as_ref() != Some(profile))
            {
                state
                    .store
                    .subject_write(JobWrite::Supersede {
                        id: previous.id,
                        now: now(),
                    })
                    .await
                    .map_err(unavailable)?;
            }
        }
    }
    enqueue(state, candidate).await?;
    Ok(())
}
pub async fn summary(state: &AppState, channel: &LibraryChannel) -> Option<Summary> {
    state
        .store
        .subject_job(JobQuery::Channel {
            owner: channel.owner_user_id,
            id: channel.id.clone(),
            revision: channel.revision,
            now: now(),
        })
        .await
        .ok()
        .flatten()
        .as_ref()
        .map(Summary::from)
}
pub async fn catalogue(
    state: &AppState,
    recipe: &LibraryChannelRecipe,
) -> Result<(Vec<ChannelMatch>, String), ApiError> {
    let mut raw = state
        .store
        .library_channel_catalog_snapshot((CHANNEL_CANDIDATE_ROWS_MAX + 1) as i64)
        .await
        .map_err(unavailable)?;
    if raw.len() > CHANNEL_CANDIDATE_ROWS_MAX {
        return Err(invalid(
            "Catalogue exceeds 100,000 rows; narrow the library scope.",
        ));
    }
    let snapshot = digest(&raw);
    raw.sort_by_key(|c| (c.item_id, c.file_id));
    Ok((subject_candidates(recipe, raw).map_err(invalid)?, snapshot))
}
async fn cached(
    state: &AppState,
    job: &SubjectJob,
    candidates: &[ChannelMatch],
) -> Result<BTreeMap<i64, SubjectDecision>, ApiError> {
    let mut result = BTreeMap::new();
    let Some(profile) = job.classifier_profile.as_deref() else {
        return Ok(result);
    };
    let hashes = candidates
        .iter()
        .map(|c| (c.candidate.item_id, Metadata::digest(&c.candidate)))
        .collect::<BTreeMap<_, _>>();
    for chunk in candidates.chunks(200) {
        let ids = chunk
            .iter()
            .map(|c| (c.candidate.item_id, Metadata::digest(&c.candidate)))
            .collect::<Vec<_>>();
        let rows = state
            .store
            .subject_decisions(job.owner_user_id, &job.subject_digest, profile, &ids)
            .await
            .map_err(unavailable)?;
        if rows
            .iter()
            .any(|row| row.last_used_ms < now() - PREVIEW_TTL_MS)
        {
            state
                .store
                .subject_write(JobWrite::Touch {
                    owner: job.owner_user_id,
                    subject: job.subject_digest.clone(),
                    profile: profile.to_owned(),
                    keys: ids.clone(),
                    now: now(),
                })
                .await
                .map_err(unavailable)?;
        }
        for row in rows {
            if hashes.get(&row.item_id) == Some(&row.metadata_digest) {
                result.entry(row.item_id).or_insert(row.decision);
            }
        }
    }
    Ok(result)
}
fn counts(candidates: &[ChannelMatch], decisions: &BTreeMap<i64, SubjectDecision>) -> Counts {
    let mut counts = Counts {
        total: candidates.len(),
        ..Counts::default()
    };
    for c in candidates {
        if let Some(d) = decisions.get(&c.candidate.item_id) {
            counts.processed += 1;
            match d.verdict {
                Verdict::Match => counts.matched += 1,
                Verdict::NoMatch => counts.rejected += 1,
                Verdict::Uncertain => counts.uncertain += 1,
            }
        }
    }
    counts
}
async fn enabled(state: &AppState) -> Result<bool, ApiError> {
    Ok(plurx_core::store::stored_switch(
        state
            .store
            .get_setting(ENABLE_KEY)
            .await
            .map_err(unavailable)?
            .as_deref(),
        true,
    ))
}

async fn classify_enabled(
    state: &AppState,
    provider: &local::LocalSearch,
    subject: &str,
    batch: &[(String, Metadata)],
) -> Result<BTreeMap<String, SubjectDecision>, String> {
    if !enabled(state)
        .await
        .map_err(|_| "provider_setting_unavailable")?
    {
        return Err("provider_paused".into());
    }
    provider.classify(subject, batch).await
}

pub async fn worker(state: AppState, shutdown: CancellationToken) {
    let mut last_maintenance = 0;
    loop {
        if now() - last_maintenance >= 60_000 {
            last_maintenance = now();
            // Best-effort retention cleanup: expired subject rows remain
            // harmless and the minute maintenance loop retries pruning.
            crate::store_result::observe(
                crate::store_result::Operation::PruneChannelSubjects,
                crate::store_result::Discard::BestEffort,
                state
                    .store
                    .subject_write(JobWrite::Prune { now: now() })
                    .await,
            );
            if let Ok(provider) = local::LocalSearch::configured() {
                match provider.profile().await {
                    Ok(profile) => observe(|o| {
                        o.profile = Some(profile);
                        o.error = None;
                    }),
                    Err(error) => observe(|o| {
                        o.profile = None;
                        o.error = Some(error);
                    }),
                }
            }
        }
        let result = turn(&state, &shutdown).await;
        if let Err(error) = result {
            tracing::warn!(error=?error,"Subject worker turn failed");
        }
        tokio::select! {_=shutdown.cancelled()=>return,_=tokio::time::sleep(Duration::from_secs(2))=>{}}
    }
}
pub(crate) async fn turn(state: &AppState, shutdown: &CancellationToken) -> Result<(), ApiError> {
    let Some(mut job) = state
        .store
        .subject_job(JobQuery::Pending { now: now() })
        .await
        .map_err(unavailable)?
    else {
        observe(|o| o.pending = 0);
        return Ok(());
    };
    observe(|o| o.pending = 1);
    let claim = uuid::Uuid::new_v4().to_string();
    if !state
        .store
        .subject_write(JobWrite::Claim {
            id: job.id.clone(),
            claim: claim.clone(),
            now: now(),
        })
        .await
        .map_err(unavailable)?
    {
        return Ok(());
    }
    let mut renewal = tokio::time::interval(Duration::from_secs(20));
    renewal.tick().await;
    let job_id = job.id.clone();
    let work = process(state, &mut job, &claim);
    tokio::pin!(work);
    loop {
        tokio::select! {
            result=&mut work=>return result,
            _=shutdown.cancelled()=>return Ok(()),
            _=renewal.tick()=>{
                if !state.store.subject_write(JobWrite::Renew{id:job_id.clone(),claim:claim.clone(),now:now()}).await.map_err(unavailable)? {return Ok(());}

            }
        }
    }
}
async fn process(state: &AppState, job: &mut SubjectJob, claim: &str) -> Result<(), ApiError> {
    job.error = None;
    let mut new_decisions = Vec::new();
    let result = process_inner(state, job, claim, &mut new_decisions).await;
    if let Err(error) = result {
        job.error = Some(format!("{error:?}"));
        job.state = if matches!(
            error,
            ApiError::Conflict(_)
                | ApiError::Typed {
                    status: StatusCode::CONFLICT,
                    ..
                }
        ) {
            "superseded"
        } else {
            "failed"
        }
        .into();
    }
    job.result_revision = job.result_revision.saturating_add(1);
    let delay = if job.state == "waiting_for_provider" {
        10_000
    } else {
        0
    };
    observe(|o| {
        o.error = job.error.clone();
    });
    let committed = state
        .store
        .subject_write(JobWrite::Commit {
            job: job.clone(),
            claim: claim.to_owned(),
            decisions: new_decisions,
            now: now(),
            delay_ms: delay,
        })
        .await
        .map_err(unavailable)?;
    if committed && job.state == "superseded" && job.channel_id.is_some() {
        let mut replacement = job.clone();
        replacement.id = uuid::Uuid::new_v4().to_string();
        replacement.state = "queued".into();
        replacement.created_ms = now();
        replacement.catalogue_digest = None;
        replacement.result_revision = 0;
        replacement.counts = Counts::default();
        replacement.error = None;
        replacement.published_ms = None;
        enqueue(state, replacement).await?;
    }
    Ok(())
}
async fn process_inner(
    state: &AppState,
    job: &mut SubjectJob,
    claim: &str,
    new_decisions: &mut Vec<CachedDecision>,
) -> Result<(), ApiError> {
    let (mut candidates, snapshot) = catalogue(state, &job.recipe).await?;
    if job
        .catalogue_digest
        .as_ref()
        .is_some_and(|previous| previous != &snapshot)
    {
        job.state = "superseded".into();
        job.error = Some(
            "Catalogue changed; start a fresh preview. Saved matching resumes automatically."
                .into(),
        );
        return Ok(());
    }
    // Restart reconstruction always derives progress from current metadata keys.
    job.catalogue_digest = Some(snapshot.clone());
    job.counts.total = candidates.len();
    if candidates.is_empty() {
        job.state = "complete".into();
        return Ok(());
    }
    if job.classifier_profile.is_none() {
        if let Some(previous) = state
            .store
            .subject_job(JobQuery::Profile {
                owner: job.owner_user_id,
                subject: job.subject_digest.clone(),
                now: now(),
            })
            .await
            .map_err(unavailable)?
        {
            job.classifier_profile = previous.classifier_profile;
        }
    }
    let provider = local::LocalSearch::configured().map_err(unavailable)?;
    let profile_result = provider.profile().await;
    match &profile_result {
        Ok(profile) => {
            observe(|o| o.profile = Some(profile.clone()));
            job.classifier_profile = Some(profile.clone());
        }
        Err(_) => observe(|o| o.profile = None),
    }
    let initial = cached(state, job, &candidates).await?;
    let immediate = candidates
        .iter()
        .filter(|c| {
            c.candidate.explicitly_included
                || initial
                    .get(&c.candidate.item_id)
                    .is_some_and(|d| d.verdict == Verdict::Match)
        })
        .cloned()
        .collect::<Vec<_>>();
    job.counts = counts(&candidates, &initial);
    job.selection_count = immediate.len();
    if !immediate.is_empty()
        && (job.published_ms.is_none() || job.counts.processed == job.counts.total)
    {
        if let Some(id) = &job.channel_id {
            if let Some(channel) = state
                .store
                .get_library_channel(job.owner_user_id, false, id)
                .await
                .map_err(unavailable)?
            {
                if Some(channel.revision) == job.channel_revision {
                    crate::http::library_channels::publish_subject(
                        state,
                        &channel,
                        immediate,
                        snapshot.clone(),
                        &job.id,
                        claim,
                        job.activate_next_programme,
                    )
                    .await?;
                    job.published_ms = Some(now());
                }
            }
        }
    }
    if job.counts.processed == job.counts.total {
        job.state = "complete".into();
        job.error = profile_result.err();
        return Ok(());
    }
    if let Err(error) = profile_result {
        job.state = "waiting_for_provider".into();
        job.error = Some(error);
        job.counts = counts(&candidates, &initial);
        return Ok(());
    }
    let mut decisions = cached(state, job, &candidates).await?;
    observe(|o| {
        o.metadata_total = candidates.len();
        o.missing_overviews = candidates
            .iter()
            .filter(|c| c.candidate.overview.trim().is_empty())
            .count();
        o.truncated = candidates
            .iter()
            .filter(|c| Metadata::from_candidate_local(&c.candidate).truncated)
            .count();
    });
    // Lexical priority changes time to feedback only; every scoped item is visited.
    let words = job
        .recipe
        .subject
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>();
    candidates.sort_by_key(|c| {
        let text = format!("{} {}", c.candidate.title, c.candidate.overview).to_lowercase();
        (
            std::cmp::Reverse(
                words
                    .iter()
                    .filter(|w| w.len() > 3 && text.contains(w.as_str()))
                    .count(),
            ),
            c.candidate.item_id,
        )
    });
    let mut batch: Vec<(String, Metadata)> = Vec::new();
    let mut selected: Vec<&ChannelCandidate> = Vec::new();
    for c in &candidates {
        if decisions.contains_key(&c.candidate.item_id) {
            continue;
        }
        let next = (
            format!("b{}", batch.len()),
            Metadata::from_candidate_local(&c.candidate),
        );
        batch.push(next);
        selected.push(&c.candidate);
        if batch.len() == 128 {
            break;
        }
    }
    if !batch.is_empty() {
        if !enabled(state).await? {
            job.state = "waiting_for_provider".into();
            job.error = Some("Local rule matching is paused in Settings → Live TV.".into());
            job.counts = counts(&candidates, &decisions);
            return Ok(());
        }
        // A changed artifact never contaminates a frozen profile.
        match provider.profile().await {
            Ok(profile) if Some(&profile) == job.classifier_profile.as_ref() => {}
            Ok(_) => {
                job.classifier_profile = None;
                job.state = "queued".into();
                job.error = Some("Model changed; reconstructing decisions.".into());
                return Ok(());
            }
            Err(e) => {
                job.state = "waiting_for_provider".into();
                job.error = Some(e);
                return Ok(());
            }
        }
        let subject = job.recipe.subject.as_deref().unwrap_or_default();
        let mut response = classify_enabled(state, &provider, subject, &batch).await;
        if response.is_err()
            && response
                .as_ref()
                .err()
                .is_none_or(|e| e != "provider_paused")
        {
            response = classify_enabled(state, &provider, subject, &batch).await;
        }
        let mut response = match response {
            Ok(rows) => rows,
            Err(e) => {
                job.state = if e.contains("unreachable")
                    || e.contains("timeout")
                    || e.contains("http_")
                    || e.contains("paused")
                    || e.contains("setting_unavailable")
                {
                    "waiting_for_provider"
                } else {
                    "failed"
                }
                .into();
                job.error = Some(e);
                job.counts = counts(&candidates, &decisions);
                return Ok(());
            }
        };
        for ((id, metadata), candidate) in batch.iter().zip(selected) {
            let mut paused = false;
            if !response.contains_key(id) {
                match classify_enabled(state, &provider, subject, &[(id.clone(), metadata.clone())])
                    .await
                {
                    Ok(row) => response.extend(row),
                    Err(error) => {
                        paused =
                            error == "provider_paused" || error == "provider_setting_unavailable"
                    }
                }
            }
            if let Some(decision) = response.remove(id) {
                new_decisions.push(CachedDecision {
                    last_used_ms: now(),
                    item_id: candidate.item_id,
                    metadata_digest: Metadata::digest(candidate),
                    decision: decision.clone(),
                });
                decisions.insert(candidate.item_id, decision);
            } else {
                job.error = Some(
                    if !paused && enabled(state).await? {
                        "provider_invalid_or_missing_row_after_retry"
                    } else {
                        "provider_paused"
                    }
                    .into(),
                );
            }
        }
    }
    if !new_decisions.is_empty() {
        match provider.profile().await {
            Ok(profile) if job.classifier_profile.as_ref() == Some(&profile) => {}
            other => {
                new_decisions.clear();
                job.state = "queued".into();
                job.error = Some("Local classifier version changed during matching; retrying without caching that response.".into());
                observe(|o| o.profile = other.ok());
                return Ok(());
            }
        }
    }
    job.counts = counts(&candidates, &decisions);
    job.state = if job.error.as_deref() == Some("provider_paused") {
        "waiting_for_provider"
    } else if job.error.is_some() {
        "failed"
    } else if job.counts.processed == job.counts.total {
        "complete"
    } else {
        "queued"
    }
    .into();
    let matches = candidates
        .into_iter()
        .filter(|c| {
            c.candidate.explicitly_included
                || decisions
                    .get(&c.candidate.item_id)
                    .is_some_and(|d| d.verdict == Verdict::Match)
        })
        .map(|mut c| {
            if let Some(d) = decisions.get(&c.candidate.item_id) {
                c.reasons.push(d.reason.clone());
            }
            c
        })
        .collect::<Vec<_>>();
    job.selection_count = matches.len();
    if let Some(id) = &job.channel_id {
        if let Some(channel) = state
            .store
            .get_library_channel(job.owner_user_id, false, id)
            .await
            .map_err(unavailable)?
        {
            if Some(channel.revision) != job.channel_revision {
                job.state = "superseded".into();
                return Ok(());
            }
            let should_publish = channel.active_generation_id.is_none() || job.state == "complete";
            if should_publish && !matches.is_empty() {
                crate::http::library_channels::publish_subject(
                    state,
                    &channel,
                    matches,
                    snapshot,
                    &job.id,
                    claim,
                    job.activate_next_programme,
                )
                .await?;
                job.published_ms = Some(now());
            }
            if job.state == "complete" && job.counts.matched == 0 {
                job.error=Some("No automatic matches; any existing schedule is retained. Explicit selections still apply.".into());
            }
        }
    }
    Ok(())
}
