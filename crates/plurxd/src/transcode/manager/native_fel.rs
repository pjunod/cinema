use super::*;
use crate::copyseg::{self, NativeSplitAuthor};
use crate::producer_spawn::{ChildStdio, ChildStdios, Descriptors};
use plurx_core::domain::MediaFile;
use plurx_core::fmp4::{self, FragmentReader, Unit};
use plurx_core::process::{self, bounded};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

// The normal 1x native startup window is three rolling targets. Read one
// extra rolling target and keep the last complete segment as return proof.
const HEAD_TARGET_SECS: u32 = 3 * plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS;
const COPY_EXTENT_SECS: u32 =
    HEAD_TARGET_SECS + 2 * plurx_core::transcode::ROLLING_PRESENTATION_TARGET_SECS;
const COPY_LIMIT_BYTES: usize = 1024 * 1024 * 1024;
const COPY_WALL_LIMIT: Duration = Duration::from_secs(180);
const HEAD_TARGET_HINT: &str = "native-fel-head-title";
const HEAD_RETRY: Duration = Duration::from_secs(90);

struct ObservedHead {
    dir: tempfile::TempDir,
    _reservation: crate::playback_cache::CacheReservation,
    init: fmp4::Init,
}

impl ObservedHead {
    async fn segment(&self, index: u64) -> Result<Vec<u8>, String> {
        tokio::fs::read(self.dir.path().join(fmp4::segment_name(index)))
            .await
            .map_err(|error| error.to_string())
    }

    fn author(&self) -> Result<NativeSplitAuthor, String> {
        NativeSplitAuthor::new(self.init.clone()).map_err(|error| error.to_string())
    }
}

impl TranscodeManager {
    /// Produce comparison media through the same rolling copy and cut logic.
    /// Comparing against the actual copy is required again when serving a
    /// prepared segment, since the viewer can choose another soundtrack.
    async fn observe_native_fel_head(
        &self,
        file: &MediaFile,
        source: &crate::fragment_index_cluster::SourceFence,
        probe: &str,
        audio_index: Option<i64>,
        executable: &Path,
        cancel: &CancellationToken,
    ) -> Result<ObservedHead, String> {
        if crate::background_jobs::source_budget().is_none() {
            return Err("native FEL prefix requires owned background source admission".into());
        }
        let root = self.work_dir.join("native-fel");
        tokio::fs::create_dir_all(&root)
            .await
            .map_err(|error| error.to_string())?;
        let dir = tempfile::Builder::new()
            .prefix("head-")
            .tempdir_in(root)
            .map_err(|error| error.to_string())?;
        // Hold the mux output and the equally bounded cut output until all
        // media for this attempt has been consumed. The caller owns this path.
        let reservation = crate::playback_cache::reserve(dir.path(), 2 * COPY_LIMIT_BYTES as u64)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("native FEL prefix has no cache budget")?;
        let mux = dir.path().join("observed.mp4");
        let stdout = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&mux)
            .map_err(|error| error.to_string())?;
        let _output = process::source_lifetime::register_daemon_file(&stdout, &mux)
            .map_err(|error| error.to_string())?;
        let video = super::manager_start::rolling_copy_video_options(
            file,
            Some(probe),
            self.dv_strippable(),
            false,
            true,
        );
        let start = plurx_core::transcode::copy_pipe_args_with_audio_delivery(
            file,
            0.0,
            audio_index,
            true,
            plurx_core::transcode::Pacing::unpaced(),
            video,
            None,
        );
        let input =
            crate::source_input::SourceInput::open_current_held(&source.handle, "native-fel-head")
                .await?
                .ok_or("native FEL prefix lost its source owner")?;
        let mut args =
            crate::source_input::source_args(&start, Path::new(&file.path), &[], Some(input.url()));
        let input_index = args
            .iter()
            .position(|arg| arg == "-i")
            .ok_or("native FEL copy has no input")?;
        args.splice(input_index..input_index, ["-threads".into(), "1".into()]);
        args.splice(0..0, ["-filter_threads".into(), "1".into()]);
        let output = args.len() - 1;
        args.splice(
            output..output,
            [
                "-threads:a".into(),
                "1".into(),
                "-t".into(),
                COPY_EXTENT_SECS.to_string(),
            ],
        );
        let mut command = tokio::process::Command::new(executable);
        command.current_dir(dir.path()).args(args);
        let manifest = Descriptors::default()
            .prepare(
                &mut command,
                ChildStdios {
                    stdin: ChildStdio::Null,
                    stdout: ChildStdio::File(&stdout),
                    stderr: ChildStdio::Piped,
                },
            )
            .map_err(|error| error.to_string())?;
        let copied =
            process::source_lifetime::mediated(bounded::output_command_bundle_env_to_file(
                &mut command,
                COPY_WALL_LIMIT,
                32 * 1024,
                COPY_LIMIT_BYTES,
                process::ChildWork::background("native FEL prefix observation"),
                &[],
                stdout,
                Some(manifest),
            ))
            .await;
        input
            .close_and_wait()
            .await
            .map_err(|error| error.to_string())?;
        let copied = copied.map_err(|error| error.to_string())?;
        if !copied.status.success() || cancel.is_cancelled() || !source.unchanged() {
            return Err("native FEL prefix did not finish against the admitted source".into());
        }
        let observed = tokio::fs::File::open(&mux)
            .await
            .map_err(|error| error.to_string())?;
        let outcome = copyseg::run_observed_retained(
            observed,
            dir.path().to_path_buf(),
            "native-fel-head",
            copyseg::Limits::default(),
            file,
            video,
            None,
            None,
            None,
            None,
            None,
            Some(cancel),
        )
        .await;
        if !matches!(outcome, copyseg::Outcome::Completed(_)) || cancel.is_cancelled() {
            return Err(format!(
                "native FEL prefix copy cuts unavailable: {outcome:?}"
            ));
        }
        tokio::fs::remove_file(mux)
            .await
            .map_err(|error| error.to_string())?;
        let init = tokio::fs::read(dir.path().join("init.mp4"))
            .await
            .map_err(|error| error.to_string())?;
        let mut reader = FragmentReader::new();
        reader.push(&init);
        let Some(Unit::Init(init)) = reader.next_unit().map_err(|error| error.to_string())? else {
            return Err("native FEL prefix has no observed copy init".into());
        };
        Ok(ObservedHead {
            dir,
            _reservation: reservation,
            init,
        })
    }
}

struct HeadRenderRecipe {
    runtime: crate::vodencode::dv_runtime::RuntimeRecipe,
    plan: plurx_core::transcode::ResolvedTranscode,
    options: TranscodeOptions,
    grid: plurx_core::transcode::VodFrameGrid,
    executable: crate::ffmpeg::EncodedExecutable,
    audio_index: Option<i64>,
    admission: TranscodeResourceEstimate,
    required: PendingFelQualification,
}

impl TranscodeManager {
    async fn native_fel_head_recipe(
        &self,
        source: Arc<crate::fragment_index_cluster::SourceFence>,
        planning: &plurx_core::store::PlaybackPlanningSnapshot,
        tools: Arc<crate::vodencode::dv_runtime::ToolSet>,
    ) -> Result<HeadRenderRecipe, String> {
        use crate::source_probe::fel_qualification::{default_native_request, Clock};
        use crate::vodencode::dv_runtime::{failure_episode_key, RuntimeRecipe};
        use plurx_core::transcode::dv_fel_qualification as qualification;
        use plurx_core::transcode::dv_processing::{DvDestination, DvProcessingMode};

        let file = &planning.file;
        let preferences = Self::vod_dv_preferences_from_snapshot(planning);
        if !preferences.fel_reencode
            || !preferences.conversion_permitted
            || file.dolby_vision.profile != Some(7)
            || file.dolby_vision.el_present != Some(true)
        {
            return Err("the source or settings do not authorize a FEL head".into());
        }
        let stored = planning
            .probe_json
            .as_deref()
            .ok_or("missing stored source probe")?;
        let held = source
            .handle
            .try_clone()
            .map_err(|error| error.to_string())?;
        let probe = self
            .probe_vod_source_once(file, Arc::new(held))
            .await?
            .ok_or("native FEL head requires a sealed source probe")?;
        let probe = probe.document;
        if !crate::ffmpeg::compare_probe_documents(stored, &probe)?.same {
            return Err("the source changed before FEL head preparation".into());
        }
        let document = serde_json::from_str(&probe).map_err(|error| error.to_string())?;
        let clock = Clock::from_document(&document)?;
        let grid = plurx_core::transcode::VodFrameGrid::new(clock.rate.0, clock.rate.1)
            .ok_or("native FEL head has no usable picture grid")?;
        let backend = self.fel_identity(&tools.backend_digest()).await?;
        let request = default_native_request(file, &document, clock, backend)?;
        if !matches!(
            qualification::verdict_state(Some(stored), file.size, file.mtime, &request.identity),
            qualification::VerdictState::Current(verdict) if verdict.admitted,
        ) {
            return Err("the exact native FEL recipe is not admitted".into());
        }
        let required = PendingFelQualification {
            file_id: file.id,
            settings_generation: planning.generation,
            source_object_version: source.object_version().to_owned(),
            request,
        };
        let audio_index = Self::select_tracks_with_prefs(
            file,
            None,
            None,
            &Self::lang_prefs_from(&planning.settings),
            true,
        )
        .audio_index;
        let mut options = self.live_lookup_options(
            self.rate_control_snapshot(),
            Encoder::Software,
            file,
            i64::from(clock.native_raster.1),
            0.0,
            audio_index,
            None,
            Some(1),
            OutputGrade::Hdr10,
        );
        let envelope = required.request.envelope;
        let (plan, encoder) = self
            .resolve_completed_dv_window_plan(
                file,
                &mut options,
                source.as_ref(),
                Some(envelope),
                grid,
                planning,
            )
            .await?;
        self.reconstruction_qualified(
            planning,
            envelope,
            &plan,
            grid,
            encoder,
            source.object_version(),
            Some(&required),
        )
        .await?;
        let stream = document
            .get("streams")
            .and_then(serde_json::Value::as_array)
            .and_then(|streams| {
                streams.iter().find(|stream| {
                    stream.get("index").and_then(serde_json::Value::as_u64)
                        == Some(u64::from(clock.video_index))
                })
            })
            .ok_or("held native FEL video stream disappeared")?;
        let source_tick = stream
            .get("time_base")
            .and_then(serde_json::Value::as_str)
            .and_then(|tick| tick.split_once('/'))
            .ok_or("held source has no rational time base")?;
        let source_tick = (
            source_tick
                .0
                .parse::<u32>()
                .map_err(|error| error.to_string())?,
            source_tick
                .1
                .parse::<u32>()
                .map_err(|error| error.to_string())?,
        );
        let source_clock = crate::transcode::source_preparation::held_source_clock(&probe)?;
        let source_origin = crate::transcode::source_preparation::held_dv_source_origin(
            stream,
            source_tick,
            source_clock,
        )?;
        let executable = crate::ffmpeg::EncodedExecutable::capture()
            .await?
            .bind_linux_plan(&plan)?;
        let decode = Self::processed_renderer_decode(
            Path::new(&plurx_core::transcode::vaapi_device()).exists(),
            crate::ffmpeg::has_hevc_main10_vaapi_decode().await,
        );
        let destination = DvDestination::Profile81;
        let mode = DvProcessingMode::Fel;
        let failure_episode = failure_episode_key(
            "background-native-fel-head",
            source.object_version(),
            &tools,
            preferences,
            destination,
        )?;
        let admission = TranscodeResourceEstimate {
            hardware_slot: true,
            cpu_threads: crate::dv_segment::streaming_helper_cpu_threads(mode, decode)
                + if plan.options().input_has_audio { 5 } else { 3 },
            decoder_threads: Some(crate::dv_segment::STREAMING_DECODER_THREADS),
        };
        use plurx_core::transcode::dv_processing::{
            source_master_metadata, source_mastering_metadata, source_mastering_minimum,
        };

        let runtime = RuntimeRecipe {
            tools,
            source,
            source_offsets: Arc::new(tokio::sync::Semaphore::new(1)),
            source_frame_probe: None,
            source_tick,
            source_origin,
            source_clock,
            native_bl: clock.native_raster,
            preferences,
            display_target: None,
            display: None,
            display_target_note: None,
            source_mastering: source_mastering_metadata(stream),
            source_mastering_minimum: source_mastering_minimum(stream),
            source_master_metadata: source_master_metadata(stream),
            destination,
            mode,
            decode,
            // The job owns the preemptible qualification permit externally.
            reconstruction_slot: None,
            encoder,
            vaapi_hdr_policy: (encoder == Encoder::Vaapi)
                .then_some(self.caps.vaapi_hdr_policy)
                .flatten(),
            runtime_cache: self.runtime_cache.clone(),
            shader_cache: Arc::new(
                tempfile::Builder::new()
                    .prefix("dv-shader-cache-")
                    .tempdir_in(&self.runtime_cache)
                    .map_err(|error| error.to_string())?,
            ),
            prepared: std::sync::Mutex::new(None),
            failure_episode,
            preparation_budget: Duration::from_secs(30),
            report_revision: std::sync::atomic::AtomicU64::new(0),
            published_receipt: std::sync::Mutex::new(None),
            adaptive_terminal: std::sync::OnceLock::new(),
        };
        Ok(HeadRenderRecipe {
            runtime,
            plan,
            options,
            grid,
            executable,
            audio_index,
            admission,
            required,
        })
    }
}

/// The current node keeps bounded media, but no renderer or source job
/// reservation. A playback can borrow it only while its exact source,
/// qualification, settings and actual ordinary split still agree.
pub(super) struct NativeFelHead {
    pub(super) required: PendingFelQualification,
    pub(super) audio_index: Option<i64>,
    pub(super) init: copyseg::NativeSplitInitIds,
    pub(super) return_video: [u8; 32],
    pub(super) plan: plurx_core::transcode::dv_processing::DvProcessingPlan,
    pub(super) objects: Vec<NativeFelHeadObject>,
    tools: Arc<crate::vodencode::dv_runtime::ToolSet>,
    executable: crate::ffmpeg::EncodedExecutable,
    source: Arc<crate::fragment_index_cluster::SourceFence>,
    _dir: tempfile::TempDir,
    // Keep the prepared bytes charged until the last session releases them.
    _reservation: crate::playback_cache::CacheReservation,
}

pub(super) struct NativeFelHeadObject {
    pub(super) proof: copyseg::NativeFelSegmentProof,
    path: PathBuf,
    len: usize,
}

struct HeadExtension<'a>(&'a FragmentAdmission);
impl Drop for HeadExtension<'_> {
    fn drop(&mut self) {
        self.0.release_extension();
    }
}

struct HeadRenderAdmission<'a> {
    permit: crate::vodencode::EncodePermit,
    _extension: HeadExtension<'a>,
    _slot: QualificationSlot,
}

impl TranscodeManager {
    pub(crate) async fn prepare_native_fel_head(
        &self,
        file: &MediaFile,
        source_version: &str,
        tools: Arc<crate::vodencode::dv_runtime::ToolSet>,
        admission: &FragmentAdmission,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        if !self.native_fel_head_is_target(file.id)
            || !self.fel_qualification_capable()
            || file.dolby_vision.profile != Some(7)
            || file.dolby_vision.el_present != Some(true)
            || cancel.is_cancelled()
        {
            return Ok(());
        }
        let Some(planning) = self
            .store
            .playback_planning_snapshot(file.id, &QUALITY_PLANNING_KEYS)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        if planning.file.size != file.size || planning.file.mtime != file.mtime {
            return Ok(());
        }
        let source = Arc::new(
            crate::fragment_index_cluster::open_source_playback_fence(
                &planning.file,
                Some(source_version),
            )
            .await?,
        );
        if self
            .warm_native_fel_head(
                &planning.file,
                planning.generation,
                planning.probe_json.as_deref(),
            )
            .await
            .is_some()
        {
            return Ok(());
        }
        let recipe = self
            .native_fel_head_recipe(source, &planning, tools)
            .await?;
        let slot = self
            .try_qualification_slot(cancel.clone())
            .ok_or("native FEL renderer is busy")?;
        let admission = HeadRenderAdmission {
            permit: self
                .extend_fragment_admission(admission, &recipe.admission, Some(1))
                .await?,
            _extension: HeadExtension(admission),
            _slot: slot,
        };
        let observed = self
            .observe_native_fel_head(
                &planning.file,
                &recipe.runtime.source,
                planning
                    .probe_json
                    .as_deref()
                    .ok_or("native FEL source probe was removed")?,
                recipe.audio_index,
                &recipe.executable.path,
                cancel,
            )
            .await?;
        let prepared = self
            .render_native_fel_head(&planning.file, recipe, &observed, &admission, cancel)
            .await?;
        drop(observed);
        drop(admission);
        let current = self
            .store
            .playback_planning_snapshot(file.id, &QUALITY_PLANNING_KEYS)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("native FEL source changed while rendering")?;
        if cancel.is_cancelled()
            || !prepared
                .current(
                    &current.file,
                    current.generation,
                    current.probe_json.as_deref(),
                )
                .await
        {
            return Err("native FEL prefix became stale before it could be retained".into());
        }
        if !self.native_fel_head_is_target(file.id) {
            return Ok(());
        }
        let staged = prepared.objects.len();
        *self.native_fel_head.lock().await = Some(Arc::new(prepared));
        let hint = async {
            let cache = plurx_core::fs_secure::SecureDirectory::open(&self.runtime_cache).await?;
            cache
                .atomic_write_child(HEAD_TARGET_HINT, file.id.to_string().as_bytes())
                .await
        };
        if let Err(error) = hint.await {
            tracing::debug!(
                target: "plurxd::transcode",
                file_id = file.id,
                %error,
                "could not retain FEL preparation target hint"
            );
        }
        tracing::info!(
            target: "plurxd::transcode",
            file_id = file.id,
            staged,
            "admitted native FEL head is ready"
        );
        Ok(())
    }

    async fn render_native_fel_head(
        &self,
        file: &MediaFile,
        recipe: HeadRenderRecipe,
        observed: &ObservedHead,
        admission: &HeadRenderAdmission<'_>,
        cancel: &CancellationToken,
    ) -> Result<NativeFelHead, String> {
        use sha2::Digest;
        let raw = plurx_core::transcode::manifest::read_bounded_playlist(
            observed.dir.path(),
            "index.m3u8",
        )
        .await?
        .ok_or("native FEL copy has no segment inventory")?;
        let index = parse_playlist(std::str::from_utf8(&raw).map_err(|error| error.to_string())?);
        let mut author = observed.author()?;
        let init = copyseg::NativeSplitInitIds::of(&author);
        let dir = tempfile::Builder::new()
            .prefix("rendered-")
            .tempdir_in(self.work_dir.join("native-fel"))
            .map_err(|error| error.to_string())?;
        let reservation = crate::playback_cache::reserve(dir.path(), COPY_LIMIT_BYTES as u64)
            .await
            .map_err(|error| error.to_string())?
            .ok_or("native FEL output has no cache budget")?;
        let mut objects = Vec::new();
        let mut selected = None;
        let mut prior: Option<(u32, i64, copyseg::NativeSplitSegment)> = None;
        let mut written = 0usize;
        let mut return_video = None;
        for (ordinal, extent) in index.iter().enumerate() {
            if cancel.is_cancelled() || extent.index != ordinal as i64 {
                return Err("native FEL candidate copy lost ownership or contiguity".into());
            }
            let sequence = u32::try_from(ordinal)
                .ok()
                .and_then(|index| index.checked_mul(256))
                .ok_or("native FEL copy sequence overflows")?;
            let raw = observed.segment(ordinal as u64).await?;
            let segment = author
                .segment(&raw, sequence)
                .map_err(|error| error.to_string())?;
            if let Some((prior_sequence, prior_end_ms, previous)) = prior.take() {
                let entries = previous.fel_windows(recipe.grid)?;
                let (windows, decision) = recipe
                    .runtime
                    .render_native_fel_windows(
                        file,
                        &recipe.plan,
                        &recipe.options,
                        recipe.grid,
                        &entries,
                        &recipe.executable.path,
                        &admission.permit,
                        cancel,
                        selected.as_ref(),
                    )
                    .await?;
                selected = Some(decision);
                let (bytes, proof) = previous
                    .enhanced_video(&author, windows, prior_sequence)?
                    .into_parts();
                written = written
                    .checked_add(bytes.len())
                    .filter(|bytes| *bytes <= COPY_LIMIT_BYTES)
                    .ok_or("native FEL output exceeds retained cache bound")?;
                let path = dir.path().join(fmp4::segment_name(objects.len() as u64));
                let len = bytes.len();
                tokio::fs::write(&path, bytes)
                    .await
                    .map_err(|error| error.to_string())?;
                objects.push(NativeFelHeadObject { proof, path, len });
                if prior_end_ms >= i64::from(HEAD_TARGET_SECS) * 1000
                    && author.video.admits_ordinary_return(&segment.public_video)
                {
                    return_video = Some(sha2::Sha256::digest(&segment.public_video.bytes).into());
                    break;
                }
            }
            prior = Some((sequence, extent.end_ms, segment));
        }
        let return_video =
            return_video.ok_or("no verified ordinary return after the FEL startup horizon")?;
        let plan = selected.ok_or("native FEL preparation did not complete any windows")?;
        Ok(NativeFelHead {
            required: recipe.required,
            audio_index: recipe.audio_index,
            init,
            return_video,
            plan,
            objects,
            tools: recipe.runtime.tools.clone(),
            executable: recipe.executable,
            source: recipe.runtime.source.clone(),
            _dir: dir,
            _reservation: reservation,
        })
    }

    pub(crate) fn observe_native_fel_backend(&self, identity: Option<String>) {
        *self
            .native_fel_backend
            .lock()
            .expect("FEL backend observation") = identity;
    }

    pub(super) fn native_fel_start_verdict(
        &self,
        source: &plurx_core::store::PlaybackPlanningSnapshot,
    ) -> Option<bool> {
        // The title worker already validated the backend. This observation
        // selects a layout only; each renderer still validates its own recipe.
        let probe = source.probe_json.as_deref()?;
        let identity = self
            .native_fel_backend
            .lock()
            .expect("FEL backend observation")
            .clone()?;
        let request = native_fel_request_from_probe(&source.file, probe, identity).ok()?;
        native_fel_verdict(&source.file, probe, &request)
    }

    pub(super) async fn warm_native_fel_head(
        &self,
        file: &MediaFile,
        generation: i64,
        probe: Option<&str>,
    ) -> Option<Arc<NativeFelHead>> {
        let head = self.native_fel_head.lock().await.as_ref()?.clone();
        head.current(file, generation, probe).await.then_some(head)
    }
}

impl NativeFelHead {
    async fn current(&self, file: &MediaFile, generation: i64, probe: Option<&str>) -> bool {
        use plurx_core::transcode::dv_fel_qualification::{verdict_state, VerdictState};
        self.required.file_id == file.id
            && self.required.settings_generation == generation
            && matches!(verdict_state(probe, file.size, file.mtime, &self.required.request.identity),
                VerdictState::Current(verdict) if verdict.admitted)
            && self.source.unchanged()
            && self.source.reopen(file).await.is_ok()
            && self.tools.is_current().await
            && self.executable.is_current().await
    }
}

impl NativeFelHead {
    /// This work is optional and races normal publication. An incomplete,
    /// late or mismatched head leaves the ordinary snapshot available on time.
    pub(super) fn stage_for_start(
        self: Arc<Self>,
        session: Arc<Session>,
        attempt: u64,
        scratch_cap: Arc<AtomicI64>,
    ) {
        tokio::spawn(async move {
            let Some(split) = &session.native_split else {
                return;
            };
            let Some(recovery) = &session.recovery else {
                return;
            };
            let Some(permit) = &session.scratch else {
                return;
            };
            let Some(_writer) = permit.ledger().register_writer(permit.key()) else {
                return;
            };
            let grants = || {
                copyseg::WriteGrants::new(
                    Arc::clone(permit.ledger()),
                    permit.key(),
                    Arc::clone(&scratch_cap),
                    session.scratch_envelope,
                )
            };
            let deadline = Instant::now() + Duration::from_secs(8);
            let mut staged = Vec::with_capacity(self.objects.len());
            let result = async {
                use tokio::io::AsyncReadExt;
                for (index, object) in self.objects.iter().enumerate() {
                    let index = index as u64;
                    let actual = self
                        .wait_for_ordinary_video(&session, split, index, attempt, deadline)
                        .await?;
                    if !object.proof.matches_ordinary(actual) {
                        return Err(
                            "prepared FEL does not match this session's ordinary video".to_owned()
                        );
                    }
                    let file = tokio::fs::File::open(&object.path)
                        .await
                        .map_err(|error| error.to_string())?;
                    let mut bytes = Vec::with_capacity(object.len);
                    file.take(object.len as u64 + 1)
                        .read_to_end(&mut bytes)
                        .await
                        .map_err(|error| error.to_string())?;
                    if bytes.len() != object.len {
                        return Err("prepared FEL storage changed before publication".into());
                    }
                    let report = object.proof.report_for_publication(
                        &bytes,
                        &self.plan,
                        &recovery.incarnation_id,
                    )?;
                    let name = format!("split-fel-{attempt}-{index}-{}.m4s", uuid::Uuid::new_v4());
                    if let Err(error) = copyseg::write_native_split_fel(
                        session.dir.clone(),
                        grants(),
                        &name,
                        &bytes,
                    )
                    .await
                    {
                        let _ =
                            tokio::fs::remove_file(session.dir.join(format!("{name}.tmp"))).await;
                        return Err(error);
                    }
                    staged.push(copyseg::PreparedNativeFelObject {
                        object: name,
                        bytes: bytes.len() as i64,
                        report,
                    });
                }
                let actual = self
                    .wait_for_ordinary_video(
                        &session,
                        split,
                        self.objects.len() as u64,
                        attempt,
                        deadline,
                    )
                    .await?;
                if actual != self.return_video {
                    return Err("prepared FEL has no matching ordinary return".into());
                }
                let _transition = session.child_transition.lock().await;
                let clock = session.publication.lock().await;
                if clock.served.is_some()
                    || session.control.current_producer_attempt() != attempt
                    || session.control.is_retired()
                    || session.replacing_child.load(Acquire)
                    || session.failed.load(Relaxed)
                    || !self.source.unchanged()
                {
                    return Err("prepared FEL lost its first-publication owner".into());
                }
                if let Err(rejected) =
                    split.stage_prepared_head(attempt, std::mem::take(&mut staged))
                {
                    staged = rejected;
                    return Err(
                        "prepared FEL first-publication decision was already settled".into(),
                    );
                }
                Ok::<_, String>(())
            }
            .await;
            if let Err(reason) = result {
                split.decline_prepared_head();
                queue_rejected_native_fel_objects(&session, staged);
                tracing::debug!(
                    target: "plurxd::transcode",
                    file_id = session.file_id,
                    %reason,
                    "native FEL head kept ordinary startup"
                );
            }
        });
    }

    async fn wait_for_ordinary_video(
        &self,
        session: &Session,
        split: &copyseg::NativeSplitInventory,
        index: u64,
        attempt: u64,
        deadline: Instant,
    ) -> Result<[u8; 32], String> {
        loop {
            let change = split.changed.notified();
            tokio::pin!(change);
            change.as_mut().enable();
            if Instant::now() >= deadline
                || session.control.is_retired()
                || session.control.current_producer_attempt() != attempt
                || session.publication.lock().await.served.is_some()
            {
                return Err("native FEL head missed first publication".into());
            }
            if let Some(actual) = split.observed_video(index, &self.init) {
                return Ok(actual);
            }
            tokio::select! {
                () = change => {}
                () = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)) => {
                    return Err("native FEL head missed the ordinary copy frontier".into());
                }
            }
        }
    }
}

pub(super) struct FelHeadTarget {
    file_id: i64,
    retry_after: Instant,
}

impl FelHeadTarget {
    pub(super) fn load(root: &Path) -> Option<Self> {
        // A local hint only locates work. The current snapshot still has to
        // authorize the exact source and prove its qualifying recipe.
        use std::io::Read;
        let path = root.join(HEAD_TARGET_HINT);
        let file = plurx_core::fs_secure::open_read_nofollow_blocking(&path).ok()?;
        let metadata = file.metadata().ok()?;
        if !metadata.is_file() || metadata.len() > 24 {
            return None;
        }
        let mut hint = String::new();
        file.take(24).read_to_string(&mut hint).ok()?;
        let file_id = hint.parse::<i64>().ok()?;
        (file_id > 0).then_some(Self {
            file_id,
            retry_after: Instant::now(),
        })
    }
}

impl TranscodeManager {
    pub(super) fn note_native_fel_head_target(&self, file: &MediaFile) {
        if file.dolby_vision.profile != Some(7) || file.dolby_vision.el_present != Some(true) {
            return;
        }
        let mut target = self
            .native_fel_target
            .lock()
            .expect("native FEL head target");
        if target
            .as_ref()
            .is_none_or(|current| current.file_id != file.id)
        {
            *target = Some(FelHeadTarget {
                file_id: file.id,
                retry_after: Instant::now(),
            });
        }
    }

    pub(super) fn native_fel_head_is_target(&self, file_id: i64) -> bool {
        self.native_fel_target
            .lock()
            .expect("native FEL head target")
            .as_ref()
            .is_some_and(|target| target.file_id == file_id)
    }

    pub(crate) async fn queue_missing_native_fel_head(
        &self,
        node: &str,
        tools: &crate::vodencode::dv_runtime::ToolSet,
        scanner: &str,
        now_ms: i64,
    ) -> Result<(), String> {
        let file_id = {
            let mut target = self
                .native_fel_target
                .lock()
                .expect("native FEL head target");
            let Some(target) = target.as_mut() else {
                return Ok(());
            };
            let now = Instant::now();
            if now < target.retry_after {
                return Ok(());
            }
            target.retry_after = now + HEAD_RETRY;
            target.file_id
        };
        if !self.fel_qualification_capable() {
            return Ok(());
        }
        let Some(planning) = self
            .store
            .playback_planning_snapshot(file_id, &QUALITY_PLANNING_KEYS)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(());
        };
        let file = &planning.file;
        let preferences = Self::vod_dv_preferences_from_snapshot(&planning);
        if !preferences.fel_reencode
            || !preferences.conversion_permitted
            || file.dolby_vision.profile != Some(7)
            || file.dolby_vision.el_present != Some(true)
        {
            return Ok(());
        }
        let Some(probe) = planning.probe_json.as_deref() else {
            return Ok(());
        };
        if self
            .warm_native_fel_head(file, planning.generation, Some(probe))
            .await
            .is_some()
        {
            return Ok(());
        }
        let identity = self.fel_identity(&tools.backend_digest()).await?;
        let request = native_fel_request_from_probe(file, probe, identity)?;
        if native_fel_verdict(file, probe, &request) != Some(true) {
            return Ok(());
        }
        self.store
            .request_dv_fel_head(plurx_core::store::dv_title_jobs::FelHeadRequest {
                file_id,
                node: node.to_owned(),
                scanner_digest: scanner.to_owned(),
                request,
                now_ms,
                deadline_ms: now_ms.saturating_add(30 * 60 * 1000),
            })
            .await
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn native_fel_request_from_probe(
    file: &MediaFile,
    probe: &str,
    identity: String,
) -> Result<plurx_core::transcode::dv_fel_qualification::FelRecipeRequest, String> {
    use crate::source_probe::fel_qualification::{default_native_request, Clock};
    let document = serde_json::from_str(probe).map_err(|error| error.to_string())?;
    let clock = Clock::from_document(&document)?;
    default_native_request(file, &document, clock, identity)
}

fn native_fel_verdict(
    file: &MediaFile,
    probe: &str,
    request: &plurx_core::transcode::dv_fel_qualification::FelRecipeRequest,
) -> Option<bool> {
    use plurx_core::transcode::dv_fel_qualification::{verdict_state, VerdictState};
    match verdict_state(Some(probe), file.size, file.mtime, &request.identity) {
        VerdictState::Missing => None,
        VerdictState::Current(verdict) => Some(verdict.admitted),
    }
}
