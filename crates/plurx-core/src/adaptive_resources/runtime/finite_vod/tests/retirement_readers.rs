//! Real Software retirement with counted response owners. A delay retains the
//! original operation and supplies no replacement close or refund proof.
use super::*;
use crate::adaptive_resources::runtime::finite_vod::{
    cleanup_tests::original_software_fixture_with_producer, FiniteCloseIdentity,
    FiniteCloseWorkspace, FiniteFile, FiniteOutput, FiniteReader, FiniteResponseBuffer,
    FiniteVodError, FiniteWriterOutcome, SoftwareAvcPumpPlan, SoftwareVodEvent, SoftwareVodOwner,
};
use crate::adaptive_resources::ResourceVector;
use crate::fmp4::{CutPolicy, FiniteSegmenterLimits, FragmentReaderBounds, MergeMemoryLimits};
use crate::segplan::{PlanCut, PlanEntry, PlanEntryKind, SegmentPlan, SEGPLAN_VERSION};
use crate::vod_generation::GenerationMemoryLimits;
use native_result_fixture::{Gate, Kind};
use std::time::{Duration, Instant};
use v2::scratch::ScratchLifecycleV1;

fn deadline() -> Instant {
    Instant::now() + Duration::from_secs(90)
}

fn complete_output_plan() -> SoftwareAvcPumpPlan {
    let grid = crate::transcode::VodFrameGrid::new(24, 1).expect("canonical software grid");
    let plan = SegmentPlan {
        version: SEGPLAN_VERSION,
        timescale: grid.numerator,
        entries: vec![PlanEntry {
            index: 0,
            kind: PlanEntryKind::Video,
            start_ticks: 0,
            duration_ticks: u64::from(grid.numerator),
            est_bytes: 1_000_000,
            cut: PlanCut::EndOfStream,
            fragments: 1,
        }],
        target_duration: 1,
    };
    let memory = GenerationMemoryLimits {
        reader: FragmentReaderBounds {
            buffer_bytes: 1024 * 1024,
            unit_bytes: 1024 * 1024,
            max_tracks: 8,
            max_runs: 64,
            max_samples: 4096,
        },
        read_chunk_bytes: 4096,
        retained_fragment_memory_bytes: 1024 * 1024,
        retained_fragments: 1,
        init_memory_bytes: 1024 * 1024,
        segmenter: FiniteSegmenterLimits {
            merge: MergeMemoryLimits {
                workspace_bytes: 1024 * 1024,
                output_bytes: 1024 * 1024,
                max_tracks: 8,
                max_samples: 4096,
            },
            prefix_memory_bytes: 1024 * 1024,
            split_memory_bytes: 1024 * 1024,
            published_batch_bytes: 1024 * 1024,
            retained_fragments: 1,
        },
    };
    SoftwareAvcPumpPlan::new(
        plan,
        0,
        grid,
        CutPolicy::new(1, 1, 1024 * 1024, 2, grid.numerator),
        memory,
        4096,
        1024,
        "retirement-reader-test".into(),
    )
    .expect("explicit bounded complete-output profile before admission")
}

struct FixtureRoot(tempfile::TempDir);

impl std::ops::Deref for FixtureRoot {
    type Target = tempfile::TempDir;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Drop for FixtureRoot {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.0.disable_cleanup(true);
            eprintln!(
                "retained failed retirement-reader fixture: {}",
                self.0.path().display()
            );
        }
    }
}

struct OriginalOutput {
    runtime: Arc<NodeResourceRuntime>,
    owner: SoftwareVodOwner,
    preparation: OwnedPreparation,
    member: Uuid,
    output: FiniteOutput,
    init_len: u64,
    original_close_identity: FiniteCloseIdentity,
    original_reservation: u64,
    // Drop the installation after its original native selectors.
    root: FixtureRoot,
}

#[derive(Clone, Copy)]
enum EarlyReader {
    Unarmed,
    Opened,
}

impl OriginalOutput {
    async fn produced() -> Self {
        Self::produce(None).await.0
    }

    async fn produce(early: Option<EarlyReader>) -> (Self, Option<FiniteReader>) {
        let parent = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/finite-vod-fixtures");
        std::fs::create_dir_all(&parent).expect("repo-local native fixture parent");
        let parent = std::fs::canonicalize(parent).expect("canonical native ancestry");
        let root = FixtureRoot(
            tempfile::Builder::new()
                .prefix("retirement-reader-")
                .tempdir_in(parent)
                .expect("actual finite installation"),
        );
        let (runtime, mut owner, preparation, member) =
            original_software_fixture_with_producer(root.path(), complete_output_plan());
        let mut events = owner.take_events().expect("original producer events");
        owner
            .start(deadline())
            .await
            .expect("actual software producer");
        let output = owner.output();
        let mut initialized = 0_u32;
        let mut materialized = 0_u32;
        let mut published_bytes = 0_u64;
        let mut early_reader = None;
        if early.is_some() {
            // Initialized follows the original Init publication and sync. Do
            // not even construct join_producer until this selection is ready:
            // its completed join fences later native opens on the original R.
            loop {
                match tokio::time::timeout_at(deadline().into(), events.recv())
                    .await
                    .expect("actual initial publication event watchdog")
                {
                    Some(SoftwareVodEvent::Initialized(_)) => {
                        initialized += 1;
                        let len = output
                            .file_len(FiniteFile::Init)
                            .await
                            .expect("original Init inventory before join")
                            .expect("Initialized names the actual published Init");
                        assert!(len > 1);
                        let cap = if matches!(early, Some(EarlyReader::Opened)) {
                            1
                        } else {
                            len
                        };
                        let reader = output
                            .prepare_reader(FiniteFile::Init, 0, len, cap, deadline())
                            .await
                            .expect("original reader selection before producer join");
                        if matches!(early, Some(EarlyReader::Opened)) {
                            let first = reader
                                .next_chunk(deadline())
                                .await
                                .expect("actual native open before producer join")
                                .expect("first original byte before the entry fence");
                            assert_eq!(first.as_ref().len(), 1);
                            drop(first);
                        }
                        {
                            let bundle = preparation.state.lock().await;
                            assert!(!bundle.roles[0].finite_vod.as_ref().unwrap().producer_joined);
                        }
                        early_reader = Some(reader);
                        break;
                    }
                    Some(SoftwareVodEvent::Materialized { entry, bytes, .. }) => {
                        assert_eq!(entry, 0, "only the admitted segment is produced");
                        materialized += 1;
                        published_bytes += bytes;
                    }
                    // Preserve the real completion diagnostics if native
                    // setup failed before it could emit Initialized.
                    None => break,
                }
            }
        }
        let completion = {
            let joined = owner.join_producer();
            tokio::pin!(joined);
            let mut open = true;
            loop {
                tokio::select! {
                    result = &mut joined => break result.expect("original root and writer joins"),
                    event = events.recv(), if open => {
                        match event {
                            Some(SoftwareVodEvent::Initialized(_)) => initialized += 1,
                            Some(SoftwareVodEvent::Materialized { entry, bytes, .. }) => {
                                assert_eq!(entry, 0, "only the admitted segment is produced");
                                materialized += 1;
                                published_bytes += bytes;
                            }
                            None => open = false,
                        }
                    }
                }
            }
        };
        while let Ok(event) = events.try_recv() {
            match event {
                SoftwareVodEvent::Initialized(_) => initialized += 1,
                SoftwareVodEvent::Materialized { entry, bytes, .. } => {
                    assert_eq!(entry, 0, "only the admitted segment is produced");
                    materialized += 1;
                    published_bytes += bytes;
                }
            }
        }
        drop(events);
        eprintln!(
            "original retirement fixture completion: {completion:?}; initialized={initialized}; materialized={materialized}; published_bytes={published_bytes}"
        );
        assert!(
            completion
                .root_status
                .expect("actual root status")
                .success(),
            "actual producer failed before retirement behavior: {completion:?}"
        );
        let writer = completion.writer.expect("actual writer completion");
        assert_eq!(writer.outcome, FiniteWriterOutcome::Ran);
        assert!(writer.completed_output);
        let init_len = output
            .file_len(FiniteFile::Init)
            .await
            .expect("original inventory")
            .expect("actual published init");
        assert!(init_len > 1);
        let original_close_identity = preparation.finite_identity(0);
        let original_reservation = preparation.state.lock().await.roles[0].binding.reservation;
        (
            Self {
                runtime,
                owner,
                preparation,
                member,
                output,
                init_len,
                original_close_identity,
                original_reservation,
                root,
            },
            early_reader,
        )
    }

    async fn lifecycle(&self) -> ScratchLifecycleV1 {
        let bundle = self.preparation.state.lock().await;
        bundle.roles[0].bootstrap[&self.member]
            .fold()
            .unwrap()
            .state()
            .asset
            .lifecycle
    }

    async fn freeze(&self) {
        self.owner
            .freeze_output(deadline())
            .await
            .expect("actual producer freeze");
        assert_eq!(self.lifecycle().await, ScratchLifecycleV1::Frozen);
    }

    async fn identity(&self) -> (i64, Uuid, Uuid, u16, u32) {
        let bundle = self.preparation.state.lock().await;
        let role = &bundle.roles[0];
        assert!(!role.refunded);
        assert!(
            role.retirement.is_none(),
            "no target committed while waiting"
        );
        let handle = role.handle.as_ref().expect("same original obligation");
        let state = role.bootstrap[&self.member].fold().unwrap().state();
        (
            role.sequence,
            handle.obligation_id(),
            handle.graph_id(),
            state.asset.totals.pending_jobs,
            state.asset.totals.readers,
        )
    }

    async fn reader(&self, memory_cap: u64) -> FiniteReader {
        self.output
            .prepare_reader(FiniteFile::Init, 0, self.init_len, memory_cap, deadline())
            .await
            .expect("actual counted reader")
    }

    async fn finish(self, retained: FiniteOutput) {
        let mut ticket = retained
            .prepare_eviction(deadline())
            .await
            .expect("original retained cleanup");
        assert_eq!(ticket.original_identity(), self.original_close_identity);
        assert_ne!(
            self.preparation.state.lock().await.roles[0]
                .binding
                .reservation,
            self.original_reservation,
            "R-to-S changes the reservation, never the original close identity"
        );
        let Self {
            runtime,
            owner,
            preparation,
            output,
            root,
            ..
        } = self;
        drop((retained, output, owner, preparation));
        let original = ticket.original_identity();
        let mut workspace = FiniteCloseWorkspace::new();
        ticket
            .finish_retirement_in(&mut workspace, deadline())
            .await
            .expect("known native cleanup and final refund with fixed workspace");
        let completed = match ticket.into_completed() {
            Ok(completed) => completed,
            Err(_) => panic!("real retained close must consume its empty ticket"),
        };
        assert_eq!(completed.original_identity(), original);
        let final_state = runtime.accounting().expect("final accounting");
        assert_eq!(final_state.held, 0);
        assert_eq!(
            runtime
                .affected_owner_inventory()
                .snapshot()
                .unwrap()
                .records()
                .count(),
            0,
            "final physical/application destruction and ledger refund retire the original census row"
        );
        assert_eq!(final_state.unknown_peak_producers, 0);
        assert_eq!(final_state.used, ResourceVector::default());
        Arc::try_unwrap(runtime)
            .ok()
            .expect("all original selectors dropped")
            .drain()
            .expect("original journal worker joined");
        drop(root);
    }
}

fn assert_deadline(error: FiniteVodError) {
    assert_eq!(error.into_io_error().kind(), std::io::ErrorKind::TimedOut);
}

async fn read_init(reader: &FiniteReader, mut buffer: FiniteResponseBuffer, len: u64) {
    while (buffer.len() as u64) < len {
        let chunk = reader
            .next_chunk(deadline())
            .await
            .expect("same counted reader after retirement")
            .expect("actual init chunk");
        buffer
            .try_extend_from_slice(chunk.as_ref())
            .expect("admitted response partition");
    }
    assert_eq!(buffer.len() as u64, len);
    assert!(buffer.windows(4).any(|marker| marker == b"moov"));
    reader
        .close(deadline())
        .await
        .expect("positive native reader close");
}

#[tokio::test]
async fn finite_response_allocation_waits_for_original_frozen_retirement_without_early_credit() {
    let fixture = OriginalOutput::produced().await;
    fixture.freeze().await;
    let before = fixture.identity().await;
    let charge = fixture.runtime.accounting().unwrap().used;
    let result = fixture
        .output
        .allocate_response_buffer(
            fixture.init_len as usize,
            Instant::now() + Duration::from_millis(50),
        )
        .await;
    assert_deadline(result.err().expect("Frozen cannot grant a response slot"));
    assert_eq!(fixture.identity().await, before);
    assert_eq!(fixture.runtime.accounting().unwrap().used, charge);
    assert_eq!(
        fixture.runtime.accounting().unwrap().unknown_peak_producers,
        1
    );

    let mut allocation = Box::pin(
        fixture
            .output
            .allocate_response_buffer(fixture.init_len as usize, deadline()),
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut allocation)
            .await
            .is_err()
    );
    assert_eq!(
        fixture.identity().await,
        before,
        "waiting creates no journal operation"
    );
    let (buffer, retained) = tokio::join!(allocation, fixture.owner.retire_output(deadline()));
    let buffer = buffer.expect("original retirement wakes response allocation");
    let retained = retained.expect("same R-to-S commit");
    let reader = fixture.reader(fixture.init_len).await;
    read_init(&reader, buffer, fixture.init_len).await;
    drop(reader);
    assert_eq!(
        fixture.runtime.accounting().unwrap().unknown_peak_producers,
        0
    );
    fixture.finish(retained).await;
}

#[tokio::test]
async fn finite_reader_preparation_waits_for_original_frozen_retirement_without_new_arm() {
    let fixture = OriginalOutput::produced().await;
    let buffer = fixture
        .output
        .allocate_response_buffer(fixture.init_len as usize, deadline())
        .await
        .expect("original response partition before freeze");
    assert_eq!(fixture.lifecycle().await, ScratchLifecycleV1::Open);
    let joined_identity = fixture.identity().await;
    let joined_charge = fixture.runtime.accounting().unwrap().used;
    let result = fixture
        .output
        .prepare_reader(
            FiniteFile::Init,
            0,
            fixture.init_len,
            fixture.init_len,
            Instant::now() + Duration::from_millis(50),
        )
        .await;
    assert_deadline(
        result
            .err()
            .expect("joined R cannot enter a new native reader"),
    );
    assert_eq!(
        fixture.identity().await,
        joined_identity,
        "post-join selection waits without creating a lease or Arm"
    );
    assert_eq!(fixture.runtime.accounting().unwrap().used, joined_charge);
    assert_eq!(
        fixture.runtime.accounting().unwrap().unknown_peak_producers,
        1
    );
    fixture.freeze().await;
    let before = fixture.identity().await;
    let charge = fixture.runtime.accounting().unwrap().used;
    let result = fixture
        .output
        .prepare_reader(
            FiniteFile::Init,
            0,
            fixture.init_len,
            fixture.init_len,
            Instant::now() + Duration::from_millis(50),
        )
        .await;
    assert_deadline(result.err().expect("Frozen cannot arm a new reader"));
    assert_eq!(
        fixture.identity().await,
        before,
        "expired selection creates no lease/Arm"
    );
    assert_eq!(fixture.runtime.accounting().unwrap().used, charge);

    let mut selection = Box::pin(fixture.output.prepare_reader(
        FiniteFile::Init,
        0,
        fixture.init_len,
        fixture.init_len,
        deadline(),
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut selection)
            .await
            .is_err()
    );
    assert_eq!(fixture.identity().await, before);
    let (reader, retained) = tokio::join!(selection, fixture.owner.retire_output(deadline()));
    let reader = reader.expect("original retirement wakes new reader selection");
    let retained = retained.expect("same R-to-S commit");
    read_init(&reader, buffer, fixture.init_len).await;
    drop(reader);
    fixture.finish(retained).await;
}

#[tokio::test]
async fn finite_retirement_waits_for_same_frozen_reader_io_and_transfers_its_lease() {
    let (fixture, reader) = OriginalOutput::produce(Some(EarlyReader::Opened)).await;
    let reader = reader.expect("same reader opened and read before producer join");
    fixture.freeze().await;
    let lease = reader.fixture_lease_id();
    let gate = Gate::install_next(Kind::ReaderRead, lease);
    let mut read = Box::pin(reader.next_chunk(deadline()));
    tokio::select! {
        result = &mut read => panic!("original read returned before held native result: {}", result.is_ok()),
        result = tokio::time::timeout(Duration::from_secs(10), gate.reached()) => {
            result.expect("same actual pread reached native-result gate");
        }
    }
    drop(read); // The same live reader retains its original armed native job.
    let native = gate.identity();
    let charge = fixture.runtime.accounting().unwrap().used;
    let result = fixture
        .owner
        .retire_output(Instant::now() + Duration::from_millis(50))
        .await;
    assert_deadline(
        result
            .err()
            .expect("retirement waits for original reader Observe"),
    );
    let before = fixture.identity().await;
    assert_eq!(before.3, 1, "same read holds the sole asset IO slot");
    assert_eq!(before.4, 1, "same original reader lease remains charged");
    assert_eq!(gate.identity(), native);
    assert_eq!(fixture.runtime.accounting().unwrap().used, charge);
    assert_eq!(
        fixture.runtime.accounting().unwrap().unknown_peak_producers,
        1
    );

    let mut retirement = Box::pin(fixture.owner.retire_output(deadline()));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut retirement)
            .await
            .is_err()
    );
    assert_eq!(fixture.identity().await, before);
    assert_eq!(gate.identity(), native);
    drop(gate);
    let (chunk, retained) = tokio::join!(reader.next_chunk(deadline()), retirement);
    let chunk = chunk
        .expect("same read observed after native result release")
        .expect("actual next byte");
    assert_eq!(chunk.as_ref().len(), 1);
    drop(chunk);
    let retained = retained.expect("same retirement resumes after original reader Observe");
    assert_eq!(reader.fixture_lease_id(), lease);
    reader
        .close(deadline())
        .await
        .expect("same transferred reader closes positively");
    drop(reader);
    assert_eq!(
        fixture.runtime.accounting().unwrap().unknown_peak_producers,
        0
    );
    fixture.finish(retained).await;
}

#[tokio::test]
async fn finite_freeze_waits_for_prepared_unarmed_reader_and_drop_settles_original_selection() {
    let (fixture, reader) = OriginalOutput::produce(Some(EarlyReader::Unarmed)).await;
    let reader = reader.expect("same reader selected before producer join");
    let before = fixture.identity().await;
    let charge = fixture.runtime.accounting().unwrap().used;
    let result = fixture
        .owner
        .freeze_output(Instant::now() + Duration::from_millis(50))
        .await;
    assert_deadline(result.expect_err("unarmed selected reader blocks Fence"));
    assert_eq!(fixture.lifecycle().await, ScratchLifecycleV1::Open);
    assert_eq!(
        fixture.identity().await,
        before,
        "no Freeze or reader Arm while caller is live"
    );
    assert_eq!(fixture.runtime.accounting().unwrap().used, charge);
    assert_eq!(
        fixture.runtime.accounting().unwrap().unknown_peak_producers,
        1
    );

    let mut freeze = Box::pin(fixture.owner.freeze_output(deadline()));
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut freeze)
        .await
        .is_err());
    assert_eq!(fixture.identity().await, before);
    drop(reader); // Only the original unarmed selection requests closure.
    freeze
        .await
        .expect("same selection is settled while Open before Fence");
    assert_eq!(fixture.lifecycle().await, ScratchLifecycleV1::Frozen);
    let after = fixture.identity().await;
    assert_eq!(after.1, before.1, "same original obligation");
    assert_eq!(after.2, before.2, "same original graph");
    assert_eq!(after.3, 0, "no native reader IO left unresolved");
    assert_eq!(after.4, 0, "dropped unentered reader acquired no lease");
    let retained = fixture
        .owner
        .retire_output(deadline())
        .await
        .expect("same conserving retirement");
    fixture.finish(retained).await;
}

async fn assert_original_reader_ack_pending(
    fixture: &OriginalOutput,
    lease: Uuid,
    original: [Uuid; 3],
    stage: bootstrap::readers::ack_observation_fixture::Stage,
) {
    use bootstrap::readers::ack_observation_fixture::{pending_ids, Stage};
    let bundle = fixture.preparation.state.lock().await;
    let role = &bundle.roles[0];
    let command = match stage {
        Stage::Arm => original[0],
        Stage::Observe => original[1],
    };
    let transition = role
        .transition
        .as_ref()
        .expect("same queued reader transition");
    assert!(transition.effect == Effect::Bootstrap(fixture.member));
    assert_eq!(transition.operation.id, command);
    assert!(
        transition.operation.pending.is_some(),
        "original reply remains unobserved"
    );
    assert!(transition.operation.result.is_none());
    assert_eq!(
        pending_ids(role, fixture.member, lease),
        (original, command)
    );
}

async fn frozen_reader_before_ack_observed_waits_for_original_handoff(
    stage: bootstrap::readers::ack_observation_fixture::Stage,
) {
    use bootstrap::readers::ack_observation_fixture::Gate as AckObservationGate;
    let (fixture, reader) = OriginalOutput::produce(Some(EarlyReader::Opened)).await;
    let reader = reader.expect("same reader opened before producer join");
    fixture.freeze().await;
    let lease = reader.fixture_lease_id();
    let gate = AckObservationGate::install(lease, stage);
    let mut read = Box::pin(reader.next_chunk(deadline()));
    let original = tokio::select! {
        result = &mut read => panic!("reader returned before its queued {stage:?} ACK observation was paused: {}", result.is_ok()),
        result = tokio::time::timeout(Duration::from_secs(10), gate.reached()) => {
            result.expect("original command queued before ACK observed")
        }
    };
    // Only the borrowed read leaves. The real command may already have
    // committed in SQL; its exact runtime ACK remains unobserved here.
    drop(read);
    assert_original_reader_ack_pending(&fixture, lease, original, stage).await;
    let before = fixture.identity().await;
    let charge = fixture.runtime.accounting().unwrap().used;
    assert_eq!(
        fixture.runtime.accounting().unwrap().unknown_peak_producers,
        1
    );

    let expired_allocation = fixture
        .output
        .allocate_response_buffer(
            fixture.init_len as usize,
            Instant::now() + Duration::from_millis(50),
        )
        .await;
    assert_deadline(
        expired_allocation
            .err()
            .expect("queued reader ACK cannot grant a new body"),
    );
    let expired_selection = fixture
        .output
        .prepare_reader(
            FiniteFile::Init,
            0,
            fixture.init_len,
            fixture.init_len,
            Instant::now() + Duration::from_millis(50),
        )
        .await;
    assert_deadline(
        expired_selection
            .err()
            .expect("queued reader ACK cannot arm a new reader"),
    );
    assert_original_reader_ack_pending(&fixture, lease, original, stage).await;
    assert_eq!(fixture.identity().await, before);
    assert_eq!(fixture.runtime.accounting().unwrap().used, charge);

    let mut allocation = Box::pin(
        fixture
            .output
            .allocate_response_buffer(fixture.init_len as usize, deadline()),
    );
    let mut selection = Box::pin(fixture.output.prepare_reader(
        FiniteFile::Init,
        0,
        fixture.init_len,
        fixture.init_len,
        deadline(),
    ));
    let mut retirement = Box::pin(fixture.owner.retire_output(deadline()));
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut allocation)
            .await
            .is_err(),
        "response allocation waits for this original handoff, rather than refusing State"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut selection)
            .await
            .is_err(),
        "new reader waits without arming into the original reader transition"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut retirement)
            .await
            .is_err(),
        "retirement cannot consume another reader's unobserved ACK"
    );
    assert_original_reader_ack_pending(&fixture, lease, original, stage).await;
    assert_eq!(
        fixture.identity().await,
        before,
        "no replacement command or reader lease"
    );
    assert_eq!(fixture.runtime.accounting().unwrap().used, charge);
    assert_eq!(
        fixture.runtime.accounting().unwrap().unknown_peak_producers,
        1
    );

    drop(gate);
    let (chunk, retained, buffer, selected) = tokio::join!(
        reader.next_chunk(deadline()),
        retirement,
        allocation,
        selection,
    );
    let chunk = chunk
        .expect("same original read observes its retained ACK")
        .expect("original next byte");
    assert_eq!(chunk.as_ref().len(), 1);
    drop(chunk);
    assert_eq!(reader.fixture_lease_id(), lease);
    let retained = retained.expect("same original conserving R-to-S operation");
    let buffer = buffer.expect("known S wakes original allocation request");
    let selected = selected.expect("known S wakes original reader selection");
    reader
        .close(deadline())
        .await
        .expect("same transferred reader closes positively");
    drop(reader);
    read_init(&selected, buffer, fixture.init_len).await;
    drop(selected);
    assert_eq!(
        fixture.runtime.accounting().unwrap().unknown_peak_producers,
        0
    );
    fixture.finish(retained).await;
}

#[tokio::test]
async fn finite_frozen_reader_arm_before_ack_observed_waits_for_same_handoff() {
    frozen_reader_before_ack_observed_waits_for_original_handoff(
        bootstrap::readers::ack_observation_fixture::Stage::Arm,
    )
    .await;
}

#[tokio::test]
async fn finite_frozen_reader_observe_before_ack_observed_waits_for_same_handoff() {
    frozen_reader_before_ack_observed_waits_for_original_handoff(
        bootstrap::readers::ack_observation_fixture::Stage::Observe,
    )
    .await;
}

#[tokio::test]
async fn conserving_retirement_keeps_original_census_identity_through_final_close() {
    use crate::adaptive_resources::owner_inventory::{OwnerFamily, OwnerKind};
    let fixture = OriginalOutput::produced().await;
    let before = fixture
        .runtime
        .affected_owner_inventory()
        .snapshot()
        .unwrap();
    let original = before.records().next().expect("original R row");
    assert_eq!(before.records().count(), 1);
    assert_eq!(original.family, OwnerFamily::BufferedVod);
    assert_eq!(original.kind, OwnerKind::Producer);
    let identity = original.identity;
    fixture.freeze().await;
    let retained = fixture
        .owner
        .retire_output(deadline())
        .await
        .expect("actual conserving R-to-S commit");
    let after = fixture
        .runtime
        .affected_owner_inventory()
        .snapshot()
        .unwrap();
    assert_eq!(
        after.records().count(),
        1,
        "replacement never registers a second owner"
    );
    let survivor = after.records().next().unwrap();
    assert_eq!(
        survivor.identity, identity,
        "the same original identity survives the swap"
    );
    assert_eq!(survivor.family, OwnerFamily::RetainedResidency);
    assert_eq!(survivor.kind, OwnerKind::Residency);
    assert!(after.epoch() > before.epoch());
    fixture.finish(retained).await;
}
