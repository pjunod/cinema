//! Private Source growing-range backing. Neither this physical owner nor
//! its status observations can mint a Sharing grant or an Analysis claim.
use super::*;
use crate::{
    copyrange::CopyRangeProduction,
    source_generation::{SourceRenditionOwner, SourceRenditionOwners},
};

pub(crate) struct SourceGrowingPreparation {
    owners: Arc<SourceRenditionOwners>,
    source_owner: Arc<SourceRenditionOwner>,
    stop: tokio_util::sync::CancellationToken,
}
impl SourceGrowingPreparation {
    pub(super) fn retire(&self) {
        self.stop.cancel();
        self.source_owner.begin_detach();
    }
    pub(super) async fn wait_stopped(
        &self,
    ) -> crate::source_generation::SourceProducerAssociationsSettled {
        self.retire();
        let _retained_owners = Arc::clone(&self.owners);
        self.source_owner.detach_and_wait().await
    }
}
/// The control lease a growing copy keeps: the shared session lease every
/// shared client is promised (`lease_timeout_ms` 300 000). Its Start
/// bootstrap advertises it, its control replies report it, and the copy
/// retires only once that long has passed without viewer activity, so each
/// layer names the lease that is actually honoured.
pub(crate) const GROWING_LEASE_TIMEOUT_MS: u32 = crate::playback_control::VOD_LEASE_TIMEOUT_MS;

/// When a growing copy last saw its viewer, against an explicit clock.
pub(crate) struct GrowingActivity(std::sync::Mutex<Instant>);
impl GrowingActivity {
    pub(crate) fn new(now: Instant) -> Self {
        Self(std::sync::Mutex::new(now))
    }
    pub(crate) fn note_at(&self, now: Instant) {
        let mut last = self.0.lock().expect("Source growing activity");
        *last = (*last).max(now);
    }
    pub(crate) fn expired_at(&self, now: Instant) -> bool {
        now.saturating_duration_since(*self.0.lock().expect("Source growing activity"))
            >= Duration::from_millis(u64::from(GROWING_LEASE_TIMEOUT_MS))
    }
}

pub(crate) struct SourceGrowingBacking {
    pub(crate) session_id: String,
    pub(crate) file: Arc<plurx_core::domain::MediaFile>,
    pub(crate) request: SessionRequest,
    pub(crate) production: Arc<CopyRangeProduction>,
    pub(crate) delivery: Arc<crate::meter::Meter>,
    activity: GrowingActivity,
    pub(crate) lifecycle: tokio::sync::Mutex<()>,
    pub(crate) control: tokio::sync::Mutex<super::control::GrowingControlState>,
    pub(crate) owners: Arc<SourceRenditionOwners>,
    pub(crate) source_owner: Arc<SourceRenditionOwner>,
    assignment: SourceDispatchAssignment,
    source: crate::fragment_index_cluster::SourceFence,
    catalog_namespace: String,
    observation_source: plurx_core::store::analysis_coordination::AnalysisCoordinationSource,
    recipe: crate::copy_analysis::CopyRecipe,
    range_id: uuid::Uuid,
    range_epoch: u64,
}
impl SourceGrowingBacking {
    /// The path is independently reopened and checked against this range's
    /// held object, so a replacement cannot authorize cached old AV bytes.
    /// A copy that is no longer live, or an object proven not the one this
    /// range holds, is the session's end: a retry could only keep failing
    /// against a replaced file. An object that could not be read just now
    /// passes; the caller bounds that by the confirmed lease.
    pub(crate) async fn physical_fence(
        &self,
    ) -> Result<crate::fragment_index_cluster::SourceFence, super::LiveRefusal> {
        if !self.is_live() {
            return Err(super::LiveRefusal::terminal(SourceWorkerError::Unavailable));
        }
        self.source
            .reopen_classified(&self.file)
            .await
            .map_err(|error| super::LiveRefusal::from_fence(&error))
    }
    pub(crate) fn is_live(&self) -> bool {
        self.production.is_live()
            && self.owners.contains_live_assignment(&self.assignment)
            && self.source.unchanged()
    }
    /// Viewer activity: the first publication, an authenticated resource
    /// fetch, or a control exchange the fence accepted. A refused exchange is
    /// not, or a viewer whose anchor never reaches the produced range would
    /// hold this copy without being served; neither is B's own Status, which
    /// says B's owner is alive, not that anyone is watching.
    pub(crate) fn note_activity(&self) {
        self.activity.note_at(Instant::now());
    }
    /// Whether the lease has run out at `now`: one lease after the last
    /// viewer activity. The retirement loop asks it for the current instant.
    pub(crate) fn activity_expired_at(&self, now: Instant) -> bool {
        self.activity.expired_at(now)
    }
    fn activity_expired(&self) -> bool {
        self.activity_expired_at(Instant::now())
    }
    pub(crate) async fn observe_demand(
        &self,
        demand: crate::playback_control::PlaybackDemandSnapshot,
    ) -> Result<(), String> {
        self.note_activity();
        let watermarks = rolling_source_watermarks(demand.playback_rate.clamp(0.25, 4.0));
        self.production.observe_demand(demand, watermarks).await
    }
    pub(crate) fn retire(&self) {
        self.production.retire();
        self.source_owner.begin_detach();
    }
    pub(crate) async fn wait_stopped(
        &self,
    ) -> Result<crate::source_generation::SourceProducerAssociationsSettled, SourceWorkerError>
    {
        self.retire();
        let _ = self.production.wait_stopped().await;
        Ok(self.source_owner.detach_and_wait().await)
    }
}

impl SourceViewerActor {
    /// Every refusal states whether it passes ([`super::LiveRefusal`]): a
    /// resource the receiver must not retire a live session over is
    /// `transient`; a malformed request or a session that is over is not.
    pub(super) async fn open_growing_resource_owned(
        &self,
        backing: Arc<SourceGrowingBacking>,
        resource: &SharingHlsResource,
        deadline: Instant,
    ) -> Result<SourceOpenedResource, super::LiveRefusal> {
        use super::LiveRefusal;
        let unsupported = LiveRefusal::terminal(SourceWorkerError::Unsupported);
        let ended = LiveRefusal::terminal(SourceWorkerError::Unavailable);
        let pending = LiveRefusal::transient(SourceWorkerError::Unavailable);
        let native = self
            .0
            .state
            .lock()
            .expect("Source native resource")
            .native
            .clone();
        let query = resource.as_str().split_once('?').map(|(_, query)| query);
        if query.is_some_and(|query| {
            query.split('&').any(|part| match part.split_once('=') {
                Some(("native", "1")) => false,
                Some(("subtitle", value)) => native
                    .as_ref()
                    .is_none_or(|tracks| value.parse::<i64>().ok() != tracks.selected()),
                _ => true,
            })
        }) || (query.is_some() && native.is_none())
        {
            return Err(unsupported);
        }
        let kind = if resource.kind() == SharingHlsResourceKind::Index
            && query.is_some_and(|query| query.split('&').any(|part| part == "native=1"))
        {
            SharingHlsResourceKind::Master
        } else {
            resource.kind()
        };
        {
            let mut current = self.0.state.lock().expect("Source worker state");
            if current.retirement_requested
                || current
                    .start
                    .as_ref()
                    .and_then(|start| start.as_ref().ok())
                    .is_none_or(|start| start.session_id != backing.session_id)
            {
                return Err(ended);
            }
            // Bodies close as their transfers end.
            if current.bodies >= 64 {
                return Err(pending);
            }
            current.bodies += 1;
        }
        let mut guard = SourceResponseGuard {
            owner: Arc::clone(&self.0),
            source: None,
            source_request: None,
        };
        let initial = self.0.gate.current_owned(&self.0.assignment).await?;
        // An observation older than its bound is refreshed by the next read.
        initial
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| pending)?;
        guard.source = Some(
            backing
                .physical_fence()
                .await
                .map_err(|refusal| self.0.gate.bounded(refusal))?,
        );
        let guard = Arc::new(guard);
        let custody = resource::SourceResourceReadCustody::new(
            Arc::clone(&guard),
            Arc::clone(&self.0.resource_hooks),
        );
        let payload = match kind {
            SharingHlsResourceKind::Index | SharingHlsResourceKind::Video => {
                SourceResourcePayload::Playlist(backing.production.files.lock().await.playlist())
            }
            SharingHlsResourceKind::Init | SharingHlsResourceKind::MediaSegment => {
                let name = resource
                    .as_str()
                    .split('?')
                    .next()
                    .unwrap_or("")
                    .rsplit('/')
                    .next()
                    .unwrap_or("");
                // The wait window starts here: what production has
                // committed by now is what a deadline is judged against.
                let waiter = crate::copyrange::CommitWaiter::begin(
                    backing.production.as_ref(),
                    tokio::time::Instant::from_std(deadline),
                )
                .await;
                let mut ready = loop {
                    let changed = backing.production.changed.notified();
                    tokio::pin!(changed);
                    changed.as_mut().enable();
                    let files = backing.production.files.lock().await;
                    let segment = files.segments().iter().find(|segment| segment.name == name);
                    if (kind == SharingHlsResourceKind::Init
                        && name == "init.mp4"
                        && files.canonical_init().is_some())
                        || segment.is_some()
                    {
                        let etag = segment
                            .map(|segment| segment.sha256.clone())
                            .unwrap_or_else(|| format!("{}-init", backing.session_id));
                        let duration = segment.map(|segment| {
                            (segment.seconds * 1000.0)
                                .round()
                                .clamp(0.0, u32::MAX as f64) as u32
                        });
                        let mut ready = custody
                            .open_ready_child(
                                &files.payload,
                                &files.payload_path.join(name),
                                name,
                                &etag,
                                &backing.delivery,
                            )
                            .await
                            .map_err(|_| pending)?;
                        ready.observed_media_duration_ms = duration;
                        break ready;
                    }
                    // A range that ended without it, or a copy that died,
                    // will never publish it; one still running may yet.
                    if files.complete() || !backing.is_live() {
                        return Err(ended);
                    }
                    drop(files);
                    if let Err(end) = waiter.changed(changed).await {
                        // Anything not yet published is at or past the
                        // production head. A viewer waiting there while the
                        // bank holds is ahead of the bank's own picture of
                        // it: re-anchor the bank to it, so production resumes,
                        // and a hold that persists no longer excuses a wait.
                        if end == crate::copyrange::DeadlineEnd::Slow
                            && backing.production.is_holding()
                        {
                            if let Err(error) = backing.production.reanchor_at_head().await {
                                // The renewal tick's own read-bank refresh
                                // ends a copy whose observer fails.
                                tracing::warn!(target: "plurxd::transcode", incarnation = %self.0.assignment.binding().incarnation_id(), stage = "growing.reanchor", %error, "Source growing copy could not re-anchor its read bank");
                            }
                        }
                        return Err(
                            self.deadline_refusal(end == crate::copyrange::DeadlineEnd::Stalled)
                        );
                    }
                };
                // The opened immutable segment is pinned through the real body.
                ready.delivery = Arc::clone(&backing.delivery);
                SourceResourcePayload::File(ready)
            }
            SharingHlsResourceKind::Master
            | SharingHlsResourceKind::SubtitlePlaylist { .. }
            | SharingHlsResourceKind::SubtitleSegment { .. } => {
                // The master needs no prepared subtitle tracks: without them
                // it admits none and still declares the session's range.
                let tracks = native.as_ref();
                if kind != SharingHlsResourceKind::Master && tracks.is_none() {
                    return Err(unsupported);
                }
                if tracks.is_some_and(|tracks| !tracks.matches(&self.0.assignment, &backing.file)) {
                    return Err(ended);
                }
                let manager = self.0.manager.upgrade().ok_or(ended)?;
                let probe = self
                    .0
                    .state
                    .lock()
                    .expect("Source copy presentation")
                    .presentation_probe
                    .clone();
                let files = backing.production.files.lock().await;
                let video = files.playlist();
                // Known once the copy's first landing is verified.
                let origin = files.media_origin_ms().ok_or(pending)? as f64 / 1000.0;
                drop(files);
                match (kind, tracks) {
                    (SharingHlsResourceKind::Master, tracks) => {
                        let SessionKind::Copy {
                            aac,
                            preserve_dolby_vision,
                            convert_dolby_vision,
                            dual_layer_as_copied,
                        } = backing.request.kind
                        else {
                            return Err(unsupported);
                        };
                        let (codecs, supplemental_codecs) = copied_hls_codecs(
                            &backing.file,
                            backing.request.audio_index,
                            CopySessionOptions {
                                transcode_audio: aac,
                                preserve_dolby_vision,
                                convert_dolby_vision,
                                dual_layer_as_copied,
                            },
                            probe.as_deref(),
                        );
                        let context = HlsContext {
                            codec_facts: Some(super::super::response::FrozenHlsCodecFacts::audio(
                                backing.request.audio_delivery.as_ref(),
                                !backing.file.audio_streams.is_empty(),
                                aac,
                            )),
                            bandwidth: None,
                            file_id: backing.file.id,
                            start_seconds: backing.request.start_seconds,
                            media_origin_seconds: origin,
                            codecs,
                            supplemental_codecs,
                            frame_rate: frozen_video_frame_rate(probe.as_deref()),
                        };
                        SourceResourcePayload::Playlist(crate::http::hls::source_native_master(
                            &backing.file,
                            tracks.and_then(|tracks| tracks.selected()),
                            &context,
                            &tracks.map(|tracks| tracks.indexes()).unwrap_or_default(),
                        ))
                    }
                    (SharingHlsResourceKind::SubtitlePlaylist { index }, Some(tracks)) => {
                        if !tracks.has_track(index) {
                            return Err(unsupported);
                        }
                        SourceResourcePayload::Playlist(crate::http::hls::source_native_playlist(
                            &video,
                        ))
                    }
                    (SharingHlsResourceKind::SubtitleSegment { index }, Some(tracks)) => {
                        if !tracks.has_track(index) {
                            return Err(unsupported);
                        }
                        if let Some(revision) = resource.cached_subtitle_revision() {
                            let cached = tracks
                                .cached(index, revision, &custody)
                                .await
                                .map_err(subtitle_refusal)?;
                            let absent = cached.is_none();
                            let (bytes, complete) = cached.unwrap_or_default();
                            SourceResourcePayload::CachedSubtitle {
                                bytes,
                                complete,
                                absent,
                            }
                        } else {
                            let name = resource
                                .as_str()
                                .split('?')
                                .next()
                                .unwrap_or("")
                                .rsplit('/')
                                .next()
                                .unwrap_or("");
                            let sequence = name
                                .strip_prefix("seg")
                                .and_then(|name| name.strip_suffix(".vtt"))
                                .and_then(|name| name.parse::<u64>().ok())
                                .ok_or(unsupported)?;
                            // The same windowed sparse read the full-VOD
                            // Source uses; no separate whole-track extraction.
                            let bytes = tracks
                                .segment(
                                    &manager, index, &video, sequence, origin, &custody, deadline,
                                )
                                .await
                                .map_err(subtitle_refusal)?
                                .ok_or(pending)?;
                            if bytes.len() > 2 * 1024 * 1024 {
                                return Err(LiveRefusal::terminal(SourceWorkerError::Capacity));
                            }
                            SourceResourcePayload::SubtitleText(bytes)
                        }
                    }
                    _ => return Err(unsupported),
                }
            }
        };
        if let SourceResourcePayload::Playlist(bytes) = &payload {
            plurx_core::sharing_resources::validate_sharing_playlist(resource, bytes)
                .map_err(|_| unsupported)?;
        }
        if Instant::now() >= deadline {
            return Err(LiveRefusal::transient(SourceWorkerError::Deadline));
        }
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        proof
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| pending)?;
        if !backing.is_live()
            || self
                .0
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
        {
            return Err(ended);
        }
        let route = self.0.gate.renew_with(&self.0.assignment, &proof).await?;
        if route.session_id != backing.session_id
            || route.publication_ready_at_ms != 0
            || guard
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
        {
            return Err(ended);
        }
        // Every authenticated resource the viewer fetches is activity; only
        // a served media segment is presentation evidence.
        backing.note_activity();
        if kind == SharingHlsResourceKind::MediaSegment {
            backing.note_media_served().await;
        }
        drop(custody);
        // A response body still shares this guard; it is released with it.
        let guard = Arc::try_unwrap(guard)
            .map_err(|_| LiveRefusal::transient(SourceWorkerError::Unresolved))?;
        Ok(SourceOpenedResource { payload, guard })
    }
}

impl SourceViewerActor {
    /// A resource wait that reached its deadline. It passes only if the
    /// range's production advanced during the wait
    /// ([`crate::copyrange::CopyRangeProduction::stalled_since`]): a slow
    /// producer is still producing, and the viewer's next request will find
    /// more. A producer that committed nothing in a whole wait window,
    /// without holding by design, is stuck: no retry can be served, so the
    /// refusal is terminal and the session is retired, closing its
    /// connections, rather than kept alive by its viewer's polling.
    pub(super) fn deadline_refusal(&self, stalled: bool) -> super::LiveRefusal {
        if !stalled {
            return super::LiveRefusal::transient(SourceWorkerError::Deadline);
        }
        // The inner wait and the request's own deadline can both judge the
        // same stall; only the one that starts the retirement says so.
        if !self.is_retiring() {
            tracing::warn!(target: "plurxd::transcode", incarnation = %self.0.assignment.binding().incarnation_id(), stage = "growing.producer_stalled", "Source growing copy committed nothing in a resource wait; retiring it");
        }
        let stuck = super::LiveRefusal::terminal(SourceWorkerError::Unavailable);
        self.retire_on_terminal(Some(&stuck));
        stuck
    }
}

/// A selected-caption window's own answer: still preparing (`Deadline`) or
/// over its concurrent-work bound (`Capacity`) passes; anything else is this
/// track's final answer for this request.
pub(super) fn subtitle_refusal(error: SourceWorkerError) -> super::LiveRefusal {
    if matches!(
        error,
        SourceWorkerError::Deadline | SourceWorkerError::Capacity
    ) {
        super::LiveRefusal::transient(error)
    } else {
        super::LiveRefusal::terminal(error)
    }
}

impl TranscodeManager {
    async fn prepare_source_growing(
        &self,
        owner: &Arc<SourceViewerInner>,
        state: &crate::state::AppState,
        prepared: &crate::http::hls::PreparedSourcePlayback,
        session_id: String,
        deadline: Instant,
        stage: &mut &'static str,
    ) -> Result<Arc<SourceGrowingBacking>, SourceWorkerError> {
        use crate::copyrange::{
            run_source_census, spawn_source_generation, CopyGenerationOwner, CopyRangeFiles,
            ObservedCopyMux,
        };
        *stage = "growing.copy_kind";
        let SessionKind::Copy {
            aac,
            preserve_dolby_vision,
            convert_dolby_vision,
            ..
        } = prepared.request().kind
        else {
            return Err(SourceWorkerError::Unsupported);
        };
        *stage = "growing.preparation_authority";
        let authority = owner.gate.current_preparation(&owner.assignment).await?;
        *stage = "growing.probe_evidence";
        let probe = self
            .store
            .source_index_probe_evidence(&authority)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?;
        if probe
            .as_ref()
            .is_some_and(|probe| probe.len() > 1024 * 1024)
        {
            *stage = "growing.probe_bound";
            return Err(SourceWorkerError::Unsupported);
        }
        // The master presents the same probe this copy was planned from.
        owner
            .state
            .lock()
            .expect("Source copy presentation")
            .presentation_probe
            .clone_from(&probe);
        let video = self.source_copy_video_options(
            prepared.file(),
            probe.as_deref(),
            preserve_dolby_vision,
            convert_dolby_vision,
        );
        *stage = "growing.copy_admission";
        let permit = self.admit_source_copy(deadline).await?;
        *stage = "growing.rendition_owner";
        let owners = Arc::new(SourceRenditionOwners::default());
        let source_owner = owners
            .attach(&owner.assignment)
            .map_err(|_| SourceWorkerError::Unavailable)?;
        owners
            .arm_initial_permit(&source_owner, permit, &self.admissions, &owner.gate)
            .map_err(|_| SourceWorkerError::Unavailable)?;
        let preparing = Arc::new(SourceGrowingPreparation {
            owners: Arc::clone(&owners),
            source_owner: Arc::clone(&source_owner),
            stop: tokio_util::sync::CancellationToken::new(),
        });
        {
            let mut current = owner.state.lock().expect("Source worker state");
            if current.retirement_requested {
                *stage = "growing.retirement_requested";
                return Err(SourceWorkerError::Unavailable);
            }
            current.backing = SourceBacking::PreparingRange(Arc::clone(&preparing));
        }
        let file = Arc::new(prepared.file().clone());
        *stage = "growing.playback_fence";
        let source = crate::fragment_index_cluster::open_source_playback_fence(&file, None)
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
        *stage = "growing.source_request";
        let request = self
            .foreground_source_request(
                &file,
                &owner.assignment.binding().incarnation_id().to_string(),
                owner.assignment.binding().incarnation_id(),
                plurx_core::store::background_jobs_resources::ForegroundSourcePolicy::Rolling,
            )
            .ok_or(SourceWorkerError::Unavailable)?;
        *stage = "growing.head_dispatch";
        // The stage alone cannot tell a dispatch capacity refusal from an
        // authority that could not be proven current; each names its reason.
        let at = *stage;
        let dispatch = owners.begin_generation().map_err(|reason| {
            tracing::warn!(target: "plurxd::transcode", stage = at, reason, "Source producer dispatch refused");
            SourceWorkerError::Unavailable
        })?;
        let permission = dispatch
            .authorize_preparing_before_spawn()
            .await
            .map_err(|reason| {
                tracing::warn!(target: "plurxd::transcode", stage = at, reason = %reason, "Source producer authority refused");
                SourceWorkerError::Unavailable
            })?;
        *stage = "growing.head_input";
        let input = crate::source_input::SourceInput::open_foreground_held(
            request.clone(),
            source
                .reopen(&file)
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?,
            "source-copy-head",
        )
        .await
        .map_err(|_| SourceWorkerError::Unavailable)?;
        *stage = "growing.copy_head_census";
        let census = Arc::new(
            run_source_census(
                input,
                source.object_version().to_owned(),
                &file,
                video,
                CopyGenerationOwner::Source {
                    owners: &owners,
                    dispatch,
                    authority: permission,
                },
                Arc::new(crate::prodrun::ProducerSlot::new()),
                &self.runtime_cache,
                Some(deadline),
                &preparing.stop,
            )
            .await
            .map_err(|error| {
                tracing::warn!(target: "plurxd::transcode", reason = %error, "Source copy head refused");
                SourceWorkerError::copy_failure(&error)
            })?,
        );
        *stage = "growing.copy_head_clock";
        if census.format_start.is_none() {
            return Err(SourceWorkerError::CopyClockUnknown);
        }
        // Every later generation reads this same held object; a census of a
        // different witness cannot establish its clock or normalization.
        if census.source_object_version != source.object_version() {
            *stage = "growing.object_version";
            return Err(SourceWorkerError::RescanRequired);
        }
        *stage = "growing.normalization_contract";
        let contract = Arc::new(
            crate::copy_normalization::CopyNormalizationContract::from_head(
                &file,
                video,
                &census.raw_canonical_init,
                &census.raw_canonical_fragment,
            )
            .map_err(|_| SourceWorkerError::Unsupported)?,
        );
        *stage = "growing.copy_head_begin";
        let head = contract
            .begin(
                census.raw_canonical_init.clone(),
                census.raw_av_init.clone(),
            )
            .map_err(|_| SourceWorkerError::RescanRequired)?;
        *stage = "growing.copy_head_caps";
        prepared
            .validate_copy_head(head.canonical_init())
            .map_err(|_| SourceWorkerError::RescanRequired)?;
        // The cluster's shared catalogue identity: file ids are scoped to it,
        // and every member reports the same value.
        *stage = "growing.catalog_namespace";
        let namespace = state
            .store
            .instance_id()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
        let video_identity = crate::fragindex::identity_for(&file, video).argv_fingerprint;
        *stage = "growing.observation_source";
        let observation_source = crate::copy_analysis::source_for_copy(
            &namespace,
            &file,
            &video_identity,
            source.object_version(),
            owner.assignment.owner_node_id(),
        )
        .map_err(|_| SourceWorkerError::Unavailable)?;
        *stage = "growing.observation_root";
        let observation_root = state
            .copy_observation_dir
            .as_ref()
            .ok_or(SourceWorkerError::Unavailable)?;
        let range_id = uuid::Uuid::new_v4();
        *stage = "growing.range_files";
        let files = CopyRangeFiles::create(
            &self.work_dir,
            observation_root,
            namespace.clone(),
            observation_source.clone(),
            owner.assignment.clone(),
            range_id,
            1,
            census.first_dts,
            0,
            contract.init_contract_sha256().to_owned(),
            contract.promotion().clone(),
        )
        .await
        .map_err(|_| SourceWorkerError::Unavailable)?;
        // The one recipe of this range. An Analysis gap fill of the same
        // source repeats it, so its rows are comparable with these.
        let recipe = crate::copy_analysis::CopyRecipe {
            video,
            audio_index: prepared.request().audio_index,
            transcode_audio: aac,
            audio_delivery: prepared.request().audio_delivery.clone(),
        };
        *stage = "growing.copy_mux";
        let mux = ObservedCopyMux::new(
            Arc::clone(&contract),
            census.first_dts,
            0,
            recipe.tracks(&file),
            video.retains_hevc_parameter_sets(),
            file.video_codec.as_deref() == Some("hevc"),
        )
        .map_err(|_| SourceWorkerError::Unsupported)?;
        *stage = "growing.copy_args";
        let args = recipe
            .args(&file, prepared.request().start_seconds)
            .map_err(|_| SourceWorkerError::Unsupported)?;
        *stage = "growing.primary_dispatch";
        // The stage alone cannot tell a dispatch capacity refusal from an
        // authority that could not be proven current; each names its reason.
        let at = *stage;
        let dispatch = owners.begin_generation().map_err(|reason| {
            tracing::warn!(target: "plurxd::transcode", stage = at, reason, "Source producer dispatch refused");
            SourceWorkerError::Unavailable
        })?;
        let permission = dispatch
            .authorize_preparing_before_spawn()
            .await
            .map_err(|reason| {
                tracing::warn!(target: "plurxd::transcode", stage = at, reason = %reason, "Source producer authority refused");
                SourceWorkerError::Unavailable
            })?;
        // The census Input and every reader are joined. Reopen independently
        // against the exact object before the primary operation gets a URL.
        *stage = "growing.primary_input";
        let mut input = crate::source_input::SourceInput::open_foreground_held(
            request,
            source
                .reopen(&file)
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?,
            "source-copy-primary",
        )
        .await
        .map_err(|_| SourceWorkerError::Unavailable)?;
        let seek_bootstrap =
            match input.seal_seek_bootstrap_limit(crate::copyrange::SOURCE_SEEK_BOOTSTRAP_BYTES) {
                Ok(handle) => handle,
                Err(_) => {
                    let _ = input.close_and_wait().await;
                    return Err(SourceWorkerError::Unavailable);
                }
            };
        *stage = "growing.source_observer";
        let observer = input.observer();
        input
            .publish_source_observer(&owners, 1)
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
        *stage = "growing.primary_spawn";
        let running = spawn_source_generation(
            input,
            &file.path,
            &args,
            &self.runtime_cache,
            CopyGenerationOwner::Source {
                owners: &owners,
                dispatch,
                authority: permission,
            },
            Arc::new(crate::prodrun::ProducerSlot::new()),
        )
        .await
        .map_err(|_| SourceWorkerError::Unavailable)?;
        let production = CopyRangeProduction::start(
            files,
            running,
            mux,
            census,
            0,
            observer,
            (prepared.request().start_seconds * 1000.0).round().max(0.0) as i64,
            rolling_source_watermarks(1.0),
            seek_bootstrap,
        );
        let backing = Arc::new(SourceGrowingBacking {
            session_id,
            file,
            request: prepared.request().clone(),
            production,
            delivery: Arc::new(crate::meter::Meter::for_method("copy")),
            activity: GrowingActivity::new(Instant::now()),
            lifecycle: tokio::sync::Mutex::new(()),
            control: Default::default(),
            owners,
            source_owner,
            assignment: owner.assignment.clone(),
            source,
            catalog_namespace: namespace,
            observation_source,
            recipe,
            range_id,
            range_epoch: 1,
        });
        {
            let mut current = owner.state.lock().expect("Source worker state");
            if current.retirement_requested {
                backing.retire();
            }
            current.backing = SourceBacking::Growing(Arc::clone(&backing));
        }
        Ok(backing)
    }
}

impl TranscodeManager {
    pub(super) async fn run_source_growing_owner(
        self: Arc<Self>,
        owner: Arc<SourceViewerInner>,
        state: Arc<crate::state::AppState>,
        prepared: crate::http::hls::PreparedSourcePlayback,
        deadline: Instant,
    ) {
        let actor = SourceViewerActor(Arc::clone(&owner));
        let mut unowned_existing = true;
        let mut backing = None;
        let mut startup_stage = "growing.deadline";
        let start = async {
            if Instant::now() >= deadline {
                return Err(SourceWorkerError::Deadline);
            }
            startup_stage = "growing.existing_route";
            if self
                .store
                .media_session_route_by_incarnation(
                    &owner.assignment.binding().incarnation_id().to_string(),
                )
                .await
                .map_err(|_| SourceWorkerError::Unresolved)?
                .is_some()
            {
                return Err(SourceWorkerError::Unresolved);
            }
            unowned_existing = false;
            startup_stage = "growing.delivery_refusal";
            if source_delivery_refusal(
                prepared.request(),
                prepared.file(),
                prepared.native_subtitles(),
            )
            .is_some()
                || prepared.request().previous_session_id.is_some()
                || prepared.request().reopen_reason.is_some()
            {
                return Err(SourceWorkerError::Unsupported);
            }
            startup_stage = "growing.start_budget";
            let startup_budget = self
                .source_start_budget_for_request(prepared.request())
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
            let deadline = deadline.min(Instant::now() + startup_budget);
            startup_stage = "growing.native_tracks";
            if prepared.native_subtitles().0 {
                let tracks = super::super::source_subtitles::SourceNativeTracks::prepare(
                    prepared.file(),
                    &owner.assignment,
                    prepared.native_subtitles().1,
                    state.subs_dir.clone(),
                )
                .map_err(|_| SourceWorkerError::Unsupported)?;
                owner.state.lock().expect("Source native actor").native = Some(Arc::new(tracks));
            }
            let session_id = uuid::Uuid::new_v4().to_string();
            let actual = self
                .prepare_source_growing(
                    &owner,
                    &state,
                    &prepared,
                    session_id.clone(),
                    deadline,
                    &mut startup_stage,
                )
                .await?;
            backing = Some(Arc::clone(&actual));
            // Existing fresh shared Apple/Android attachment starts at 1x.
            // A future prepared successor must pass its accepted actual rate;
            // this constructor never resets a retained playback preference.
            let policy = if prepared.request().transport.as_deref() == Some("hlsjs") {
                RollingStartupPolicy::FirstReloadWindow
            } else {
                RollingStartupPolicy::NativePresentationWindow
            };
            startup_stage = "growing.eligible_window";
            let origin = actual
                .production
                .wait_eligible(
                    (prepared.request().start_seconds * 1000.0)
                        .round()
                        .clamp(0.0, i64::MAX as f64) as i64,
                    policy.bootstrap_ms(1.0) as u64,
                    deadline,
                )
                .await
                .map_err(|error| {
                    // copy_failure folds nine distinct copy-clock codes into
                    // CopyClockUnknown; keep the producer's own code, as the
                    // census stage does ("Source copy head refused").
                    tracing::warn!(
                        target: "plurxd::transcode",
                        reason = %error,
                        start_ms = (prepared.request().start_seconds * 1000.0).round() as i64,
                        "Source copy range window refused"
                    );
                    SourceWorkerError::copy_failure(&error)
                })?;
            startup_stage = "growing.source_height";
            let facts = StartInfo {
                processed_dv_profile: None,
                processed_fel: None,
                degraded_processing: None,
                processing_note: None,
                retained_output: None,
                audio_delivery: prepared.request().audio_delivery.clone(),
                session_id: session_id.clone(),
                playlist_url: format!("/api/v1/hls/{session_id}/index.m3u8"),
                duration_ms: actual.file.duration_ms,
                start_seconds: prepared.request().start_seconds,
                media_origin_seconds: origin as f64 / 1000.0,
                target_height: actual
                    .file
                    .height
                    .ok_or(SourceWorkerError::RescanRequired)?,
                kind: prepared.request().kind,
                encoder: "copy",
                grade: OutputGrade::Sdr,
                vod: false,
                control_lease_timeout_ms: GROWING_LEASE_TIMEOUT_MS,
            };
            startup_stage = "growing.start_response";
            let incarnation = owner.assignment.binding().incarnation_id().to_string();
            let response = prepared
                .start_response(&state, &facts, &incarnation, 1)
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
            startup_stage = "growing.physical_fence";
            actual.physical_fence().await?;
            startup_stage = "growing.deadline_or_retirement";
            if Instant::now() >= deadline
                || owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_requested
            {
                return Err(SourceWorkerError::Deadline);
            }
            startup_stage = "growing.activation_authority";
            let members = owner
                .gate
                .source_members
                .observe()
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?
                .ok_or(SourceWorkerError::Unavailable)?;
            let SourceWriteAuthorityRead::Ready(authority) = self
                .store
                .prepare_source_activation_authority(
                    &owner.assignment,
                    &owner.gate.master,
                    &members,
                )
                .await
                .map_err(|_| SourceWorkerError::Unresolved)?
            else {
                return Err(SourceWorkerError::Unavailable);
            };
            startup_stage = "growing.activation_write";
            let binding = owner.assignment.binding();
            let now = crate::fragment_index_cluster::unix_ms();
            let activation = MediaSessionActivation {
                incarnation_id: incarnation,
                session_id: session_id.clone(),
                principal: binding.principal().clone(),
                playback_id: prepared.request().playback_id.clone(),
                recovery_epoch: String::new(),
                expected_predecessor_incarnation_id: None,
                fence_predecessor: true,
                request_id: Some(binding.request_id().into()),
                request_fingerprint: prepared.fingerprint().into(),
                owner_node_id: owner.assignment.owner_node_id().into(),
                recipe_json: serde_json::to_string(prepared.request())
                    .map_err(|_| SourceWorkerError::Unavailable)?,
                response_json: serde_json::to_string(&response)
                    .map_err(|_| SourceWorkerError::Unavailable)?,
                publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
                media_origin_ms: origin,
                now_ms: now,
                lease_expires_at_ms: now.saturating_add(super::SOURCE_SESSION_LEASE_MS),
                expected_desired_revision: None,
            };
            owner
                .state
                .lock()
                .expect("Source worker state")
                .planned_session = Some(session_id.clone());
            let outcome = self
                .store
                .activate_source_media_session(&authority, &activation)
                .await
                .map_err(|_| SourceWorkerError::Unresolved)?
                .ok_or(SourceWorkerError::Unavailable)?;
            authority
                .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                .map_err(|_| SourceWorkerError::Unavailable)?;
            if outcome.route.session_id != session_id || !actual.is_live() {
                return Err(SourceWorkerError::Conflict);
            }
            owner.gate.activated(&outcome.route);
            startup_stage = "growing.publication_authority";
            let members = owner
                .gate
                .source_members
                .observe()
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?
                .ok_or(SourceWorkerError::Unavailable)?;
            let SourcePublicationAuthorityRead::Ready(proof) = self
                .store
                .prepare_source_publication_authority(
                    &owner.assignment,
                    &owner.gate.master,
                    &members,
                )
                .await
                .map_err(|_| SourceWorkerError::Unresolved)?
            else {
                return Err(SourceWorkerError::Unavailable);
            };
            startup_stage = "growing.published_physical_fence";
            actual.physical_fence().await?;
            startup_stage = "growing.retirement_requested";
            if owner
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
            {
                return Err(SourceWorkerError::Unavailable);
            }
            startup_stage = "growing.publication_write";
            let route = self
                .store
                .complete_source_media_session_publication(&proof)
                .await
                .map_err(|_| SourceWorkerError::Unresolved)?
                .ok_or(SourceWorkerError::Unavailable)?;
            proof
                .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
                .map_err(|_| SourceWorkerError::Unavailable)?;
            if route.session_id != session_id || route.publication_ready_at_ms != 0 {
                return Err(SourceWorkerError::Conflict);
            }
            actual.note_activity();
            Ok(response)
        }
        .await;
        // Every growing start that does not publish ends here with its stage,
        // once, exactly as the indexed owner's does.
        if let Err(error) = &start {
            tracing::warn!(target: "plurxd::transcode", stage = startup_stage, error_class = ?error, retirement_requested = owner.state.lock().expect("Source worker state").retirement_requested, "Source producer startup failed");
        }
        owner.state.lock().expect("Source worker state").start = Some(start.clone());
        owner.changed.notify_waiters();
        if let (Ok(_), Some(backing)) = (&start, backing.as_ref()) {
            // This is the existing Source lease heartbeat, not a new watcher.
            let mut renewal = tokio::time::interval(Duration::from_secs(10));
            renewal.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            renewal.tick().await;
            let mut notified_manifest = None;
            loop {
                let retirement = owner.changed.notified();
                tokio::pin!(retirement);
                retirement.as_mut().enable();
                let physical = backing.production.changed.notified();
                tokio::pin!(physical);
                physical.as_mut().enable();
                if owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_requested
                    || !backing.is_live()
                {
                    break;
                }
                // No Analysis enqueue/claim/IO precedes the strict accepted
                // rendered milestone. Full inbox retains the latest durable
                // checkpoint and retries on existing producer/lease events.
                let candidate = {
                    let files = backing.production.files.lock().await;
                    files
                        .current()
                        .filter(|checkpoint| {
                            checkpoint.first_frame_rendered
                                && notified_manifest.as_ref() != Some(&checkpoint.manifest_sha256)
                        })
                        .zip(files.canonical_init())
                        .map(|(checkpoint, init)| (init, checkpoint))
                };
                if let Some((init, checkpoint)) = candidate {
                    // Advisory only: a failed authority read skips this
                    // notification; authority loss is the renewal tick's call.
                    if let Ok(authority) = owner.gate.current_owned(&owner.assignment).await {
                        let manifest = checkpoint.manifest_sha256.clone();
                        let message=crate::copy_analysis::CompletedCopyRange{file:Arc::clone(&backing.file),catalog_namespace:backing.catalog_namespace.clone(),source:backing.observation_source.clone(),authority:crate::copy_analysis::CopyObservationAuthority::Source{assignment:Box::new(owner.assignment.clone()),authority:plurx_core::store::analysis_coordination::AnalysisSourceInterestAuthority::Active(authority)},range_id:backing.range_id,range_epoch:backing.range_epoch,init,checkpoint,recipe:backing.recipe.clone()};
                        if state.copy_analysis.notify(message) {
                            notified_manifest = Some(manifest);
                        }
                    }
                }
                tokio::select! {
                    ()=state.shutdown.cancelled()=>break,
                    ()=retirement=>{},
                    ()=physical=>{},
                    _=renewal.tick()=>{
                        #[cfg(test)]
                        owner.gate.ticked();
                        if backing.activity_expired() || backing.production.refresh_read_bank().await.is_err() {break;}
                        // The lease is renewed only for an object proven
                        // unchanged. One that could not be read just now is
                        // retried at the next tick without renewing, so the
                        // confirmed lease bounds how long it can pass.
                        match owner.gate.tick_fence(backing.physical_fence().await) { super::TickFence::Renew => {}, super::TickFence::Skip => continue, super::TickFence::End => break }
                        // Only a refusal that does not pass ends the owner; the
                        // confirmed lease bounds how long transient ones may last.
                        if owner.gate.renew_owned(&owner.assignment).await.is_err_and(|refusal| !refusal.is_transient()) {
                            owner.state.lock().expect("Source retirement cause").retirement_mode=crate::sharing_connection_custody::DriverCloseMode::Revoke;
                            break;
                        }
                    },
                }
            }
        }
        actor.request_retirement();
        let preparations = SourcePreparationSettlements::default();
        self.finish_source_owner(
            &owner,
            &state,
            state.shutdown.clone(),
            unowned_existing,
            None,
            preparations,
        )
        .await;
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    /// A copy that saw one relayed control exchange outlives the 60 s rolling
    /// window it used to retire on, and keeps exactly the lease its Start and
    /// control replies advertise; a later exchange extends it.
    #[test]
    fn growing_copy_keeps_the_lease_it_advertises_after_one_control_exchange() {
        assert_eq!(
            GROWING_LEASE_TIMEOUT_MS,
            crate::playback_control::VOD_LEASE_TIMEOUT_MS
        );
        let start = Instant::now();
        let activity = GrowingActivity::new(start);
        let exchange = start + Duration::from_secs(5);
        activity.note_at(exchange);
        let lease = Duration::from_millis(u64::from(GROWING_LEASE_TIMEOUT_MS));
        assert!(!activity.expired_at(exchange + Duration::from_secs(61)));
        assert!(!activity.expired_at(exchange + lease - Duration::from_millis(1)));
        assert!(activity.expired_at(exchange + lease));
        // A later exchange extends it; an older reading never rewinds it.
        let again = exchange + Duration::from_secs(250);
        activity.note_at(again);
        activity.note_at(exchange);
        assert!(!activity.expired_at(exchange + lease));
        assert!(activity.expired_at(again + lease));
    }
}
