//! Complete static output from the original finite software publication.
//! This module handles metadata only. The caller retains the same R owner,
//! joins its original writer, and performs the known R-to-S transfer.

use super::*;
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteApplicationLease, FiniteFile, FiniteOutput, FiniteWriterOutcome, SoftwareVodEvent,
    SoftwareVodOwner, SoftwareVodRequest,
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

/// Complete pure P-side output plan. The original P owner remains charged
/// while the request, immutable plan and publication buffers are allocated.
/// After its native close, R admission consumes this object synchronously.
pub(crate) struct StaticArtifactAdmission {
    pub(super) request: SoftwareVodRequest,
    pub(super) policy: StaticArtifactPolicy,
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
            .checked_add(self.request.requested_heap_bytes()?)?
            .checked_add(self.publication.requested_heap_bytes()?)?
            .checked_add(self.logical.requested_heap_bytes()?)
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
        events: &mut tokio::sync::mpsc::Receiver<SoftwareVodEvent>,
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
    pub(super) fn reject_late_events(
        &mut self,
        events: &mut tokio::sync::mpsc::Receiver<SoftwareVodEvent>,
    ) -> io::Result<()> {
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
    policy: StaticArtifactPolicy,
    owner: SoftwareVodOwner,
    output: FiniteOutput,
    events: Option<tokio::sync::mpsc::Receiver<SoftwareVodEvent>>,
    publication: StaticArtifactPublication,
    logical: retained_manifest::LogicalOutput,
    artifact_id: uuid::Uuid,
    manifest: Option<retained_manifest::ArtifactManifest>,
    started: bool,
    // Drop after every application payload above is destroyed.
    _application_lease: FiniteApplicationLease,
}

impl StaticArtifactRun {
    pub(super) fn new(
        mut owner: SoftwareVodOwner,
        publication: StaticArtifactPublication,
        logical: retained_manifest::LogicalOutput,
        application_lease: FiniteApplicationLease,
        policy: StaticArtifactPolicy,
    ) -> Self {
        let events = owner.take_events();
        let output = owner.output();
        Self {
            policy,
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
        let manifest = self
            .manifest
            .as_ref()
            .ok_or(io::ErrorKind::InvalidData)?
            .clone();
        let mut artifact = manifest.into_finite_artifact(retained)?;
        artifact.static_policy = Some(self.policy);
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
