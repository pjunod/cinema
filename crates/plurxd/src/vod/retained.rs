//! Private immutable names for observed, completely published VOD output.
//! A recipe directory is mutable; an issued artifact is never that directory.
use super::output_measurement::CompleteOutputObservation;
use super::*;

const MAX_ARTIFACTS: usize = 64;
const RETAINED_GC_BATCH: usize = 32;

#[path = "rolling_retained.rs"]
mod rolling_retained;
pub(crate) use rolling_retained::{
    RetainedOutputRow, RetainedRelease, RollingArtifact, RollingCollection,
};

/// Process-private measured authority. Serialized rates cannot construct it.
pub(crate) struct MeasuredCandidateCostProof {
    artifact: Arc<RetainedVodArtifact>,
    binding: RetainedCandidateBinding,
}

/// Advisory PUBLIC HTTP sidecar only. Never a worker envelope or authority.
#[derive(Clone, serde::Serialize)]
pub(crate) struct MeasuredCandidateOutput {
    pub(crate) candidate_id: plurx_core::playback::candidate::CandidateId,
    pub(crate) recipe_digest: [u8; 32],
    pub(crate) route: plurx_core::playback::candidate::CandidateRoute,
    pub(crate) artifact_id: String,
    pub(crate) output_identity: String,
    pub(crate) qualification: &'static str,
    pub(crate) average_bps: u64,
    pub(crate) peak_bps: u64,
}

impl MeasuredCandidateCostProof {
    pub(crate) fn average_bps(&self) -> u64 {
        self.artifact.observation.rates.average_bps
    }
    pub(crate) fn rfc_peak_bps(&self) -> u64 {
        self.artifact.observation.rates.rfc_peak_bps
    }
    pub(crate) fn artifact_facts(&self) -> crate::transcode::RetainedOutputFacts {
        self.artifact.facts()
    }
    pub(crate) fn public_descriptor(&self) -> MeasuredCandidateOutput {
        let facts = self.artifact_facts();
        MeasuredCandidateOutput {
            candidate_id: self.binding.candidate_id,
            recipe_digest: self.binding.recipe_digest,
            route: self.binding.route,
            artifact_id: facts.artifact_id,
            output_identity: facts.output_identity,
            qualification: "complete_full_mux_rfc8216_v1",
            average_bps: self.average_bps(),
            peak_bps: self.rfc_peak_bps(),
        }
    }
}

impl VodServe {
    pub(crate) fn measured_candidate_cost(
        &self,
        candidate: &plurx_core::playback::candidate::QualityCandidate,
        request: &SessionRequest,
        source: &crate::fragment_index_cluster::SourceFence,
    ) -> Option<MeasuredCandidateCostProof> {
        let context = request.candidate_context.as_ref()?;
        if !source.unchanged()
            || !candidate.identity_matches()
            || !candidate.decoder_compatible
            || candidate.id != context.candidate_id
            || candidate.recipe_digest != context.recipe_digest
            || candidate.normalized_geometry != context.normalized_geometry
            || candidate.grade != context.grade
            || request.presentation != crate::transcode::Presentation::Vod
            || context
                .owner_node_id
                .as_ref()
                .is_some_and(|owner| Some(owner) != self.shared.cluster_node_id.as_ref())
        {
            return None;
        }
        let state = self
            .shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        let artifact = state.entries.values().find_map(|entry| {
            let artifact = &entry.artifact;
            let binding = artifact.candidate.as_ref()?;
            (binding.candidate_id == candidate.id
                && binding.kind == request.kind
                && binding.normalized_geometry == context.normalized_geometry
                && binding.profile == context.profile
                && binding.recipe_digest == candidate.recipe_digest
                && binding.route == candidate.route
                && binding.grade == candidate.grade
                && binding.file_id == request.file_id
                && binding.audio_index == request.audio_index
                && binding.audio_offset_ms == request.audio_offset_ms
                && binding.subtitle_burn == request.subtitle_burn
                && artifact.source_version == source.object_version()
                && artifact
                    .audio_delivery
                    .as_ref()
                    .zip(request.audio_delivery.as_ref())
                    .is_some_and(|(actual, requested)| {
                        actual.valid_snapshot()
                            && requested.valid_snapshot()
                            && actual.byte_identity() == requested.byte_identity()
                    }))
            .then(|| Arc::clone(artifact))
        })?;
        let binding = artifact.candidate.clone()?;
        let proof = MeasuredCandidateCostProof { artifact, binding };
        proof.artifact_facts().valid().then_some(proof)
    }
}

/// Synthetic completed bytes through the existing Sink/retained registry.
/// This checks digest-key isolation, not encoded-media qualification.
#[cfg(test)]
pub(crate) async fn test_reorder_candidate_cost_isolation(
    off: &plurx_core::playback::candidate::QualityCandidate,
    on: &plurx_core::playback::candidate::QualityCandidate,
) -> ([u8; 32], Option<[u8; 32]>) {
    use crate::vodgen::Sink;
    let temp = crate::test_tempdir().expect("reorder cost fixture");
    let serve = super::tests::bare_serve(temp.path());
    let mut rendition = super::tests::synthetic_rendition(temp.path()).await;
    let path = temp.path().join("source.bin");
    tokio::fs::write(&path, b"reorder cost source")
        .await
        .expect("source");
    let file = super::tests::media_file_at(path, 10_000);
    let source = crate::fragment_index_cluster::open_source_fence(&file, None)
        .await
        .expect("source fence");
    let audio = plurx_core::playback::audio::AudioDelivery {
        action: plurx_core::playback::audio::AudioAction::None,
        downmix: None,
        reason: "synthetic producer has no audio".to_owned(),
    };
    let kind = SessionKind::Transcode {
        height: i64::from(off.target_height),
    };
    let context = crate::transcode::TranscodeManager::candidate_context(off);
    let owned = Arc::get_mut(&mut rendition).expect("unshared synthetic rendition");
    owned.source = Some(source);
    owned.recipe.file = file.clone();
    owned.recipe.audio_delivery = Some(audio.clone());
    owned.recipe.measured_candidate = Some(RetainedCandidateBinding {
        kind,
        normalized_geometry: off.normalized_geometry,
        profile: context.profile,
        candidate_id: off.id,
        recipe_digest: off.recipe_digest,
        file_id: file.id,
        audio_index: None,
        audio_offset_ms: 0,
        subtitle_burn: None,
        grade: off.grade,
        route: off.route,
    });
    let init = b"synthetic reorder init";
    let init_digest = hex::encode(Sha256::digest(init));
    *owned.identity.get_mut() = IdentityState {
        identity: Some(InitIdentity {
            muxer_init: init_digest.clone(),
            served_init: init_digest,
            promotion: Default::default(),
        }),
        from_disk: false,
    };
    tokio::fs::write(rendition.dir.path().join(INIT_NAME), init)
        .await
        .expect("init");
    serve.shared.retained_artifacts.collect(temp.path()).await;
    let mut request = SessionRequest {
        sdr_master_codecs: None,
        continuous_media: None,
        vod_only: false,
        passive_vod: false,
        finite_bitrate_limit_bps: None,
        quality_catalog: None,
        candidate_context: Some(Box::new(context)),
        file_id: file.id,
        playback_id: "reorder-cost".to_owned(),
        request_id: None,
        control_sequence: None,
        automatic: true,
        previous_session_id: None,
        reopen_reason: None,
        kind,
        start_seconds: 0.0,
        audio_index: None,
        audio_delivery: Some(audio),
        audio_claim: None,
        subtitle_burn: None,
        audio_offset_ms: 0,
        hdr10: false,
        presentation: crate::transcode::Presentation::Vod,
        block_budget_secs: None,
        transport: None,
    };
    let source = rendition.source.as_ref().expect("held source");
    assert!(
        serve
            .measured_candidate_cost(off, &request, source)
            .is_none(),
        "planned numbers cannot substitute for complete bytes"
    );
    let sink = RenditionSink {
        retirement: tokio_util::sync::CancellationToken::new(),
        shared: Arc::clone(&serve.shared),
        rendition: Arc::clone(&rendition),
        epoch: 0,
    };
    for entry in 0..rendition.plan.len() {
        sink.materialize(entry as u32, vec![7; 1000 + entry])
            .await
            .expect("synthetic publication");
    }
    sink.completed_output().await;
    let deadline = Instant::now() + Duration::from_secs(5);
    let proof = loop {
        if let Some(proof) = serve.measured_candidate_cost(off, &request, source) {
            break proof;
        }
        assert!(
            Instant::now() < deadline,
            "complete retained proof not issued"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let descriptor = proof.public_descriptor();
    assert_eq!(descriptor.qualification, "complete_full_mux_rfc8216_v1");
    assert!(proof.average_bps() > 0 && proof.rfc_peak_bps() > 0);
    request.candidate_context = Some(Box::new(
        crate::transcode::TranscodeManager::candidate_context(on),
    ));
    let other = serve
        .measured_candidate_cost(on, &request, source)
        .map(|proof| proof.public_descriptor().recipe_digest);
    // Existing artifact and its proof remain held while the other exact
    // candidate is queried; a miss is not caused by GC or a mismatched context.
    assert!(serve
        .shared
        .retained_artifacts
        .acquire_expected(&proof.artifact_facts(), &rendition)
        .is_some());
    (descriptor.recipe_digest, other)
}

#[derive(Debug)]
pub(crate) struct RetainedVodArtifact {
    pub(super) private_preparation_origin: std::sync::OnceLock<PreparedOrigin>,
    pub(super) id: uuid::Uuid,
    pub(super) observation: CompleteOutputObservation,
    pub(super) directory: PathBuf,
    pub(super) charge: u64,
    pub(super) init_bytes: u64,
    pub(super) repair: Mutex<()>,
    pub(super) recipe_key: String,
    pub(super) source_version: String,
    pub(super) candidate: Option<RetainedCandidateBinding>,
    pub(super) audio_delivery: Option<plurx_core::playback::audio::AudioDelivery>,
    pub(super) durable: bool,
    pub(super) logical: Option<super::retained_manifest::LogicalOutput>,
    pub(super) validated: AtomicBool,
    pub(super) sealed_identity: std::sync::OnceLock<[u8; 32]>,
}

/// Process-private provenance minted only by post-settlement exposure. It is
/// absent from manifests, facts and every public/worker envelope.
#[derive(Debug)]
pub(super) struct PreparedOrigin {
    _reservation: uuid::Uuid,
    artifact_id: uuid::Uuid,
    executable: String,
    engine: String,
    source_metadata: [u8; 32],
}

/// Private versioned metadata identity complements the held physical object
/// and LogicalOutput. It is never serialized as acquisition authority.
pub(crate) fn manual_source_metadata(file: &MediaFile) -> Option<[u8; 32]> {
    let bytes = serde_json::to_vec(&(
        "manual-source-v1",
        file.id,
        file.item_id,
        &file.container,
        &file.video_codec,
        &file.video_profile,
        file.width,
        file.height,
        file.bit_depth,
        &file.hdr,
        &file.hdr_format,
        &file.dolby_vision,
        &file.audio_streams,
    ))
    .ok()?;
    (bytes.len() <= 64 * 1024).then(|| Sha256::digest(bytes).into())
}

/// Closed manual intent binding; source metadata alone cannot identify a
/// selected audio stream, offset, delivery, grade or output transform.
pub(crate) fn manual_copy_policy_generation(
    file: &MediaFile,
    intent: &plurx_core::store::background_jobs::CopyOutputIntent,
) -> Option<String> {
    let bytes = serde_json::to_vec(&(
        "manual-copy-intent-v2",
        manual_source_metadata(file)?,
        intent,
    ))
    .ok()?;
    (bytes.len() <= 64 * 1024)
        .then(|| format!("manual-copy-v2:{}", hex::encode(Sha256::digest(bytes))))
}

pub(crate) fn encoded_policy_generation(
    file: &MediaFile,
    intent: &plurx_core::store::background_jobs::EncodedOutputIntent,
) -> Option<String> {
    let bytes = serde_json::to_vec(&(
        "encoded-output-intent-v1",
        manual_source_metadata(file)?,
        &file.subtitle_streams,
        intent,
    ))
    .ok()?;
    (bytes.len() <= 64 * 1024)
        .then(|| format!("encoded-output-v1:{}", hex::encode(Sha256::digest(bytes))))
}

impl RetainedVodArtifact {
    pub(super) fn facts(&self) -> crate::transcode::RetainedOutputFacts {
        crate::transcode::RetainedOutputFacts {
            artifact_id: self.id.to_string(),
            output_identity: if self.logical.is_some() {
                self.sealed_identity
                    .get()
                    .map(hex::encode)
                    .unwrap_or_default()
            } else {
                // Private runtime-only legacy observations have no durable
                // manifest. They cannot be discovered/adopted after restart.
                hex::encode(self.observation.rates.identity)
            },
            average_bps: self.observation.rates.average_bps,
            peak_bps: self.observation.rates.rfc_peak_bps,
        }
    }
    pub(super) async fn open(
        self: &Arc<Self>,
        index: Option<u32>,
        delivery: &Arc<crate::meter::Meter>,
        shared: &Shared,
        rendition: &Rendition,
        budget: Duration,
    ) -> Result<SegmentReady, VodError> {
        let (name, digest) = match index {
            Some(index) => {
                let member = self
                    .observation
                    .members
                    .get(index as usize)
                    .ok_or_else(|| VodError::Io(io::ErrorKind::NotFound.into()))?;
                (segment_name(u64::from(index)), hex::encode(member.digest))
            }
            None => (INIT_NAME.to_owned(), self.observation.served_init.clone()),
        };
        let path = self.directory.join(&name);
        let etag = format!("{}-{digest}", self.id);
        let opened = open_ready(&path, &etag, delivery).await;
        let mut ready = match opened {
            Ok(ready) => ready,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Ok(_repair) = self.repair.try_lock() else {
                    return Err(VodError::Pending {
                        retry_after: PENDING_RETRY_AFTER,
                    });
                };
                match tokio::time::timeout(
                    budget,
                    self.repair_exact(shared, rendition, &name, &digest, index),
                )
                .await
                {
                    Ok(Ok(())) => open_ready(&path, &etag, delivery)
                        .await
                        .map_err(VodError::Io)?,
                    // Artifact-local: never record_failure on a live rendition.
                    _ => {
                        return Err(VodError::ProducerFailed(
                            "retained_artifact_unavailable: exact bytes could not be restored"
                                .to_owned(),
                        ))
                    }
                }
            }
            Err(error) => return Err(VodError::Io(error)),
        };
        ready.retained_lease = Some(Arc::clone(self));
        ready.observed_media_duration_ms =
            index.and_then(|index| plan_media_duration_ms(rendition, index));
        Ok(ready)
    }

    async fn repair_exact(
        self: &Arc<Self>,
        shared: &Shared,
        rendition: &Rendition,
        name: &str,
        digest: &str,
        index: Option<u32>,
    ) -> io::Result<()> {
        if self.durable {
            // Verified old bytes are never authority for a new producer.
            return Err(io::ErrorKind::InvalidData.into());
        }
        let gate = shared.rendition_build_gate(&rendition.key);
        let _gate = gate.lock().await;
        let init = rendition.identity.lock().await.identity.clone();
        let _manifest = rendition.manifest.lock().await;
        let valid = || {
            !rendition.closed.load(Acquire)
                && rendition.gen_epoch.load(Acquire) == self.observation.epoch
                && rendition
                    .source
                    .as_ref()
                    .is_some_and(|source| source.unchanged())
                && init
                    .as_ref()
                    .is_some_and(|init| init.served_init == self.observation.served_init)
                && rendition
                    .output_measurement
                    .lock()
                    .expect("output measurement lock")
                    .matches_origin(
                        &self.observation.rates.identity,
                        self.observation.epoch,
                        &self.observation.served_init,
                    )
        };
        if !valid() || !recipe_engine_is_current(&rendition.recipe).await {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let expected_bytes = index.map_or(self.init_bytes, |index| {
            self.observation.members[index as usize].bytes
        });
        let source = rendition.dir.path().join(name);
        let mut file = tokio::fs::File::open(&source).await?;
        if file.metadata().await?.len() != expected_bytes {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut hash = Sha256::new();
        let mut buffer = [0_u8; 65_536];
        let mut read_bytes = 0_u64;
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            read_bytes = read_bytes
                .checked_add(read as u64)
                .ok_or(io::ErrorKind::InvalidData)?;
            if read_bytes > expected_bytes {
                return Err(io::ErrorKind::InvalidData.into());
            }
            hash.update(&buffer[..read]);
        }
        if read_bytes != expected_bytes
            || hex::encode(hash.finalize()) != digest
            || !valid()
            || !recipe_engine_is_current(&rendition.recipe).await
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        retained_hard_link(self, source, self.directory.join(name)).await
    }
}

struct RetainedEntry {
    artifact: Arc<RetainedVodArtifact>,
    used: Instant,
    idle_since: Option<Instant>,
}

/// Why a completed output was not handed to assembly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AssemblyRefusal {
    /// Another assembly holds the registry's single metadata-clone slot.
    Busy,
    /// This output can never be assembled under the current budget/identity.
    Refused,
}

/// The reservation precedes the bounded metadata clone and detached task.
/// Cancellation keeps any possibly-created links charged until collection.
pub(super) struct AssemblyReservation {
    shared: Arc<Shared>,
    charge: u64,
    staging: Option<Arc<RetainedVodArtifact>>,
    published: bool,
}

impl Drop for AssemblyReservation {
    fn drop(&mut self) {
        let mut state = self
            .shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        state.assembling = false;
        if self.published {
            return;
        }
        if let Some(artifact) = self.staging.take() {
            state.retired.push_back(artifact);
        } else {
            state.bytes = state.bytes.saturating_sub(self.charge);
        }
    }
}

#[derive(Default)]
struct RetainedState {
    entries: HashMap<[u8; 32], RetainedEntry>,
    retired: VecDeque<Arc<RetainedVodArtifact>>,
    rolling: HashMap<[u8; 32], rolling_retained::RollingEntry>,
    rolling_retired: VecDeque<Arc<RollingArtifact>>,
    bytes: u64,
    preparations: HashMap<uuid::Uuid, u64>,
    assembling: bool,
    namespace_owner: Option<std::fs::File>,
    namespace: Option<PathBuf>,
    startup_scan: Option<std::fs::ReadDir>,
    startup_done: bool,
    orphans: VecDeque<PathBuf>,
    /// Live, unpublished rolling collections: what free-space shedding
    /// abandons and what Activity lists as collecting. Weak, so this index
    /// never keeps a session's collection alive.
    collections: HashMap<uuid::Uuid, Weak<rolling_retained::RollingCollection>>,
}

impl RetainedState {
    fn artifact_count(&self) -> usize {
        self.entries.len()
            + self.retired.len()
            + self.rolling.len()
            + self.rolling_retired.len()
            + self.preparations.len()
    }
}

struct PreparationAssemblyOutcome {
    preparation: Option<Arc<super::copy_preparation::CopyPreparation>>,
    completed: bool,
}

impl Drop for PreparationAssemblyOutcome {
    fn drop(&mut self) {
        if let Some(preparation) = self.preparation.as_ref() {
            if !self.completed {
                preparation.fail();
            }
            preparation.progress.notify_waiters();
        }
    }
}

#[derive(Default)]
pub(super) struct RetainedArtifactRegistry {
    state: StdMutex<RetainedState>,
    /// Serialises collection, assembly and the AWAITED half of a lazy
    /// validation. Only async owners hold it, so it is always released when
    /// its holder returns or times out.
    collector: Arc<Mutex<()>>,
    /// The single blocking lazy-validation slot. The blocking reader owns it
    /// for as long as its thread runs, so a storage read that hangs past its
    /// request's timeout pins only this slot (later validations refuse
    /// instead of piling up blocked threads), never the collector.
    validation: Arc<Mutex<()>>,
}

impl RetainedArtifactRegistry {
    #[cfg(test)]
    pub(super) fn test_has_artifact(&self, facts: &crate::transcode::RetainedOutputFacts) -> bool {
        self.state
            .lock()
            .expect("retained registry lock")
            .entries
            .values()
            .any(|entry| entry.artifact.facts() == *facts)
    }

    #[cfg(test)]
    pub(super) fn test_preparation_count(&self) -> usize {
        self.state
            .lock()
            .expect("retained registry lock")
            .preparations
            .len()
    }

    pub(super) async fn reserve_preparation(
        shared: &Arc<Shared>,
        cap: u64,
        budget: u64,
    ) -> Option<Arc<super::copy_preparation::PreparationAllowance>> {
        if cap == 0 || cap > budget || !shared.retained_artifacts.own_namespace(&shared.base).await
        {
            return None;
        }
        shared.retained_artifacts.collect_orphans().await;
        let mut state = shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        let reserved = state
            .preparations
            .values()
            .try_fold(0_u64, |sum, cap| sum.checked_add(*cap))?;
        if !state.startup_done
            || !state.orphans.is_empty()
            || state.artifact_count() >= MAX_ARTIFACTS
            || state.bytes.checked_add(reserved)?.checked_add(cap)? > budget
        {
            return None;
        }
        let nonce = uuid::Uuid::new_v4();
        state.preparations.insert(nonce, cap);
        Some(Arc::new(
            super::copy_preparation::PreparationAllowance::new(shared, nonce, cap),
        ))
    }

    /// What `budget` still admits beside retained bytes and open
    /// reservations.
    pub(super) fn remaining(&self, budget: u64) -> u64 {
        let state = self.state.lock().expect("retained registry lock");
        let reserved = state
            .preparations
            .values()
            .fold(0_u64, |sum, bytes| sum.saturating_add(*bytes));
        budget.saturating_sub(state.bytes.saturating_add(reserved))
    }

    pub(super) fn release_preparation(&self, nonce: uuid::Uuid) {
        self.state
            .lock()
            .expect("retained registry lock")
            .preparations
            .remove(&nonce);
    }
    pub(super) fn retire_prepared(&self, artifact: Arc<RetainedVodArtifact>) {
        self.state
            .lock()
            .expect("retained registry lock")
            .retired
            .push_back(artifact);
    }

    /// Reuses the existing maintenance owner for a refused/busy completion;
    /// there is no second scheduler and no payload or cold-title scan.
    ///
    /// Returns true only when this call handed a new assembly to a detached
    /// owner. A completion already handed off (in flight or assembled) is not
    /// offered again, and a registry busy with another assembly defers: the
    /// next maintenance tick offers it again. Only a reservation that cannot
    /// ever fit this rendition's own completed output fails its preparation.
    pub(super) fn offer(shared: &Arc<Shared>, rendition: &Arc<Rendition>) -> bool {
        let preparation = rendition.preparation();
        let measurement = rendition
            .output_measurement
            .lock()
            .expect("output measurement lock");
        let Some(rates) = measurement.complete_rates() else {
            return false;
        };
        let mut offered = rendition
            .retained_offer
            .lock()
            .expect("retained offer lock");
        if *offered == Some(rates.identity) {
            return false;
        }
        let reservation = match Self::reserve(shared, rendition, &rates) {
            Ok(reservation) => reservation,
            Err(AssemblyRefusal::Busy) => return false,
            Err(AssemblyRefusal::Refused) => {
                if let Some(preparation) = preparation.as_ref() {
                    preparation.fail();
                }
                return false;
            }
        };
        let Some(observation) = measurement.complete_observation() else {
            return false;
        };
        *offered = Some(rates.identity);
        drop(offered);
        drop(measurement);
        let shared = Arc::clone(shared);
        let rendition = Arc::clone(rendition);
        tokio::spawn(async move {
            let identity = observation.rates.identity;
            let published = shared
                .retained_artifacts
                .assemble(&shared, &rendition, observation, reservation, preparation)
                .await;
            if !published {
                // Make an unassembled completion offerable again by the
                // existing maintenance owner.
                let mut offered = rendition
                    .retained_offer
                    .lock()
                    .expect("retained offer lock");
                if *offered == Some(identity) {
                    *offered = None;
                }
            }
        });
        true
    }

    async fn own_namespace(&self, base: &Path) -> bool {
        let namespace = base.join(".retained");
        {
            let state = self.state.lock().expect("retained registry lock");
            if state.namespace_owner.is_some() {
                return state.namespace.as_ref() == Some(&namespace);
            }
        }
        let owned_namespace = namespace.clone();
        let opened = tokio::task::spawn_blocking(move || -> io::Result<_> {
            match std::fs::symlink_metadata(&namespace) {
                Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                    return Err(io::ErrorKind::InvalidData.into())
                }
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    std::fs::create_dir(&namespace)?
                }
                Err(error) => return Err(error),
            }
            let path = namespace.join(".owner.lock");
            if std::fs::symlink_metadata(&path)
                .is_ok_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
            {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let mut options = std::fs::OpenOptions::new();
            options.read(true).write(true).create(true).truncate(false);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
            }
            let owner = options.open(path)?;
            if owner.metadata()?.file_type().is_symlink() {
                return Err(io::ErrorKind::InvalidData.into());
            }
            owner.try_lock().map_err(io::Error::other)?;
            Ok((owner, std::fs::read_dir(namespace)?))
        })
        .await;
        let Ok(Ok((owner, scan))) = opened else {
            return false;
        };
        let mut state = self.state.lock().expect("retained registry lock");
        if state.namespace_owner.is_none() {
            state.namespace_owner = Some(owner);
            state.namespace = Some(owned_namespace);
            state.startup_scan = Some(scan);
        }
        true
    }

    /// A single owned namespace lease permits orphan cleanup. Until startup
    /// cleanup converges, unknown stale bytes conservatively reserve the whole
    /// retained allowance: no new allocation is admitted, playback is unchanged.
    async fn collect_orphans(&self) {
        let restart = {
            let state = self.state.lock().expect("retained registry lock");
            (!state.startup_done && state.startup_scan.is_none())
                .then(|| state.namespace.clone())
                .flatten()
        };
        if let Some(namespace) = restart {
            let scan = tokio::task::spawn_blocking(move || std::fs::read_dir(namespace)).await;
            match scan {
                Ok(Ok(scan)) => {
                    self.state
                        .lock()
                        .expect("retained registry lock")
                        .startup_scan = Some(scan);
                }
                _ => return,
            }
        }
        let scan = {
            let mut state = self.state.lock().expect("retained registry lock");
            if state.startup_done || state.orphans.len() >= MAX_ARTIFACTS {
                None
            } else {
                state.startup_scan.take()
            }
        };
        if let Some(mut scan) = scan {
            // Directory reads may block. The iterator is never advanced while
            // holding the registry lock or on the async maintenance executor.
            // Cancellation leaves None, causing a conservative restart.
            let room = MAX_ARTIFACTS.saturating_sub({
                let state = self.state.lock().expect("retained registry lock");
                state.orphans.len() + state.artifact_count()
            });
            let batch = tokio::task::spawn_blocking(move || {
                let mut paths = Vec::new();
                let mut durable = Vec::new();
                let mut done = false;
                let mut error = None;
                for _ in 0..RETAINED_GC_BATCH.min(room) {
                    match scan.next() {
                        Some(Ok(entry)) => {
                            let name = entry.file_name();
                            if name == ".owner.lock" {
                                continue;
                            }
                            if entry
                                .file_type()
                                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
                                && uuid::Uuid::parse_str(&name.to_string_lossy()).is_ok()
                            {
                                let path = entry.path();
                                // Only bounded manifest metadata is read at
                                // startup. Payload authority remains absent.
                                match super::retained_manifest::ArtifactManifest::read(&path)
                                    .and_then(|manifest| manifest.into_artifact(path.clone()))
                                {
                                    Ok(artifact) => durable.push(Arc::new(artifact)),
                                    Err(_) => paths.push(path),
                                }
                            } else {
                                // Never delete or silently stop accounting for
                                // unknown occupants of the private namespace.
                                error = Some(io::ErrorKind::InvalidData.into());
                                break;
                            }
                        }
                        Some(Err(failure)) => {
                            error = Some(failure);
                            break;
                        }
                        None => {
                            done = true;
                            break;
                        }
                    }
                }
                (scan, paths, durable, done, error)
            })
            .await;
            let Ok((scan, paths, durable, done, mut error)) = batch else {
                return;
            };
            let mut state = self.state.lock().expect("retained registry lock");
            for path in paths {
                if !state.orphans.contains(&path) {
                    state.orphans.push_back(path);
                }
            }
            for artifact in durable {
                let identity = artifact.observation.rates.identity;
                if let Some(existing) = state.entries.get(&identity) {
                    if existing.artifact.id != artifact.id {
                        error = Some(io::ErrorKind::InvalidData.into());
                    }
                    continue;
                }
                let Some(bytes) = state.bytes.checked_add(artifact.charge) else {
                    error = Some(io::ErrorKind::InvalidData.into());
                    continue;
                };
                state.bytes = bytes;
                state.entries.insert(
                    identity,
                    RetainedEntry {
                        artifact,
                        used: Instant::now(),
                        idle_since: None,
                    },
                );
            }
            if let Some(error) = error {
                tracing::warn!(target: "plurxd::vodserve", %error, "retained orphan discovery remains unresolved");
            } else {
                state.startup_done = done;
                if !done {
                    state.startup_scan = Some(scan);
                }
            }
        }
        let candidate = self
            .state
            .lock()
            .expect("retained registry lock")
            .orphans
            .front()
            .cloned();
        let Some(path) = candidate else {
            return;
        };
        match remove_artifact_within(&path, Instant::now() + RETAINED_GC_TICK_BUDGET).await {
            Ok(true) => {
                self.state
                    .lock()
                    .expect("retained registry lock")
                    .orphans
                    .pop_front();
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(target: "plurxd::vodserve", %error, path = %path.display(), "retained orphan keeps the allowance reserved")
            }
        }
    }
    pub(super) fn acquire_expected_for_request(
        &self,
        facts: &crate::transcode::RetainedOutputFacts,
        rendition: &Rendition,
        incoming_logical: &Option<super::retained_manifest::LogicalOutput>,
    ) -> Option<Arc<RetainedVodArtifact>> {
        if !facts.valid() {
            return None;
        }
        let origin = {
            let state = self.state.lock().expect("retained registry lock");
            *state
                .entries
                .iter()
                .find(|(_, entry)| entry.artifact.facts() == *facts)?
                .0
        };
        let artifact = self.acquire(&origin)?;
        (artifact.facts() == *facts
            && artifact.logical == *incoming_logical
            && (artifact.recipe_key == rendition.key
                || ((artifact.durable
                    || artifact
                        .private_preparation_origin
                        .get()
                        .is_some_and(|origin| origin.artifact_id == artifact.id))
                    && artifact.logical.is_some()
                    && artifact.observation.preimage.playlist.as_slice()
                        == rendition.playlist.as_slice()))
            && rendition.source.as_ref().is_some_and(|source| {
                source.unchanged() && source.object_version() == artifact.source_version
            }))
        .then_some(artifact)
    }

    /// Manual NEW attachments may acquire only locally minted, settled private
    /// preparation. A manifest, job result, recipe alias or rate cannot mint
    /// PreparedOrigin; restart discovery does not participate in this lookup.
    pub(super) async fn acquire_prepared_manual(
        &self,
        rendition: &Rendition,
        incoming_logical: &Option<super::retained_manifest::LogicalOutput>,
        incoming_file: &MediaFile,
    ) -> Option<Arc<RetainedVodArtifact>> {
        if rendition.recipe.measured_candidate.is_some() || incoming_logical.is_none() {
            return None;
        }
        self.acquire_prepared_private(rendition, incoming_logical, incoming_file, None)
            .await
    }

    /// A new candidate attachment may borrow only a locally settled origin,
    /// never a persisted job result or a discovered manifest.
    pub(super) async fn acquire_prepared_candidate(
        &self,
        rendition: &Rendition,
        incoming_logical: &Option<super::retained_manifest::LogicalOutput>,
        incoming_file: &MediaFile,
        request: &SessionRequest,
    ) -> Option<Arc<RetainedVodArtifact>> {
        if tracing::enabled!(target: "plurxd::retained_reuse", tracing::Level::DEBUG) {
            tracing::debug!(target: "plurxd::retained_reuse",
                context_present = request.candidate_context.is_some(),
                binding_present = rendition.recipe.measured_candidate.is_some(),
                incoming_logical_present = incoming_logical.is_some(),
                request_audio_present = request.audio_delivery.is_some(),
                recipe_audio_present = rendition.recipe.audio_delivery.is_some(),
                request_audio_valid = request.audio_delivery.as_ref().is_some_and(|a| a.valid_snapshot()),
                recipe_audio_valid = rendition.recipe.audio_delivery.as_ref().is_some_and(|a| a.valid_snapshot()),
                audio_identity_matches = request.audio_delivery.as_ref().zip(rendition.recipe.audio_delivery.as_ref())
                    .is_some_and(|(r, a)| r.byte_identity() == a.byte_identity()),
                "Prepared candidate acquisition prerequisites");
            if let Some((context, binding)) = request
                .candidate_context
                .as_ref()
                .zip(rendition.recipe.measured_candidate.as_ref())
            {
                let candidate = &context.selected_candidate;
                let matches = serde_json::json!({
                    "presentation_vod": request.presentation == crate::transcode::Presentation::Vod,
                    "candidate_identity": candidate.identity_matches(),
                    "decoder_compatible": candidate.decoder_compatible,
                    "candidate_id": candidate.id == context.candidate_id,
                    "candidate_recipe": candidate.recipe_digest == context.recipe_digest,
                    "candidate_geometry": candidate.normalized_geometry == context.normalized_geometry,
                    "candidate_grade": candidate.grade == context.grade,
                    "binding_id": binding.candidate_id == context.candidate_id,
                    "binding_recipe": binding.recipe_digest == context.recipe_digest,
                    "binding_geometry": binding.normalized_geometry == context.normalized_geometry,
                    "binding_profile": binding.profile == context.profile,
                    "binding_grade": binding.grade == context.grade,
                    "binding_route": binding.route == candidate.route,
                    "binding_kind": binding.kind == request.kind,
                    "binding_request_file": binding.file_id == request.file_id,
                    "binding_incoming_file": binding.file_id == incoming_file.id,
                    "binding_audio_index": binding.audio_index == request.audio_index,
                    "binding_request_audio_offset": binding.audio_offset_ms == request.audio_offset_ms,
                    "binding_incoming_audio_offset": binding.audio_offset_ms == incoming_file.audio_offset_ms,
                    "binding_subtitle": binding.subtitle_burn == request.subtitle_burn,
                });
                tracing::debug!(target: "plurxd::retained_reuse", request_binding_fields = %matches,
                    "Prepared candidate acquisition request comparisons");
            }
        }
        let context = request.candidate_context.as_ref()?;
        let binding = rendition.recipe.measured_candidate.as_ref()?;
        let candidate = &context.selected_candidate;
        if request.presentation != crate::transcode::Presentation::Vod
            || !candidate.identity_matches()
            || !candidate.decoder_compatible
            || candidate.id != context.candidate_id
            || candidate.recipe_digest != context.recipe_digest
            || candidate.normalized_geometry != context.normalized_geometry
            || candidate.grade != context.grade
            || binding.candidate_id != context.candidate_id
            || binding.recipe_digest != context.recipe_digest
            || binding.normalized_geometry != context.normalized_geometry
            || binding.profile != context.profile
            || binding.grade != context.grade
            || binding.route != candidate.route
            || binding.kind != request.kind
            || binding.file_id != request.file_id
            || binding.file_id != incoming_file.id
            || binding.audio_index != request.audio_index
            || binding.audio_offset_ms != request.audio_offset_ms
            || binding.audio_offset_ms != incoming_file.audio_offset_ms
            || binding.subtitle_burn != request.subtitle_burn
        {
            return None;
        }
        // HTTP encoded delivery remains provisional until this incoming
        // producer selects its route. Never borrow a shared recipe as authority.
        let audio = if matches!(request.kind, SessionKind::Transcode { .. }) {
            incoming_logical
                .as_ref()?
                .encoded_audio_for_request(request, incoming_file)?
        } else {
            request.audio_delivery.as_ref()?
        };
        if !audio.valid_snapshot()
            || !rendition
                .recipe
                .audio_delivery
                .as_ref()
                .is_some_and(|actual| {
                    actual.valid_snapshot() && actual.byte_identity() == audio.byte_identity()
                })
        {
            return None;
        }
        self.acquire_prepared_private(rendition, incoming_logical, incoming_file, Some(binding))
            .await
    }

    async fn acquire_prepared_private(
        &self,
        rendition: &Rendition,
        incoming_logical: &Option<super::retained_manifest::LogicalOutput>,
        incoming_file: &MediaFile,
        candidate: Option<&RetainedCandidateBinding>,
    ) -> Option<Arc<RetainedVodArtifact>> {
        if incoming_logical.is_none() {
            tracing::debug!(target: "plurxd::retained_reuse",
                incoming_logical_present = false,
                "Prepared retained output acquisition unavailable");
            return None;
        }
        let (executable_digest, engine_digest) = match &rendition.recipe.encoding {
            Some(encoding) => {
                if !recipe_engine_is_current(&rendition.recipe).await {
                    tracing::debug!(target: "plurxd::retained_reuse",
                        recipe_engine_current = false,
                        "Prepared retained output acquisition unavailable");
                    return None;
                }
                (
                    encoding.executable.digest.clone(),
                    encoding.engine.digest.clone(),
                )
            }
            None => {
                let executable = crate::ffmpeg::EncodedExecutable::capture().await.ok()?;
                let engine = crate::ffmpeg::EncodedEngine::capture(None).await.ok()?;
                (executable.digest, engine.digest)
            }
        };
        let Some(source_metadata) = manual_source_metadata(incoming_file) else {
            tracing::debug!(target: "plurxd::retained_reuse",
                source_metadata_available = false,
                "Prepared retained output acquisition unavailable");
            return None;
        };
        let facts = {
            // Registry is capped at MAX_ARTIFACTS. Keep no lock over awaits.
            let state = self.state.lock().expect("retained registry lock");
            state.entries.values().find_map(|entry| {
                let artifact = &entry.artifact;
                // Emit only bounded equality facts for a relevant retained
                // candidate. The authoritative predicate below stays separate.
                if tracing::enabled!(target: "plurxd::retained_reuse", tracing::Level::DEBUG)
                    && artifact.candidate.as_ref().zip(candidate).is_some_and(|(actual, requested)| {
                        actual.candidate_id == requested.candidate_id
                    })
                {
                    let origin = artifact.private_preparation_origin.get();
                    let actual = artifact.candidate.as_ref().expect("matched candidate");
                    let requested = candidate.expect("matched candidate");
                    let binding = serde_json::json!({
                        "kind": actual.kind == requested.kind,
                        "normalized_geometry": actual.normalized_geometry == requested.normalized_geometry,
                        "profile": actual.profile == requested.profile,
                        "candidate_id": actual.candidate_id == requested.candidate_id,
                        "recipe_digest": actual.recipe_digest == requested.recipe_digest,
                        "file_id": actual.file_id == requested.file_id,
                        "audio_index": actual.audio_index == requested.audio_index,
                        "audio_offset_ms": actual.audio_offset_ms == requested.audio_offset_ms,
                        "subtitle_burn": actual.subtitle_burn == requested.subtitle_burn,
                        "grade": actual.grade == requested.grade,
                        "route": actual.route == requested.route,
                    });
                    let logical = artifact.logical.as_ref().zip(incoming_logical.as_ref())
                        .map(|(actual, requested)| actual.comparison(requested));
                    tracing::debug!(target: "plurxd::retained_reuse",
                        origin_present = origin.is_some(),
                        origin_artifact_matches = origin.is_some_and(|o| o.artifact_id == artifact.id),
                        executable_matches = origin.is_some_and(|o| o.executable == executable_digest),
                        engine_matches = origin.is_some_and(|o| o.engine == engine_digest),
                        source_metadata_matches = origin.is_some_and(|o| o.source_metadata == source_metadata),
                        candidate_matches = artifact.candidate.as_ref() == candidate,
                        actual_audio_valid = artifact.audio_delivery.as_ref().is_some_and(|a| a.valid_snapshot()),
                        requested_audio_valid = rendition.recipe.audio_delivery.as_ref().is_some_and(|a| a.valid_snapshot()),
                        audio_identity_matches = artifact.audio_delivery.as_ref().zip(rendition.recipe.audio_delivery.as_ref())
                            .is_some_and(|(a, r)| a.byte_identity() == r.byte_identity()),
                        validated = artifact.validated.load(Acquire),
                        logical_matches = artifact.logical == *incoming_logical,
                        playlist_matches = artifact.observation.preimage.playlist == rendition.playlist,
                        source_present = rendition.source.is_some(),
                        source_unchanged = rendition.source.as_ref().is_some_and(|s| s.unchanged()),
                        source_version_matches = rendition.source.as_ref().is_some_and(|s| s.object_version() == artifact.source_version),
                        candidate_fields = %binding,
                        logical_fields = ?logical,
                        "Prepared retained output acquisition comparison");
                }
                let origin = artifact.private_preparation_origin.get()?;
                (origin.artifact_id == artifact.id
                    && origin.executable == executable_digest
                    && origin.engine == engine_digest
                    && origin.source_metadata == source_metadata
                    && artifact.candidate.as_ref() == candidate
                    && candidate.is_none_or(|_| {
                        artifact
                            .audio_delivery
                            .as_ref()
                            .zip(rendition.recipe.audio_delivery.as_ref())
                            .is_some_and(|(actual, requested)| {
                                actual.valid_snapshot()
                                    && requested.valid_snapshot()
                                    && actual.byte_identity() == requested.byte_identity()
                            })
                    })
                    && artifact.validated.load(Acquire)
                    && artifact.logical == *incoming_logical
                    && artifact.observation.preimage.playlist == rendition.playlist
                    && rendition.source.as_ref().is_some_and(|source| {
                        source.unchanged() && source.object_version() == artifact.source_version
                    }))
                .then(|| artifact.facts())
            })
        }?;
        self.acquire_expected_for_request(&facts, rendition, incoming_logical)
    }

    /// Only an exact issued proof may request lazy full-byte validation. The
    /// registry lease protects it from GC throughout the blocking operation.
    pub(super) async fn reacquire_expected_for_request(
        &self,
        facts: &crate::transcode::RetainedOutputFacts,
        shared: &Shared,
        rendition: &Arc<Rendition>,
        incoming_logical: &Option<super::retained_manifest::LogicalOutput>,
        budget: Duration,
    ) -> Option<Arc<RetainedVodArtifact>> {
        if let Some(artifact) =
            self.acquire_expected_for_request(facts, rendition, incoming_logical)
        {
            return Some(artifact);
        }
        if !facts.valid() {
            return None;
        }
        let deadline = Instant::now().checked_add(budget)?;
        // A previous validation's blocking read may still be running past its
        // own timeout; refuse rather than start a second blocked thread.
        let validator = Arc::clone(&self.validation).try_lock_owned().ok()?;
        // The collector is held by this awaiting request only, and dropped
        // the moment it returns, including when its timeout below fires. The
        // blocking reader's artifact lease (not the collector) is what keeps
        // `collect` from deleting the bytes it is still reading.
        let _collector = self.collector.try_lock().ok()?;
        if !self.own_namespace(&shared.base).await {
            return None;
        }
        self.collect_orphans().await;
        let artifact = {
            let state = self.state.lock().expect("retained registry lock");
            Arc::clone(
                &state
                    .entries
                    .values()
                    .find(|entry| entry.artifact.facts() == *facts)?
                    .artifact,
            )
        };
        if artifact.facts() != *facts
            || !artifact.durable
            || artifact.logical.is_none()
            || artifact.logical != *incoming_logical
            || artifact.observation.preimage.playlist.as_slice() != rendition.playlist.as_slice()
            || !rendition
                .source
                .as_ref()
                .is_some_and(|s| s.unchanged() && s.object_version() == artifact.source_version)
        {
            return None;
        }
        let lease = Arc::clone(&artifact);
        let expected_rendition = Arc::clone(rendition);
        let expected_logical = artifact.logical.clone();
        let remaining = deadline.checked_duration_since(Instant::now())?;
        let validated = tokio::time::timeout(
            remaining,
            tokio::task::spawn_blocking(move || {
                // Timeout/cancellation cannot release the single validation slot
                // while blocking file reads still hold their artifact lease;
                // the collector is not held here, so a hung read cannot keep
                // assembly or collection waiting behind it.
                let _reservation = validator;
                let manifest = super::retained_manifest::ArtifactManifest::read(&lease.directory)?;
                if !manifest.matches_request(&expected_rendition, &expected_logical)
                    || manifest.id != lease.id.to_string()
                    || Some(&manifest.logical) != expected_logical.as_ref()
                    || Some(&manifest.seal()?) != lease.sealed_identity.get()
                    || manifest.observation()?.rates != lease.observation.rates
                {
                    return Err(io::ErrorKind::InvalidData.into());
                }
                manifest.validate_files(&lease.directory, deadline)
            }),
        )
        .await;
        if !matches!(validated, Ok(Ok(Ok(()))))
            || !rendition
                .source
                .as_ref()
                .is_some_and(|s| s.unchanged() && s.object_version() == artifact.source_version)
        {
            return None;
        }
        artifact.validated.store(true, Release);
        self.acquire_expected_for_request(facts, rendition, incoming_logical)
    }
    #[cfg(test)]
    pub(super) fn acquire_expected(
        &self,
        facts: &crate::transcode::RetainedOutputFacts,
        rendition: &Rendition,
    ) -> Option<Arc<RetainedVodArtifact>> {
        self.acquire_expected_for_request(facts, rendition, &rendition.recipe.retained_logical)
    }
    #[cfg(test)]
    async fn reacquire_expected(
        &self,
        facts: &crate::transcode::RetainedOutputFacts,
        shared: &Shared,
        rendition: &Arc<Rendition>,
        budget: Duration,
    ) -> Option<Arc<RetainedVodArtifact>> {
        self.reacquire_expected_for_request(
            facts,
            shared,
            rendition,
            &rendition.recipe.retained_logical,
            budget,
        )
        .await
    }
    pub(super) fn acquire(&self, identity: &[u8; 32]) -> Option<Arc<RetainedVodArtifact>> {
        let mut state = self.state.lock().expect("retained registry lock");
        let entry = state.entries.get_mut(identity)?;
        if !entry.artifact.validated.load(Acquire) {
            return None;
        }
        entry.used = Instant::now();
        entry.idle_since = None;
        Some(Arc::clone(&entry.artifact))
    }

    pub(super) fn reserve(
        shared: &Arc<Shared>,
        rendition: &Rendition,
        rates: &plurx_core::output_measurement::CompleteOutputRates,
    ) -> Result<AssemblyReservation, AssemblyRefusal> {
        let preparation = rendition.preparation();
        let prepare_nonce = preparation.as_ref().map(|prepare| prepare.allowance.nonce);
        if preparation
            .as_ref()
            .is_some_and(|prepare| prepare.allowance.footprint().is_none())
        {
            return Err(AssemblyRefusal::Refused);
        }
        let mut state = shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        // One queued metadata clone at a time. Another rendition's assembly
        // is not a property of this output: defer, never refuse.
        if state.assembling {
            return Err(AssemblyRefusal::Busy);
        }
        let reserved = state
            .preparations
            .values()
            .try_fold(0_u64, |sum, cap| sum.checked_add(*cap))
            .ok_or(AssemblyRefusal::Refused)?;
        let own_cap = prepare_nonce
            .and_then(|nonce| state.preparations.get(&nonce).copied())
            .unwrap_or(0);
        let reserved = reserved
            .checked_sub(own_cap)
            .ok_or(AssemblyRefusal::Refused)?;
        if state.entries.contains_key(&rates.identity)
            || state.artifact_count() - usize::from(own_cap > 0) >= MAX_ARTIFACTS
            || state
                .bytes
                .checked_add(reserved)
                .and_then(|bytes| bytes.checked_add(rates.wire_bytes))
                .is_none_or(|bytes| bytes > rendition.completed_cache_budget)
        {
            return Err(AssemblyRefusal::Refused);
        }
        state.bytes += rates.wire_bytes;
        if let Some(nonce) = prepare_nonce {
            if own_cap > 0 {
                state.preparations.insert(nonce, 0);
            }
        }
        state.assembling = true;
        Ok(AssemblyReservation {
            shared: Arc::clone(shared),
            charge: rates.wire_bytes,
            staging: None,
            published: false,
        })
    }

    /// A detached owner runs this after the completion callback releases all
    /// manifest locks. No cold create waits for this bounded link operation.
    /// Returns whether the artifact was published (or privately staged).
    pub(super) async fn assemble(
        &self,
        shared: &Shared,
        rendition: &Arc<Rendition>,
        observation: CompleteOutputObservation,
        mut reservation: AssemblyReservation,
        preparation: Option<Arc<super::copy_preparation::CopyPreparation>>,
    ) -> bool {
        let mut completion = PreparationAssemblyOutcome {
            preparation: preparation.clone(),
            completed: false,
        };
        // Queue behind a running collection or validation instead of giving
        // up: this owner is detached, and giving up here would fail a
        // completed full-title preparation for a momentary GC pass.
        let preparing = self.collector.lock().await;
        if !self.own_namespace(&shared.base).await {
            return false;
        }
        self.collect_orphans().await;
        let Ok(init_metadata) = tokio::fs::metadata(rendition.dir.path().join(INIT_NAME)).await
        else {
            return false;
        };
        let init_bytes = init_metadata.len();
        let manifest_charge = if rendition.recipe.retained_logical.is_some() {
            super::retained_manifest::MAX_MANIFEST
        } else {
            0
        };
        let Some(extra_charge) = init_bytes.checked_add(manifest_charge) else {
            return false;
        };
        let Some(charge) = observation.rates.wire_bytes.checked_add(extra_charge) else {
            return false;
        };
        drop(preparing);
        {
            let mut state = self.state.lock().expect("retained registry lock");
            let Some(reserved) = state
                .preparations
                .values()
                .try_fold(0_u64, |sum, cap| sum.checked_add(*cap))
            else {
                return false;
            };
            if !state.startup_done
                || !state.orphans.is_empty()
                || state
                    .bytes
                    .checked_add(reserved)
                    .and_then(|bytes| bytes.checked_add(extra_charge))
                    .is_none_or(|bytes| bytes > rendition.completed_cache_budget)
            {
                return false;
            }
            state.bytes += extra_charge;
            reservation.charge = charge;
        }
        let id = uuid::Uuid::new_v4();
        let directory = shared.base.join(".retained").join(id.to_string());
        let artifact = Arc::new(RetainedVodArtifact {
            private_preparation_origin: std::sync::OnceLock::new(),
            id,
            observation,
            directory,
            charge,
            init_bytes,
            repair: Mutex::new(()),
            recipe_key: rendition.key.clone(),
            source_version: rendition
                .source
                .as_ref()
                .map_or(String::new(), |source| source.object_version().to_owned()),
            candidate: rendition.recipe.measured_candidate.clone(),
            audio_delivery: rendition.recipe.audio_delivery.clone(),
            durable: false,
            logical: rendition.recipe.retained_logical.clone(),
            validated: AtomicBool::new(true),
            sealed_identity: std::sync::OnceLock::new(),
        });
        reservation.staging = Some(Arc::clone(&artifact));
        let result = self.link_complete(shared, rendition, &artifact).await;
        if let Err(error) = &result {
            tracing::debug!(target: "plurxd::vodserve", %error, artifact = %artifact.id,
                "complete retained artifact assembly unavailable");
        }
        if let Some(preparation) = preparation {
            // The origin captured at offer time remains private until the
            // exact job settlement and post-statement source revalidation.
            // A revoked origin must never fall through to ordinary exposure.
            if result.is_ok()
                && preparation.live(rendition).await
                && preparation.stage(Arc::clone(&artifact))
            {
                reservation.published = true;
                completion.completed = true;
            }
            preparation.progress.notify_waiters();
            return completion.completed;
        }
        let mut state = self.state.lock().expect("retained registry lock");
        if result.is_ok() {
            state.entries.insert(
                artifact.observation.rates.identity,
                RetainedEntry {
                    artifact,
                    used: Instant::now(),
                    idle_since: None,
                },
            );
            reservation.published = true;
        }
        reservation.published
    }

    /// A successful queue statement is historical evidence, not registry
    /// authority. Expose only the still-held private body after independent
    /// post-statement source, engine and complete observation checks.
    pub(super) async fn expose_prepared(
        &self,
        shared: &Arc<Shared>,
        rendition: &Arc<Rendition>,
        preparation: &super::copy_preparation::CopyPreparation,
        artifact: &Arc<RetainedVodArtifact>,
    ) -> bool {
        let _gate = shared
            .rendition_build_gate(&rendition.key)
            .lock_owned()
            .await;
        let readers = rendition.readers.lock().await;
        if !readers.is_empty()
            || !preparation.publication_current(rendition)
            || !preparation
                .engine
                .is_current_with_executable(&preparation.executable)
                .await
            || !recipe_engine_is_current(&rendition.recipe).await
            || !rendition
                .output_measurement
                .lock()
                .expect("output measurement lock")
                .complete_observation()
                .is_some_and(|current| current == artifact.observation)
        {
            return false;
        }
        let Some(source_metadata) = manual_source_metadata(&rendition.recipe.file) else {
            return false;
        };
        preparation.publish_staged(rendition, artifact, |artifact| {
            let mut state = self.state.lock().expect("retained registry lock");
            let _ = artifact.private_preparation_origin.set(PreparedOrigin {
                _reservation: preparation.allowance.nonce,
                artifact_id: artifact.id,
                executable: preparation.executable.digest.clone(),
                engine: preparation.engine.digest.clone(),
                source_metadata,
            });
            state.preparations.remove(&preparation.allowance.nonce);
            state.entries.insert(
                artifact.observation.rates.identity,
                RetainedEntry {
                    artifact,
                    used: Instant::now(),
                    idle_since: None,
                },
            );
        })
    }

    async fn link_complete(
        &self,
        shared: &Shared,
        rendition: &Rendition,
        artifact: &Arc<RetainedVodArtifact>,
    ) -> io::Result<()> {
        let gate = shared.rendition_build_gate(&rendition.key);
        let _gate = gate.lock().await;
        let init = rendition.identity.lock().await.identity.clone();
        let _manifest = rendition.manifest.lock().await;
        if !recipe_engine_is_current(&rendition.recipe).await {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let valid = || {
            !rendition.closed.load(Acquire)
                && rendition
                    .source
                    .as_ref()
                    .is_some_and(|source| source.unchanged())
                && init
                    .as_ref()
                    .is_some_and(|init| init.served_init == artifact.observation.served_init)
                && rendition
                    .output_measurement
                    .lock()
                    .expect("output measurement lock")
                    .complete_observation()
                    .is_some_and(|now| now == artifact.observation)
        };
        if !valid() || !recipe_engine_is_current(&rendition.recipe).await {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let lease = Arc::clone(artifact);
        tokio::task::spawn_blocking(move || {
            // If the async owner is canceled, the filesystem operation itself
            // retains a lease until settlement; GC cannot overtake it.
            std::fs::create_dir(&lease.directory)
        })
        .await
        .map_err(io::Error::other)??;
        let versions = rendition
            .publication_versions
            .lock()
            .expect("publication versions lock")
            .clone();
        if tokio::fs::metadata(rendition.dir.path().join(INIT_NAME))
            .await?
            .len()
            != artifact.init_bytes
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        retained_hard_link(
            artifact,
            rendition.dir.path().join(INIT_NAME),
            artifact.directory.join(INIT_NAME),
        )
        .await?;
        for (index, member) in artifact.observation.members.iter().enumerate() {
            if versions.get(index).copied().flatten() != Some(member.publication) {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let name = segment_name(index as u64);
            let source = rendition.dir.path().join(&name);
            if tokio::fs::metadata(&source).await?.len() != member.bytes {
                return Err(io::ErrorKind::InvalidData.into());
            }
            retained_hard_link(artifact, source, artifact.directory.join(name)).await?;
        }
        if !valid() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if let Some(logical) = rendition.recipe.retained_logical.clone() {
            let manifest =
                super::retained_manifest::ArtifactManifest::from_artifact(artifact, logical);
            let seal = manifest.seal()?;
            let lease = Arc::clone(artifact);
            tokio::task::spawn_blocking(move || manifest.commit(&lease.directory))
                .await
                .map_err(io::Error::other)??;
            artifact
                .sealed_identity
                .set(seal)
                .map_err(|_| io::ErrorKind::InvalidData)?;
        }
        Ok(())
    }

    /// The single maintenance owner keeps failed cleanup charged. An artifact
    /// cannot be reacquired once moved from the index to the deletion queue.
    pub(super) async fn collect(&self, base: &Path) {
        let Ok(_collector) = self.collector.try_lock() else {
            return;
        };
        if !self.own_namespace(base).await {
            return;
        }
        self.collect_orphans().await;
        self.collect_rolling().await;
        {
            let mut state = self.state.lock().expect("retained registry lock");
            for entry in state.entries.values_mut() {
                if Arc::strong_count(&entry.artifact) == 1 {
                    entry.idle_since.get_or_insert_with(Instant::now);
                } else {
                    entry.idle_since = None;
                }
            }
            if state.retired.is_empty() {
                let idle = state
                    .entries
                    .iter()
                    .filter(|(_, entry)| {
                        entry
                            .idle_since
                            .is_some_and(|since| since.elapsed() >= SESSION_IDLE_TTL)
                    })
                    .min_by_key(|(_, entry)| entry.used)
                    .map(|(identity, _)| *identity);
                if let Some(identity) = idle {
                    let entry = state
                        .entries
                        .remove(&identity)
                        .expect("selected retained entry");
                    state.retired.push_back(entry.artifact);
                }
            }
        }
        let artifact = self
            .state
            .lock()
            .expect("retained registry lock")
            .retired
            .front()
            .cloned();
        let Some(artifact) = artifact else {
            return;
        };
        if Arc::strong_count(&artifact) != 2 {
            return;
        }
        let result = remove_artifact_batch(&artifact.directory).await;
        match result {
            Ok(true) => {
                let mut state = self.state.lock().expect("retained registry lock");
                state.retired.pop_front();
                state.bytes = state.bytes.saturating_sub(artifact.charge);
            }
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(target: "plurxd::vodserve", %error, artifact = %artifact.id, "retained artifact cleanup remains charged")
            }
        }
    }
}

async fn retained_hard_link(
    artifact: &Arc<RetainedVodArtifact>,
    source: PathBuf,
    destination: PathBuf,
) -> io::Result<()> {
    let lease = Arc::clone(artifact);
    tokio::task::spawn_blocking(move || {
        let _lease = lease;
        std::fs::hard_link(source, destination)
    })
    .await
    .map_err(io::Error::other)?
}

/// How long one collector tick may spend unlinking one directory. A batch of
/// 32 files per 30 s tick took about 56 minutes to release a two-hour rolling
/// title; this keeps each tick bounded without a second scheduler.
pub(super) const RETAINED_GC_TICK_BUDGET: Duration = Duration::from_millis(250);

/// [`remove_artifact_batch`] repeated until the directory is gone or the
/// deadline passes. `Ok(true)` only when the directory no longer exists.
pub(super) async fn remove_artifact_within(path: &Path, deadline: Instant) -> io::Result<bool> {
    loop {
        if remove_artifact_batch(path).await? {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
    }
}

async fn remove_artifact_batch(path: &Path) -> io::Result<bool> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
            return Err(io::ErrorKind::InvalidData.into())
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
    }
    let mut entries = tokio::fs::read_dir(path).await?;
    for _ in 0..RETAINED_GC_BATCH {
        let Some(entry) = entries.next_entry().await? else {
            tokio::fs::remove_dir(path).await?;
            return Ok(true);
        };
        let name = entry.file_name();
        let name = name.to_str().ok_or(io::ErrorKind::InvalidData)?;
        if name != INIT_NAME
            && planned_index(name).is_none()
            && name != super::retained_manifest::MANIFEST_NAME
            && name != ".complete.tmp"
            && !rolling_retained::owned_name(name)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        if !entry.file_type().await?.is_file() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        tokio::fs::remove_file(entry.path()).await?;
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn durable_fixture() -> (
        tempfile::TempDir,
        Arc<VodServe>,
        Arc<Rendition>,
        crate::transcode::RetainedOutputFacts,
    ) {
        let (temp, serve, rendition, facts, _) = durable_fixture_with_candidate(false).await;
        (temp, serve, rendition, facts)
    }

    async fn durable_fixture_with_candidate(
        encoded_candidate: bool,
    ) -> (
        tempfile::TempDir,
        Arc<VodServe>,
        Arc<Rendition>,
        crate::transcode::RetainedOutputFacts,
        SessionRequest,
    ) {
        use crate::vodgen::Sink;
        let temp = crate::test_tempdir().expect("durable fixture");
        let serve = crate::vodserve::tests::bare_serve(temp.path());
        let mut rendition = crate::vodserve::tests::synthetic_rendition(temp.path()).await;
        let source_path = temp.path().join("source.bin");
        tokio::fs::write(&source_path, b"exact durable source")
            .await
            .expect("source");
        let mut file = crate::vodserve::tests::media_file_at(source_path, 10_000);
        if encoded_candidate {
            file.audio_streams = vec![plurx_core::domain::AudioStream {
                index: 0,
                codec: "aac".into(),
                channels: Some(2),
                sample_rate: Some(48_000),
                channel_layout: Some("stereo".into()),
                language: None,
                title: None,
                default: true,
            }];
        }
        let mut request = SessionRequest {
            sdr_master_codecs: None,
            continuous_media: None,
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            candidate_context: None,
            file_id: file.id,
            playback_id: "durable".into(),
            quality_catalog: None,
            request_id: None,
            control_sequence: None,
            automatic: false,
            previous_session_id: None,
            reopen_reason: None,
            kind: SessionKind::Copy {
                aac: false,
                preserve_dolby_vision: false,
                convert_dolby_vision: false,
            },
            start_seconds: 0.0,
            audio_index: None,
            audio_delivery: None,
            audio_claim: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        };
        if encoded_candidate {
            use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
            let digest = [42; 32];
            let candidate = QualityCandidate {
                id: CandidateId::for_recipe_digest(digest),
                recipe_digest: digest,
                route: CandidateRoute::Encode,
                normalized_geometry: true,
                width: 854,
                height: 480,
                target_height: 480,
                average_bps: None,
                peak_bps: None,
                grade: plurx_core::transcode::OutputGrade::Sdr,
                decoder_compatible: true,
                complete_cache: false,
                sustainable: true,
            };
            request.kind = SessionKind::Transcode { height: 480 };
            request.audio_claim = Some(plurx_core::playback::audio::canonical_producer_claim());
            request.candidate_context = Some(Box::new(
                crate::transcode::TranscodeManager::candidate_context(&candidate),
            ));
        }
        let mut resolved_request = request.clone();
        if encoded_candidate {
            let audio = plurx_core::playback::audio::resolve_audio(
                file.audio_streams.first(),
                &request.audio_claim.as_ref().expect("typed claim").profile(),
                plurx_core::playback::audio::AudioRoute::EncodedVod,
                file.audio_offset_ms,
            );
            resolved_request.audio_delivery = Some(audio);
        }
        let owned = Arc::get_mut(&mut rendition).expect("unshared rendition");
        if let Some(context) = request.candidate_context.as_ref() {
            owned.recipe.measured_candidate = Some(RetainedCandidateBinding {
                kind: request.kind,
                normalized_geometry: context.normalized_geometry,
                profile: context.profile,
                candidate_id: context.candidate_id,
                recipe_digest: context.recipe_digest,
                file_id: file.id,
                audio_index: request.audio_index,
                audio_offset_ms: request.audio_offset_ms,
                subtitle_burn: request.subtitle_burn,
                grade: context.grade,
                route: context.selected_candidate.route,
            });
            owned.recipe.audio_delivery = resolved_request.audio_delivery.clone();
        }
        owned.key = "a".repeat(64);
        owned.source = Some(
            crate::fragment_index_cluster::open_source_fence(&file, None)
                .await
                .expect("fence"),
        );
        owned.recipe.retained_logical =
            Some(super::super::retained_manifest::LogicalOutput::resolve(
                &resolved_request,
                None,
                &file,
                owned.recipe.video,
            ));
        owned.recipe.file = file;
        let init = b"actual retained fixture init";
        let digest = hex::encode(Sha256::digest(init));
        *owned.identity.get_mut() = IdentityState {
            identity: Some(InitIdentity {
                muxer_init: digest.clone(),
                served_init: digest,
                promotion: Default::default(),
            }),
            from_disk: false,
        };
        tokio::fs::write(rendition.dir.path().join(INIT_NAME), init)
            .await
            .expect("init");
        serve.shared.retained_artifacts.collect(temp.path()).await;
        let sink = RenditionSink {
            retirement: tokio_util::sync::CancellationToken::new(),
            shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition),
            epoch: 0,
        };
        for index in 0..rendition.plan.len() {
            sink.materialize(index as u32, vec![index as u8; 1000 + index])
                .await
                .expect("actual sink commit");
        }
        sink.completed_output().await;
        let deadline = Instant::now() + Duration::from_secs(5);
        let artifact = loop {
            let rates = rendition
                .output_measurement
                .lock()
                .expect("measurement")
                .complete_rates()
                .expect("full coverage");
            if let Some(artifact) = serve.shared.retained_artifacts.acquire(&rates.identity) {
                break artifact;
            }
            assert!(Instant::now() < deadline, "assembly did not complete");
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        let facts = artifact.facts();
        if encoded_candidate {
            // This fixture publishes real complete Sink bytes and source fences;
            // its private preparation authority is minted only inside the test.
            // It is a registry reuse regression, not encoded-media qualification.
            artifact
                .private_preparation_origin
                .set(PreparedOrigin {
                    _reservation: uuid::Uuid::new_v4(),
                    artifact_id: artifact.id,
                    executable: crate::ffmpeg::EncodedExecutable::capture()
                        .await
                        .expect("current executable")
                        .digest,
                    engine: crate::ffmpeg::EncodedEngine::capture(None)
                        .await
                        .expect("current engine")
                        .digest,
                    source_metadata: manual_source_metadata(&rendition.recipe.file)
                        .expect("source metadata"),
                })
                .expect("one private origin");
        }
        drop(artifact);
        drop(sink);
        (temp, serve, rendition, facts, request)
    }

    #[tokio::test]
    async fn encoded_candidate_reuses_completed_output_with_resolved_claim_audio() {
        let (_temp, serve, rendition, facts, request) = durable_fixture_with_candidate(true).await;
        assert!(
            request.audio_delivery.is_none(),
            "ordinary encoded create is provisional"
        );
        let logical = &rendition.recipe.retained_logical;
        let reused = serve
            .shared
            .retained_artifacts
            .acquire_prepared_candidate(&rendition, logical, &rendition.recipe.file, &request)
            .await
            .expect("reuse exact server-resolved encoded audio");
        assert_eq!(reused.facts(), facts);
        let member = reused
            .open(
                Some(0),
                &Arc::new(crate::meter::Meter::default()),
                &serve.shared,
                &rendition,
                Duration::from_secs(1),
            )
            .await
            .expect("exact retained member");
        assert_eq!(member.len, 1000);
        assert!(member.etag.contains(&facts.artifact_id));

        let mut altered = request.clone();
        altered.audio_claim.as_mut().expect("claim").sinks[0].max_channels = 1;
        assert!(
            serve
                .shared
                .retained_artifacts
                .acquire_prepared_candidate(&rendition, logical, &rendition.recipe.file, &altered)
                .await
                .is_none(),
            "changed route claim refuses"
        );
        altered = request.clone();
        altered.audio_offset_ms = 1;
        assert!(
            serve
                .shared
                .retained_artifacts
                .acquire_prepared_candidate(&rendition, logical, &rendition.recipe.file, &altered)
                .await
                .is_none(),
            "changed source offset refuses"
        );
        altered = request.clone();
        altered.audio_delivery = Some(plurx_core::playback::audio::AudioDelivery {
            action: plurx_core::playback::audio::AudioAction::None,
            downmix: None,
            reason: "wrong retained snapshot".into(),
        });
        assert!(
            serve
                .shared
                .retained_artifacts
                .acquire_prepared_candidate(&rendition, logical, &rendition.recipe.file, &altered)
                .await
                .is_none(),
            "explicit mismatched audio must not fallback"
        );
        let mut invalid = request.clone();
        invalid.audio_delivery = Some(plurx_core::playback::audio::AudioDelivery {
            action: plurx_core::playback::audio::AudioAction::Encode {
                codec: "aac".into(),
                channels: 0,
                layout: None,
                bitrate_kbps: 0,
                sample_rate: 0,
            },
            downmix: None,
            reason: "invalid issued snapshot".into(),
        });
        assert!(
            serve
                .shared
                .retained_artifacts
                .acquire_prepared_candidate(&rendition, logical, &rendition.recipe.file, &invalid)
                .await
                .is_none(),
            "invalid explicit snapshot must not fallback"
        );
        let mut claimless = request.clone();
        claimless.audio_claim = None;
        assert!(
            serve
                .shared
                .retained_artifacts
                .acquire_prepared_candidate(&rendition, logical, &rendition.recipe.file, &claimless)
                .await
                .is_none(),
            "missing snapshot cannot borrow authority without its typed claim"
        );
        let mut altered_logical = serde_json::to_value(logical).expect("logical");
        altered_logical["audio_delivery"] =
            serde_json::to_value(altered.audio_delivery).expect("wrong audio");
        let altered_logical = serde_json::from_value(altered_logical).expect("altered logical");
        assert!(
            serve
                .shared
                .retained_artifacts
                .acquire_prepared_candidate(
                    &rendition,
                    &altered_logical,
                    &rendition.recipe.file,
                    &request
                )
                .await
                .is_none(),
            "wrong resolved audio refuses"
        );
    }

    #[tokio::test]
    async fn durable_manifest_commits_only_complete_exact_full_mux_observation() {
        let (_temp, serve, rendition, facts) = durable_fixture().await;
        let artifact = serve
            .shared
            .retained_artifacts
            .acquire_expected(&facts, &rendition)
            .expect("issued artifact");
        let manifest = super::super::retained_manifest::ArtifactManifest::read(&artifact.directory)
            .expect("atomic complete manifest");
        assert_eq!(
            manifest.observation().expect("recomputed reducer").rates,
            artifact.observation.rates
        );
        assert_eq!(
            manifest.origin.identity(),
            artifact.observation.rates.identity
        );
        assert!(manifest.matches(&rendition));
        let incomplete = artifact
            .directory
            .parent()
            .expect("namespace")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&incomplete).expect("interrupted directory");
        std::fs::write(incomplete.join(".complete.tmp"), b"{partial").expect("crash residue");
        assert!(super::super::retained_manifest::ArtifactManifest::read(&incomplete).is_err());
        let mut incomplete_observation = PublishedOutputMeasurement::default();
        let init = rendition
            .identity
            .lock()
            .await
            .identity
            .clone()
            .expect("init");
        incomplete_observation.observe(
            &rendition,
            &init,
            0,
            0,
            artifact.observation.members[0].clone(),
        );
        incomplete_observation.complete(0, true);
        assert!(incomplete_observation.complete_observation().is_none());
    }

    #[tokio::test]
    async fn durable_manifest_refuses_paths_duplicates_bounds_and_contradictory_identity() {
        let (_temp, serve, rendition, facts) = durable_fixture().await;
        let artifact = serve
            .shared
            .retained_artifacts
            .acquire_expected(&facts, &rendition)
            .expect("artifact");
        let path = artifact
            .directory
            .join(super::super::retained_manifest::MANIFEST_NAME);
        let original = std::fs::read(&path).expect("manifest");
        let value: serde_json::Value = serde_json::from_slice(&original).expect("JSON");
        for altered in [
            {
                let mut v = value.clone();
                v["version"] = 2.into();
                v
            },
            {
                let mut v = value.clone();
                v["members"] = serde_json::json!([]);
                v
            },
            {
                let mut v = value.clone();
                v["origin"]["served_init"] = "not-a-digest".into();
                v
            },
            {
                let mut v = value.clone();
                v["id"] = uuid::Uuid::new_v4().to_string().into();
                v
            },
        ] {
            std::fs::write(&path, serde_json::to_vec(&altered).expect("encode"))
                .expect("negative fixture");
            assert!(
                super::super::retained_manifest::ArtifactManifest::read(&artifact.directory)
                    .is_err()
            );
        }
        let duplicate = format!(
            "{{\"version\":1,{}",
            std::str::from_utf8(&original)
                .expect("utf8")
                .trim_start_matches('{')
        );
        std::fs::write(&path, duplicate).expect("duplicate field");
        assert!(
            super::super::retained_manifest::ArtifactManifest::read(&artifact.directory).is_err()
        );
        std::fs::write(
            &path,
            vec![b' '; super::super::retained_manifest::MAX_MANIFEST as usize + 1],
        )
        .expect("bounded refusal fixture");
        assert!(
            super::super::retained_manifest::ArtifactManifest::read(&artifact.directory).is_err()
        );
        std::fs::write(&path, &original).expect("restore fixture");
        let mut manifest =
            super::super::retained_manifest::ArtifactManifest::read(&artifact.directory)
                .expect("original");
        manifest.origin.playlist = String::from_utf8(manifest.origin.playlist)
            .expect("playlist")
            .replace(&segment_name(0), "../outside.m4s")
            .into_bytes();
        assert!(manifest.observation().is_err());
    }

    #[tokio::test]
    async fn durable_artifact_lazy_reacquisition_validates_bytes_without_startup_scan() {
        let (temp, old, rendition, facts) = durable_fixture().await;
        let directory = old
            .shared
            .retained_artifacts
            .acquire_expected(&facts, &rendition)
            .expect("artifact")
            .directory
            .clone();
        drop(old);
        let fresh = crate::vodserve::tests::bare_serve(temp.path());
        fresh.shared.retained_artifacts.collect(temp.path()).await;
        let identity = super::super::retained_manifest::ArtifactManifest::read(&directory)
            .expect("manifest")
            .origin
            .identity();
        assert!(
            fresh.shared.retained_artifacts.acquire(&identity).is_none(),
            "metadata is not payload validation"
        );
        let artifact = fresh
            .shared
            .retained_artifacts
            .reacquire_expected(&facts, &fresh.shared, &rendition, Duration::from_secs(5))
            .await
            .expect("exact lazy restore");
        assert!(artifact.durable);
        assert!(
            artifact.candidate.is_none(),
            "old execution is not new proposal authority"
        );
        assert_eq!(artifact.facts(), facts);
        drop(artifact);
        drop(fresh);
        std::fs::write(directory.join(segment_name(0)), b"contradictory bytes")
            .expect("corruption");
        let refusing = crate::vodserve::tests::bare_serve(temp.path());
        assert!(refusing
            .shared
            .retained_artifacts
            .reacquire_expected(&facts, &refusing.shared, &rendition, Duration::from_secs(5))
            .await
            .is_none());
        assert!(refusing
            .shared
            .retained_artifacts
            .acquire(&identity)
            .is_none());
    }

    /// A lazy validation whose blocking read is still running (here: its slot
    /// is held, as a read hung past its timeout would hold it) pins only the
    /// validation slot. The collector stays free, so assembly and collection
    /// do not wait behind storage, and a second validation refuses at once
    /// instead of starting another blocked thread.
    #[tokio::test]
    async fn a_hung_lazy_validation_pins_its_slot_not_the_collector() {
        let (temp, old, rendition, facts) = durable_fixture().await;
        drop(old);
        let fresh = crate::vodserve::tests::bare_serve(temp.path());
        fresh.shared.retained_artifacts.collect(temp.path()).await;
        let hung = Arc::clone(&fresh.shared.retained_artifacts.validation)
            .try_lock_owned()
            .expect("the blocking slot is free");
        let started = Instant::now();
        assert!(fresh
            .shared
            .retained_artifacts
            .reacquire_expected(&facts, &fresh.shared, &rendition, Duration::from_secs(5))
            .await
            .is_none());
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "a second validation refuses instead of waiting for the hung one"
        );
        assert!(
            fresh.shared.retained_artifacts.collector.try_lock().is_ok(),
            "a hung validation never holds the collector assembly waits on"
        );
        drop(hung);
        let artifact = fresh
            .shared
            .retained_artifacts
            .reacquire_expected(&facts, &fresh.shared, &rendition, Duration::from_secs(5))
            .await
            .expect("validates once the slot is free");
        assert_eq!(artifact.facts(), facts);
        assert!(
            fresh.shared.retained_artifacts.collector.try_lock().is_ok(),
            "the awaiting request released the collector when it returned"
        );
    }

    #[tokio::test]
    async fn durable_validation_and_get_leases_block_gc_until_last_owner_releases() {
        let (temp, old, rendition, facts) = durable_fixture().await;
        drop(old);
        let fresh = crate::vodserve::tests::bare_serve(temp.path());
        fresh.shared.retained_artifacts.collect(temp.path()).await;
        let charged = fresh
            .shared
            .retained_artifacts
            .state
            .lock()
            .expect("registry")
            .bytes;
        assert!(
            charged > 0,
            "discovered bytes are charged before validation"
        );
        assert!(fresh
            .shared
            .retained_artifacts
            .reacquire_expected(&facts, &fresh.shared, &rendition, Duration::ZERO)
            .await
            .is_none());
        assert_eq!(
            fresh
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .bytes,
            charged
        );
        let artifact = fresh
            .shared
            .retained_artifacts
            .reacquire_expected(&facts, &fresh.shared, &rendition, Duration::from_secs(5))
            .await
            .expect("validated lease");
        let directory = artifact.directory.clone();
        let init = artifact
            .open(
                None,
                &Arc::new(crate::meter::Meter::default()),
                &fresh.shared,
                &rendition,
                Duration::from_secs(1),
            )
            .await
            .expect("private init GET");
        let media = artifact
            .open(
                Some(0),
                &Arc::new(crate::meter::Meter::default()),
                &fresh.shared,
                &rendition,
                Duration::from_secs(1),
            )
            .await
            .expect("private media GET");
        let identity = artifact.observation.rates.identity;
        drop(artifact);
        {
            let mut state = fresh
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry");
            state.entries.get_mut(&identity).expect("entry").idle_since =
                Some(Instant::now() - SESSION_IDLE_TTL);
        }
        fresh.shared.retained_artifacts.collect(temp.path()).await;
        assert!(
            directory.exists(),
            "init/media response leases protect the private incarnation"
        );
        drop(init);
        drop(media);
        {
            let mut state = fresh
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry");
            state.entries.get_mut(&identity).expect("entry").idle_since =
                Some(Instant::now() - SESSION_IDLE_TTL);
        }
        for _ in 0..4 {
            fresh.shared.retained_artifacts.collect(temp.path()).await;
        }
        assert!(
            !directory.exists(),
            "GC removes only the unleased incarnation"
        );
        assert_eq!(
            fresh
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .bytes,
            0
        );
    }

    #[tokio::test]
    async fn durable_content_seal_refuses_same_cost_body_and_logical_manifest_forgery() {
        let (temp, old, rendition, facts) = durable_fixture().await;
        let artifact = old
            .shared
            .retained_artifacts
            .acquire_expected(&facts, &rendition)
            .expect("sealed artifact");
        let directory = artifact.directory.clone();
        let path = directory.join(super::super::retained_manifest::MANIFEST_NAME);
        let original = std::fs::read(&path).expect("manifest");
        let mut value: serde_json::Value = serde_json::from_slice(&original).expect("JSON");
        let bytes = value["members"][0]["bytes"]
            .as_u64()
            .expect("actual member length") as usize;
        let forged_body = vec![99_u8; bytes];
        value["members"][0]["digest"] =
            serde_json::to_value(<[u8; 32]>::from(Sha256::digest(&forged_body))).expect("digest");
        std::fs::write(directory.join(segment_name(0)), forged_body)
            .expect("same-length contradictory body");
        std::fs::write(&path, serde_json::to_vec(&value).expect("encode"))
            .expect("contradictory manifest");
        let forged = super::super::retained_manifest::ArtifactManifest::read(&directory)
            .expect("structurally valid hint");
        assert_eq!(
            forged.origin.identity(),
            artifact.observation.rates.identity,
            "execution origin alone cannot bind members"
        );
        assert_eq!(
            forged.observation().expect("rates").rates,
            artifact.observation.rates,
            "same cost is not same bytes"
        );
        assert_ne!(
            hex::encode(forged.seal().expect("seal")),
            facts.output_identity
        );
        drop(artifact);
        drop(old);
        let fresh = crate::vodserve::tests::bare_serve(temp.path());
        assert!(fresh
            .shared
            .retained_artifacts
            .reacquire_expected(&facts, &fresh.shared, &rendition, Duration::from_secs(5))
            .await
            .is_none());
        drop(fresh);
        value["logical"]["audio_offset_ms"] = 17.into();
        std::fs::write(&path, serde_json::to_vec(&value).expect("encode"))
            .expect("forged logical facts");
        let forged = super::super::retained_manifest::ArtifactManifest::read(&directory)
            .expect("typed untrusted hint");
        assert_ne!(
            hex::encode(forged.seal().expect("seal")),
            facts.output_identity
        );
        assert!(!forged.matches(&rendition));
        let refusing = crate::vodserve::tests::bare_serve(temp.path());
        assert!(refusing
            .shared
            .retained_artifacts
            .reacquire_expected(&facts, &refusing.shared, &rendition, Duration::from_secs(5))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn measured_candidate_cost_requires_complete_actual_audio_route_source_and_digest() {
        use crate::vodgen::Sink;
        use plurx_core::playback::candidate::{CandidateId, CandidateRoute, QualityCandidate};
        let temp = crate::test_tempdir().expect("cost fixture");
        let serve = crate::vodserve::tests::bare_serve(temp.path());
        let mut rendition = crate::vodserve::tests::synthetic_rendition(temp.path()).await;
        let path = temp.path().join("source.bin");
        tokio::fs::write(&path, b"cost source")
            .await
            .expect("source");
        let file = crate::vodserve::tests::media_file_at(path, 10_000);
        let source = crate::fragment_index_cluster::open_source_fence(&file, None)
            .await
            .expect("source fence");
        let digest = [42; 32];
        let candidate_id = CandidateId::for_recipe_digest(digest);
        let audio = plurx_core::playback::audio::AudioDelivery {
            action: plurx_core::playback::audio::AudioAction::None,
            downmix: None,
            reason: "actual fixture producer has no audio".to_owned(),
        };
        let kind = SessionKind::Copy {
            aac: false,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        let owned = Arc::get_mut(&mut rendition).expect("unshared");
        owned.source = Some(source);
        owned.recipe.file = file.clone();
        owned.recipe.audio_delivery = Some(audio.clone());
        // This unit supplies the private dispatch-attested binding. Production
        // can mint it only after the actual recipe equality check.
        owned.recipe.measured_candidate = Some(RetainedCandidateBinding {
            kind,
            normalized_geometry: true,
            profile: None,
            candidate_id,
            recipe_digest: digest,
            file_id: file.id,
            audio_index: None,
            audio_offset_ms: 0,
            subtitle_burn: None,
            grade: plurx_core::transcode::OutputGrade::Sdr,
            route: CandidateRoute::Remux,
        });
        let init = b"actual fixture init";
        let init_digest = hex::encode(Sha256::digest(init));
        *owned.identity.get_mut() = IdentityState {
            identity: Some(InitIdentity {
                muxer_init: init_digest.clone(),
                served_init: init_digest,
                promotion: Default::default(),
            }),
            from_disk: false,
        };
        tokio::fs::write(rendition.dir.path().join(INIT_NAME), init)
            .await
            .expect("init");
        serve.shared.retained_artifacts.collect(temp.path()).await;
        let candidate = QualityCandidate {
            id: candidate_id,
            recipe_digest: digest,
            route: CandidateRoute::Remux,
            normalized_geometry: true,
            width: 1280,
            height: 720,
            target_height: 720,
            average_bps: Some(u64::MAX),
            peak_bps: Some(u64::MAX),
            grade: plurx_core::transcode::OutputGrade::Sdr,
            decoder_compatible: true,
            complete_cache: true,
            sustainable: true,
        };
        let request = SessionRequest {
            sdr_master_codecs: None,
            continuous_media: None,
            vod_only: false,
            passive_vod: false,
            finite_bitrate_limit_bps: None,
            candidate_context: Some(Box::new(
                crate::transcode::TranscodeManager::candidate_context(&candidate),
            )),
            quality_catalog: None,
            file_id: file.id,
            playback_id: "cost-fixture".to_owned(),
            request_id: None,
            control_sequence: None,
            automatic: true,
            previous_session_id: None,
            reopen_reason: None,
            kind,
            start_seconds: 0.0,
            audio_index: None,
            audio_delivery: Some(audio),
            audio_claim: None,
            subtitle_burn: None,
            audio_offset_ms: 0,
            hdr10: false,
            presentation: crate::transcode::Presentation::Vod,
            block_budget_secs: None,
            transport: None,
        };
        let source = rendition.source.as_ref().expect("source");
        assert!(
            serve
                .measured_candidate_cost(&candidate, &request, source)
                .is_none(),
            "complete_cache and planned numbers are not measurement"
        );
        let sink = RenditionSink {
            retirement: tokio_util::sync::CancellationToken::new(),
            shared: Arc::clone(&serve.shared),
            rendition: Arc::clone(&rendition),
            epoch: 0,
        };
        for entry in 0..rendition.plan.len() {
            sink.materialize(entry as u32, vec![7; 1000 + entry])
                .await
                .expect("actual sink publication");
        }
        sink.completed_output().await;
        let deadline = Instant::now() + Duration::from_secs(5);
        let proof = loop {
            if let Some(proof) = serve.measured_candidate_cost(&candidate, &request, source) {
                break proof;
            }
            assert!(
                Instant::now() < deadline,
                "completed retained proof never issued"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        };
        assert!(proof.average_bps() > 0 && proof.average_bps() < u64::MAX);
        assert!(proof.rfc_peak_bps() > 0 && proof.rfc_peak_bps() < u64::MAX);
        let descriptor = proof.public_descriptor();
        assert_eq!(descriptor.recipe_digest, digest);
        assert_eq!(descriptor.qualification, "complete_full_mux_rfc8216_v1");
        assert_eq!(descriptor.artifact_id, proof.artifact_facts().artifact_id);
        assert!(serve
            .shared
            .retained_artifacts
            .acquire_expected(&proof.artifact_facts(), &rendition)
            .is_some());
        let mut altered = request.clone();
        altered.audio_delivery = None;
        assert!(serve
            .measured_candidate_cost(&candidate, &altered, source)
            .is_none());
        altered = request.clone();
        altered.audio_index = Some(7);
        assert!(serve
            .measured_candidate_cost(&candidate, &altered, source)
            .is_none());
        altered = request.clone();
        altered.kind = SessionKind::Copy {
            aac: true,
            preserve_dolby_vision: false,
            convert_dolby_vision: false,
        };
        assert!(serve
            .measured_candidate_cost(&candidate, &altered, source)
            .is_none());
        altered = request.clone();
        altered
            .candidate_context
            .as_mut()
            .expect("context")
            .owner_node_id = Some("other-node".to_owned());
        assert!(serve
            .measured_candidate_cost(&candidate, &altered, source)
            .is_none());
        let mut other_candidate = candidate.clone();
        other_candidate.route = CandidateRoute::Encode;
        assert!(serve
            .measured_candidate_cost(&other_candidate, &request, source)
            .is_none());
        other_candidate = candidate.clone();
        other_candidate.recipe_digest[31] ^= 1;
        assert!(serve
            .measured_candidate_cost(&other_candidate, &request, source)
            .is_none());
        let private = temp.path().join(".retained").join(&descriptor.artifact_id);
        serve.shared.retained_artifacts.collect(temp.path()).await;
        assert!(
            private.exists(),
            "held opaque proof pins the exact artifact"
        );
        drop(proof);
        // Registry publication precedes reservation release by a few
        // instructions. Wait for that actual assembly boundary, not a timer.
        for _ in 0..100 {
            if !serve
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .assembling
            {
                break;
            }
            tokio::task::yield_now().await;
        }
        serve.shared.retained_artifacts.collect(temp.path()).await;
        let state = serve
            .shared
            .retained_artifacts
            .state
            .lock()
            .expect("registry");
        assert!(
            state.entries.values().all(|entry| entry
                .idle_since
                .is_some_and(|since| since.elapsed() < Duration::from_secs(1))),
            "idle grace starts after final proof release"
        );
    }

    #[tokio::test]
    async fn namespace_lease_unknown_orphans_and_preclone_reservations_fail_closed() {
        let temp = crate::test_tempdir().expect("namespace fixture");
        let serve = crate::vodserve::tests::bare_serve(temp.path());
        let rendition = crate::vodserve::tests::synthetic_rendition(temp.path()).await;
        serve.shared.retained_artifacts.collect(temp.path()).await;
        assert!(
            serve
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .startup_done
        );
        let other = crate::vodserve::tests::bare_serve(temp.path());
        other.shared.retained_artifacts.collect(temp.path()).await;
        assert!(
            other
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .namespace_owner
                .is_none(),
            "another owner never scans or deletes our namespace"
        );
        let rates = plurx_core::output_measurement::CompleteOutputRates {
            identity: [3; 32],
            wire_bytes: 1000,
            duration_micros: 1_000_000,
            average_bps: 8000,
            rfc_peak_bps: 8000,
            segment_burst_bps: 8000,
        };
        let reserved = RetainedArtifactRegistry::reserve(&serve.shared, &rendition, &rates)
            .expect("first reservation");
        assert!(
            matches!(
                RetainedArtifactRegistry::reserve(&serve.shared, &rendition, &rates),
                Err(AssemblyRefusal::Busy)
            ),
            "no second queued metadata clone; busy defers rather than refuses"
        );
        assert_eq!(
            serve
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .bytes,
            1000
        );
        drop(reserved);
        assert_eq!(
            serve
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .bytes,
            0,
            "no filesystem mutation releases reservation on cancellation"
        );
        assert!(
            !serve
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .assembling
        );
        drop(other);
        drop(serve);
        tokio::fs::write(
            temp.path().join(".retained").join("unknown-occupant"),
            b"must not delete",
        )
        .await
        .expect("unknown namespace occupant");
        let restored = crate::vodserve::tests::bare_serve(temp.path());
        restored
            .shared
            .retained_artifacts
            .collect(temp.path())
            .await;
        assert!(
            !restored
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .startup_done,
            "unknown bytes cannot silently become uncharged free capacity"
        );
        assert!(temp
            .path()
            .join(".retained")
            .join("unknown-occupant")
            .exists());
        #[cfg(unix)]
        {
            let symlink_fixture = crate::test_tempdir().expect("symlink fixture");
            let outside = crate::test_tempdir().expect("outside fixture");
            std::os::unix::fs::symlink(outside.path(), symlink_fixture.path().join(".retained"))
                .expect("symlink");
            let refused = crate::vodserve::tests::bare_serve(symlink_fixture.path());
            refused
                .shared
                .retained_artifacts
                .collect(symlink_fixture.path())
                .await;
            assert!(refused
                .shared
                .retained_artifacts
                .state
                .lock()
                .expect("registry")
                .namespace_owner
                .is_none());
            assert!(!outside.path().join(".owner.lock").exists());
        }
    }

    // ---- D3 (main-merge defects 2026-10-04): prompt, bounded release ------

    fn d3_rolling_tree(dir: &Path, segments: usize) {
        std::fs::create_dir_all(dir).expect("artifact dir");
        std::fs::write(dir.join("init.mp4"), b"init").expect("init");
        for index in 0..segments {
            std::fs::write(dir.join(format!("seg{index:05}.m4s")), [index as u8; 16])
                .expect("segment");
        }
    }

    #[tokio::test]
    async fn retired_rolling_directory_is_deleted_within_one_tick() {
        // A two-hour title is ~1,800 segments: at 32 files per 30 s tick
        // the old collector needed about 56 minutes to release it.
        let temp = crate::test_tempdir().expect("tick");
        let dir = temp.path().join("artifact");
        d3_rolling_tree(&dir, 1_800);
        assert!(
            remove_artifact_within(&dir, Instant::now() + RETAINED_GC_TICK_BUDGET)
                .await
                .expect("bounded removal"),
            "one tick's budget releases a whole two-hour rolling artifact"
        );
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn rolling_orphans_from_a_crash_are_swept_at_first_maintain() {
        let temp = crate::test_tempdir().expect("orphan");
        let orphan = temp
            .path()
            .join(".retained")
            .join(uuid::Uuid::new_v4().to_string());
        // A rolling collection's directory has no manifest: after a crash it
        // is an orphan, and retention is refused while any remain.
        d3_rolling_tree(&orphan, 600);
        let serve = crate::vodserve::tests::bare_serve(temp.path());
        serve.shared.retained_artifacts.collect(temp.path()).await;
        assert!(
            !orphan.exists(),
            "the first maintenance tick sweeps a crash-orphaned rolling directory"
        );
        assert_eq!(serve.retained_cleanup_pending(), 0);
    }
}
