use super::*;

#[test]
fn sharing_route_decoder_preserves_namespace_and_refuses_corrupt_projection() {
    let conn = rusqlite::Connection::open_in_memory().expect("fixture connection");
    let grant = uuid::Uuid::new_v4().to_string();
    let viewer = "a".repeat(64);
    let key = format!("share:{grant}:{viewer}");
    let decode = |kind: &str,
                  user: Option<i64>,
                  grant: Option<&str>,
                  viewer: Option<&str>,
                  key: &str| {
        conn.query_row("SELECT 'incarnation' AS incarnation_id, 'session' AS session_id, ?2 AS user_id, 'playback' AS playback_id, 'fingerprint' AS request_fingerprint, 'node' AS owner_node_id, 1 AS owner_epoch, 2000 AS lease_expires_at_ms, 'active' AS state, NULL AS terminal_reason, 1000 AS publication_ready_at_ms, '{}' AS recipe_json, '{}' AS response_json, 0 AS produced_playable_through_ms, 0 AS fetched_through_ms, 0 AS media_origin_ms, 0 AS media_sequence, 0 AS discontinuity_sequence, 1000 AS updated_at_ms, 'recovery' AS recovery_epoch, NULL AS drain_deadline_ms, ?5 AS owner_key, ?1 AS principal_kind, ?3 AS share_grant_id, ?4 AS share_viewer_key", rusqlite::params![kind, user, grant, viewer, key], |row| Ok(decode_route(&mut Row::Borrowed(row)))).expect("SQL projection")
    };
    let local = decode("local", Some(7), None, None, "local:7").expect("local route");
    assert_eq!(local.principal.local_user_id(), Some(7));
    let shared = decode("sharing", None, Some(&grant), Some(&viewer), &key).expect("shared route");
    assert_eq!(
        shared.principal,
        crate::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str(&grant).expect("grant"),
            &viewer
        )
        .expect("principal")
    );
    assert_eq!(shared.principal.local_user_id(), None);
    assert!(decode("sharing", Some(7), Some(&grant), Some(&viewer), &key).is_err());
    assert!(decode("sharing", None, Some(&grant), None, &key).is_err());
    assert!(decode("sharing", None, Some(&grant), Some(&viewer), "local:7").is_err());
    assert!(decode("local", Some(0), None, None, "local:0").is_err());
    assert!(decode("local", Some(-1), None, None, "local:-1").is_err());
    assert!(decode("local", Some(7), None, None, "local:07").is_err());
}

#[test]
fn sharing_route_owned_decoder_refuses_missing_or_wrongly_typed_metadata() {
    let columns = serde_json::json!([
        {"name":"principal_kind","value":{"Text":"sharing"}},
        {"name":"user_id","value":"Null"},
        {"name":"share_grant_id","value":{"Text":uuid::Uuid::new_v4().to_string()}},
        {"name":"share_viewer_key","value":{"Text":"a".repeat(64)}}
    ]);
    let mut missing = Row::Owned(
        serde_json::from_value(serde_json::json!({"columns":columns})).expect("owned row"),
    );
    assert!(decode_route(&mut missing).is_err());
    let mut wrong = Row::Owned(
        serde_json::from_value(
            serde_json::json!({"columns":[{"name":"principal_kind","value":{"Integer":1}}]}),
        )
        .expect("owned row"),
    );
    assert!(decode_route(&mut wrong).is_err());
}

#[test]
fn sharing_request_decoder_preserves_complete_principal_and_refuses_mixed_ownership() {
    let conn = rusqlite::Connection::open_in_memory().expect("fixture connection");
    let grant = "00000000-0000-4000-a000-000000000001";
    let viewer = "a".repeat(64);
    let key = format!("share:{grant}:{viewer}");
    let decode = |kind: &str,
                  user: Option<i64>,
                  grant: Option<&str>,
                  viewer: Option<&str>,
                  key: &str| {
        conn.query_row("SELECT 'fingerprint' AS request_fingerprint, 'playback' AS playback_id, 'starting' AS state, 'incarnation' AS incarnation_id, NULL AS owner_node_id, 1000 AS claim_expires_at_ms, ?1 AS principal_kind, ?2 AS user_id, ?3 AS share_grant_id, ?4 AS share_viewer_key, ?5 AS owner_key", rusqlite::params![kind, user, grant, viewer, key], |row| Ok(decode_request(&mut Row::Borrowed(row)))).expect("SQL request projection")
    };
    assert_eq!(
        decode("local", Some(7), None, None, "local:7")
            .expect("local")
            .principal
            .local_user_id(),
        Some(7)
    );
    let shared = decode("sharing", None, Some(grant), Some(&viewer), &key).expect("sharing");
    assert_eq!(
        shared.principal,
        crate::playback_principal::PlaybackPrincipal::sharing(
            uuid::Uuid::parse_str(grant).expect("grant UUID"),
            &viewer
        )
        .expect("principal")
    );
    assert!(decode("sharing", Some(7), Some(grant), Some(&viewer), &key).is_err());
    assert!(decode("sharing", None, Some(grant), None, &key).is_err());
    assert!(decode("sharing", None, Some(grant), Some(&viewer), "local:7").is_err());
    assert!(decode("local", Some(0), None, None, "local:0").is_err());
    assert!(decode("local", Some(7), None, None, "local:07").is_err());
}

#[test]
fn sharing_request_owned_decoder_returns_errors_for_incomplete_and_wrong_type_rows() {
    for columns in [
        serde_json::json!([]),
        serde_json::json!([{"name":"principal_kind","value":{"Integer":1}}]),
        serde_json::json!([
            {"name":"principal_kind","value":{"Text":"local"}},
            {"name":"user_id","value":{"Integer":7}},
            {"name":"owner_key","value":{"Text":"local:7"}},
            {"name":"share_grant_id","value":"Null"},
            {"name":"share_viewer_key","value":"Null"}
        ]),
    ] {
        let mut row = Row::Owned(
            serde_json::from_value(serde_json::json!({"columns":columns})).expect("owned row"),
        );
        assert!(decode_request(&mut row).is_err());
    }
}
#[test]
fn sharing_inventory_and_staged_decoders_refuse_incomplete_owned_rows() {
    for columns in [
        serde_json::json!([]),
        serde_json::json!([{"name":"principal_kind","value":{"Integer":1}}]),
        serde_json::json!([
            {"name":"principal_kind","value":{"Text":"local"}},
            {"name":"user_id","value":{"Integer":7}},
            {"name":"owner_key","value":{"Text":"local:7"}},
            {"name":"share_grant_id","value":"Null"},
            {"name":"share_viewer_key","value":"Null"}
        ]),
    ] {
        let make_row = || {
            Row::Owned(
                serde_json::from_value(serde_json::json!({"columns":columns})).expect("owned row"),
            )
        };
        assert!(OwnedLeaseRow::from(&mut make_row()).0.is_err());
        assert!(StagedRow::from(&mut make_row()).0.is_err());
        assert!(PrincipalDesiredRow::from(&mut make_row()).0.is_err());
        assert!(RecoveryRow::from(&mut make_row()).0.is_err());
    }
}
