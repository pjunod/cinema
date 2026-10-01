//! Private immutable names for observed, completely published VOD output.
//! A recipe directory is mutable; an issued artifact is never that directory.
use super::output_measurement::CompleteOutputObservation;
use super::*;

const MAX_ARTIFACTS: usize = 64;
const RETAINED_GC_BATCH: usize = 32;

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

#[derive(Debug)]
pub(crate) struct RetainedVodArtifact {
    pub(super) id: uuid::Uuid,
    pub(super) observation: CompleteOutputObservation,
    directory: PathBuf,
    charge: u64,
    init_bytes: u64,
    repair: Mutex<()>,
    recipe_key: String,
    source_version: String,
    candidate: Option<RetainedCandidateBinding>,
    audio_delivery: Option<plurx_core::playback::audio::AudioDelivery>,
}

impl RetainedVodArtifact {
    pub(super) fn facts(&self) -> crate::transcode::RetainedOutputFacts {
        crate::transcode::RetainedOutputFacts {
            artifact_id: self.id.to_string(),
            output_identity: hex::encode(self.observation.rates.identity),
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
    bytes: u64,
    assembling: bool,
    namespace_owner: Option<std::fs::File>,
    namespace: Option<PathBuf>,
    startup_scan: Option<std::fs::ReadDir>,
    startup_done: bool,
    orphans: VecDeque<PathBuf>,
}

#[derive(Default)]
pub(super) struct RetainedArtifactRegistry {
    state: StdMutex<RetainedState>,
    collector: Mutex<()>,
}

impl RetainedArtifactRegistry {
    /// Reuses the existing maintenance owner for a refused/busy completion;
    /// there is no second scheduler and no payload or cold-title scan.
    pub(super) fn offer(shared: &Arc<Shared>, rendition: &Arc<Rendition>) -> bool {
        let measurement = rendition
            .output_measurement
            .lock()
            .expect("output measurement lock");
        let Some(rates) = measurement.complete_rates() else {
            return false;
        };
        let Some(reservation) = Self::reserve(shared, rendition, &rates) else {
            return false;
        };
        let Some(observation) = measurement.complete_observation() else {
            return false;
        };
        drop(measurement);
        let shared = Arc::clone(shared);
        let rendition = Arc::clone(rendition);
        tokio::spawn(async move {
            shared
                .retained_artifacts
                .assemble(&shared, &rendition, observation, reservation)
                .await;
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
            let room = MAX_ARTIFACTS.saturating_sub(
                self.state
                    .lock()
                    .expect("retained registry lock")
                    .orphans
                    .len(),
            );
            let batch = tokio::task::spawn_blocking(move || {
                let mut paths = Vec::new();
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
                                paths.push(entry.path());
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
                (scan, paths, done, error)
            })
            .await;
            let Ok((scan, paths, done, error)) = batch else {
                return;
            };
            let mut state = self.state.lock().expect("retained registry lock");
            for path in paths {
                if !state.orphans.contains(&path) {
                    state.orphans.push_back(path);
                }
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
        match remove_artifact_batch(&path).await {
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
    pub(super) fn acquire_expected(
        &self,
        facts: &crate::transcode::RetainedOutputFacts,
        rendition: &Rendition,
    ) -> Option<Arc<RetainedVodArtifact>> {
        if !facts.valid() {
            return None;
        }
        let identity: [u8; 32] = hex::decode(&facts.output_identity).ok()?.try_into().ok()?;
        let artifact = self.acquire(&identity)?;
        (artifact.facts() == *facts
            && artifact.recipe_key == rendition.key
            && rendition.source.as_ref().is_some_and(|source| {
                source.unchanged() && source.object_version() == artifact.source_version
            }))
        .then_some(artifact)
    }
    pub(super) fn acquire(&self, identity: &[u8; 32]) -> Option<Arc<RetainedVodArtifact>> {
        let mut state = self.state.lock().expect("retained registry lock");
        let entry = state.entries.get_mut(identity)?;
        entry.used = Instant::now();
        entry.idle_since = None;
        Some(Arc::clone(&entry.artifact))
    }

    pub(super) fn reserve(
        shared: &Arc<Shared>,
        rendition: &Rendition,
        rates: &plurx_core::output_measurement::CompleteOutputRates,
    ) -> Option<AssemblyReservation> {
        let mut state = shared
            .retained_artifacts
            .state
            .lock()
            .expect("retained registry lock");
        if state.assembling
            || state.entries.contains_key(&rates.identity)
            || state.entries.len() + state.retired.len() >= MAX_ARTIFACTS
            || state
                .bytes
                .checked_add(rates.wire_bytes)
                .is_none_or(|bytes| bytes > rendition.completed_cache_budget)
        {
            return None;
        }
        state.bytes += rates.wire_bytes;
        state.assembling = true;
        Some(AssemblyReservation {
            shared: Arc::clone(shared),
            charge: rates.wire_bytes,
            staging: None,
            published: false,
        })
    }

    /// A detached owner runs this after the completion callback releases all
    /// manifest locks. No cold create waits for this bounded link operation.
    pub(super) async fn assemble(
        &self,
        shared: &Shared,
        rendition: &Arc<Rendition>,
        observation: CompleteOutputObservation,
        mut reservation: AssemblyReservation,
    ) {
        let Ok(preparing) = self.collector.try_lock() else {
            return;
        };
        if !self.own_namespace(&shared.base).await {
            return;
        }
        self.collect_orphans().await;
        let Ok(init_metadata) = tokio::fs::metadata(rendition.dir.path().join(INIT_NAME)).await
        else {
            return;
        };
        let init_bytes = init_metadata.len();
        let Some(charge) = observation.rates.wire_bytes.checked_add(init_bytes) else {
            return;
        };
        drop(preparing);
        {
            let mut state = self.state.lock().expect("retained registry lock");
            if !state.startup_done
                || !state.orphans.is_empty()
                || state
                    .bytes
                    .checked_add(init_bytes)
                    .is_none_or(|bytes| bytes > rendition.completed_cache_budget)
            {
                return;
            }
            state.bytes += init_bytes;
            reservation.charge = charge;
        }
        let id = uuid::Uuid::new_v4();
        let directory = shared.base.join(".retained").join(id.to_string());
        let artifact = Arc::new(RetainedVodArtifact {
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
        });
        reservation.staging = Some(Arc::clone(&artifact));
        let result = self.link_complete(shared, rendition, &artifact).await;
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
                && rendition.gen_epoch.load(Acquire) == artifact.observation.epoch
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
                    .is_some_and(|now| now.rates == artifact.observation.rates)
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
        if name != INIT_NAME && planned_index(name).is_none() {
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
            candidate_context: Some(crate::transcode::CandidateExecutionContext {
                retained_output: None,
                owner_node_id: None,
                candidate_id,
                recipe_digest: digest,
                normalized_geometry: true,
                grade: candidate.grade,
                profile: None,
            }),
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
            RetainedArtifactRegistry::reserve(&serve.shared, &rendition, &rates).is_none(),
            "no second queued metadata clone"
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
}
