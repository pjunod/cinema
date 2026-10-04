//! Qualification-only code compiled into an archived, unchanged old Store.
use plurx_core::store::MediaSessionStore;

pub async fn exercise_old_store(store: &dyn MediaSessionStore, backend: &str, rebuilt: bool) {
    let stage = if rebuilt { "candidate" } else { "baseline" };
    let route = store
        .media_session_route_by_incarnation("00000000-0000-4000-a000-000000000072")
        .await
        .expect("old named-column route read")
        .expect("retained local route");
    assert_eq!(route.user_id, 1);
    assert_eq!(route.incarnation_id, "00000000-0000-4000-a000-000000000072");
    let pointer = store
        .media_session_route_for_playback(1, "playback")
        .await
        .expect("old pointer read")
        .expect("retained pointer");
    assert_eq!(
        pointer.incarnation_id,
        "00000000-0000-4000-a000-000000000072"
    );
    let owned = store
        .owned_media_sessions("node", 100)
        .await
        .expect("old owner inventory/count");
    assert_eq!(owned.len(), 1);
    let expired = store
        .expired_media_sessions(9500, None, 10)
        .await
        .expect("old expiry inventory/count");
    assert_eq!(expired.len(), 1);
    eprintln!("OLD-STORE {backend} {stage}: route=PASS pointer=PASS owner-count=1 expiry-count=1");

    let insert = store
        .claim_media_session_request(
            1,
            "probe-insert",
            &"c".repeat(64),
            "probe-playback",
            "00000000-0000-4000-a000-000000000070",
            100,
            9000,
        )
        .await;
    let upsert = store
        .record_desired_selection(1, "probe-desired", &"d".repeat(64), "v1;quality=auto", 100)
        .await;
    if rebuilt {
        let insert_error = insert
            .expect_err("old request insert must refuse missing owner_key")
            .to_string();
        let upsert_error = upsert
            .expect_err("old desired upsert must refuse obsolete conflict target")
            .to_string();
        let expected_insert_error = if backend == "hiqlite-three-voters" {
            // The old replicated insert itself contains the old conflict target.
            insert_error.contains("ON CONFLICT") || insert_error.contains("conflict")
        } else {
            insert_error.contains("owner_key")
        };
        assert!(
            expected_insert_error,
            "unexpected insert error: {insert_error}"
        );
        assert!(
            upsert_error.contains("ON CONFLICT") || upsert_error.contains("conflict"),
            "unexpected upsert error: {upsert_error}"
        );
        eprintln!("OLD-STORE {backend} {stage}: insert=INCOMPATIBLE {insert_error}");
        eprintln!("OLD-STORE {backend} {stage}: upsert=INCOMPATIBLE {upsert_error}");
    } else {
        insert.expect("old request insert works on baseline");
        let desired = upsert.expect("old desired upsert works on baseline");
        assert_eq!(desired.revision, 1);
        let updated = store
            .record_desired_selection(
                1,
                "probe-desired",
                &"e".repeat(64),
                "v1;quality=original",
                110,
            )
            .await
            .expect("old existing-row upsert works on baseline");
        assert_eq!(updated.revision, 2);
        eprintln!("OLD-STORE {backend} {stage}: insert=PASS upsert=PASS");
    }
    let ended = store
        .end_media_session("00000000-0000-4000-a000-000000000071", "admin_stop", 200)
        .await
        .expect("old terminal cleanup")
        .expect("old cleanup finds retained local row");
    assert_eq!(ended.state, "ended");
    store
        .maintain_media_sessions(10000)
        .await
        .expect("old maintenance cleanup");
    assert!(store
        .owned_media_sessions("node", 10000)
        .await
        .expect("post-cleanup count")
        .is_empty());
    eprintln!("OLD-STORE {backend} {stage}: terminal-cleanup=PASS maintenance=PASS remaining-owner-count=0");
}
