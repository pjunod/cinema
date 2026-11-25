//! Complete static output from the original finite software publication.
//! This module handles metadata only. The caller retains the same R owner,
//! joins its original writer, and performs the known R-to-S transfer.

use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteApplicationLease, FiniteFile, FiniteOutput, FiniteWriterOutcome, SoftwareVodEvent,
    SoftwareVodEvents, SoftwareVodOwner, SoftwareVodRequest,
};

/// Compact process-private policy facts from the original admitted snapshot.
/// Equality only withdraws logical exposure. It never grants native custody or
/// proves an output closed; that still requires the original finite S owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StaticArtifactPolicy {
    pub(super) file_id: i64,
    generation: i64,
    metadata: [u8; 32],
}

impl StaticArtifactPolicy {
    pub(crate) fn from_snapshot(
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    ) -> Option<Self> {
        use sha2::Digest;
        struct BoundedDigest {
            digest: sha2::Sha256,
            bytes: usize,
        }
        impl std::io::Write for BoundedDigest {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.bytes = self
                    .bytes
                    .checked_add(bytes.len())
                    .filter(|bytes| *bytes <= 64 * 1024)
                    .ok_or(io::ErrorKind::InvalidData)?;
                self.digest.update(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        if snapshot.generation < 0 {
            return None;
        }
        let mut digest = BoundedDigest {
            digest: sha2::Sha256::new(),
            bytes: 0,
        };
        serde_json::to_writer(&mut digest, &snapshot.file).ok()?;
        Some(Self {
            file_id: snapshot.file.id,
            generation: snapshot.generation,
            metadata: digest.digest.finalize().into(),
        })
    }

    pub(super) fn matches_snapshot(
        self,
        snapshot: &plurx_core::store::PlaybackPlanningSnapshot,
    ) -> bool {
        Self::from_snapshot(snapshot) == Some(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StaticArtifactExposure {
    Exposed,
    /// Registry pressure is retryable; it says nothing about policy or S.
    Retry,
    /// A successful current Store read positively refused the original policy.
    Obsolete,
}

/// What a NEW candidate attachment later needs to borrow the settled static
/// S: the same dispatch-attested candidate binding the run encoded under and
/// the executable, engine and source metadata it ran with. It is captured
/// from the original recipe before that recipe is released, and is the only
/// authority the exposed artifact carries; a Store row or manifest never
/// stands in for it.
pub(crate) struct StaticArtifactProvenance {
    pub(super) candidate: Option<RetainedCandidateBinding>,
    pub(super) executable: String,
    pub(super) engine: String,
    pub(super) source_metadata: [u8; 32],
}

impl StaticArtifactProvenance {
    pub(super) fn requested_heap_bytes(&self) -> Option<usize> {
        self.executable
            .capacity()
            .checked_add(self.engine.capacity())
    }
}

/// Complete pure P-side output plan. The original P owner remains charged
/// while the request, immutable plan and publication buffers are allocated.
/// After its native close, R admission consumes this object synchronously.
pub(crate) struct StaticArtifactAdmission {
    pub(super) request: SoftwareVodRequest,
    pub(super) cache_grant: crate::playback_cache::CacheReservation,
    pub(super) compatibility: crate::vodencode::FiniteSoftwareCompatibility,
    pub(super) policy: StaticArtifactPolicy,
    pub(super) provenance: StaticArtifactProvenance,
    pub(super) workspace: u64,
    pub(super) publication: StaticArtifactPublication,
    pub(super) logical: retained_manifest::LogicalOutput,
    pub(super) application_owner_bytes: usize,
}

impl StaticArtifactAdmission {
    /// P-to-R handoff heap that remains live after pure planning returns.
    /// The P application partition must cover this before P releases its
    /// unknown producer peak; future R parser/native work is separate.
    pub(super) fn requested_heap_bytes(&self) -> Option<usize> {
        std::mem::size_of::<Self>()
            .checked_add(crate::vodencode::FiniteSoftwareCompatibility::requested_heap_bytes()?)?
            .checked_add(self.request.requested_heap_bytes()?)?
            .checked_add(self.publication.requested_heap_bytes()?)?
            .checked_add(self.logical.requested_heap_bytes()?)?
            .checked_add(self.provenance.requested_heap_bytes()?)
    }
}

/// Own the exact plan and observed prefix so cancellation of a waiter cannot
/// discard facts already removed from the original event channel. Construct
/// only after these capacities enter the hard R application partition.
pub(super) struct StaticArtifactPublication {
    recipe_key: String,
    source_version: String,
    plan: SegmentPlan,
    playlist: Vec<u8>,
    epoch: u64,
    identity: Option<plurx_core::vod_generation::identity::InitIdentity>,
    next_entry: u32,
    measurement: PublishedOutputMeasurement,
}

impl StaticArtifactPublication {
    pub(super) fn set_epoch(&mut self, epoch: u64) {
        // Only the fresh hard-capacity observation supplies the epoch; this
        // runs before the collector sees any post-publication event.
        assert!(self.identity.is_none() && self.next_entry == 0);
        self.epoch = epoch;
    }

    pub(super) fn requested_heap_bytes(&self) -> Option<usize> {
        self.recipe_key
            .capacity()
            .checked_add(self.source_version.capacity())?
            .checked_add(self.plan.requested_heap_bytes()?)?
            .checked_add(self.playlist.capacity())
    }

    pub(super) fn new(
        recipe_key: String,
        source_version: String,
        plan: SegmentPlan,
        playlist: Vec<u8>,
        epoch: u64,
    ) -> io::Result<Self> {
        if recipe_key.len() != 64
            || source_version.is_empty()
            || plan.is_empty()
            || plan.len() > 8192
            || playlist.len() > retained_manifest::STATIC_MANIFEST_LIMIT
            || playlist != plan.playlist().as_bytes()
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(Self {
            recipe_key,
            source_version,
            plan,
            playlist,
            epoch,
            identity: None,
            next_entry: 0,
            measurement: PublishedOutputMeasurement::default(),
        })
    }

    /// Only the core event channel may supply these post-publication facts.
    /// A duplicate, missing init, wrong entry or incomplete tail is refused.
    pub(super) fn observe(&mut self, event: SoftwareVodEvent) -> io::Result<()> {
        match event {
            SoftwareVodEvent::Initialized(identity)
                if self.identity.is_none() && self.next_entry == 0 =>
            {
                self.identity = Some(identity);
            }
            SoftwareVodEvent::Materialized {
                entry,
                bytes,
                digest,
            } if self.identity.is_some()
                && entry == self.next_entry
                && (entry as usize) < self.plan.len()
                && bytes > 0 =>
            {
                self.measurement.observe_static(
                    &self.recipe_key,
                    &self.playlist,
                    &self.plan,
                    self.identity.as_ref().expect("checked original init"),
                    self.epoch,
                    entry,
                    output_measurement::ObservedOutputMember {
                        bytes,
                        digest,
                        publication: 0,
                    },
                    &self.source_version,
                );
                self.next_entry = self
                    .next_entry
                    .checked_add(1)
                    .ok_or(io::ErrorKind::InvalidData)?;
            }
            _ => return Err(io::ErrorKind::InvalidData.into()),
        }
        Ok(())
    }

    pub(super) fn complete(&mut self) -> io::Result<output_measurement::CompleteOutputObservation> {
        if self.identity.is_none() || self.next_entry as usize != self.plan.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.measurement.complete(self.epoch, true);
        self.measurement
            .complete_observation()
            .ok_or(io::ErrorKind::InvalidData.into())
    }

    /// Drain the original bounded pump channel without joining the producer
    /// yet; source parents remain live for the post-publication fence.
    pub(super) async fn await_complete_events(
        &mut self,
        events: &mut SoftwareVodEvents,
        deadline: Instant,
    ) -> io::Result<()> {
        while self.identity.is_none() || self.next_entry as usize != self.plan.len() {
            let event = tokio::time::timeout_at(deadline.into(), events.recv())
                .await
                .map_err(|_| io::ErrorKind::TimedOut)?
                .ok_or(io::ErrorKind::UnexpectedEof)?;
            self.observe(event)?;
        }
        Ok(())
    }

    /// Call after the original writer/root join and before freeze. A late
    /// duplicate cannot turn an already published manifest into retained S.
    pub(super) fn reject_late_events(&mut self, events: &mut SoftwareVodEvents) -> io::Result<()> {
        match events.try_recv() {
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
            | Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => Ok(()),
            Ok(_) => Err(io::ErrorKind::InvalidData.into()),
        }
    }

    /// Publish before `SoftwareVodOwner::join_producer`: that join closes the
    /// source parents, and no later pathname check can replace them. Errors
    /// retain the caller's original R and the core's publication operation.
    pub(super) async fn publish_before_join(
        &mut self,
        artifact_id: uuid::Uuid,
        logical: &retained_manifest::LogicalOutput,
        output: &FiniteOutput,
        deadline: Instant,
    ) -> io::Result<retained_manifest::ArtifactManifest> {
        let observation = self.complete()?;
        let init_bytes = output
            .file_len(FiniteFile::Init)
            .await
            .map_err(|error| error.into_io_error())?
            .filter(|bytes| *bytes > 0)
            .ok_or(io::ErrorKind::InvalidData)?;
        let manifest = retained_manifest::ArtifactManifest::from_finite_observation(
            artifact_id,
            observation,
            logical.clone(),
            init_bytes,
        );
        let (mut bytes, _seal) = manifest.encode_finite_bounded()?;
        output
            .publish_complete_artifact_manifest(&mut bytes, deadline)
            .await
            .map_err(|error| error.into_io_error())?;
        Ok(manifest)
    }
}

/// The original R owner, channel cursor and manifest identity stay together
/// across cancellation of a job waiter. The manager registers this object
/// before `start` enters native work and keeps it until a known S handoff or
/// its original cancellation ticket has settled.
pub(crate) struct StaticArtifactRun {
    cache_grant: Option<crate::playback_cache::FiniteCacheReservation>,
    compatibility: Option<crate::vodencode::FiniteSoftwareCompatibility>,
    policy: StaticArtifactPolicy,
    provenance: StaticArtifactProvenance,
    owner: SoftwareVodOwner,
    output: FiniteOutput,
    events: Option<SoftwareVodEvents>,
    publication: StaticArtifactPublication,
    logical: retained_manifest::LogicalOutput,
    artifact_id: uuid::Uuid,
    manifest: Option<retained_manifest::ArtifactManifest>,
    started: bool,
    // Drop after every application payload above is destroyed.
    _application_lease: FiniteApplicationLease,
}

impl StaticArtifactRun {
    #[expect(
        clippy::too_many_arguments,
        reason = "Each argument is a distinct admitted owner the one original static run must hold until its own retirement; a grouping struct would carry no invariant."
    )]
    pub(super) fn new(
        mut owner: SoftwareVodOwner,
        cache_grant: crate::playback_cache::FiniteCacheReservation,
        compatibility: crate::vodencode::FiniteSoftwareCompatibility,
        publication: StaticArtifactPublication,
        logical: retained_manifest::LogicalOutput,
        application_lease: FiniteApplicationLease,
        policy: StaticArtifactPolicy,
        provenance: StaticArtifactProvenance,
    ) -> Self {
        let events = owner.take_events();
        let output = owner.output();
        Self {
            cache_grant: Some(cache_grant),
            compatibility: Some(compatibility),
            policy,
            provenance,
            owner,
            output,
            events,
            publication,
            logical,
            artifact_id: uuid::Uuid::new_v4(),
            manifest: None,
            started: false,
            _application_lease: application_lease,
        }
    }

    pub(crate) fn take_cache_grant(
        &mut self,
    ) -> Option<crate::playback_cache::FiniteCacheReservation> {
        self.cache_grant.take()
    }

    #[cfg(all(test, feature = "cluster-integration-tests"))]
    pub(super) fn original_output_for_test(&self) -> FiniteOutput {
        self.output.clone()
    }

    pub(crate) fn take_compatibility(
        &mut self,
    ) -> Option<crate::vodencode::FiniteSoftwareCompatibility> {
        self.compatibility.take()
    }

    pub(crate) fn request_stop(&self) -> io::Result<()> {
        self.owner
            .request_stop()
            .map_err(|error| error.into_io_error())
    }

    /// Begin cancellation on the original R graph. The returned compact
    /// selector stays with the manager while this rich run and its application
    /// lease are destroyed before final settlement.
    pub(crate) async fn prepare_cancel(
        &self,
        deadline: Instant,
    ) -> io::Result<plurx_core::adaptive_resources::runtime::finite_vod::FiniteCloseTicket> {
        self.owner
            .prepare_cancel(deadline)
            .await
            .map_err(|error| error.into_io_error())
    }

    pub(super) fn projected_application_bytes(
        publication: &StaticArtifactPublication,
        logical: &retained_manifest::LogicalOutput,
        registry_bytes: usize,
    ) -> Option<usize> {
        // The common finite profile already charges the one original
        // compatibility Arc. Add this run's actual fixed wrapper field via
        // size_of<Self>, without charging that same allocation a second time.
        std::mem::size_of::<Self>()
            .checked_add(publication.requested_heap_bytes()?)?
            .checked_add(logical.requested_heap_bytes()?)?
            .checked_add(registry_bytes)
    }

    /// A repeated call resumes the same source cells, event receiver and
    /// publication identity. No Store job fact is used as native authority.
    pub(crate) async fn complete(
        &mut self,
        deadline: Instant,
    ) -> io::Result<super::retained::RetainedVodArtifact> {
        if !self.started {
            self.owner
                .start(deadline)
                .await
                .map_err(|error| error.into_io_error())?;
            self.started = true;
        }
        let events = self.events.as_mut().ok_or(io::ErrorKind::InvalidData)?;
        self.publication
            .await_complete_events(events, deadline)
            .await?;
        if self.manifest.is_none() {
            self.manifest = Some(
                self.publication
                    .publish_before_join(self.artifact_id, &self.logical, &self.output, deadline)
                    .await?,
            );
        }
        let completion = self
            .owner
            .join_producer()
            .await
            .map_err(|error| error.into_io_error())?;
        if !completion
            .root_status
            .is_some_and(|status| status.success())
            || !completion.writer.is_some_and(|writer| {
                writer.outcome == FiniteWriterOutcome::Ran && writer.completed_output
            })
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        self.publication.reject_late_events(events)?;
        self.owner
            .freeze_output(deadline)
            .await
            .map_err(|error| error.into_io_error())?;
        let retained = self
            .owner
            .retire_output(deadline)
            .await
            .map_err(|error| error.into_io_error())?;
        if let Some(compatibility) = self.compatibility.take() {
            compatibility.known_retired();
        }
        if let Some(grant) = self.cache_grant.take() {
            grant.retained();
        }
        let manifest = self
            .manifest
            .as_ref()
            .ok_or(io::ErrorKind::InvalidData)?
            .clone();
        let mut artifact = manifest.into_finite_artifact(retained)?;
        artifact.static_policy = Some(self.policy);
        // The settled S keeps the run's own candidate binding and origin so a
        // later NEW candidate attachment resolves it exactly like a privately
        // prepared complete output. The manifest it was built from carries
        // neither: a discovered manifest must never mint this authority.
        artifact.candidate = self.provenance.candidate.clone();
        let _ = artifact.private_preparation_origin.set(
            super::retained::PreparedOrigin::settled_static(
                uuid::Uuid::new_v4(),
                artifact.id,
                &self.provenance,
            ),
        );
        Ok(artifact)
    }
}

impl VodServe {
    fn static_store_output(
        artifact: &super::retained::RetainedVodArtifact,
    ) -> Result<plurx_core::store::background_jobs::CopyOutputJobOutput, String> {
        let facts = artifact.facts();
        let rates = &artifact.observation.rates;
        Ok(plurx_core::store::background_jobs::CopyOutputJobOutput {
            artifact_id: facts.artifact_id,
            output_identity: facts.output_identity,
            source_object_version: artifact.source_version.clone(),
            wire_bytes: i64::try_from(rates.wire_bytes).map_err(|_| "static wire byte overflow")?,
            duration_micros: i64::try_from(rates.duration_micros)
                .map_err(|_| "static duration overflow")?,
            average_bps: rates.average_bps,
            peak_bps: rates.rfc_peak_bps,
        })
    }

    /// Resolve the original immutable Store command after a lost await.
    /// Exact full DTO and positive job state are logical publication facts;
    /// they never replace the same retained finite S owner.
    pub(crate) async fn reconcile_static_artifact_store(
        &self,
        artifact: &super::retained::RetainedVodArtifact,
        original: &plurx_core::store::background_jobs::JobToken,
        intent: &plurx_core::store::background_jobs::EncodedOutputIntent,
    ) -> Result<Option<bool>, String> {
        use plurx_core::store::background_jobs::{JobPublishOutcome, JobState};
        let record = self
            .shared
            .store
            .encoded_output_publication(&original.claim_id)
            .await
            .map_err(|error| error.to_string())?;
        let Some(record) = record else {
            return Ok(None);
        };
        let expected = Self::static_store_output(artifact)?;
        if !record.request.token.same_claim_as(original)
            || record.request.intent != *intent
            || record.request.output != expected
        {
            return Err(
                "original static Store publication command conflicts with its retained S".into(),
            );
        }
        let positive = matches!(record.result,
            JobPublishOutcome::Published { ref job_id, ref result_ref }
                | JobPublishOutcome::AlreadyPublished { ref job_id, ref result_ref }
                if job_id == &original.job_id && result_ref == &expected.artifact_id);
        if !positive {
            return Ok(Some(false));
        }
        let job = self
            .shared
            .store
            .background_job(&original.job_id)
            .await
            .map_err(|error| error.to_string())?;
        Ok(Some(job.is_some_and(|job| {
            job.state == JobState::Succeeded
                && job.result_ref.as_deref() == Some(expected.artifact_id.as_str())
        })))
    }

    /// The Store row is historical attachment authority only. The original
    /// core S selector remains in `artifact`; Store confirmation and current
    /// logical policy are both required before this node exposes that Arc.
    pub(crate) async fn commit_static_artifact_store(
        &self,
        artifact: &super::retained::RetainedVodArtifact,
        fence: &crate::background_jobs::JobFence,
        intent: &plurx_core::store::background_jobs::EncodedOutputIntent,
    ) -> Result<bool, String> {
        let output = Self::static_store_output(artifact)?;
        fence
            .publish_encoded_output(intent.clone(), output)
            .await
            .map_err(|error| error.to_string())
    }

    pub(crate) async fn prepare_static_artifact_eviction(
        &self,
        artifact: &super::retained::RetainedVodArtifact,
        deadline: Instant,
    ) -> Result<plurx_core::adaptive_resources::runtime::finite_vod::FiniteCloseTicket, String>
    {
        artifact
            .finite_output
            .as_ref()
            .ok_or("static artifact has no original retained S")?
            .prepare_eviction(deadline)
            .await
            .map_err(|error| error.to_string())
    }

    /// Store has acknowledged the same exact output. A lost waiter between
    /// that acknowledgement and registry insertion can retry this step
    /// without reusing the now-consumed Store job token.
    pub(crate) async fn expose_published_static_artifact(
        &self,
        artifact: Arc<super::retained::RetainedVodArtifact>,
        intent: &plurx_core::store::background_jobs::EncodedOutputIntent,
        file_id: i64,
        expected_policy_generation: &str,
        budget: u64,
    ) -> Result<StaticArtifactExposure, String> {
        let policy = artifact
            .static_policy
            .ok_or("static artifact has no original policy snapshot")?;
        let current = self
            .shared
            .store
            .playback_planning_snapshot(file_id, &[])
            .await
            .map_err(|error| error.to_string())?;
        if current
            .as_ref()
            .filter(|snapshot| policy.matches_snapshot(snapshot))
            .and_then(|snapshot| super::retained::encoded_policy_generation(&snapshot.file, intent))
            .as_deref()
            != Some(expected_policy_generation)
        {
            return Ok(StaticArtifactExposure::Obsolete);
        }
        Ok(
            if self
                .shared
                .retained_artifacts
                .expose_finite_static(artifact, budget)
                .is_ok()
            {
                StaticArtifactExposure::Exposed
            } else {
                StaticArtifactExposure::Retry
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::domain::{ItemKind, LibraryKind, NewItem, NewLibrary};
    use plurx_core::store::background_jobs::{
        ClaimJob, ClaimOutcome, EncodedOutputIntent, EnqueueJob, EnqueueOutcome, JobKind,
        JobPayload, JobPublishOutcome, JobRequest, PublishEncodedOutputJob,
    };
    use plurx_core::store::{BackgroundJobStore, LibraryStore, MediaStore, SqliteStore};

    /// A retained static S whose logical facts are exactly `identity`; the
    /// reconcile reads only those facts, never the native owner.
    fn retained(identity: [u8; 32]) -> super::super::retained::RetainedVodArtifact {
        use super::super::output_measurement::{CompleteOutputObservation, OutputOrigin};
        let rates = plurx_core::output_measurement::CompleteOutputRates {
            identity,
            wire_bytes: 100,
            duration_micros: 1_000_000,
            average_bps: 800,
            rfc_peak_bps: 1000,
            segment_burst_bps: 1000,
        };
        super::super::retained::RetainedVodArtifact {
            private_preparation_origin: std::sync::OnceLock::new(),
            id: uuid::Uuid::new_v4(),
            observation: CompleteOutputObservation {
                preimage: OutputOrigin {
                    nonce: [1; 16],
                    epoch: 1,
                    source_version: "object:1".into(),
                    recipe_key: "static".into(),
                    playlist: Vec::new(),
                    muxer_init: String::new(),
                    served_init: String::new(),
                },
                rates,
                epoch: 1,
                served_init: String::new(),
                members: Vec::new(),
            },
            directory: PathBuf::from("/nonexistent/static"),
            finite_output: None,
            static_policy: None,
            charge: 0,
            init_bytes: 0,
            repair: Mutex::new(()),
            recipe_key: "static".into(),
            source_version: "object:1".into(),
            candidate: None,
            audio_delivery: None,
            durable: false,
            logical: None,
            validated: std::sync::atomic::AtomicBool::new(true),
            sealed_identity: std::sync::OnceLock::new(),
        }
    }

    fn intent() -> EncodedOutputIntent {
        EncodedOutputIntent {
            target_node_id: "node-a".into(),
            target_height: 360,
            requested_height: Some(360),
            copy_for_burn: None,
            audio_index: None,
            audio_offset_ms: 0,
            audio_claim: plurx_core::playback::audio::AudioClaim {
                decoders: vec!["aac".into()],
                sinks: vec![],
            },
            audio_delivery: plurx_core::playback::audio::AudioDelivery {
                action: plurx_core::playback::audio::AudioAction::None,
                downmix: None,
                reason: "no_audio".into(),
            },
            subtitle_burn: None,
            subtitle_digest: None,
            hdr10_requested: false,
            grade: plurx_core::transcode::OutputGrade::Sdr,
            normalized_geometry: false,
            profile: None,
            width: 640,
            height: 360,
            plan_digest: "a".repeat(64),
            executable_digest: "b".repeat(64),
            engine_digest: "c".repeat(64),
            candidate_id: None,
            candidate_digest: None,
        }
    }

    #[tokio::test]
    async fn a_lost_static_publication_await_reconciles_from_the_original_command() {
        let store = Arc::new(SqliteStore::open_in_memory().expect("store"));
        let library = store
            .create_library(&NewLibrary {
                name: "static".into(),
                kind: LibraryKind::Movies,
                paths: vec!["/media".into()],
                anime: false,
            })
            .await
            .expect("library");
        let item = store
            .insert_item(&NewItem {
                library_id: library.id,
                kind: ItemKind::Movie,
                parent_id: None,
                title: "static".into(),
                year: None,
                season_number: None,
                episode_number: None,
            })
            .await
            .expect("item");
        let file = store
            .upsert_file(item, "/media/static.mkv", 100, 1, &Default::default())
            .await
            .expect("file");
        let intent = intent();
        assert!(intent.valid());
        let id = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            store
                .enqueue_job(EnqueueJob {
                    id: id.clone(),
                    payload: JobPayload::EncodedOutputPrepare {
                        encoded_output_version: 1,
                        file_id: file,
                        source_generation: "source:1".into(),
                        source_size: 100,
                        source_mtime: 1,
                        source_object_version: "object:1".into(),
                        policy_generation: "policy:1".into(),
                        intent: intent.clone(),
                        candidate_catalog: None,
                        scratch_bytes: 1000,
                        reason: "recent_demand".into(),
                    },
                    dedupe_key: "static:1".into(),
                    priority: 1,
                    not_before_ms: 1000,
                    now_ms: 1000,
                    request: JobRequest {
                        scope: "encoded".into(),
                        request_id: "static:1".into(),
                        request_digest: "d".repeat(64),
                        consumer_kind: "encoded_output".into(),
                        consumer_ref: file.to_string(),
                        target_node_id: Some("node-a".into()),
                        deadline_ms: None,
                        retain_identity: false,
                    },
                })
                .await
                .expect("enqueue"),
            EnqueueOutcome::Accepted { .. }
        ));
        let queued = store.background_job(&id).await.expect("job").expect("row");
        let token = match store
            .claim_job(ClaimJob {
                job_id: id.clone(),
                expected_revision: queued.revision,
                node_id: "node-a".into(),
                boot_id: uuid::Uuid::new_v4().to_string(),
                claim_id: uuid::Uuid::new_v4().to_string(),
                kind: JobKind::EncodedOutputPrepare,
                payload_version: 1,
                now_ms: 1001,
                dispatched_at_ms: 1001,
            })
            .await
            .expect("claim")
        {
            ClaimOutcome::Claimed { job } => job.token.expect("token"),
            other => panic!("claim {other:?}"),
        };
        let base = tempfile::tempdir().expect("vod base");
        let serve = VodServe::new(base.path().join("vod"), store.clone());
        let artifact = retained([9; 32]);
        // The publication is acknowledged by the Store, but the worker loses
        // the await: only the durable command can tell it what happened.
        assert!(matches!(
            store
                .publish_encoded_output_job(PublishEncodedOutputJob {
                    token: token.clone(),
                    intent: intent.clone(),
                    output: VodServe::static_store_output(&artifact).expect("output"),
                    now_ms: 1002,
                })
                .await
                .expect("publish"),
            JobPublishOutcome::Published { .. }
        ));
        assert_eq!(
            serve
                .reconcile_static_artifact_store(&artifact, &token, &intent)
                .await,
            Ok(Some(true)),
            "the original positive command and succeeded job prove the publication"
        );
        assert!(
            serve
                .reconcile_static_artifact_store(&retained([8; 32]), &token, &intent)
                .await
                .is_err(),
            "a different retained S conflicts with the original command"
        );
        let mut unpublished = token.clone();
        unpublished.claim_id = uuid::Uuid::new_v4().to_string();
        assert_eq!(
            serve
                .reconcile_static_artifact_store(&artifact, &unpublished, &intent)
                .await,
            Ok(None),
            "a claim that never published has no original command"
        );
    }
}
