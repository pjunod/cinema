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
pub(crate) struct SourceGrowingBacking {
    pub(crate) session_id: String,
    pub(crate) file: Arc<plurx_core::domain::MediaFile>,
    pub(crate) request: SessionRequest,
    pub(crate) production: Arc<CopyRangeProduction>,
    pub(crate) delivery: Arc<crate::meter::Meter>,
    activity: std::sync::Mutex<Instant>,
    pub(crate) lifecycle: tokio::sync::Mutex<()>,
    pub(crate) control: tokio::sync::Mutex<super::control::GrowingControlState>,
    pub(crate) owners: Arc<SourceRenditionOwners>,
    pub(crate) source_owner: Arc<SourceRenditionOwner>,
    assignment: SourceDispatchAssignment,
    source: crate::fragment_index_cluster::SourceFence,
    catalog_namespace: String,
    observation_source: plurx_core::store::analysis_coordination::AnalysisCoordinationSource,
    range_id: uuid::Uuid,
    range_epoch: u64,
}
impl SourceGrowingBacking {
    /// The path is independently reopened and checked against this range's
    /// held object, so a replacement cannot authorize cached old AV bytes.
    pub(crate) async fn physical_fence(
        &self,
    ) -> Result<crate::fragment_index_cluster::SourceFence, SourceWorkerError> {
        if !self.is_live() {
            return Err(SourceWorkerError::Unavailable);
        }
        self.source
            .reopen(&self.file)
            .await
            .map_err(|_| SourceWorkerError::Unavailable)
    }
    pub(crate) fn is_live(&self) -> bool {
        self.production.is_live()
            && self.owners.contains_live_assignment(&self.assignment)
            && self.source.unchanged()
    }
    pub(crate) fn note_activity(&self) {
        *self.activity.lock().expect("Source growing activity") = Instant::now();
    }
    fn activity_expired(&self) -> bool {
        self.activity
            .lock()
            .expect("Source growing activity")
            .elapsed()
            >= Duration::from_millis(u64::from(crate::playback_control::ROLLING_LEASE_TIMEOUT_MS))
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
    pub(super) async fn open_growing_resource_owned(
        &self,
        backing: Arc<SourceGrowingBacking>,
        resource: &SharingHlsResource,
        deadline: Instant,
    ) -> Result<SourceOpenedResource, SourceWorkerError> {
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
            return Err(SourceWorkerError::Unsupported);
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
                || current.bodies >= 64
                || current
                    .start
                    .as_ref()
                    .and_then(|start| start.as_ref().ok())
                    .is_none_or(|start| start.session_id != backing.session_id)
            {
                return Err(SourceWorkerError::Unavailable);
            }
            current.bodies += 1;
        }
        let mut guard = SourceResponseGuard {
            owner: Arc::clone(&self.0),
            source: None,
            source_request: None,
        };
        let initial = self.0.gate.current_owned(&self.0.assignment).await?;
        initial
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| SourceWorkerError::Unavailable)?;
        guard.source = Some(backing.physical_fence().await?);
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
                            .map_err(|_| SourceWorkerError::Unavailable)?;
                        ready.observed_media_duration_ms = duration;
                        break ready;
                    }
                    if files.complete() || !backing.is_live() {
                        return Err(SourceWorkerError::Unavailable);
                    }
                    drop(files);
                    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), changed)
                        .await
                        .map_err(|_| SourceWorkerError::Deadline)?;
                };
                // The opened immutable segment is pinned through the real body.
                ready.delivery = Arc::clone(&backing.delivery);
                SourceResourcePayload::File(ready)
            }
            SharingHlsResourceKind::Master
            | SharingHlsResourceKind::SubtitlePlaylist { .. }
            | SharingHlsResourceKind::SubtitleSegment { .. } => {
                let tracks = native.as_ref().ok_or(SourceWorkerError::Unsupported)?;
                if !tracks.matches(&self.0.assignment, &backing.file) {
                    return Err(SourceWorkerError::Unavailable);
                }
                let manager = self
                    .0
                    .manager
                    .upgrade()
                    .ok_or(SourceWorkerError::Unavailable)?;
                let probe = tracks.presentation_probe();
                let files = backing.production.files.lock().await;
                let video = files.playlist();
                let origin = files
                    .media_origin_ms()
                    .ok_or(SourceWorkerError::Unavailable)? as f64
                    / 1000.0;
                drop(files);
                match kind {
                    SharingHlsResourceKind::Master => {
                        let SessionKind::Copy {
                            aac,
                            preserve_dolby_vision,
                            convert_dolby_vision,
                            dual_layer_as_copied,
                        } = backing.request.kind
                        else {
                            return Err(SourceWorkerError::Unsupported);
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
                            tracks.selected(),
                            &context,
                            &tracks.indexes(),
                        ))
                    }
                    SharingHlsResourceKind::SubtitlePlaylist { index } => {
                        if !tracks.has_track(index) {
                            return Err(SourceWorkerError::Unsupported);
                        }
                        SourceResourcePayload::Playlist(crate::http::hls::source_native_playlist(
                            &video,
                        ))
                    }
                    SharingHlsResourceKind::SubtitleSegment { index } => {
                        if !tracks.has_track(index) {
                            return Err(SourceWorkerError::Unsupported);
                        }
                        if let Some(revision) = resource.cached_subtitle_revision() {
                            let cached = tracks.cached(index, revision, &custody).await?;
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
                                .ok_or(SourceWorkerError::Unsupported)?;
                            // The same windowed sparse read the full-VOD
                            // Source uses; no separate whole-track extraction.
                            let bytes = tracks
                                .segment(
                                    &manager, index, &video, sequence, origin, &custody, deadline,
                                )
                                .await?
                                .ok_or(SourceWorkerError::Unavailable)?;
                            if bytes.len() > 2 * 1024 * 1024 {
                                return Err(SourceWorkerError::Capacity);
                            }
                            SourceResourcePayload::SubtitleText(bytes)
                        }
                    }
                    _ => return Err(SourceWorkerError::Unsupported),
                }
            }
        };
        if let SourceResourcePayload::Playlist(bytes) = &payload {
            plurx_core::sharing_resources::validate_sharing_playlist(resource, bytes)
                .map_err(|_| SourceWorkerError::Unsupported)?;
        }
        if Instant::now() >= deadline {
            return Err(SourceWorkerError::Deadline);
        }
        let proof = self.0.gate.current_owned(&self.0.assignment).await?;
        proof
            .validate_observation_freshness(crate::fragment_index_cluster::unix_ms())
            .map_err(|_| SourceWorkerError::Unavailable)?;
        if !backing.is_live()
            || self
                .0
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
        {
            return Err(SourceWorkerError::Unavailable);
        }
        let route = self.0.gate.renew_with(&self.0.assignment, &proof).await?;
        if route.session_id != backing.session_id
            || route.publication_ready_at_ms != 0
            || guard
                .source
                .as_ref()
                .is_none_or(|source| !source.unchanged())
        {
            return Err(SourceWorkerError::Unavailable);
        }
        if kind == SharingHlsResourceKind::MediaSegment {
            backing.note_activity();
            backing.note_media_served().await;
        }
        drop(custody);
        let guard = Arc::try_unwrap(guard).map_err(|_| SourceWorkerError::Unresolved)?;
        Ok(SourceOpenedResource { payload, guard })
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
    ) -> Result<Arc<SourceGrowingBacking>, SourceWorkerError> {
        use crate::copyrange::{
            run_source_census, spawn_source_generation, CopyRangeFiles, ObservedCopyMux,
        };
        use plurx_core::{
            segplan::TrackDurations,
            transcode::{CopyVideoOptions, Pacing},
        };
        let SessionKind::Copy {
            aac,
            preserve_dolby_vision,
            convert_dolby_vision,
            ..
        } = prepared.request().kind
        else {
            return Err(SourceWorkerError::Unsupported);
        };
        let authority = owner.gate.current_preparation(&owner.assignment).await?;
        let probe = self
            .store
            .source_index_probe_evidence(&authority)
            .await
            .map_err(|_| SourceWorkerError::Unresolved)?;
        if probe
            .as_ref()
            .is_some_and(|probe| probe.len() > 1024 * 1024)
        {
            return Err(SourceWorkerError::Unsupported);
        }
        // The native master presents the same probe this copy was planned from.
        if let Some(tracks) = owner
            .state
            .lock()
            .expect("Source copy presentation")
            .native
            .as_ref()
        {
            tracks.set_presentation_probe(probe.as_deref());
        }
        let video = CopyVideoOptions::from_probe(
            prepared.file(),
            probe.as_deref(),
            false,
            preserve_dolby_vision,
        )
        .with_dolby_vision_conversion(convert_dolby_vision);
        let permit = self.admit_source_copy(deadline).await?;
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
                return Err(SourceWorkerError::Unavailable);
            }
            current.backing = SourceBacking::PreparingRange(Arc::clone(&preparing));
        }
        let file = Arc::new(prepared.file().clone());
        let source = crate::fragment_index_cluster::open_source_playback_fence(&file, None)
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
        let request = self
            .foreground_source_request(
                &file,
                &owner.assignment.binding().incarnation_id().to_string(),
                owner.assignment.binding().incarnation_id(),
                plurx_core::store::background_jobs_resources::ForegroundSourcePolicy::Rolling,
            )
            .ok_or(SourceWorkerError::Unavailable)?;
        let dispatch = owners
            .begin_generation()
            .map_err(|_| SourceWorkerError::Unavailable)?;
        let permission = dispatch
            .authorize_preparing_before_spawn()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
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
        let census = Arc::new(
            run_source_census(
                input,
                source.object_version().to_owned(),
                &file,
                video,
                &owners,
                dispatch,
                permission,
                Arc::new(crate::prodrun::ProducerSlot::new()),
                &self.runtime_cache,
                deadline,
                &preparing.stop,
            )
            .await
            .map_err(|error| {
                tracing::warn!(target:"plurxd::transcode",reason=%error,"Source copy head refused");
                SourceWorkerError::copy_failure(&error)
            })?,
        );
        if census.format_start.is_none() {
            return Err(SourceWorkerError::CopyClockUnknown);
        }
        // Every later generation reads this same held object; a census of a
        // different witness cannot establish its clock or normalization.
        if census.source_object_version != source.object_version() {
            return Err(SourceWorkerError::RescanRequired);
        }
        let contract = Arc::new(
            crate::copy_normalization::CopyNormalizationContract::from_head(
                &file,
                video,
                &census.raw_canonical_init,
                &census.raw_canonical_fragment,
            )
            .map_err(|_| SourceWorkerError::Unsupported)?,
        );
        let head = contract
            .begin(
                census.raw_canonical_init.clone(),
                census.raw_av_init.clone(),
            )
            .map_err(|_| SourceWorkerError::RescanRequired)?;
        prepared
            .validate_copy_head(head.canonical_init())
            .map_err(|_| SourceWorkerError::RescanRequired)?;
        // The cluster's shared catalogue identity: file ids are scoped to it,
        // and every member reports the same value.
        let namespace = state
            .store
            .instance_id()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
        let video_identity = crate::fragindex::identity_for(&file, video).argv_fingerprint;
        let observation_source = crate::copy_analysis::source_for_copy(
            &namespace,
            &file,
            &video_identity,
            source.object_version(),
            owner.assignment.owner_node_id(),
        )
        .map_err(|_| SourceWorkerError::Unavailable)?;
        let observation_root = state
            .copy_observation_dir
            .as_ref()
            .ok_or(SourceWorkerError::Unavailable)?;
        let range_id = uuid::Uuid::new_v4();
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
        let audio_rate = prepared
            .request()
            .audio_delivery
            .as_ref()
            .and_then(|audio| audio.bitrate_kbps())
            .map(|rate| rate.saturating_mul(1000))
            .unwrap_or(if aac { 256000 } else { 640000 });
        // The planner reserves conservative audio headroom; actual AV bytes
        // and repaired presentation span are still checked by the kernel.
        let tracks = TrackDurations {
            video_ms: file.duration_ms.unwrap_or(0),
            audio_ms: file.duration_ms.unwrap_or(0),
            audio_bits_per_second: audio_rate,
        };
        let mux = ObservedCopyMux::new(
            Arc::clone(&contract),
            census.first_dts,
            0,
            tracks,
            video.retains_hevc_parameter_sets(),
            file.video_codec.as_deref() == Some("hevc"),
        )
        .map_err(|_| SourceWorkerError::Unsupported)?;
        let args = plurx_core::transcode::copy_pipe_args_with_index_and_packet_clock(
            &file,
            prepared.request().start_seconds,
            prepared.request().audio_index,
            aac,
            Pacing::unpaced(),
            video,
            prepared.request().audio_delivery.as_ref(),
            "pipe:6",
            "pipe:7",
        )
        .map_err(|_| SourceWorkerError::Unsupported)?;
        let dispatch = owners
            .begin_generation()
            .map_err(|_| SourceWorkerError::Unavailable)?;
        let permission = dispatch
            .authorize_preparing_before_spawn()
            .await
            .map_err(|_| SourceWorkerError::Unavailable)?;
        // The census Input and every reader are joined. Reopen independently
        // against the exact object before the primary operation gets a URL.
        let input = crate::source_input::SourceInput::open_foreground_held(
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
        let observer = input.observer();
        owners.set_source_observer(1, observer.clone());
        let running = spawn_source_generation(
            input,
            &file.path,
            &args,
            &self.runtime_cache,
            &owners,
            dispatch,
            permission,
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
            activity: std::sync::Mutex::new(Instant::now()),
            lifecycle: tokio::sync::Mutex::new(()),
            control: Default::default(),
            owners,
            source_owner,
            assignment: owner.assignment.clone(),
            source,
            catalog_namespace: namespace,
            observation_source,
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
        let start = async {
            if Instant::now() >= deadline {
                return Err(SourceWorkerError::Deadline);
            }
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
            let startup_budget = self
                .source_start_budget_for_request(prepared.request())
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
            let deadline = deadline.min(Instant::now() + startup_budget);
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
                .prepare_source_growing(&owner, &state, &prepared, session_id.clone(), deadline)
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
                .map_err(|error| SourceWorkerError::copy_failure(&error))?;
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
                control_lease_timeout_ms: crate::playback_control::ROLLING_LEASE_TIMEOUT_MS,
            };
            let incarnation = owner.assignment.binding().incarnation_id().to_string();
            let response = prepared
                .start_response(&state, &facts, &incarnation, 1)
                .await
                .map_err(|_| SourceWorkerError::Unavailable)?;
            actual.physical_fence().await?;
            if Instant::now() >= deadline
                || owner
                    .state
                    .lock()
                    .expect("Source worker state")
                    .retirement_requested
            {
                return Err(SourceWorkerError::Deadline);
            }
            let members = owner
                .gate
                .membership
                .observe_source_admission_members()
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
                lease_expires_at_ms: now.saturating_add(60_000),
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
            let members = owner
                .gate
                .membership
                .observe_source_admission_members()
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
            actual.physical_fence().await?;
            if owner
                .state
                .lock()
                .expect("Source worker state")
                .retirement_requested
            {
                return Err(SourceWorkerError::Unavailable);
            }
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
                        let message=crate::copy_analysis::CompletedCopyRange{file:Arc::clone(&backing.file),catalog_namespace:backing.catalog_namespace.clone(),source:backing.observation_source.clone(),authority:crate::copy_analysis::CopyObservationAuthority::Source{assignment:owner.assignment.clone(),authority:plurx_core::store::analysis_coordination::AnalysisSourceInterestAuthority::Active(authority)},range_id:backing.range_id,range_epoch:backing.range_epoch,init,checkpoint};
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
                        if backing.activity_expired() || backing.production.refresh_read_bank().await.is_err() || backing.physical_fence().await.is_err(){break;}
                        match owner.gate.current_owned(&owner.assignment).await {Ok(proof) if owner.gate.renew_with(&owner.assignment,&proof).await.is_ok()=>{},_=>{owner.state.lock().expect("Source retirement cause").retirement_mode=crate::sharing_connection_custody::DriverCloseMode::Revoke;break;}}
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
