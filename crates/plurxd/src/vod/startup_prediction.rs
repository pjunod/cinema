//! Bounded, process-local resume predictions for exact ordinary copy renditions.
use super::*;
use crate::subtitle_translation::ReadPreparationSlot;
use plurx_core::domain::{InProgressItem, WatchState};
use plurx_core::segplan::{PlanCut, PlanEntryKind};
use std::sync::Weak;
use tokio_util::sync::CancellationToken;

const MAX_SEEDS: usize = 64;
const SEED_TTL: Duration = Duration::from_secs(3_600);
const RETRY_INTERVAL: Duration = Duration::from_secs(30);
const PREPARATION_TIME: Duration = Duration::from_secs(20);
const SOURCE_REAP_GRACE: Duration = Duration::from_secs(1);
const MAX_SEGMENTS: usize = 8;
const RUNWAY_SECONDS: u64 = 18;
const BYTE_CAP: u64 = 64 * 1024 * 1024;

struct Seed {
    user_id: i64,
    item_id: i64,
    incarnation: Weak<()>,
    rendition_key: String,
    producer: Weak<ProducerSlot>,
    expires: Instant,
    rendered: bool,
    attempted_at: Option<Instant>,
}

#[derive(Default)]
pub(super) struct ResumePredictions {
    seeds: StdMutex<VecDeque<Seed>>,
}

impl ResumePredictions {
    fn remember(&self, user_id: i64, session: &Session, rendition: &Rendition) {
        let Ok(mut seeds) = self.seeds.try_lock() else {
            return;
        };
        let incarnation = Arc::downgrade(&session.incarnation);
        if seeds
            .iter()
            .any(|seed| seed.incarnation.ptr_eq(&incarnation))
        {
            return;
        }
        if seeds.len() == MAX_SEEDS {
            seeds.pop_front();
        }
        seeds.push_back(Seed {
            user_id,
            item_id: session.file.item_id,
            incarnation,
            rendition_key: rendition.key.clone(),
            producer: Arc::downgrade(&rendition.slot),
            expires: Instant::now() + SEED_TTL,
            rendered: false,
            attempted_at: None,
        });
        tracing::debug!(target: "plurxd::vodserve", file_id = session.file.id,
            "remembered ordinary VOD resume candidate");
    }

    pub(super) fn confirm_rendered(&self, incarnation: &Arc<()>) {
        let Ok(mut seeds) = self.seeds.try_lock() else {
            return;
        };
        let incarnation = Arc::downgrade(incarnation);
        if let Some(seed) = seeds
            .iter_mut()
            .find(|seed| seed.incarnation.ptr_eq(&incarnation))
        {
            if !seed.rendered {
                tracing::debug!(target: "plurxd::vodserve", item_id = seed.item_id,
                    "VOD resume candidate reached rendering");
            }
            seed.rendered = true;
            seed.expires = Instant::now() + SEED_TTL;
        }
    }

    fn take_prediction(
        &self,
        user_id: i64,
        progress: &[InProgressItem],
        renditions: &HashMap<String, Arc<Rendition>>,
    ) -> Option<(Arc<Rendition>, WatchState)> {
        let now = Instant::now();
        let mut seeds = self.seeds.try_lock().ok()?;
        seeds.retain(|seed| {
            seed.expires > now
                && renditions
                    .get(&seed.rendition_key)
                    .is_some_and(|rendition| seed.producer.ptr_eq(&Arc::downgrade(&rendition.slot)))
        });
        for row in progress {
            if row.state.watched || row.state.position_ms <= 0 || row.item.offloaded_at.is_some() {
                continue;
            }
            let mut matches = seeds.iter_mut().rev().filter(|seed| {
                seed.user_id == user_id && seed.item_id == row.item.id && seed.rendered
            });
            let Some(seed) = matches.next() else {
                continue;
            };
            if matches.any(|other| !seed.producer.ptr_eq(&other.producer))
                || seed
                    .attempted_at
                    .is_some_and(|at| now.duration_since(at) < RETRY_INTERVAL)
            {
                continue;
            }
            if let Some(rendition) = renditions.get(&seed.rendition_key) {
                seed.attempted_at = Some(now);
                return Some((Arc::clone(rendition), row.state));
            }
        }
        None
    }
}

pub(super) fn startup_window(plan: &SegmentPlan, saved_ms: i64) -> Option<(u32, u32)> {
    if saved_ms < 0 || plan.timescale == 0 {
        return None;
    }
    let ticks =
        u64::try_from(u128::try_from(saved_ms).ok()? * u128::from(plan.timescale) / 1_000).ok()?;
    let target = plan
        .entries
        .partition_point(|entry| entry.start_ticks <= ticks);
    let target = target.checked_sub(1)?;
    let entry = plan.entries.get(target)?;
    if entry.kind != PlanEntryKind::Video || ticks >= entry.end_ticks() {
        return None;
    }
    let mut first = target;
    while first > 0 && plan.entries[first - 1].cut != PlanCut::Clean {
        first -= 1;
        if target - first >= MAX_SEGMENTS {
            return None;
        }
    }
    let mut bytes = 0u64;
    let runway = ticks.saturating_add(RUNWAY_SECONDS * u64::from(plan.timescale));
    let mut last = None;
    for entry in plan.entries.iter().skip(first).take(MAX_SEGMENTS) {
        bytes = bytes.checked_add(entry.est_bytes)?;
        if bytes > BYTE_CAP || entry.kind != PlanEntryKind::Video {
            break;
        }
        last = Some(entry.index);
        if entry.end_ticks() >= runway {
            break;
        }
    }
    Some((
        plan.entries[first].index,
        last.filter(|last| *last >= entry.index)?,
    ))
}

impl VodServe {
    /// The exact playback attachment already has its initialization segment
    /// and decode window. Cold items keep AVPlayer's ordinary readiness path.
    /// This remains an optional observation and never delays session creation.
    pub(crate) fn startup_seek_ready_ms(&self, session_id: &str, requested_ms: i64) -> Option<i64> {
        if requested_ms <= 0 {
            return None;
        }
        let sessions = self.shared.sessions.try_lock().ok()?;
        let session = sessions.get(session_id)?;
        // The manifest's existing hint floors a floating-point timestamp.
        // The session reply echoes the client's rounded millisecond.
        if !session
            .preferred_start_ms
            .is_some_and(|hint| hint == requested_ms || hint == requested_ms - 1)
            || !session.children.is_empty()
            || !matches!(session.kind, SessionKind::Copy { .. })
        {
            return None;
        }
        let rendition = session.live_rendition()?;
        let (first, last) = startup_window(&rendition.plan, requested_ms)?;
        let manifest = rendition.manifest.try_lock().ok()?;
        if manifest.next_gap(first).is_some_and(|gap| gap <= last) {
            return None;
        }
        let identity = rendition.identity.try_lock().ok()?;
        identity.identity.as_ref().map(|_| requested_ms)
    }

    /// The native authenticated ingress already authorized this exact file.
    /// A contended registry can skip this hint without delaying the start.
    pub(crate) fn remember_resume_candidate(&self, user_id: i64, file_id: i64, session_id: &str) {
        let Ok(sessions) = self.shared.sessions.try_lock() else {
            return;
        };
        let Some(session) = sessions.get(session_id).filter(|session| {
            session.file.id == file_id
                && session.children.is_empty()
                && session.passive_grant.is_none()
                && session.prepared_incarnation.is_none()
                && session.retained_output.is_none()
                && matches!(session.kind, SessionKind::Copy { .. })
        }) else {
            return;
        };
        let Some(rendition) = session.live_rendition().filter(|rendition| {
            rendition.recipe.encoding.is_none()
                && !rendition.recipe.source_telemetry
                && rendition.index.is_some()
                && rendition.source.is_some()
                && rendition.private_storage.is_none()
        }) else {
            return;
        };
        self.shared
            .resume_predictions
            .remember(user_id, session, rendition);
    }

    /// Prepare the authenticated detail selection through the same copy recipe,
    /// index and rendition key as ordinary Play. This call owns a background
    /// slot before resolving the recipe. It never allocates a player session.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn prepare_selected_copy(
        &self,
        mut prepared: VodRecipeRequest<'_>,
        file: &MediaFile,
        settings: &VodSettings,
        user_id: i64,
        position_ms: i64,
        shutdown: CancellationToken,
        slot: ReadPreparationSlot,
    ) {
        let Some(node_id) = self.shared.cluster_node_id.as_deref() else {
            return;
        };
        let request = prepared.request;
        if position_ms < 0
            || shutdown.is_cancelled()
            || request.file_id != file.id
            || !matches!(request.kind, SessionKind::Copy { .. })
            || request.presentation != crate::transcode::Presentation::Vod
            || request.passive_vod
            || request.vod_only
            || request.source_telemetry.is_some()
            || request.subtitle_burn.is_some()
            || request.continuous_media.is_some()
            || prepared.encoding.is_some()
            || prepared.soundtrack.is_some()
            || prepared.companion.is_some()
            || prepared.root_admission.is_some()
            || prepared.measured_candidate.is_some()
            || !matches!(prepared.retained_capture, RetainedOutputCapture::New)
        {
            return;
        }
        prepared.admission_priority = crate::admission::Priority::Speculative;
        let attachment = match self
            .resolve_rendition_for_purpose(
                &mut prepared,
                file,
                settings,
                None,
                None,
                super::shared::AttachmentPurpose::SelectedDetail,
            )
            .await
        {
            Ok((attachment, _)) => attachment,
            Err(reason) => {
                tracing::debug!(target: "plurxd::vodserve", file_id = file.id, %reason,
                    "selected detail copy prerequisites unavailable");
                return;
            }
        };
        let rendition = Arc::clone(&attachment.rendition);
        // Foreground attach must be able to take this same build gate while
        // a bounded background producer holds its independent source lease.
        drop(attachment);
        tracing::debug!(target: "plurxd::vodserve", file_id = file.id, position_ms,
            rendition = %rendition.key, "selected detail resolved ordinary copy rendition");
        prepare_startup_window(
            Arc::clone(&self.shared.store),
            node_id,
            &rendition,
            user_id,
            StartupPosition::Selected(position_ms),
            shutdown,
            slot,
        )
        .await;
    }

    pub(crate) fn offer_predicted_resume(
        self: &Arc<Self>,
        user_id: i64,
        progress: &[InProgressItem],
        shutdown: &CancellationToken,
        slot: ReadPreparationSlot,
    ) {
        let Some(node_id) = self.shared.cluster_node_id.clone() else {
            return;
        };
        let Ok(renditions) = self.shared.renditions.try_lock() else {
            return;
        };
        let Some((rendition, watch)) =
            self.shared
                .resume_predictions
                .take_prediction(user_id, progress, &renditions)
        else {
            return;
        };
        drop(renditions);
        let store = Arc::clone(&self.shared.store);
        let shutdown = shutdown.clone();
        tokio::spawn(async move {
            prepare_startup_window(
                store,
                &node_id,
                &rendition,
                user_id,
                StartupPosition::Predicted(watch),
                shutdown,
                slot,
            )
            .await;
        });
    }
}

enum StartupPosition {
    Predicted(WatchState),
    Selected(i64),
}

async fn prepare_startup_window(
    store: Arc<dyn Store>,
    node_id: &str,
    rendition: &Arc<Rendition>,
    user_id: i64,
    position: StartupPosition,
    shutdown: CancellationToken,
    slot: ReadPreparationSlot,
) {
    let file = &rendition.recipe.file;
    let position_ms = match position {
        StartupPosition::Predicted(watch) => watch.position_ms,
        StartupPosition::Selected(position_ms) => position_ms,
    };
    let Some((first, last)) = startup_window(&rendition.plan, position_ms) else {
        tracing::debug!(target: "plurxd::vodserve", file_id = file.id,
            "no bounded video window for predicted VOD resume");
        return;
    };
    if window_materialized(rendition, first, last).await {
        tracing::debug!(target: "plurxd::vodserve", file_id = file.id,
            "predicted VOD resume is already materialized");
        return;
    }
    let admission = async {
        !rendition.closed.load(Acquire)
            && rendition.readers.lock().await.is_empty()
            && matches!(store.get_user(user_id).await, Ok(Some(_)))
            && match position {
                StartupPosition::Selected(_) => true,
                StartupPosition::Predicted(watch) => {
                    matches!(store.watch_state(user_id, file.item_id).await, Ok(Some(current)) if current == watch)
                }
            }
            && matches!(store.get_file(file.id).await, Ok(Some(current))
                if current.item_id == file.item_id && current.size == file.size
                    && current.mtime == file.mtime && current.path == file.path)
            && store
                .get_setting(plurx_core::store::keys::VOD_PRESENTATION)
                .await
                .is_ok_and(|setting| setting.is_none_or(|value| value.trim() != "0"))
    };
    let admitted = tokio::select! {
        _ = shutdown.cancelled() => false,
        result = tokio::time::timeout(Duration::from_secs(2), admission) => result.unwrap_or(false),
    };
    if !admitted {
        tracing::debug!(target: "plurxd::vodserve", file_id = file.id,
            "predicted VOD resume no longer meets admission");
        return;
    }
    run_with_source_custody(store, node_id, rendition, first, last, shutdown, slot).await;
}

async fn window_materialized(rendition: &Rendition, first: u32, last: u32) -> bool {
    if rendition
        .manifest
        .lock()
        .await
        .next_gap(first)
        .is_some_and(|gap| gap <= last)
    {
        return false;
    }
    rendition.identity.lock().await.identity.is_some()
}

async fn run_with_source_custody(
    store: Arc<dyn Store>,
    node_id: &str,
    rendition: &Arc<Rendition>,
    first: u32,
    last: u32,
    shutdown: CancellationToken,
    slot: ReadPreparationSlot,
) {
    use crate::background_jobs::{settle_read_lease, SourceReadBudget};
    use plurx_core::cluster::coordination::{LeaseClaim, StoreCoordinator};
    use plurx_core::store::background_jobs_resources::{
        BackgroundSourceOwner, BackgroundSourceRegistration,
    };
    let Ok(coordinator) = StoreCoordinator::new(Arc::clone(&store), node_id.to_owned()) else {
        return;
    };
    let Ok(LeaseClaim::Acquired(lease)) = coordinator
        .acquire(
            &slot.resource(node_id),
            PREPARATION_TIME + Duration::from_secs(5),
        )
        .await
    else {
        return;
    };
    let file = &rendition.recipe.file;
    let deadline = Instant::now() + PREPARATION_TIME;
    let owner = BackgroundSourceOwner::ReadAttestation {
        lease,
        file_id: file.id,
        source_size: file.size,
        source_mtime: file.mtime,
    };
    let admission_deadline = Instant::now() + SOURCE_REAP_GRACE;
    let custody = loop {
        match store
            .register_background_source_custody(owner.clone(), crate::state::clock_ms())
            .await
        {
            Ok(BackgroundSourceRegistration::Registered(receipt)) => break receipt,
            // Public release detaches the reader before the real producer
            // and source bridge finish reaping. Give that exact cleanup
            // time to settle without reading media or disturbing a viewer.
            Ok(BackgroundSourceRegistration::LostOwnership)
                if !shutdown.is_cancelled()
                    && Instant::now() < admission_deadline
                    && !rendition.closed.load(Acquire)
                    && rendition
                        .readers
                        .try_lock()
                        .is_ok_and(|readers| readers.is_empty()) =>
            {
                tokio::select! {
                    _ = shutdown.cancelled() => {},
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {},
                }
            }
            _ => {
                tracing::debug!(target: "plurxd::vodserve", file_id = file.id,
                    "predicted VOD resume did not obtain source custody");
                settle_read_lease(&store, &owner, None, false).await;
                return;
            }
        }
    };
    if crate::source_custody::activate_background_with_journal(&store, custody.clone())
        .await
        .is_err()
    {
        settle_read_lease(&store, &owner, Some(custody), false).await;
        return;
    }
    let cancel = shutdown.child_token();
    let budget =
        SourceReadBudget::from_existing_custody(Arc::clone(&store), &custody, cancel.clone());
    let deadline_elapsed = tokio::time::sleep_until(deadline.into());
    tokio::pin!(deadline_elapsed);
    let source_current = match rendition.source.as_ref() {
        Some(source) => {
            // A pathname check can wait on storage before the producer
            // exists. It spends the same budget as preparation.
            tokio::select! {
                biased;
                _ = cancel.cancelled() => false,
                _ = budget.wait_pressure() => false,
                _ = &mut deadline_elapsed => false,
                result = source.reopen(file) => result.is_ok(),
            }
        }
        None => false,
    };
    let work = source_current
        .then(|| {
            rendition.queue_resume_preparation(first, last, BYTE_CAP, deadline, Arc::clone(&budget))
        })
        .flatten();
    if let Some(work) = work.as_ref() {
        tokio::select! {
            _ = work.wait_settled() => {},
            _ = budget.wait_pressure() => {},
            _ = &mut deadline_elapsed => {},
            _ = shutdown.cancelled() => {},
        }
        cancel.cancel();
        work.cancel();
        work.wait_settled().await;
    }
    // The producer and inherited source-read bridge have now been joined,
    // or nothing was started. The slot survives through durable settlement.
    settle_read_lease(&store, &owner, Some(custody), true).await;
    if work.is_some() {
        let materialized = window_materialized(rendition, first, last).await;
        tracing::debug!(
            target: "plurxd::vodserve",
            file_id = file.id,
            first_entry = first,
            last_entry = last,
            materialized,
            "predicted VOD resume preparation settled"
        );
    } else {
        tracing::debug!(target: "plurxd::vodserve", file_id = file.id,
            source_current, "predicted VOD resume could not queue the producer");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::segplan::PlanEntry;

    fn plan(cuts: &[PlanCut]) -> SegmentPlan {
        let entries = cuts
            .iter()
            .enumerate()
            .map(|(index, &cut)| PlanEntry {
                index: u32::try_from(index).expect("fixture index"),
                kind: PlanEntryKind::Video,
                start_ticks: 6_000 * index as u64,
                duration_ticks: 6_000,
                est_bytes: 4 * 1024 * 1024,
                cut,
                fragments: 1,
            })
            .collect();
        SegmentPlan {
            version: 1,
            timescale: 1_000,
            entries,
            target_duration: 6,
        }
    }

    #[test]
    fn predicted_resume_starts_at_a_decode_boundary_and_excludes_the_audio_tail() {
        let mut media = plan(&[
            PlanCut::Clean,
            PlanCut::ByteCeiling,
            PlanCut::Clean,
            PlanCut::Clean,
            PlanCut::EndOfStream,
        ]);
        media.entries[4].kind = PlanEntryKind::AudioTail;
        media.entries[4].fragments = 0;

        assert_eq!(startup_window(&media, 0), Some((0, 2)));
        assert_eq!(startup_window(&media, -1), None);
        assert_eq!(startup_window(&media, 13_000), Some((1, 3)));
        assert_eq!(startup_window(&media, 24_000), None);
        assert_eq!(startup_window(&media, 30_000), None);

        media.entries[1].est_bytes = BYTE_CAP;
        assert_eq!(startup_window(&media, 13_000), None);
        assert_eq!(
            startup_window(&plan(&[PlanCut::TimeCeiling; 9]), 48_000),
            None
        );
    }
}
