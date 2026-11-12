//! The Analysis consumer of committed copy-range observations.
//!
//! Playback and Source copies send [`CompletedCopyRange`] notifications after
//! their durable commit. This worker coalesces them per exact source. Under the
//! existing Analysis lease it first fills what playback left unobserved, one
//! Analysis-owned copy generation per admitted physical phase, and once the
//! rows cover the whole title it runs the remaining metadata-owned steps: one
//! fenced full-source attestation phase, seal, local blob installation, and the
//! guarded v3 publication through a publisher job. No step here grants media
//! serving or a Source grant; every physical read is a phase's exact custody.
use super::{
    physical, CompletedCopyRange, CopyAnalysisMessage, CopyIndexDefect, CopyIndexDefectKind,
    CopyIndexDemand, CopyObservationAuthority, CopyObservationSet,
};
use plurx_core::store::analysis_coordination::{
    AnalysisCoordinationClaim, AnalysisCoordinationSource,
};
use plurx_core::store::background_jobs::{
    ClaimJob, EnqueueJob, EnqueueOutcome, JobKind, JobPublishOutcome, JobRequest,
    PublishCopyIndexV3, QuarantineCopyIndexV3Head,
};
use plurx_core::store::{
    AnalysisRequest, CopyIndexV3Artifact, CopyIndexV3HeadState, CopyIndexV3Variant,
    FragmentIndexEngineProvenance, NewAnalysisRequest, Store,
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Sources whose observations are retained in memory between notifications.
/// The durable range manifests remain the record; this is only coalescing, so
/// the least recently observed source is the one forgotten.
const MAX_PENDING_SOURCES: usize = 64;

/// Committed ranges of one working source held until its unit returns. A later
/// commit of the same range supersedes its earlier one; past this many ranges
/// the oldest is dropped, and its durable manifest remains the record.
const MAX_DEFERRED_RANGES: usize = 8;

/// Reader-found defects held between passes, one per head. A defect past this
/// is dropped; the next start that meets its head reports it again.
const MAX_PENDING_DEFECTS: usize = 64;

/// What is held of one source between passes.
enum Held {
    /// The rows observed so far, from playback ranges and Analysis fills.
    Rows(Box<CopyObservationSet>),
    /// A demand that carried no rows. The first Analysis generation of the
    /// source turns it into a set; a playback range that arrives first does
    /// the same.
    Demand {
        catalog_namespace: String,
        /// The source as playback observes it, under its own generation.
        observed: Box<AnalysisCoordinationSource>,
        /// The source the build runs for: `observed`, or the same source
        /// under a quarantined head's repair generation.
        source: Box<AnalysisCoordinationSource>,
    },
}

pub(crate) struct PendingSource {
    file: Arc<plurx_core::domain::MediaFile>,
    held: Held,
    /// The recipe of the most recent range or demand; a gap fill repeats it.
    recipe: super::CopyRecipe,
    /// The most recent Source authority this source was observed under. It
    /// joins the coordinator as metadata demand only, never as a read grant.
    /// None for a source only local playback asked for: a local authority
    /// has no Source interest to join.
    interest: Option<plurx_core::store::analysis_coordination::AnalysisSourceInterestAuthority>,
    /// The local playback that asked for this source, when one did. It
    /// names the Analysis request the build runs under.
    asked_by: Option<super::LocalCopyAuthority>,
    /// Whether this source's coordination request is known to exist in the
    /// store. A demand's is made on its first attempt, before the source
    /// domain is even looked at, so that a restart finds it; after that a
    /// busy source costs no write.
    persisted: bool,
    observed: Instant,
    /// Not before this instant: a busy source domain or a contended claim
    /// waits for the next dispatch wake after it, never a private timer.
    retry_after: Option<Instant>,
}

impl PendingSource {
    fn set(&self) -> Option<&CopyObservationSet> {
        match &self.held {
            Held::Rows(set) => Some(set),
            Held::Demand { .. } => None,
        }
    }
    fn source(&self) -> &AnalysisCoordinationSource {
        match &self.held {
            Held::Rows(set) => set.source(),
            Held::Demand { source, .. } => source,
        }
    }
    fn catalog_namespace(&self) -> &str {
        match &self.held {
            Held::Rows(set) => set.catalog_namespace(),
            Held::Demand {
                catalog_namespace, ..
            } => catalog_namespace,
        }
    }
    fn is_complete(&self) -> bool {
        self.set().is_some_and(CopyObservationSet::is_complete)
    }
    /// Fold the rows of one Analysis generation. The first rows of a source
    /// that was only asked for establish its set.
    #[cfg(unix)]
    fn observe_analysis(
        &mut self,
        phase: &plurx_core::store::analysis_coordination::AnalysisPhysicalPhase,
        rows: &super::AnalysisCopyRows,
    ) -> Result<(), super::CopyObservationError> {
        let established = match &mut self.held {
            Held::Rows(set) => return set.observe_analysis(phase, rows),
            Held::Demand {
                catalog_namespace,
                observed,
                ..
            } => CopyObservationSet::from_analysis(catalog_namespace, observed, phase, rows)?,
        };
        self.held = Held::Rows(Box::new(established));
        Ok(())
    }
}

/// The Source interest an authority can join the coordinator with.
fn source_interest(
    authority: &CopyObservationAuthority,
) -> Option<plurx_core::store::analysis_coordination::AnalysisSourceInterestAuthority> {
    match authority {
        CopyObservationAuthority::Source { authority, .. } => Some(authority.clone()),
        CopyObservationAuthority::Local(_) => None,
    }
}

#[derive(Default)]
pub(crate) struct PendingSources {
    sources: HashMap<String, PendingSource>,
    /// Sources a unit is working now, with the ranges committed meanwhile.
    working: HashMap<String, Vec<CompletedCopyRange>>,
    /// Defects readers reported since the last pass, by source key.
    defects: HashMap<String, CopyIndexDefect>,
}

impl PendingSources {
    /// Fold one inbox message: a committed range into its source's set, a
    /// reader's defect into the next pass's quarantine work.
    pub(crate) fn fold(&mut self, message: CopyAnalysisMessage, retry: Duration) {
        match message {
            CopyAnalysisMessage::Range(range) => self.observe(*range),
            CopyAnalysisMessage::Demand(demand) => self.demand(*demand, retry),
            CopyAnalysisMessage::Defect(defect) => {
                if self.defects.len() < MAX_PENDING_DEFECTS
                    || self.defects.contains_key(&defect.source_key)
                {
                    self.defects.insert(defect.source_key.clone(), defect);
                }
            }
        }
    }

    /// Hold a row-less demand for its source. A source already held or being
    /// worked needs nothing more: its set is on its way to the same index.
    /// The demand's first attempt is not before `retry` from now, on a later
    /// dispatch wake, so it never runs against the start that raised it.
    fn demand(&mut self, demand: CopyIndexDemand, retry: Duration) {
        // Only the copy engine can turn a demand into rows, and it is built
        // for unix alone; elsewhere a demand could never be met.
        if !cfg!(unix) {
            return;
        }
        let Ok(key) =
            plurx_core::store::copy_index_v3_source_key(&demand.catalog_namespace, &demand.source)
        else {
            return;
        };
        if self.working.contains_key(&key) || self.sources.contains_key(&key) {
            return;
        }
        // A demand costs nothing to ask again; rows cost a playback. So a
        // demand makes room only among other demands, and is refused when
        // every held source has rows.
        if self.sources.len() + self.working.len() >= MAX_PENDING_SOURCES {
            let Some(oldest) = self
                .sources
                .iter()
                .filter(|(_, pending)| matches!(pending.held, Held::Demand { .. }))
                .min_by_key(|(_, pending)| pending.observed)
                .map(|(key, _)| key.clone())
            else {
                return;
            };
            self.sources.remove(&oldest);
        }
        let asked_by = match &demand.authority {
            CopyObservationAuthority::Local(local) => Some(local.clone()),
            CopyObservationAuthority::Source { .. } => None,
        };
        if let Some(local) = &asked_by {
            // Attribution of the background build this starts.
            tracing::info!(
                file_id = demand.source.file_id,
                user_id = local.user_id(),
                playback_id = local.playback_id(),
                "local playback asked for its source's copy index"
            );
        }
        let now = Instant::now();
        self.sources.insert(
            key,
            PendingSource {
                file: demand.file,
                interest: source_interest(&demand.authority),
                asked_by,
                persisted: false,
                held: Held::Demand {
                    catalog_namespace: demand.catalog_namespace,
                    observed: Box::new(demand.source.clone()),
                    source: Box::new(demand.source),
                },
                recipe: demand.recipe,
                observed: now,
                retry_after: Some(now + retry),
            },
        );
    }

    /// Whether this source is held or being worked.
    pub(super) fn holds(&self, key: &str) -> bool {
        self.sources.contains_key(key) || self.working.contains_key(key)
    }

    /// Forget the least recently observed source when the bound is reached.
    fn make_room(&mut self) {
        if self.sources.len() + self.working.len() >= MAX_PENDING_SOURCES {
            if let Some(oldest) = self
                .sources
                .iter()
                .min_by_key(|(_, pending)| pending.observed)
                .map(|(key, _)| key.clone())
            {
                self.sources.remove(&oldest);
            }
        }
    }

    fn take_defects(&mut self) -> Vec<CopyIndexDefect> {
        let mut defects = self
            .defects
            .drain()
            .map(|(_, defect)| defect)
            .collect::<Vec<_>>();
        defects.sort_by(|a, b| a.source_key.cmp(&b.source_key));
        defects
    }

    /// Fold one committed range into its source's set. A contradiction between
    /// two observations of the same source discards both: neither can seal.
    pub(crate) fn observe(&mut self, message: CompletedCopyRange) {
        let Ok(key) = plurx_core::store::copy_index_v3_source_key(
            &message.catalog_namespace,
            &message.source,
        ) else {
            return;
        };
        if let Some(deferred) = self.working.get_mut(&key) {
            deferred.retain(|held| held.range_id != message.range_id);
            if deferred.len() >= MAX_DEFERRED_RANGES {
                deferred.remove(0);
            }
            deferred.push(message);
            return;
        }
        let interest = source_interest(&message.authority);
        if let Some(pending) = self.sources.get_mut(&key) {
            let folded = match &mut pending.held {
                Held::Rows(set) => set.observe(&message),
                // The first rows of a source so far only asked for.
                held @ Held::Demand { .. } => {
                    CopyObservationSet::new(&message).map(|set| *held = Held::Rows(Box::new(set)))
                }
            };
            if let Err(error) = folded {
                tracing::warn!(
                    file_id = message.source.file_id,
                    range_id = %message.range_id,
                    ?error,
                    "copy observations of one source disagree; discarding them"
                );
                self.sources.remove(&key);
            } else {
                pending.observed = Instant::now();
                pending.interest = interest;
                pending.recipe = message.recipe;
            }
            return;
        }
        match CopyObservationSet::new(&message) {
            Ok(set) => {
                self.make_room();
                self.sources.insert(
                    key,
                    PendingSource {
                        file: Arc::clone(&message.file),
                        held: Held::Rows(Box::new(set)),
                        recipe: message.recipe,
                        interest,
                        asked_by: None,
                        persisted: false,
                        observed: Instant::now(),
                        retry_after: None,
                    },
                );
            }
            Err(error) => tracing::debug!(
                file_id = message.source.file_id,
                range_id = %message.range_id,
                ?error,
                "copy observation refused"
            ),
        }
    }

    /// Hand every set that may be attempted now to its own unit. An
    /// incomplete set is attempted too: its gaps are filled when the store
    /// admits a physical phase for it.
    pub(super) fn start_ready(&mut self, now: Instant) -> Vec<(String, PendingSource)> {
        let mut keys = self
            .sources
            .iter()
            .filter(|(_, pending)| {
                (cfg!(unix) || pending.is_complete())
                    && pending.retry_after.is_none_or(|at| at <= now)
            })
            .map(|(key, _)| key.clone())
            .collect::<Vec<_>>();
        keys.sort();
        keys.into_iter()
            .filter_map(|key| {
                let source = self.sources.remove(&key)?;
                self.working.insert(key.clone(), Vec::new());
                Some((key, source))
            })
            .collect()
    }

    /// A unit returned its source. A set that may still seal waits for a later
    /// wake; then every range committed while it worked folds in.
    fn settle(
        &mut self,
        key: String,
        source: Option<PendingSource>,
        attempt: &Attempt,
        retry: Duration,
    ) {
        let deferred = self.working.remove(&key).unwrap_or_default();
        if let Some(mut source) =
            source.filter(|_| matches!(attempt, Attempt::Busy | Attempt::Retry(_)))
        {
            source.retry_after = Some(Instant::now() + retry);
            self.sources.insert(key, source);
        }
        for message in deferred {
            self.observe(message);
        }
    }
}

/// What the Analysis dispatcher hands this worker. Every field already exists
/// on the job manager; nothing here is a new authority.
pub(crate) struct CopyAnalysisContext {
    pub(crate) store: Arc<dyn Store>,
    pub(crate) node_id: String,
    pub(crate) cache_dir: PathBuf,
    pub(crate) checkpoint_root: Option<PathBuf>,
    pub(crate) lease_ms: i64,
    pub(crate) renew_every: Duration,
    pub(crate) retry_ms: i64,
    pub(crate) metrics: Arc<crate::state::AnalysisRuntimeMetrics>,
}

#[derive(Debug)]
pub(super) enum Attempt {
    /// Published now, or a healthy head already serves this source and the
    /// observations agree with it.
    Done,
    /// Another owner or a legacy request holds this source; try on a later wake.
    Busy,
    /// The attempt failed; the source set is kept for one later retry.
    Retry(String),
    /// The observations can never seal this source as they are.
    Discard(String),
    /// This generation's coordinator already ended; nothing will claim it
    /// again, so the source is dropped. Expected for a title asked for again
    /// after its build failed, and not worth a warning each time.
    Ended,
}

/// One dispatcher pass: fold the inbox, then give each ready source its own
/// unit. A unit's gap fill can wait on a paused byte budget for as long as a
/// viewer needs the disk; it holds only its own source, so every other source
/// keeps folding, sealing and publishing, and the inbox drains on every wake.
/// A source has at most one unit; its unit is its single flight.
pub(crate) async fn work_copy_observations(
    registry: &super::CopyAnalysisRegistry,
    context: Arc<CopyAnalysisContext>,
) {
    // Before this process joins or creates any coordination request: what
    // its predecessor left queued has no worker. Once per process.
    // Durable demands are read first: retiring ends their requests, and each
    // recovered demand makes its own again on its first attempt.
    let recovered = Arc::new(std::sync::Mutex::new(Vec::new()));
    registry
        .retire_stranded_once(|created_before_ms| {
            let context = Arc::clone(&context);
            let recovered = Arc::clone(&recovered);
            async move {
                let demands = recover_demands(&context, created_before_ms).await?;
                if !demands.is_empty() {
                    tracing::info!(
                        demands = demands.len(),
                        "took up the copy index demands a previous process left"
                    );
                }
                *recovered
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = demands;
                let retired = context
                    .store
                    .retire_stranded_analysis_coordination(
                        &context.node_id,
                        created_before_ms,
                        crate::state::clock_ms(),
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                if retired > 0 {
                    tracing::info!(
                        retired,
                        "retired copy coordination requests a previous process left queued"
                    );
                }
                Ok(())
            }
        })
        .await;
    let retry = Duration::from_millis(context.retry_ms.max(1_000) as u64);
    let asked = registry.asked();
    let pending = registry.pending();
    let (units, defects) = {
        let mut sources = pending.lock().await;
        let recovered = std::mem::take(
            &mut *recovered
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for demand in recovered {
            sources.fold(CopyAnalysisMessage::Demand(Box::new(demand)), retry);
        }
        for message in registry.take_pending() {
            sources.fold(message, retry);
        }
        let ready = (sources.start_ready(Instant::now()), sources.take_defects());
        forget_settled(&asked, &sources);
        ready
    };
    for (key, mut source) in units {
        let pending = Arc::clone(&pending);
        let context = Arc::clone(&context);
        let asked = Arc::clone(&asked);
        let unit_key = key.clone();
        let unit = tokio::spawn(async move {
            let attempt = publish_source(&context, &unit_key, &mut source).await;
            (source, attempt)
        });
        tokio::spawn(async move {
            // A unit that panicked still returns its key, so the source is
            // never left marked as working with nothing working it.
            let returned = unit.await;
            let mut sources = pending.lock().await;
            match returned {
                Ok((source, attempt)) => {
                    match &attempt {
                        Attempt::Done | Attempt::Busy => {}
                        Attempt::Ended => {
                            tracing::debug!("copy index generation already ended; source dropped");
                        }
                        Attempt::Retry(reason) => {
                            tracing::warn!(%reason, "copy index publication will retry");
                        }
                        Attempt::Discard(reason) => {
                            tracing::warn!(%reason, "copy observations cannot seal this source");
                        }
                    }
                    sources.settle(key, Some(source), &attempt, retry);
                }
                Err(error) => {
                    tracing::error!(%error, "copy analysis unit ended abnormally");
                    let attempt = Attempt::Discard("unit ended abnormally".into());
                    sources.settle(key, None, &attempt, retry);
                }
            }
            forget_settled(&asked, &sources);
        });
    }
    // Reader-reported defects are a few store writes each. This pass does
    // them itself, after its sources have their units, so nothing detached
    // outlives it.
    for defect in defects {
        match quarantine_defect(&context, &defect).await {
            Ok(true) => tracing::warn!(
                source_key = defect.source_key,
                blob_sha256 = defect.blob_sha256,
                head_epoch = defect.head_epoch,
                code = defect.kind.code(),
                "copy index v3 head quarantined on a reader's defect"
            ),
            Ok(false) => tracing::debug!(
                source_key = defect.source_key,
                code = defect.kind.code(),
                "copy index v3 defect did not quarantine its head"
            ),
            Err(error) => tracing::warn!(
                source_key = defect.source_key,
                code = defect.kind.code(),
                %error,
                "copy index v3 defect was not handled; the next start reports it again"
            ),
        }
    }
}

/// Whether a viewer holds this source's domain now: the attempt to return
/// if so, or if the read-only look itself failed.
async fn source_is_busy(context: &CopyAnalysisContext, file_id: i64) -> Option<Attempt> {
    match context
        .store
        .foreground_source_pressure(file_id, crate::state::clock_ms())
        .await
    {
        Ok(false) => None,
        Ok(true) => Some(Attempt::Busy),
        Err(error) => Some(Attempt::Retry(error.to_string())),
    }
}

/// The demands a previous process of this node left durable: its queued
/// coordination requests under the local-playback id, each rebuilt from the
/// catalogue and this node's current object. One that cannot be rebuilt
/// (the file changed or went, no copy recipe has the request's video
/// identity) is left out; the title asks again when it is next played.
pub(super) async fn recover_demands(
    context: &CopyAnalysisContext,
    created_before_ms: i64,
) -> Result<Vec<CopyIndexDemand>, String> {
    let rows = context
        .store
        .queued_analysis_coordination_with_prefix(
            &context.node_id,
            created_before_ms,
            super::LOCAL_PLAYBACK_REQUEST_PREFIX,
        )
        .await
        .map_err(|error| error.to_string())?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let namespace = context
        .store
        .instance_id()
        .await
        .map_err(|error| error.to_string())?;
    let mut demands = Vec::new();
    for row in rows {
        let file_id = row.source.file_id;
        match rebuild_demand(context, &namespace, row).await {
            Some(demand) => demands.push(demand),
            None => tracing::debug!(file_id, "a durable copy index demand could not be rebuilt"),
        }
    }
    Ok(demands)
}

async fn rebuild_demand(
    context: &CopyAnalysisContext,
    namespace: &str,
    row: plurx_core::store::analysis_coordination::AnalysisCoordination,
) -> Option<CopyIndexDemand> {
    let coordinated = row.source;
    if coordinated.origin_node_id != context.node_id {
        return None;
    }
    let authority = super::LocalCopyAuthority::of_durable_request(&row.request_id)?;
    let file = context
        .store
        .get_file(coordinated.file_id)
        .await
        .ok()
        .flatten()
        .filter(|file| {
            file.size == coordinated.source_size && file.mtime == coordinated.source_mtime
        })?;
    // The object this node holds now must be the one the request was for.
    let object_version = crate::fragment_index_cluster::inspect_source(&file)
        .await
        .ok()
        .filter(|object_version| *object_version == coordinated.object_version)?;
    // The request records the copy-video identity, not the options that
    // gave it. The stored probe and the three copy shapes a start can ask
    // for give the candidates; the one with that identity is the recipe.
    let probe = context
        .store
        .get_file_probe_json(file.id)
        .await
        .ok()
        .flatten();
    let have_dovi = crate::ffmpeg::has_dovi_rpu().await;
    let video = [(false, false), (true, false), (false, true)]
        .into_iter()
        .map(|(preserve, convert)| {
            plurx_core::transcode::CopyVideoOptions::from_probe(
                &file,
                probe.as_deref(),
                have_dovi,
                preserve,
            )
            .with_dolby_vision_conversion(convert)
        })
        .find(|video| {
            crate::fragindex::identity_for(&file, *video).argv_fingerprint
                == coordinated.video_identity
        })?;
    // As playback observes it, under its own generation: a quarantined
    // head's repair generation is adopted again by the head decision.
    let source = super::source_for_copy(
        namespace,
        &file,
        &coordinated.video_identity,
        &object_version,
        &context.node_id,
    )
    .ok()?;
    Some(CopyIndexDemand {
        file: Arc::new(file),
        catalog_namespace: namespace.to_owned(),
        source,
        recipe: super::CopyRecipe {
            video,
            audio_index: None,
            transcode_audio: false,
            audio_delivery: None,
        },
        authority: CopyObservationAuthority::Local(authority),
    })
}

/// Let a title ask again once nothing holds or works its source.
pub(super) fn forget_settled(
    asked: &std::sync::Mutex<std::collections::HashSet<String>>,
    sources: &PendingSources,
) {
    asked
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .retain(|key| sources.holds(key));
}

pub(super) async fn publish_source(
    context: &CopyAnalysisContext,
    key: &str,
    pending: &mut PendingSource,
) -> Attempt {
    if pending.source().origin_node_id != context.node_id {
        // Only the origin node holds the exact object and may read it.
        return Attempt::Discard("observation is not for this origin".into());
    }
    // The exact head epoch and blob these observations contradict, if any.
    let reconciled = match &mut pending.held {
        Held::Rows(set) => reconcile_with_head(context, key, set).await,
        Held::Demand { source, .. } => reconcile_demand(context, key, source).await,
    };
    let contradicted = match reconciled {
        Ok(HeadDecision::Proceed { contradicted }) => contradicted,
        Ok(HeadDecision::Done) => return Attempt::Done,
        Ok(HeadDecision::Discard(reason)) => return Attempt::Discard(reason.into()),
        Err(error) => return Attempt::Retry(error),
    };
    let source = pending.source().clone();
    // An incomplete set needs a physical phase, which the store admits only
    // while the source domain is idle. While a viewer holds it, a read-only
    // look answers without enqueueing, claiming or charging anything. A
    // contradicted head is quarantined whoever is watching: the viewer whose
    // range found it is the one being served the wrong index.
    let waits_for_idle = contradicted.is_none() && !pending.is_complete();
    // A demand lives only in memory until its request exists. Its first
    // attempt therefore makes the request before it looks at the source
    // domain, once; a restart then finds the request and takes the demand
    // up again.
    let persist_first = !pending.persisted && matches!(pending.held, Held::Demand { .. });
    if waits_for_idle && !persist_first {
        if let Some(busy) = source_is_busy(context, source.file_id).await {
            return busy;
        }
    }
    // A build local playback asked for runs under a request that says so.
    let request_id = pending.asked_by.as_ref().map_or_else(
        || uuid::Uuid::new_v4().to_string(),
        super::LocalCopyAuthority::request_id,
    );
    let request = match coordination_request(context, &source, request_id).await {
        Ok(Coordination::Queued(request)) => *request,
        Ok(Coordination::Busy) => return Attempt::Busy,
        // Nothing will ever claim this generation again, so holding the
        // source would only repeat this answer on every pass.
        Ok(Coordination::Ended) => return Attempt::Ended,
        Err(reason) => return Attempt::Retry(reason),
    };
    pending.persisted = true;
    if waits_for_idle && persist_first {
        if let Some(busy) = source_is_busy(context, source.file_id).await {
            return busy;
        }
    }
    // Record the Source demand on the coordinator. A route that has since
    // ended simply fails to join; the committed observations stay valid. A
    // source only local playback asked for has no Source demand to record.
    if let Some(interest) = &pending.interest {
        if let Err(error) = context
            .store
            .join_source_analysis_coordination(&request.request_id, &source, interest)
            .await
        {
            tracing::debug!(%error, "Source demand did not join the copy coordinator");
        }
    }
    let now = crate::state::clock_ms();
    let claim = match context
        .store
        .claim_analysis_coordination(
            &request.request_id,
            source.clone(),
            &context.node_id,
            request.fence,
            now,
            now.saturating_add(context.lease_ms),
        )
        .await
    {
        Ok(Some(claim)) => claim,
        Ok(None) => return Attempt::Busy,
        Err(error) => return Attempt::Retry(error.to_string()),
    };
    // The claim is exactly this transition of the queued row; derive the
    // running row from it so no unreadable re-read can strand the claim.
    let mut claimed = request.clone();
    claimed.state = "running".into();
    claimed.owner_node_id = claim.node_id.clone();
    claimed.fence = claim.fence;
    claimed.attempts = claimed.attempts.saturating_add(1);
    claimed.lease_expires_ms = now.saturating_add(context.lease_ms);
    let stop = CancellationToken::new();
    let lost = CancellationToken::new();
    let heartbeat = heartbeat(context, &claimed, stop.clone(), lost.clone());
    let quarantined = match &contradicted {
        Some((epoch, blob)) => {
            let namespace = pending.catalog_namespace().to_owned();
            quarantine_head(
                context,
                namespace,
                &source,
                &claim,
                *epoch,
                blob,
                plurx_core::store::COPY_INDEX_V3_CONTRADICTION_CODE,
            )
            .await
        }
        None => Ok(()),
    };
    let outcome = match quarantined {
        Ok(()) => match fill_gaps(context, pending, &source, &claim, &claimed, &lost).await {
            Ok(()) => {
                publish_claimed(context, key, pending, &source, &claim, &claimed, &lost).await
            }
            Err(attempt) => Err(attempt),
        },
        Err(attempt) => Err(attempt),
    };
    // Retire the heartbeat before the outcome write, as the legacy worker does.
    stop.cancel();
    let _ = heartbeat.await;
    match outcome {
        Ok(()) => Attempt::Done,
        Err(attempt) => {
            if !lost.is_cancelled() {
                let now = crate::state::clock_ms();
                // A generation is used once (enqueue refuses a second request
                // for it), so a refused seal is retried under the same request.
                // Busy is not charged; a failed or refused step is, so a
                // persistent fault reaches the existing attempt limit and the
                // request becomes terminal, as a legacy index build does.
                let code = match &attempt {
                    Attempt::Discard(_) => "copy_observation_refused",
                    _ => "copy_publication_retry",
                };
                let _ = context
                    .store
                    .retry_analysis_request(
                        &claimed,
                        code,
                        now,
                        now.saturating_add(context.retry_ms),
                        !matches!(attempt, Attempt::Busy),
                    )
                    .await;
            }
            attempt
        }
    }
}

/// What the head of a source means for a set of its observations.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum HeadDecision {
    /// A healthy head serves this source and the observations agree with it.
    Done,
    /// The head desires a generation no quarantine of it explains.
    Discard(&'static str),
    /// Fill, seal and publish. `contradicted` names the healthy head epoch
    /// and blob the observations disagree with, which must be quarantined
    /// first; the set has then already adopted its repair generation.
    Proceed { contradicted: Option<(i64, String)> },
}

/// Read the head by its key and settle which generation `set` seals for. A
/// healthy head is checked against the fresh observations before it is
/// called done. A quarantined head's desire is adopted only when it is
/// exactly the repair generation of the blob and epoch the head still
/// records, which is what every quarantine of it computes.
pub(super) async fn reconcile_with_head(
    context: &CopyAnalysisContext,
    key: &str,
    set: &mut CopyObservationSet,
) -> Result<HeadDecision, String> {
    let generation = set.source().requested_generation.clone();
    let (decision, adopt) = head_decision(context, key, &generation, Some(set)).await?;
    if let Some(generation) = adopt {
        set.adopt_generation(&generation);
    }
    Ok(decision)
}

/// The same decision for a source that was only asked for. It holds no rows
/// to compare, so a healthy head is simply done; a quarantined head's repair
/// generation is adopted on the same condition as for a set.
pub(super) async fn reconcile_demand(
    context: &CopyAnalysisContext,
    key: &str,
    source: &mut AnalysisCoordinationSource,
) -> Result<HeadDecision, String> {
    let (decision, adopt) = head_decision(context, key, &source.requested_generation, None).await?;
    if let Some(generation) = adopt {
        source.requested_generation = generation;
    }
    Ok(decision)
}

/// The decision, and the generation the caller must now seal for if it is
/// not the one it holds.
async fn head_decision(
    context: &CopyAnalysisContext,
    key: &str,
    generation: &str,
    set: Option<&CopyObservationSet>,
) -> Result<(HeadDecision, Option<String>), String> {
    let Some(variant) = context
        .store
        .copy_index_v3_variant(key, &context.node_id)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok((HeadDecision::Proceed { contradicted: None }, None));
    };
    let head = &variant.head;
    // The repair generation of the variant this head points at.
    let successor = head.blob_sha256.as_deref().and_then(|blob| {
        plurx_core::store::copy_index_v3_rebuild_generation(key, blob, head.head_epoch).ok()
    });
    if variant.readable().is_some() {
        // A healthy head serves this source whichever generation built it.
        let reason = match set {
            Some(set) => contradiction_with_head(context, set, &variant).await,
            None => None,
        };
        let Some(reason) = reason else {
            return Ok((HeadDecision::Done, None));
        };
        let (Some(successor), Some(blob)) = (successor, head.blob_sha256.clone()) else {
            return Ok((
                HeadDecision::Discard("contradicted head names no variant"),
                None,
            ));
        };
        tracing::warn!(
            file_id = head.file_id,
            blob_sha256 = blob,
            head_epoch = head.head_epoch,
            %reason,
            "copy observations contradict the published copy index v3"
        );
        return Ok((
            HeadDecision::Proceed {
                contradicted: Some((head.head_epoch, blob)),
            },
            Some(successor),
        ));
    }
    if head.state == CopyIndexV3HeadState::Quarantined
        && successor.as_deref() == Some(head.desired_generation.as_str())
    {
        // A sealed-init mismatch bans its blob, and this engine, being the
        // one that built it, would seal the same bytes again: the rebuild
        // would read the whole title only to be refused. The head stays
        // quarantined and readers keep their next index. Another engine
        // seals other bytes, and every other quarantine either leaves its
        // blob publishable or cannot be resealed identically.
        if let (Some(blob), Some(artifact)) = (head.blob_sha256.as_deref(), &variant.artifact) {
            let code = context
                .store
                .copy_index_v3_quarantine_code(key, head.head_epoch, blob)
                .await
                .map_err(|error| error.to_string())?;
            if code.as_deref() == Some(plurx_core::store::COPY_INDEX_V3_SEALED_INIT_CODE)
                && crate::ffmpeg::fragment_index_engine_digest().await
                    == artifact.producer_engine_sha256
            {
                return Ok((
                    HeadDecision::Discard(
                        "this engine cannot rebuild its own sealed-init mismatch",
                    ),
                    None,
                ));
            }
        }
        let adopt =
            (head.desired_generation != generation).then(|| head.desired_generation.clone());
        return Ok((HeadDecision::Proceed { contradicted: None }, adopt));
    }
    if head.desired_generation != generation {
        return Ok((
            HeadDecision::Discard("head desires another generation"),
            None,
        ));
    }
    Ok((HeadDecision::Proceed { contradicted: None }, None))
}

/// What the fresh observations of `set` contradict in the variant a healthy
/// head points at, if anything. The blob is read and decoded only here, from
/// this node's own v3 root: the origin installed it when it published. A
/// blob that is absent, unreadable or refused proves nothing about the head.
async fn contradiction_with_head(
    context: &CopyAnalysisContext,
    set: &CopyObservationSet,
    variant: &CopyIndexV3Variant,
) -> Option<String> {
    let artifact = variant.readable()?;
    // The observed rows are this engine's cut of the source and the
    // published rows are the producer engine's. Two ffmpeg builds may
    // fragment the same bytes differently, so a difference between them says
    // which engine wrote each table and nothing about either being wrong.
    // Only when this node's engine is exactly the variant's producer engine
    // does a differing row mean one of the two reads was faulty; with any
    // other engine the head stays as published, as for a sealed-init
    // refusal.
    if crate::ffmpeg::fragment_index_engine_digest().await != artifact.producer_engine_sha256 {
        return None;
    }
    let path = v3_blob_path(&v3_root(&context.cache_dir), &artifact.blob_sha256)?;
    let blob = {
        let _pin = crate::playback_cache::pin(&path).ok()?;
        tokio::fs::read(&path).await.ok()?
    };
    let index = crate::copy_index_v3::decode_verified(&blob, artifact).ok()?;
    set.contradiction(&index)
}

/// Quarantine the exact healthy head `claim` owns the repair generation of.
/// The store moves it only at this epoch and blob; a head that has moved
/// since is read again by the next pass.
async fn quarantine_head(
    context: &CopyAnalysisContext,
    catalog_namespace: String,
    source: &AnalysisCoordinationSource,
    claim: &AnalysisCoordinationClaim,
    expected_epoch: i64,
    bad_blob_sha256: &str,
    code: &str,
) -> Result<(), Attempt> {
    match context
        .store
        .quarantine_copy_index_v3_head(QuarantineCopyIndexV3Head {
            catalog_namespace,
            source: source.clone(),
            claim: claim.clone(),
            expected_epoch,
            bad_blob_sha256: bad_blob_sha256.to_owned(),
            code: code.to_owned(),
            now_ms: crate::state::clock_ms(),
        })
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(Attempt::Busy),
        Err(error) => Err(Attempt::Retry(error.to_string())),
    }
}

/// Act on one defect a reader reported: re-read the head, check what this
/// worker can check itself, and quarantine it under a claim on its repair
/// generation. `Ok(false)` means the head was left as it is. The request was
/// borrowed only for that write and is ended as a stranded coordinator: this
/// node may not be the origin and holds no observations, so the origin's
/// next observation of the source enqueues its own. Nothing here reads the
/// source.
pub(super) async fn quarantine_defect(
    context: &CopyAnalysisContext,
    defect: &CopyIndexDefect,
) -> Result<bool, String> {
    let store = &context.store;
    let Some(variant) = store
        .copy_index_v3_variant(&defect.source_key, &context.node_id)
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let head = &variant.head;
    let Some(artifact) = variant.readable().filter(|artifact| {
        head.head_epoch == defect.head_epoch && artifact.blob_sha256 == defect.blob_sha256
    }) else {
        // Already quarantined, republished or gone: the report is stale.
        return Ok(false);
    };
    match defect.kind {
        CopyIndexDefectKind::UnindexedHdr10 => {
            // A fact of the bytes themselves, checked again on this node's
            // verified copy rather than taken from the report.
            let Some(path) = v3_blob_path(&v3_root(&context.cache_dir), &artifact.blob_sha256)
            else {
                return Ok(false);
            };
            let blob = {
                let _pin = crate::playback_cache::pin(&path).map_err(|error| error.to_string())?;
                tokio::fs::read(&path).await
            };
            if !blob.is_ok_and(|blob| crate::copy_index_v3::lacks_indexed_hdr10(&blob, artifact)) {
                return Ok(false);
            }
        }
        CopyIndexDefectKind::NoHolder => {
            // A holder that appeared since the report keeps the head.
            let holders = store
                .copy_index_v3_locations(&artifact.contract_key, &artifact.blob_sha256)
                .await
                .map_err(|error| error.to_string())?;
            if !holders.is_empty() {
                return Ok(false);
            }
        }
        CopyIndexDefectKind::SealedInit => {
            // A generation here promoted another init than the seal
            // published. The served init is this engine's muxer init plus
            // the index's promotion, so the two can differ for two reasons:
            // the variant is wrong, or this node's ffmpeg is not the one
            // that built it and simply writes its init differently. Only the
            // first is a defect of the head. When this engine is exactly the
            // variant's producer engine the second reason is excluded: the
            // same engine sealed a contract it does not itself produce for
            // this source, on any node. With any other engine nothing is
            // known about the variant, and quarantining it would take a
            // good index away from every node whose engine does match; the
            // generation's refusal already keeps this process off it.
            if crate::ffmpeg::fragment_index_engine_digest().await
                != artifact.producer_engine_sha256
            {
                return Ok(false);
            }
        }
    }
    let successor = plurx_core::store::copy_index_v3_rebuild_generation(
        &head.source_key,
        &artifact.blob_sha256,
        head.head_epoch,
    )
    .map_err(|error| error.to_string())?;
    let source = AnalysisCoordinationSource {
        file_id: head.file_id,
        source_size: head.source_size,
        source_mtime: head.source_mtime,
        origin_node_id: head.origin_node_id.clone(),
        object_version: head.object_version.clone(),
        video_identity: head.raw_video_identity.clone(),
        contract_version: head.contract_version,
        requested_generation: successor,
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let Coordination::Queued(request) = coordination_request(context, &source, request_id).await?
    else {
        return Ok(false);
    };
    let now = crate::state::clock_ms();
    let Some(claim) = store
        .claim_analysis_coordination(
            &request.request_id,
            source.clone(),
            &context.node_id,
            request.fence,
            now,
            now.saturating_add(context.lease_ms),
        )
        .await
        .map_err(|error| error.to_string())?
    else {
        return Ok(false);
    };
    let quarantined = quarantine_head(
        context,
        head.catalog_namespace.clone(),
        &source,
        &claim,
        head.head_epoch,
        &artifact.blob_sha256,
        defect.kind.code(),
    )
    .await;
    // End the borrowed request whatever happened, uncharged. Left queued it
    // would wait for a worker of this source that may never run here.
    match store
        .retire_analysis_coordination_claim(&claim, crate::state::clock_ms())
        .await
    {
        Ok(true) => {}
        Ok(false) => tracing::warn!("the quarantine claim was already gone when it was ended"),
        Err(error) => {
            tracing::warn!(%error, "the quarantine claim was not ended; its lease ends it");
        }
    }
    match quarantined {
        Ok(()) => Ok(true),
        Err(Attempt::Busy) => Ok(false),
        Err(Attempt::Retry(reason) | Attempt::Discard(reason)) => Err(reason),
        Err(Attempt::Done | Attempt::Ended) => Ok(false),
    }
}

/// Fill every unobserved span of this source, earliest first. Each generation
/// runs inside its own physical phase: the store's capacity clause admits one
/// only while the source domain is idle, and foreground pressure pauses the
/// phase's byte budget. The rows enter the same observation set as playback
/// rows, so an overlap that differs discards the set instead of sealing it.
#[cfg(unix)]
async fn fill_gaps(
    context: &CopyAnalysisContext,
    pending: &mut PendingSource,
    source: &AnalysisCoordinationSource,
    claim: &AnalysisCoordinationClaim,
    request: &AnalysisRequest,
    lost: &CancellationToken,
) -> Result<(), Attempt> {
    use crate::copyrange::{fill_copy_gap, CopyGapFill, CopyGapFillError};
    loop {
        // A source nothing has observed is one gap: the whole title.
        let gap = match pending.set() {
            Some(set) => match set.first_gap() {
                Some(gap) => gap,
                None => break,
            },
            None => plurx_core::segplan::ranges::RangeGap {
                start: None,
                end: None,
            },
        };
        if lost.is_cancelled() {
            return Err(Attempt::Busy);
        }
        // Rows cut by an engine that changed under this process would be
        // published under the retired engine's digest.
        if !crate::ffmpeg::fragment_index_engine_is_current().await {
            return Err(Attempt::Discard(
                "the fragment-index engine changed; restart is required".into(),
            ));
        }
        if crate::fragindex::identity_for(&pending.file, pending.recipe.video).argv_fingerprint
            != source.video_identity
        {
            return Err(Attempt::Discard(
                "copy recipe is not this source's video identity".into(),
            ));
        }
        let active =
            physical::ActivePhase::begin(&context.store, source, claim.clone(), request, lost)
                .await
                .map_err(Attempt::Retry)?
                .ok_or(Attempt::Busy)?;
        let filled = fill_copy_gap(
            CopyGapFill {
                file: &pending.file,
                recipe: &pending.recipe,
                phase: &active.phase,
                // The first generation of an unobserved source establishes
                // the contract, from the title's own start; its timescale
                // places no seek.
                init_contract_sha256: pending.set().map(CopyObservationSet::init_contract_sha256),
                canonical_title_start: pending
                    .set()
                    .map_or(0, CopyObservationSet::canonical_title_start),
                timescale: pending.set().map_or(0, CopyObservationSet::timescale),
                gap,
                max_rows: plurx_core::store::MAX_CLUSTER_FRAGMENT_INDEX_BLOB_BYTES / 28,
                runtime_cache: &context.cache_dir,
                stop: &active.cancel,
            },
            || {
                let file = Arc::clone(&pending.file);
                let budget = Arc::clone(&active.budget);
                let object_version = source.object_version.clone();
                async move {
                    // The scanner facts and the exact held object are checked
                    // again for every Input this phase opens.
                    let held = crate::fragment_index_cluster::open_source_fence(
                        &file,
                        Some(&object_version),
                    )
                    .await?;
                    crate::source_input::SourceInput::open_background_held(
                        held,
                        crate::http::stream::file_content_type(&file.path),
                        budget,
                    )
                    .await
                }
            },
        )
        .await;
        let phase = active.phase.clone();
        // The generation returned only after its children, pipe tasks and
        // Inputs joined, so this phase's custody can settle as joined.
        active.finish(&context.store).await;
        let rows = filled.map_err(|error| match error {
            CopyGapFillError::Failed(reason) => Attempt::Retry(reason),
            CopyGapFillError::Contradicted(reason) | CopyGapFillError::Refused(reason) => {
                Attempt::Discard(reason)
            }
        })?;
        let observed = rows.rows.len();
        pending
            .observe_analysis(&phase, &rows)
            .map_err(|error| Attempt::Discard(format!("analysis rows: {error:?}")))?;
        // Fresh rows make this the most recently observed source, never the
        // first one forgotten.
        pending.observed = Instant::now();
        if pending.set().and_then(CopyObservationSet::first_gap) == Some(gap) {
            return Err(Attempt::Retry("copy gap fill left its gap open".into()));
        }
        tracing::info!(
            file_id = source.file_id,
            request_id = %phase.claim.request_id,
            phase_sequence = phase.phase_sequence,
            observed,
            total = pending.set().map_or(0, CopyObservationSet::observed_rows),
            "copy index gap filled by an Analysis generation"
        );
    }
    Ok(())
}

/// The copy engine is not built here; only whole-title playback seals.
#[cfg(not(unix))]
async fn fill_gaps(
    _context: &CopyAnalysisContext,
    pending: &mut PendingSource,
    _source: &AnalysisCoordinationSource,
    _claim: &AnalysisCoordinationClaim,
    _request: &AnalysisRequest,
    _lost: &CancellationToken,
) -> Result<(), Attempt> {
    if pending.is_complete() {
        Ok(())
    } else {
        Err(Attempt::Busy)
    }
}

/// What became of the coordination request for one exact source generation.
#[derive(Debug)]
pub(super) enum Coordination {
    /// Queued and this worker's to claim.
    Queued(Box<AnalysisRequest>),
    /// Another owner or a legacy request holds the source now.
    Busy,
    /// The generation's request reached a terminal state. Enqueue refuses a
    /// second request for a generation, so nothing will claim it again.
    Ended,
}

/// Join or create the queued coordination request for this exact source. A
/// legacy request for the same file/node keeps its claim: that is `Busy`.
pub(super) async fn coordination_request(
    context: &CopyAnalysisContext,
    source: &AnalysisCoordinationSource,
    request_id: String,
) -> Result<Coordination, String> {
    // A request retired as stranded does not use up its generation: enqueue
    // replaces it. When it is returned all the same, the replacement was
    // refused only by the active-request ceiling, which passes; that is
    // busy, on either backend, never ended.
    let ended = |request: &AnalysisRequest| {
        matches!(request.state.as_str(), "failed" | "cancelled" | "ready")
            && !(request.state == "cancelled"
                && request.last_error_code
                    == plurx_core::store::analysis_coordination::STRANDED_CODE)
    };
    let now = crate::state::clock_ms();
    let request = context
        .store
        .enqueue_analysis_request(&NewAnalysisRequest {
            request_id,
            file_id: source.file_id,
            source_size: source.source_size,
            source_mtime: source.source_mtime,
            component: "fragment_index".into(),
            pipeline_version: source.pipeline_marker(),
            video_identity: source.video_marker(),
            requested_generation: source.requested_generation.clone(),
            priority: "normal".into(),
            trigger: "background".into(),
            force_rebuild: false,
            target_node_id: source.origin_node_id.clone(),
            not_before_ms: now,
            created_at_ms: now,
        })
        .await
        .map_err(|error| error.to_string())?;
    if request.pipeline_version != source.pipeline_marker() {
        return Ok(Coordination::Busy);
    }
    let role = context
        .store
        .establish_analysis_coordination(&request.request_id, source.clone(), now)
        .await
        .map_err(|error| error.to_string())?;
    let Some(role) = role else {
        return Ok(if ended(&request) {
            Coordination::Ended
        } else {
            Coordination::Busy
        });
    };
    // Rendezvous: an earlier coordinator for the same exact source wins and
    // this request was retired in the same transaction.
    let current = context
        .store
        .analysis_request(&role.request_id)
        .await
        .map_err(|error| error.to_string())?;
    Ok(match current {
        Some(request) if request.state == "queued" => Coordination::Queued(Box::new(request)),
        Some(request) if ended(&request) => Coordination::Ended,
        _ => Coordination::Busy,
    })
}

fn heartbeat(
    context: &CopyAnalysisContext,
    request: &AnalysisRequest,
    stop: CancellationToken,
    lost: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    let store = Arc::clone(&context.store);
    let request_id = request.request_id.clone();
    let node_id = context.node_id.clone();
    let fence = request.fence;
    let beat = crate::state::LeaseHeartbeat {
        queue: "analysis-request",
        row: request.request_id.clone(),
        fence,
        attempts: request.attempts,
        known_expiry_ms: request.lease_expires_ms,
        lease_ms: context.lease_ms,
        renew_every: context.renew_every,
        metrics: Arc::clone(&context.metrics),
        stop,
        lost,
    };
    tokio::spawn(async move {
        beat.run(move |now, expires_at| {
            let store = Arc::clone(&store);
            let request_id = request_id.clone();
            let node_id = node_id.clone();
            async move {
                store
                    .renew_analysis_request(&request_id, &node_id, fence, now, expires_at)
                    .await
            }
        })
        .await;
    })
}

async fn publish_claimed(
    context: &CopyAnalysisContext,
    key: &str,
    pending: &PendingSource,
    source: &AnalysisCoordinationSource,
    claim: &AnalysisCoordinationClaim,
    request: &AnalysisRequest,
    lost: &CancellationToken,
) -> Result<(), Attempt> {
    // Every gap is filled by now; a source still without rows cannot seal.
    let Some(set) = pending.set() else {
        return Err(Attempt::Busy);
    };
    let namespace = set.catalog_namespace().to_owned();
    let head = context
        .store
        .ensure_copy_index_v3_head(
            &namespace,
            source.clone(),
            claim.clone(),
            crate::state::clock_ms(),
        )
        .await
        .map_err(|error| Attempt::Retry(error.to_string()))?
        .ok_or(Attempt::Busy)?;
    if head.source_key != key || head.desired_generation != source.requested_generation {
        // A quarantine moved this source to its repair generation; these
        // observations belong to the old one.
        return Err(Attempt::Discard("head desires another generation".into()));
    }
    let verified = physical::attest_full_source(
        Arc::clone(&context.store),
        Arc::clone(&pending.file),
        source.clone(),
        claim.clone(),
        request.clone(),
        context.checkpoint_root.clone(),
        lost.clone(),
    )
    .await
    .map_err(Attempt::Retry)?
    .ok_or(Attempt::Busy)?;
    if lost.is_cancelled() {
        return Err(Attempt::Busy);
    }
    let super::SealedCopyIndex {
        index,
        init_contract_sha256: init_contract,
    } = set
        .seal(&verified)
        .map_err(|error| Attempt::Discard(format!("{error:?}")))?;
    let source_sha256 = verified.sha256().to_owned();
    let engine = FragmentIndexEngineProvenance {
        engine_sha256: crate::ffmpeg::fragment_index_engine_digest().await,
        pipeline_sha256: hex::encode(Sha256::digest(
            format!("plurx/copy-analysis/pipeline/v2\0{}", source.video_identity).as_bytes(),
        )),
    };
    let blob = plurx_core::store::encode_cluster_fragment_index_blob_v3(
        &index,
        &source_sha256,
        &init_contract,
        &engine,
    )
    .map_err(|error| Attempt::Discard(error.to_string()))?;
    let output = plurx_core::store::cluster_fragment_index_output_identity(
        &index,
        &source_sha256,
        &init_contract,
    )
    .map_err(|error| Attempt::Discard(error.to_string()))?;
    let contract_key = plurx_core::store::cluster_fragment_index_v3_key(
        &namespace,
        source.file_id,
        &source_sha256,
        &output,
    )
    .ok_or_else(|| Attempt::Discard("v3 contract key".into()))?;
    let blob_sha256 = plurx_core::store::cluster_fragment_index_blob_sha256(&blob);
    let artifact = CopyIndexV3Artifact {
        blob_sha256: blob_sha256.clone(),
        contract_key: contract_key.clone(),
        catalog_namespace: namespace.clone(),
        file_id: source.file_id,
        source_sha256: source_sha256.clone(),
        contract_version: output.contract_version,
        output_contract_sha256: output.keyframe_table_sha256.clone(),
        init_contract_sha256: init_contract.clone(),
        bytes: i64::try_from(blob.len()).map_err(|_| Attempt::Discard("blob size".into()))?,
        built_by_node_id: context.node_id.clone(),
        built_at_ms: crate::state::clock_ms(),
        producer_engine_sha256: engine.engine_sha256.clone(),
    };
    install_blob(
        &v3_root(&context.cache_dir),
        &contract_key,
        &blob_sha256,
        &blob,
    )
    .await
    .map_err(Attempt::Retry)?;
    let payload =
        plurx_core::store::copy_index_v3_publish_payload(claim, source, &artifact, head.head_epoch)
            .map_err(|error| Attempt::Discard(error.to_string()))?;
    let token = claim_publisher(context, payload, claim, &head.source_key, &contract_key).await?;
    let outcome = if lost.is_cancelled() {
        Err(Attempt::Busy)
    } else {
        context
            .store
            .publish_copy_index_v3(PublishCopyIndexV3 {
                token: token.clone(),
                claim: claim.clone(),
                source: source.clone(),
                artifact,
                expected_head_epoch: head.head_epoch,
                now_ms: crate::state::clock_ms(),
            })
            .await
            .map_err(|error| Attempt::Retry(error.to_string()))
    };
    if !matches!(
        outcome,
        Ok(JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. })
    ) {
        // The publisher is one-shot per coordinator fence: a later attempt
        // enqueues its own. Settle this one rather than leave it running.
        let code = match &outcome {
            Ok(JobPublishOutcome::SourceChanged) => "source_changed",
            Ok(JobPublishOutcome::ArtifactConflict) => "artifact_conflict",
            _ => "copy_publication_unsettled",
        };
        let _ = context
            .store
            .settle_job(plurx_core::store::background_jobs::SettleJob {
                token,
                settlement: plurx_core::store::background_jobs::JobSettlement::Fail {
                    error_code: code.into(),
                },
                now_ms: crate::state::clock_ms(),
            })
            .await;
    }
    match outcome? {
        JobPublishOutcome::Published { .. } | JobPublishOutcome::AlreadyPublished { .. } => {
            tracing::info!(
                file_id = source.file_id,
                contract_key,
                blob_sha256,
                "copy index v3 published from playback observations"
            );
            Ok(())
        }
        JobPublishOutcome::LostOwnership => Err(Attempt::Busy),
        JobPublishOutcome::SourceChanged => Err(Attempt::Discard("source changed".into())),
        // A conflicting variant does not resolve by repeating the same seal.
        JobPublishOutcome::ArtifactConflict => {
            Err(Attempt::Discard("copy index v3 artifact conflict".into()))
        }
    }
}

/// Enqueue and claim this node's publisher job. Dedupe binds the exact
/// source generation, contract and coordinator fence, never engine provenance.
async fn claim_publisher(
    context: &CopyAnalysisContext,
    payload: plurx_core::store::background_jobs::JobPayload,
    claim: &AnalysisCoordinationClaim,
    source_key: &str,
    contract_key: &str,
) -> Result<plurx_core::store::background_jobs::JobToken, Attempt> {
    let generation = match &payload {
        plurx_core::store::background_jobs::JobPayload::FragmentIndexPublish {
            requested_generation,
            ..
        } => requested_generation.clone(),
        _ => return Err(Attempt::Discard("publisher payload".into())),
    };
    let dedupe = hex::encode(Sha256::digest(
        format!(
            "plurx/copy-index/publisher/v3\0{source_key}\0{generation}\0{contract_key}\0{}\0{}",
            claim.request_id, claim.fence
        )
        .as_bytes(),
    ));
    let now = crate::state::clock_ms();
    let id = uuid::Uuid::new_v4().to_string();
    let job_id = match context
        .store
        .enqueue_job(EnqueueJob {
            id,
            payload,
            dedupe_key: dedupe.clone(),
            priority: 1,
            not_before_ms: now,
            now_ms: now,
            request: JobRequest {
                scope: "copy-index-v3".into(),
                request_id: dedupe.clone(),
                request_digest: dedupe,
                consumer_kind: "copy_index_v3".into(),
                consumer_ref: claim.request_id.clone(),
                target_node_id: None,
                deadline_ms: Some(now.saturating_add(context.lease_ms)),
                retain_identity: false,
            },
        })
        .await
        .map_err(|error| Attempt::Retry(error.to_string()))?
    {
        EnqueueOutcome::Accepted { job_id, .. } => job_id,
        EnqueueOutcome::Existing {
            job_id, cancelled, ..
        } if !cancelled => job_id,
        other => return Err(Attempt::Retry(format!("publisher enqueue: {other:?}"))),
    };
    let job = context
        .store
        .background_job(&job_id)
        .await
        .map_err(|error| Attempt::Retry(error.to_string()))?
        .ok_or_else(|| Attempt::Retry("publisher job vanished".into()))?;
    let now = crate::state::clock_ms();
    let request = ClaimJob {
        job_id,
        expected_revision: job.revision,
        node_id: context.node_id.clone(),
        boot_id: process_boot_id().to_owned(),
        claim_id: uuid::Uuid::new_v4().to_string(),
        kind: JobKind::FragmentIndexPublish,
        payload_version: 1,
        now_ms: now,
        dispatched_at_ms: now,
    };
    match crate::background_jobs::claim_with_resolution(context.store.as_ref(), &job, request)
        .await
        .map_err(|error| Attempt::Retry(error.to_string()))?
    {
        Some((job, _deadline)) => job
            .token
            .ok_or_else(|| Attempt::Retry("publisher claim has no token".into())),
        None => Err(Attempt::Busy),
    }
}

/// One identity per daemon process, so a restarted daemon never resumes a
/// publisher claim it did not make.
fn process_boot_id() -> &'static str {
    static BOOT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    BOOT.get_or_init(|| uuid::Uuid::new_v4().to_string())
}

pub(crate) fn v3_root(cache_dir: &Path) -> PathBuf {
    cache_dir.join("fragment-index-v3")
}

/// Content-addressed variant path. The same bytes may serve several logical
/// associations, so the blob digest alone names the file.
pub(crate) fn v3_blob_path(root: &Path, blob_sha256: &str) -> Option<PathBuf> {
    (blob_sha256.len() == 64 && blob_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())).then(
        || {
            root.join(&blob_sha256[..2])
                .join(format!("{blob_sha256}.idx3"))
        },
    )
}

/// The blob digest a v3 root file name carries, if it is a variant.
fn v3_blob_digest(name: &str) -> Option<&str> {
    name.strip_suffix(".idx3").filter(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Let the playback-cache budget evict this node's v3 blobs, as it evicts
/// its v2 ones. A blob another member also holds is this node's disposable
/// copy; evicting it retires this node's holder rows. Coldness is the file's
/// access time, which every read moves, exactly as for the v2 root.
pub(crate) fn register_playback_cache(
    budget: &Arc<crate::playback_cache::PlaybackCacheBudget>,
    runtime_root: &Path,
    store: Arc<dyn Store>,
    node: String,
) {
    budget.register(Arc::new(CopyIndexCacheOwner {
        root: v3_root(runtime_root),
        store,
        node,
    }));
}

pub(super) struct CopyIndexCacheOwner {
    pub(super) root: PathBuf,
    pub(super) store: Arc<dyn Store>,
    pub(super) node: String,
}

#[async_trait::async_trait]
impl crate::playback_cache::CacheOwner for CopyIndexCacheOwner {
    async fn candidates(&self) -> std::io::Result<Vec<crate::playback_cache::CacheCandidate>> {
        let mut prefixes = match tokio::fs::read_dir(&self.root).await {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut candidates = Vec::new();
        while let Some(prefix) = prefixes.next_entry().await? {
            if !tokio::fs::symlink_metadata(prefix.path()).await?.is_dir() {
                continue;
            }
            let mut files = tokio::fs::read_dir(prefix.path()).await?;
            while let Some(file) = files.next_entry().await? {
                let metadata = tokio::fs::symlink_metadata(file.path()).await?;
                if !metadata.is_file() {
                    continue;
                }
                let name = file.file_name();
                let name = name.to_string_lossy();
                // A staging file an interrupted install left behind.
                let stale_temporary = name.ends_with(".tmp")
                    && metadata.modified()?.elapsed().unwrap_or_default()
                        >= Duration::from_secs(60 * 60);
                if v3_blob_digest(&name).is_none() && !stale_temporary {
                    continue;
                }
                candidates.push(crate::playback_cache::CacheCandidate {
                    path: file.path(),
                    last_used: metadata.accessed().or_else(|_| metadata.modified())?,
                });
            }
        }
        Ok(candidates)
    }

    async fn evict(
        &self,
        candidate: &crate::playback_cache::CacheCandidate,
    ) -> std::io::Result<bool> {
        if candidate.path.parent().and_then(Path::parent) != Some(self.root.as_path()) {
            return Ok(false);
        }
        let Some(name) = candidate.path.file_name().and_then(|name| name.to_str()) else {
            return Ok(false);
        };
        let blob = v3_blob_digest(name);
        if blob.is_none() && !name.ends_with(".tmp") {
            return Ok(false);
        }
        if let Some(blob) = blob {
            // The last member holding a readable head's bytes keeps them.
            // Evicting them would leave a healthy head nobody can serve,
            // whose rebuild installs the same blob here for the budget to
            // evict again.
            if self
                .store
                .copy_index_v3_last_blob_holder(blob, &self.node)
                .await
                .map_err(std::io::Error::other)?
            {
                return Ok(false);
            }
        }
        match tokio::fs::remove_file(&candidate.path).await {
            Ok(()) => {
                if let Some(blob) = blob {
                    // The variant stays valid on its other holders; only
                    // this node's rows for the bytes it no longer has go.
                    self.store
                        .forget_copy_index_v3_blob_holder(blob, &self.node)
                        .await
                        .map_err(std::io::Error::other)?;
                }
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }
}

/// Install under the node's disk budget with the same staged-write and rename
/// discipline as v2 blobs. An existing identical blob is accepted as is.
pub(crate) async fn install_blob(
    root: &Path,
    contract_key: &str,
    blob_sha256: &str,
    blob: &[u8],
) -> Result<(), String> {
    let path = v3_blob_path(root, blob_sha256).ok_or("invalid v3 blob digest")?;
    if plurx_core::store::cluster_fragment_index_blob_sha256(blob) != blob_sha256 {
        return Err("v3 blob digest mismatch".into());
    }
    if let Ok(existing) = tokio::fs::read(&path).await {
        if plurx_core::store::cluster_fragment_index_blob_sha256(&existing) == blob_sha256 {
            return Ok(());
        }
    }
    let parent = path.parent().ok_or("v3 blob path has no parent")?;
    let reservation = crate::playback_cache::reserve(parent, blob.len() as u64)
        .await
        .map_err(|error| format!("fragment-index disk budget: {error}"))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| format!("create {}: {error}", parent.display()))?;
    let staging = parent.join(format!(".{contract_key}.{}.tmp", uuid::Uuid::new_v4()));
    let written = async {
        use tokio::io::AsyncWriteExt;
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&staging)
            .await?;
        file.write_all(blob).await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&staging, &path).await
    }
    .await;
    if let Err(error) = written {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(format!("install {}: {error}", path.display()));
    }
    if let Some(reservation) = reservation {
        reservation.commit(blob.len() as u64);
    }
    Ok(())
}
