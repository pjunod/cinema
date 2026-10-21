use super::*;
use plurx_core::{
    resource_settings::{CapacityUpdate, LimitChoice, ReadState, ResourceSettings},
    store::keys,
};

#[tokio::test]
async fn typed_modes_preserve_absence_zero_and_rollback_and_legacy_writes_win() {
    for_each_backend(|store, backend| async move {
        let initial = store
            .resource_capacity_snapshot()
            .await
            .expect("schema read")
            .expect("complete adjunct schema");
        assert!(
            initial.values.is_empty(),
            "{backend}: no resolved per-node defaults persisted"
        );
        let automatic = CapacityUpdate {
            expected_revision: initial.revision,
            hardware_sessions: Some(LimitChoice::Automatic),
            software_pool_threads: Some(LimitChoice::Automatic),
        };
        assert!(
            store
                .put_resource_capacity(&automatic)
                .await
                .expect("select automatic"),
            "{backend}"
        );
        let selected = store
            .resource_capacity_snapshot()
            .await
            .expect("snapshot")
            .expect("schema");
        assert!(
            !selected.values.contains_key(keys::SW_POOL_THREADS),
            "{backend}: preserve absence"
        );
        assert_eq!(
            ResourceSettings::from_snapshot(&selected.values)
                .software_pool_threads
                .state,
            ReadState::Automatic
        );
        assert!(
            !store
                .put_resource_capacity(&automatic)
                .await
                .expect("stale CAS"),
            "{backend}: no stale form overwrite"
        );
        let explicit = CapacityUpdate {
            expected_revision: selected.revision,
            hardware_sessions: Some(LimitChoice::Explicit { value: 0 }),
            software_pool_threads: Some(LimitChoice::Explicit { value: 0 }),
        };
        assert!(store
            .put_resource_capacity(&explicit)
            .await
            .expect("explicit zeros"));
        let zero = store
            .resource_capacity_snapshot()
            .await
            .expect("snapshot")
            .expect("schema");
        let report = ResourceSettings::from_snapshot(&zero.values);
        assert_eq!(report.hardware_sessions.stored_value, Some(0));
        assert_eq!(report.software_pool_threads.stored_value, Some(0));
        assert_eq!(report.hardware_sessions.state, ReadState::Explicit);
        let auto = CapacityUpdate {
            expected_revision: zero.revision,
            ..automatic
        };
        assert!(store
            .put_resource_capacity(&auto)
            .await
            .expect("automatic retains rollback cache"));
        assert_eq!(
            store
                .get_setting(keys::MAX_HW_SESSIONS)
                .await
                .expect("rollback hardware"),
            Some("0".into())
        );
        assert_eq!(
            store
                .get_setting(keys::SW_POOL_THREADS)
                .await
                .expect("rollback software"),
            Some("0".into())
        );
        let before_legacy = store
            .resource_capacity_snapshot()
            .await
            .expect("snapshot")
            .expect("schema");
        // An actual old same-value writer, not a parser-only changed-value test.
        store
            .put_setting(keys::SW_POOL_THREADS, "0")
            .await
            .expect("legacy same-value write");
        let after = store
            .resource_capacity_snapshot()
            .await
            .expect("snapshot")
            .expect("schema");
        assert!(after.revision > before_legacy.revision, "{backend}");
        let report = ResourceSettings::from_snapshot(&after.values);
        assert_eq!(
            report.software_pool_threads.state,
            ReadState::LegacyAmbiguous
        );
        assert_eq!(report.hardware_sessions.state, ReadState::Automatic);
        assert!(!store
            .put_resource_capacity(&CapacityUpdate {
                expected_revision: before_legacy.revision,
                hardware_sessions: Some(LimitChoice::Explicit { value: 8 }),
                software_pool_threads: None
            })
            .await
            .expect("legacy writer fences stale form"));
        assert_eq!(
            store
                .get_setting(keys::MAX_HW_SESSIONS)
                .await
                .expect("unchanged"),
            Some("0".into())
        );
    })
    .await;
}

#[tokio::test]
async fn simultaneous_typed_capacity_forms_commit_only_one_complete_tuple() {
    for_each_backend(|store, backend| async move {
        let initial = store
            .resource_capacity_snapshot()
            .await
            .expect("snapshot")
            .expect("schema");
        let first = CapacityUpdate {
            expected_revision: initial.revision,
            hardware_sessions: Some(LimitChoice::Explicit { value: 1 }),
            software_pool_threads: Some(LimitChoice::Explicit { value: 2 }),
        };
        let second = CapacityUpdate {
            expected_revision: initial.revision,
            hardware_sessions: Some(LimitChoice::Explicit { value: 3 }),
            software_pool_threads: Some(LimitChoice::Explicit { value: 4 }),
        };
        let (a, b) = tokio::join!(
            store.put_resource_capacity(&first),
            store.put_resource_capacity(&second)
        );
        let (a, b) = (a.expect("first form"), b.expect("second form"));
        assert_ne!(a, b, "{backend}: exactly one CAS winner");
        let current = store
            .resource_capacity_snapshot()
            .await
            .expect("snapshot")
            .expect("schema");
        let expected = if a { (1, 2) } else { (3, 4) };
        let report = ResourceSettings::from_snapshot(&current.values);
        assert_eq!(
            (
                report.hardware_sessions.stored_value,
                report.software_pool_threads.stored_value
            ),
            (Some(expected.0), Some(expected.1)),
            "{backend}"
        );
        assert_eq!(report.hardware_sessions.state, ReadState::Explicit);
        assert_eq!(report.software_pool_threads.state, ReadState::Explicit);
    })
    .await;
}
