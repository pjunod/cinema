//! Each node installs, by itself, the models the saved AI subtitles settings
//! need, and repairs them.
//!
//! The decision runs on the node's own subtitle worker loop and is about
//! that node only. When `from_audio` is on the node needs the speech models
//! of the current quality tier ([`crate::ai_subtitles::required_speech_models`]);
//! when `translate_text` is on it needs the text model bundle. A model that is
//! missing, left as an unaccepted cache by a cancelled install, changed on
//! disk, or failed its test gets a server-owned *install* job: the same
//! fenced model operation an admin's Install runs, so only its durable
//! successful receipt makes the node ready. A model with an accepted receipt
//! from an earlier boot is left to the automatic per-boot re-test.
//!
//! At most one self-install per model is outstanding: the job is remembered
//! here and its dedupe key names this boot and the attempt number. A failed
//! attempt backs off 15 min, then 1 h, then 6 h. The outcome is reported in
//! the worker observation as [`Requirement`] rows, which the AI subtitles
//! settings page reads cluster-wide.
use super::{catalog, enqueue_model_operation, AppState, CatalogModel, ModelAction};
use plurx_core::{
    error::StoreError,
    store::background_jobs::{
        EnqueueJob, EnqueueOutcome, JobKind, JobPayload, JobRequest, JobState,
    },
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use tokio::sync::RwLock;

/// Dedupe-key suffix prefix of a self-install: `self-install:{boot}:{attempt}`.
pub(crate) const KEY_PREFIX: &str = "self-install:";
const BACKOFF_MS: [i64; 3] = [15 * 60_000, 60 * 60_000, 6 * 60 * 60_000];

/// Delay before retrying after `failures` consecutive failed attempts.
pub(crate) fn backoff_ms(failures: u32) -> i64 {
    let index = usize::try_from(failures.max(1) - 1).unwrap_or(usize::MAX);
    BACKOFF_MS[index.min(BACKOFF_MS.len() - 1)]
}

/// A conservative working-set estimate for running a model: twice its file.
pub(crate) fn memory_footprint(download_bytes: u64) -> u64 {
    download_bytes.saturating_mul(2)
}

/// One model the settings need on this node, as the node reports it.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Requirement {
    pub model_id: String,
    /// `ready`, `downloading`, `failed` or `unsupported`.
    pub state: String,
    #[serde(default)]
    pub reason: Option<String>,
    /// When this boot's test of the model last passed.
    #[serde(default)]
    pub tested_at_ms: Option<i64>,
    /// When a failed self-install is next tried.
    #[serde(default)]
    pub retry_at_ms: Option<i64>,
}
impl Requirement {
    pub(crate) fn bounded(&self) -> bool {
        self.model_id.len() <= 64
            && matches!(
                self.state.as_str(),
                "ready" | "downloading" | "failed" | "unsupported"
            )
            && self.reason.as_ref().is_none_or(|reason| {
                reason.len() <= 64
                    && reason.bytes().all(|byte| {
                        byte.is_ascii_lowercase() || byte == b'_' || byte.is_ascii_digit()
                    })
            })
            && self.tested_at_ms.is_none_or(|at| at >= 0)
            && self.retry_at_ms.is_none_or(|at| at >= 0)
    }
    fn new(model_id: &str, state: &str, reason: Option<&str>) -> Self {
        Self {
            model_id: model_id.into(),
            state: state.into(),
            reason: reason.map(Into::into),
            tested_at_ms: None,
            retry_at_ms: None,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Attempt {
    job_id: Option<String>,
    failures: u32,
    retry_at_ms: i64,
    reason: Option<String>,
}

/// This process's self-install attempts and its last reported requirements.
#[derive(Default)]
pub(crate) struct Attempts {
    rows: RwLock<BTreeMap<String, Attempt>>,
    observed: RwLock<Option<Vec<Requirement>>>,
}
impl Attempts {
    pub(crate) async fn observed(&self) -> Option<Vec<Requirement>> {
        self.observed.read().await.clone()
    }
}

/// What this node offers a model, gathered once per pass.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Facts {
    /// The packaged speech engine inspects as available and qualified.
    pub speech_engine: bool,
    /// The in-process text engine runs on this platform.
    pub text_engine: bool,
    /// Free bytes under the model store.
    pub free_disk_bytes: Option<u64>,
    /// This node's memory ([`crate::node_memory::node_memory_bytes`]).
    pub memory_bytes: Option<u64>,
    pub may_execute_speech: bool,
    pub may_execute_text: bool,
}
impl Facts {
    async fn local(state: &AppState) -> Self {
        let speech_engine = state
            .subtitle_models
            .local_observation()
            .await
            .and_then(|observation| observation.engine)
            .is_some_and(|engine| engine.status == "available");
        let authority = state.jobs.execution_authority();
        Self {
            speech_engine,
            text_engine: cfg!(unix),
            free_disk_bytes: free_disk_bytes(&state.subtitle_models.root).await,
            memory_bytes: crate::node_memory::node_memory_bytes(),
            may_execute_speech: authority
                .may_execute_job(JobKind::SubtitleModelOperation)
                .await,
            may_execute_text: authority
                .may_execute_job(JobKind::SubtitleTextModelOperation)
                .await,
        }
    }
}

/// Free space for the model store: the store itself, or the nearest
/// existing directory above it before the first install creates it.
async fn free_disk_bytes(root: &std::path::Path) -> Option<u64> {
    if !root.is_absolute() {
        return None;
    }
    let mut path = root;
    for _ in 0..8 {
        if tokio::fs::metadata(path)
            .await
            .is_ok_and(|meta| meta.is_dir())
        {
            return crate::live_tv::free_space_bytes(path.to_str()?).await;
        }
        path = path.parent()?;
    }
    None
}

/// Whether the node can hold and run a model of `download_bytes`.
fn fits(download_bytes: u64, needs_download: bool, facts: &Facts) -> Result<(), &'static str> {
    let memory = facts.memory_bytes.ok_or("memory_unknown")?;
    if memory <= memory_footprint(download_bytes) {
        return Err("insufficient_memory");
    }
    if needs_download {
        let free = facts.free_disk_bytes.ok_or("disk_unknown")?;
        if free <= download_bytes.saturating_mul(2) {
            return Err("insufficient_disk");
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Target<'a> {
    Speech(&'a CatalogModel),
    Text,
}
impl Target<'_> {
    fn model_id(&self) -> &str {
        match self {
            Self::Speech(spec) => &spec.id,
            Self::Text => crate::subtitle_text_translation::MODEL_ID,
        }
    }
}

/// Decide and enqueue this node's self-installs from the saved settings, and
/// publish the outcome in the local worker observation.
pub(crate) async fn pass(state: &AppState) -> Result<(), StoreError> {
    let settings = crate::ai_subtitles::settings::read_store(state.store.as_ref())
        .await
        .map_err(|_| StoreError::Task("ai_subtitles_settings_unavailable".into()))?
        .settings;
    let facts = Facts::local(state).await;
    pass_with(state, &settings, &facts, crate::state::clock_ms()).await?;
    Ok(())
}

pub(crate) async fn pass_with(
    state: &AppState,
    settings: &crate::ai_subtitles::settings::Settings,
    facts: &Facts,
    now_ms: i64,
) -> Result<Vec<Requirement>, StoreError> {
    let observation = state.subtitle_models.local_observation().await;
    let mut rows = Vec::new();
    if settings.from_audio {
        for id in crate::ai_subtitles::required_speech_models(settings.quality) {
            let Some(spec) = catalog().models.iter().find(|model| model.id == *id) else {
                rows.push(Requirement::new(
                    id,
                    "unsupported",
                    Some("model_unregistered"),
                ));
                continue;
            };
            let (status, tested) = observation
                .as_ref()
                .and_then(|observation| {
                    observation
                        .models
                        .iter()
                        .find(|model| model.model_id == *id)
                })
                .map_or(("unknown", None), |model| {
                    (model.status.as_str(), model.verified_at_ms)
                });
            rows.push(step(state, Target::Speech(spec), status, tested, facts, now_ms).await?);
        }
    }
    if settings.translate_text {
        let (status, tested) = text_status(state).await?;
        rows.push(step(state, Target::Text, status, tested, facts, now_ms).await?);
    }
    *state.subtitle_models.self_install.observed.write().await = Some(rows.clone());
    if let Some((_, observation)) = state.subtitle_models.local.write().await.as_mut() {
        observation.requirements = Some(rows.clone());
    }
    Ok(rows)
}

/// The text bundle's status from metadata and this boot's receipt, in the
/// speech models' vocabulary. It never hashes the bundle.
async fn text_status(state: &AppState) -> Result<(&'static str, Option<i64>), StoreError> {
    use crate::subtitle_text_translation::{artifacts, ADAPTER_CONTRACT, MODEL_ID};
    let identity = if state.subtitle_models.root.is_absolute() {
        artifacts::bundle_identity(&state.subtitle_models.root).await
    } else {
        Err("model_store_unavailable".into())
    };
    let identity = match identity {
        Ok(identity) => identity,
        Err(error) if error == "model_not_installed" || error == "model_store_unavailable" => {
            return Ok(("missing", None))
        }
        Err(_) => return Ok(("unknown", None)),
    };
    let engine = crate::subtitle_text_translation::engine_identity()
        .await
        .ok();
    let receipts = state.store.subtitle_model_receipts(&state.node_id).await?;
    let tested = receipts
        .iter()
        .find(|receipt| {
            receipt.node_id == state.node_id
                && receipt.boot_id == state.subtitle_models.boot_id
                && receipt.model_id == MODEL_ID
                && receipt.model_sha256 == artifacts::expected_bundle_digest()
                && receipt.model_file_identity == identity
                && receipt.adapter_contract == ADAPTER_CONTRACT
                && engine.as_ref().is_some_and(|(digest, file)| {
                    receipt.engine_sha256 == *digest && receipt.engine_file_identity == *file
                })
        })
        .map(|receipt| receipt.verified_at_ms);
    Ok(match tested {
        Some(at) => ("verified", Some(at)),
        None => ("unverified_cache", None),
    })
}

async fn step(
    state: &AppState,
    target: Target<'_>,
    status: &str,
    tested_at_ms: Option<i64>,
    facts: &Facts,
    now_ms: i64,
) -> Result<Requirement, StoreError> {
    let model_id = target.model_id().to_owned();
    let (download_bytes, engine, may_execute) = match target {
        Target::Speech(spec) => (
            spec.download_bytes,
            facts.speech_engine,
            facts.may_execute_speech,
        ),
        Target::Text => (
            crate::subtitle_text_translation::artifacts::bundle_download_bytes(),
            facts.text_engine,
            facts.may_execute_text,
        ),
    };
    let mut attempts = state.subtitle_models.self_install.rows.write().await;
    let attempt = attempts.entry(model_id.clone()).or_default();
    if let Some(job_id) = attempt.job_id.clone() {
        match state.store.background_job(&job_id).await? {
            Some(job)
                if matches!(
                    job.state,
                    JobState::Queued | JobState::Running | JobState::Cancelling
                ) =>
            {
                return Ok(Requirement::new(
                    &model_id,
                    "downloading",
                    Some("installing"),
                ));
            }
            Some(job) if job.state == JobState::Succeeded => *attempt = Attempt::default(),
            Some(job) => {
                attempt.failures = attempt.failures.saturating_add(1);
                attempt.retry_at_ms = now_ms.saturating_add(backoff_ms(attempt.failures));
                attempt.reason = Some(if job.state == JobState::Cancelled {
                    "install_cancelled".into()
                } else {
                    job.last_error_code
                        .filter(|code| Requirement::new("", "failed", Some(code)).bounded())
                        .unwrap_or_else(|| "install_failed".into())
                });
                attempt.job_id = None;
            }
            None => attempt.job_id = None,
        }
    }
    if status == "verified" {
        *attempt = Attempt::default();
        let mut ready = Requirement::new(&model_id, "ready", None);
        ready.tested_at_ms = tested_at_ms;
        return Ok(ready);
    }
    if status == "installed" {
        // An accepted install from an earlier boot: the automatic per-boot
        // re-test qualifies it, not a reinstall.
        return Ok(Requirement::new(
            &model_id,
            "downloading",
            Some("verifying"),
        ));
    }
    if !engine {
        return Ok(Requirement::new(
            &model_id,
            "unsupported",
            Some("engine_unavailable"),
        ));
    }
    if let Err(reason) = fits(
        download_bytes,
        matches!(status, "missing" | "unknown"),
        facts,
    ) {
        return Ok(Requirement::new(&model_id, "unsupported", Some(reason)));
    }
    if attempt.failures > 0 && now_ms < attempt.retry_at_ms {
        let mut failed = Requirement::new(&model_id, "failed", attempt.reason.as_deref());
        failed.retry_at_ms = Some(attempt.retry_at_ms);
        return Ok(failed);
    }
    if !may_execute {
        return Ok(Requirement::new(
            &model_id,
            "downloading",
            Some("waiting_for_worker"),
        ));
    }
    let suffix = format!(
        "{KEY_PREFIX}{}:{}",
        state.subtitle_models.boot_id,
        attempt.failures.min(9999)
    );
    let outcome = match target {
        Target::Speech(spec) => {
            enqueue_model_operation(
                state,
                spec,
                &state.node_id,
                ModelAction::Install,
                Some(&suffix),
            )
            .await?
        }
        Target::Text => enqueue_text_install(state, &suffix, now_ms).await?,
    };
    Ok(match outcome {
        EnqueueOutcome::Accepted { job_id, .. }
        | EnqueueOutcome::Existing {
            job_id,
            cancelled: false,
            ..
        } => {
            attempt.job_id = Some(job_id);
            Requirement::new(&model_id, "downloading", Some("installing"))
        }
        EnqueueOutcome::Existing {
            cancelled: true, ..
        } => {
            attempt.failures = attempt.failures.saturating_add(1);
            attempt.retry_at_ms = now_ms.saturating_add(backoff_ms(attempt.failures));
            attempt.reason = Some("install_cancelled".into());
            let mut failed = Requirement::new(&model_id, "failed", Some("install_cancelled"));
            failed.retry_at_ms = Some(attempt.retry_at_ms);
            failed
        }
        _ => Requirement::new(&model_id, "downloading", Some("queue_busy")),
    })
}

/// The text bundle's self-install: the same text model operation an admin's
/// Install enqueues, owned by the server (`admin_user_id` 0).
async fn enqueue_text_install(
    state: &AppState,
    suffix: &str,
    now_ms: i64,
) -> Result<EnqueueOutcome, StoreError> {
    use crate::subtitle_text_translation::{
        artifacts, ADAPTER_CONTRACT, CATALOG_REVISION, MODEL_ID,
    };
    let payload = JobPayload::SubtitleTextModelOperation {
        action: ModelAction::Install,
        model_id: MODEL_ID.into(),
        catalog_revision: CATALOG_REVISION.into(),
        model_sha256: artifacts::expected_bundle_digest(),
        adapter_contract: ADAPTER_CONTRACT.into(),
        target_node_id: state.node_id.clone(),
        target_boot_id: state.subtitle_models.boot_id.clone(),
        admin_user_id: 0,
    };
    let digest = plurx_core::store::subtitle_translation::digest(&payload)?;
    state
        .store
        .enqueue_job(EnqueueJob {
            id: uuid::Uuid::new_v4().to_string(),
            payload,
            dedupe_key: format!("subtitle-text:{suffix}"),
            priority: 0,
            not_before_ms: now_ms,
            now_ms,
            request: JobRequest {
                scope: "subtitle-text-self-install".into(),
                request_id: uuid::Uuid::new_v4().to_string(),
                request_digest: digest.clone(),
                consumer_kind: "subtitle_text_translation".into(),
                consumer_ref: digest,
                target_node_id: Some(state.node_id.clone()),
                deadline_ms: None,
                retain_identity: false,
            },
        })
        .await
}
