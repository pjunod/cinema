//! The real direct factory owns R before its original native open. A missing
//! file after that synchronous factory handoff is not an uninvoked factory.
use super::*;
use crate::transcode::source_actor::SourceFactoryIngressFixture;
use plurx_core::sharing_source_sessions::{
    SourceClaimOutcome, SourceIntentRead, SourceReleaseOutcome, SourceSessionRequest,
    SourceWriteAuthorityRead,
};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

/// Current-thread scheduling is essential here: the actual factory has no
/// await after spawning its worker, so this synchronous rename wins before
/// that worker's first poll. No native result or close proof is injected.
#[tokio::test]
async fn direct_unpublished_open_failure_returns_original_ticket_and_releases_own_r() {
    eprintln!("Direct unpublished fixture: before original fixture future construction");
    let fixture =
        super::super::real_source_start_fixture_with(super::super::SourceFixtureMode::Direct, None)
            .await;
    eprintln!("Direct unpublished fixture: original fixture returned");
    let state = Arc::clone(&fixture.state);
    let manager = Arc::clone(&state.transcode);
    let value: serde_json::Value =
        serde_json::from_slice(&fixture.request).expect("original complete direct request");
    let session: crate::http::hls::CreateSession =
        serde_json::from_value(value["session"].clone()).expect("actual direct session recipe");
    eprintln!("Direct unpublished fixture: before actual Direct preparation");
    let prepared =
        prepare_source_direct(&state, &fixture.headers, fixture.reference.clone(), session)
            .await
            .expect("actual current direct decision/preparation");
    eprintln!("Direct unpublished fixture: actual Direct preparation returned");
    let request_id = prepared.request_id.clone();
    let mut prepared = PreparedSourceStart::Direct(Box::new(prepared));
    let (credential_hash, grant) =
        super::super::current_reference(&state, &fixture.headers, &fixture.reference)
            .await
            .expect("actual current Source grant");
    assert_eq!(grant, fixture.grant);
    let now = crate::state::clock_ms();
    let SourceIntentRead::Ready(intent) = state
        .store
        .prepare_source_session_intent(
            SourceSessionRequest {
                principal: prepared.principal().clone(),
                request_id,
                request_fingerprint: prepared.fingerprint().to_owned(),
                playback_id: prepared.playback_id().to_owned(),
                incarnation_id: uuid::Uuid::new_v4(),
                ingress_registry_boot_id: state.sharing.accepted_drivers.boot_id(),
                now_ms: now,
                claim_expires_at_ms: now + 60_000,
                credential_hash,
                item_id: fixture.reference.item_id.clone(),
                file_id: fixture.reference.file_id.clone(),
                file_revision: fixture.reference.revision.clone(),
            },
            &state.sharing.key,
        )
        .await
        .expect("actual Source direct intent")
    else {
        panic!("real direct intent refused");
    };
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .expect("actual claim members")
        .expect("actual claim floor");
    let SourceClaimOutcome::Acquired(binding) = state
        .store
        .claim_source_media_session(&intent, &members)
        .await
        .expect("actual Source direct claim")
    else {
        panic!("actual direct claim not acquired");
    };
    prepared
        .bind_invocation(&binding)
        .expect("actual complete invocation binding");
    let assignment = state
        .store
        .assign_source_dispatch(&binding, &state.sharing.key, &members)
        .await
        .expect("actual direct dispatch")
        .expect("actual assigned Source worker");
    let ingress = Box::pin(SourceFactoryIngressFixture::new(
        Arc::clone(&state),
        &assignment,
    ))
    .await;
    let permission = ingress.permission().await;
    let members = state
        .membership
        .observe_source_admission_members()
        .await
        .expect("actual factory members")
        .expect("actual factory floor");
    let SourceWriteAuthorityRead::Ready(activation) = state
        .store
        .prepare_source_activation_authority(&assignment, &state.sharing.key, &members)
        .await
        .expect("actual direct activation authority")
    else {
        panic!("actual activation authority refused");
    };
    let PreparedSourceStart::Direct(prepared) = prepared else {
        panic!("fixture must keep its actual direct decision");
    };
    let original_path = prepared.source_file().path.clone();
    let missing_path = original_path.with_extension("unpublished-original");
    let runtime = state
        .resource_runtime
        .as_ref()
        .expect("real direct runtime");
    assert_eq!(
        runtime
            .accounting()
            .expect("real pre-factory accounting")
            .used,
        plurx_core::adaptive_resources::ResourceVector::default()
    );
    eprintln!("Direct unpublished fixture: before original Direct factory");
    let actor = manager
        .start_source_direct_worker(
            Arc::clone(&state),
            assignment.clone(),
            *activation,
            permission,
            *prepared,
            Instant::now() + Duration::from_secs(30),
        )
        .await
        .expect("actual admitted direct factory");
    // NO await between the factory return and this real filesystem failure.
    std::fs::rename(&original_path, &missing_path).expect("remove original path before owner poll");
    eprintln!("Direct unpublished fixture: original factory returned and source renamed before first poll");
    let charged = runtime.accounting().expect("original pre-open R").used;
    assert!(charged.handles > 0 && charged.peak_memory_bytes > 0);
    assert_eq!(manager.fixture_source_direct_restart_custody(), (1, 0, 0));
    let started = tokio::time::timeout(
        Duration::from_secs(30),
        actor.open_direct_start(Instant::now() + Duration::from_secs(30)),
    )
    .await
    .expect("FAILURE watchdog: actual missing-file open did not return");
    assert!(matches!(
        started,
        Err(
            crate::transcode::source_actor::SourceWorkerError::Unresolved
                | crate::transcode::source_actor::SourceWorkerError::Unavailable
        )
    ));
    let retired = tokio::time::timeout(Duration::from_secs(30), actor.retire())
        .await
        .expect("FAILURE watchdog: actual unpublished owner failed to retire");
    assert_eq!(
        retired,
        Err(crate::transcode::source_actor::SourceWorkerError::Unresolved)
    );
    assert!(state
        .store
        .media_session_route_by_incarnation(&assignment.binding().incarnation_id().to_string(),)
        .await
        .expect("actual unpublished route read")
        .is_none());
    assert_eq!(
        runtime
            .accounting()
            .expect("same R before original worker handoff")
            .used,
        charged
    );
    assert_eq!(
        state
            .serving
            .restart_drain_status(0)
            .await
            .admissions_in_flight,
        1
    );
    ingress.close().await;

    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            // Only the actual positive original worker join can install its
            // real native-closed ticket. No DTO or finished bit creates it.
            manager.reconcile_source_direct_tasks();
            let custody = manager.fixture_source_direct_restart_custody();
            if custody.1 == 1 {
                assert_eq!(custody.0, 0);
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("FAILURE watchdog: original unpublished ticket was stranded");
    assert_eq!(
        state
            .serving
            .restart_drain_status(0)
            .await
            .admissions_in_flight,
        1,
        "native ticket handoff alone is not the final joined settlement"
    );
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let custody = manager.fixture_source_direct_restart_custody();
            assert_eq!(custody.0, 0);
            assert_eq!(custody.1, 1);
            if custody.2 == 1 {
                manager.reconcile_source_direct_tasks();
                assert_eq!(manager.fixture_source_direct_restart_custody(), (0, 0, 0));
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("FAILURE watchdog: same unpublished ticket did not finish typed retirement");
    assert_eq!(
        runtime.accounting().expect("actual typed R refund").used,
        plurx_core::adaptive_resources::ResourceVector::default()
    );
    drop(actor);
    let status = state.serving.restart_drain_status(0).await;
    assert_eq!(status.admissions_in_flight, 0);
    assert!(
        status.drained,
        "known original closure must not poison restart uncertainty"
    );
    assert_eq!(
        state
            .store
            .settle_source_assigned_without_activation(&assignment)
            .await
            .expect("real Source SQL cleanup remains separate from direct R"),
        SourceReleaseOutcome::Released
    );
    std::fs::rename(&missing_path, &original_path).expect("restore source after actual retirement");
    drop(manager);
    drop(state);
    fixture.shutdown().await;
}
