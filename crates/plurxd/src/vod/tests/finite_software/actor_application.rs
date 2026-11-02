//! Authentic application lifetime regression. The original selected Store and
//! core role come from the parent fixture; the R producer is never started.
//! Only that original owner's no-entry cancel ticket may close/refund it.
use super::*;

#[tokio::test]
async fn original_application_lease_waits_for_last_actor_snapshot_before_same_role_close() {
    use crate::playback_control::{RollingActorAccounting, RollingControlHandle};

    let fixture = finite_cancellation_fixture().await;
    let runtime = &fixture.runtime;
    let baseline = runtime.accounting().expect("original baseline accounting");
    let deadline = Instant::now() + Duration::from_secs(90);
    let epoch = runtime
        .refresh_admission_capacity(deadline)
        .await
        .expect("actual admission capacity");
    let capacity = runtime.accounting().expect("observed capacity").capacity;
    let (mut request, _finalized_threads) = finite_profile::compile(
        &fixture.serve.shared,
        &"e".repeat(64),
        &fixture.recipe,
        None,
        &fixture.plan,
        epoch,
        capacity,
        crate::admission::software_budget() as u64,
    )
    .expect("production request from the authentic fixture");
    let application_bytes = RollingControlHandle::original_actor_frame_bytes()
        .expect("actual actor/application requested layout");
    request
        .charge_application_owner_bytes(application_bytes)
        .expect("charge before the original R is admitted");
    let client_partition = usize::try_from(request.output.client_state_bytes)
        .expect("actual original client partition");
    let owner = runtime
        .prepare_software_vod(request, deadline)
        .expect("actual original never-entered R owner");
    let held = runtime.accounting().expect("accepted original role").used;
    assert_ne!(held, baseline.used, "fixture failed to hold an actual R");
    let application = owner
        .take_application_lease(application_bytes)
        .expect("SAME admitted role's original application lease");
    let output = owner.output();
    let budget_owner_bytes = RollingActorAccounting::owner_bytes().expect("actual budget Arc");
    let client_limit = client_partition
        .checked_sub(budget_owner_bytes)
        .expect("budget owner fits original partition");
    let (metadata, clients) = tokio::time::timeout_at(
        deadline.into(),
        output.reserve_metadata_partitions(budget_owner_bytes, client_limit),
    )
    .await
    .expect("borrowed metadata wait deadline")
    .expect("original role's partition slices");
    let accounting = RollingActorAccounting::new(metadata, clients)
        .expect("initial actor state reserved before construction");
    let (control, registration) =
        RollingControlHandle::spawn_prepublication_producer_with_application(
            "actual-never-entered-application-fixture",
            application,
            accounting,
        );
    let snapshot = control
        .snapshot()
        .await
        .expect("actual original actor reply");
    let last_alias = snapshot.clone();
    let mut selector = control.original_actor_join();
    // prepare_cancel generates the original positive no-entry proof. Neither
    // the snapshot, timeout nor this fixture can manufacture that receipt.
    let mut original_ticket = owner
        .prepare_cancel(deadline)
        .await
        .expect("original no-entry native cancellation receipt");
    drop(output);
    drop(owner);
    drop(registration);
    drop(control);
    selector
        .join(deadline)
        .await
        .expect("original run frame finishes despite escaped snapshot aliases");
    drop(selector);
    drop(snapshot);
    assert_eq!(
        runtime.accounting().expect("post-join accounting").used,
        held
    );
    assert!(
        original_ticket
            .finish_retirement(Instant::now() + Duration::from_millis(20))
            .await
            .is_err(),
        "a live original application alias permitted role settlement"
    );
    assert_eq!(
        runtime.accounting().expect("alias still charged").used,
        held
    );
    drop(last_alias);
    original_ticket
        .finish_retirement(deadline)
        .await
        .expect("SAME original ticket closes after the final actual alias drops");
    assert_eq!(
        runtime.accounting().expect("known original closure").used,
        baseline.used
    );
}

struct OriginalActorFixture {
    fixture: FiniteCancellationFixture,
    owner: plurx_core::adaptive_resources::runtime::finite_vod::SoftwareVodOwner,
    control: crate::playback_control::RollingControlHandle,
    registration: crate::playback_control::RollingProducerExecutorRegistration,
    client_limit: usize,
    baseline: plurx_core::adaptive_resources::ResourceVector,
}

async fn original_actor_fixture() -> OriginalActorFixture {
    use crate::playback_control::{RollingActorAccounting, RollingControlHandle};
    let fixture = finite_cancellation_fixture().await;
    let runtime = &fixture.runtime;
    let baseline = runtime.accounting().expect("original baseline").used;
    let deadline = Instant::now() + Duration::from_secs(90);
    let epoch = runtime
        .refresh_admission_capacity(deadline)
        .await
        .expect("actual capacity");
    let (mut request, _finalized_threads) = finite_profile::compile(
        &fixture.serve.shared,
        &"e".repeat(64),
        &fixture.recipe,
        None,
        &fixture.plan,
        epoch,
        runtime.accounting().expect("capacity").capacity,
        crate::admission::software_budget() as u64,
    )
    .expect("production request");
    let application_bytes =
        RollingControlHandle::original_actor_frame_bytes().expect("real frame layout");
    request
        .charge_application_owner_bytes(application_bytes)
        .expect("charge before admission");
    let client_partition =
        usize::try_from(request.output.client_state_bytes).expect("real partition");
    let owner = runtime
        .prepare_software_vod(request, deadline)
        .expect("original never-entered R");
    let application = owner
        .take_application_lease(application_bytes)
        .expect("SAME original application lease");
    let output = owner.output();
    let budget_bytes = RollingActorAccounting::owner_bytes().expect("real budget Arc");
    let client_limit = client_partition
        .checked_sub(budget_bytes)
        .expect("original partition");
    let (metadata, clients) = output
        .reserve_metadata_partitions(budget_bytes, client_limit)
        .await
        .expect("actual original partitions");
    let accounting =
        RollingActorAccounting::new(metadata, clients).expect("initial state before construction");
    drop(output);
    let (control, registration) =
        RollingControlHandle::spawn_prepublication_producer_with_application(
            "actual-never-entered-End-fixture",
            application,
            accounting,
        );
    OriginalActorFixture {
        fixture,
        owner,
        control,
        registration,
        client_limit,
        baseline,
    }
}

impl OriginalActorFixture {
    async fn close(self) {
        let Self {
            fixture,
            owner,
            control,
            registration,
            client_limit: _,
            baseline,
        } = self;
        let deadline = Instant::now() + Duration::from_secs(90);
        let mut selector = control.original_actor_join();
        // Only the actual never-entered owner can create this positive ticket.
        let mut ticket = owner
            .prepare_cancel(deadline)
            .await
            .expect("original no-entry cancellation");
        drop(owner);
        drop(registration);
        drop(control);
        selector
            .join(deadline)
            .await
            .expect("same original actor joined");
        drop(selector);
        ticket
            .finish_retirement(deadline)
            .await
            .expect("same original role closed");
        assert_eq!(
            fixture.runtime.accounting().expect("actual closure").used,
            baseline
        );
    }
}

fn selected_end_request() -> crate::playback_control::ControlRequestV1 {
    use crate::playback_control::*;
    ControlRequestV1 {
        intent: None,
        protocol: PROTOCOL_V1.to_owned(),
        generation: "original-End-generation".to_owned(),
        control_epoch: 1,
        client_instance_id: uuid::Uuid::new_v4().to_string(),
        sequence: 1,
        demand: PlaybackDemand::End,
        position_ms: 1_000,
        buffered_from_ms: Some(0),
        buffered_through_ms: 2_000,
        playback_rate: 1.0,
        render_state: RenderState::Ended,
        seek_target_ms: None,
        observed_download_bps: None,
        selection: ClientSelection {
            quality: QualitySelection::Original,
            audio_track: None,
            subtitle: SubtitleSelection {
                mode: SubtitleMode::Off,
                track: None,
            },
            audio_offset_ms: 0,
            codec: CodecPolicy::Auto,
            dynamic_range: DynamicRangePolicy::Auto,
        },
        capabilities: Some(DynamicCapabilities {
            presentation_target: None,
            decoder_caps: None,
            platform: ClientPlatform::Web,
            max_height: 1080,
            codecs: vec![CodecPolicy::H264],
            dynamic_ranges: vec![DynamicRangePolicy::Sdr],
            dual_player_preparation: false,
        }),
        observation: None,
        acknowledgement: None,
        supported_actions: None,
    }
}

async fn selected_end_without_preparation(
    control: &crate::playback_control::RollingControlHandle,
    request: &crate::playback_control::ControlRequestV1,
    deadline_unix_ms: i64,
) -> Result<
    crate::playback_control::RollingControlOutcome,
    crate::playback_control::ControlStateError,
> {
    use crate::playback_control::*;
    // Same short original input reservation as HTTP, before snapshot clone.
    let _input = control
        .reserve_selected_payload(
            request
                .requested_control_input_bytes()
                .expect("actual input projection"),
        )
        .expect("original input reservation")
        .expect("selected original role");
    control
        .control_before_with_consumer(
            LocalControlRequest {
                session_id: "actual-never-entered-End-fixture",
                generation: &request.generation,
                owner_node_id: "fixture-node",
                owner_epoch: request.control_epoch,
                client_instance_id: &request.client_instance_id,
                sequence: request.sequence,
                snapshot: PlaybackDemandSnapshot::from(request),
                prepared_successor: PreparedSuccessorObservation::Inactive,
            },
            deadline_unix_ms,
            None,
            None,
            Some(0),
        )
        .await
}

#[tokio::test]
async fn selected_end_uses_measured_original_partition_without_preparation_context() {
    use crate::playback_control::{ControlDisposition, RollingTerminalCause};
    use plurx_core::adaptive_resources::runtime::finite_vod::FiniteClientSlot;
    let fixture = original_actor_fixture().await;
    let request = selected_end_request();
    let deadline = crate::media_sessions::unix_ms() + 4_000;
    let projection = fixture
        .control
        .preparation_requirement_before(
            &request.generation,
            request.control_epoch,
            &request,
            deadline,
        )
        .await
        .expect("pure same-actor probe")
        .expect("actual selected actor");
    assert!(!projection.needed);
    let snapshot = fixture
        .control
        .snapshot()
        .await
        .expect("same original actor");
    assert_eq!(
        snapshot.accepted_demand_sequence, None,
        "probe advanced sequence"
    );
    assert!(!snapshot.retired, "probe mutated lifecycle");
    assert_eq!(
        snapshot.last_renewal_kind,
        "actual-never-entered-End-fixture"
    );
    drop(snapshot);
    // Leave exactly the production layout's mandatory actor/input peak. The
    // filler is charged to this SAME original role; no arbitrary headroom or
    // grant factory. Any extra preparation context charge exceeds this peak.
    let fill_charge = fixture
        .client_limit
        .checked_sub(projection.control_peak)
        .expect("mandatory real peak fits original role");
    let fill_payload = fill_charge
        .checked_sub(FiniteClientSlot::required_bytes(0).expect("actual slot header"))
        .expect("nonempty filler reservation");
    let fill = fixture
        .control
        .reserve_selected_payload(fill_payload)
        .expect("real original partition filler");
    let outcome = selected_end_without_preparation(&fixture.control, &request, deadline)
        .await
        .expect("End must fit its measured real peak");
    assert_eq!(outcome.disposition, ControlDisposition::Accepted);
    assert_eq!(outcome.accepted_sequence, 1);
    assert_eq!(outcome.lease.terminal, Some(RollingTerminalCause::End));
    assert!(outcome.preparation_directive.is_none());
    drop(outcome);
    drop(fill);
    fixture.close().await;
}

#[tokio::test]
async fn selected_end_recomputes_staged_race_before_adoption_without_preparation_admission() {
    use crate::playback_control::ControlStateError;
    let fixture = original_actor_fixture().await;
    let request = selected_end_request();
    let deadline = crate::media_sessions::unix_ms() + 4_000;
    let first = fixture
        .control
        .preparation_requirement_before(
            &request.generation,
            request.control_epoch,
            &request,
            deadline,
        )
        .await
        .expect("pure probe")
        .expect("original selected actor");
    assert!(!first.needed);
    assert!(
        fixture
            .control
            .stage_preparation_for_owner(
                "actual-staged-race-successor".to_owned(),
                request.generation.clone(),
                crate::media_sessions::unix_ms() + 30_000,
                1,
                None,
            )
            .await,
        "stage actual actor slot after probe"
    );
    assert!(
        matches!(
            selected_end_without_preparation(&fixture.control, &request, deadline).await,
            Err(ControlStateError::Unavailable)
        ),
        "missing admission must refuse before End adoption"
    );
    let snapshot = fixture
        .control
        .snapshot()
        .await
        .expect("original actor after race refusal");
    assert_eq!(snapshot.accepted_demand_sequence, None);
    assert!(!snapshot.retired, "missing admission still retired actor");
    assert_eq!(
        snapshot.last_renewal_kind,
        "actual-never-entered-End-fixture"
    );
    drop(snapshot);
    let next = fixture
        .control
        .preparation_requirement_before(
            &request.generation,
            request.control_epoch,
            &request,
            deadline,
        )
        .await
        .expect("recomputed same-actor plan")
        .expect("same selected actor");
    assert!(next.needed, "real staged/End abort directive disappeared");
    fixture.close().await;
}
