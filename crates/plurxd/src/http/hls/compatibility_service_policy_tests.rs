use super::*;
#[test]
fn compatibility_bitrate_policy_is_server_owned_and_changes_exact_claim_identity() {
    let body: CreateSession = serde_json::from_value(serde_json::json!({"playback_id":"compatibility-player", "request_id":"compatibility-request", "finite_bitrate_limit_bps":750000})).expect("native create body");
    let mut request = body.into_request(1, 720);
    assert_eq!(
        request.finite_bitrate_limit_bps, None,
        "ordinary HTTP cannot set the trusted policy"
    );
    let first = bind_compatibility_vod_policy(&mut request, "native-intent".into(), Some(750_000))
        .expect("service policy");
    assert_eq!(request.finite_bitrate_limit_bps, Some(750_000));
    assert_eq!(
        first,
        bind_compatibility_vod_policy(&mut request, "native-intent".into(), Some(750_000))
            .expect("same attempt")
    );
    assert_ne!(
        first,
        bind_compatibility_vod_policy(&mut request, "native-intent".into(), Some(500_000))
            .expect("changed ceiling")
    );
    assert!(bind_compatibility_vod_policy(&mut request, "intent".into(), Some(0)).is_err());
}
#[test]
fn compatibility_service_policy_binds_vod_only_and_passive_flags_before_native_claim() {
    let body: CreateSession = serde_json::from_value(serde_json::json!({"playback_id":"compatibility-player", "request_id":"compatibility-request"})).expect("create");
    let mut request = body.into_request(1, 1080);
    let native = ServiceCreatePurpose::Native
        .bind_policy(&mut request, "native-intent".into())
        .expect("native");
    assert_eq!(native, "native-intent");
    assert!(!request.vod_only);
    assert!(!request.passive_vod);
    let expected_fingerprint = bind_compatibility_vod_policy(&mut request, native.clone(), None)
        .expect("negotiated policy");
    let policy = ServiceCreatePurpose::PassiveCompatibility {
        bitrate_limit_bps: None,
        expected_fingerprint,
    };
    let first = policy
        .bind_policy(&mut request, native.clone())
        .expect("compatibility");
    assert_ne!(first, native);
    assert_eq!(
        first,
        policy
            .bind_policy(&mut request, native)
            .expect("stable retry")
    );
    assert!(
        policy
            .bind_policy(&mut request, "changed-native-intent".into())
            .is_err(),
        "changed normalized body is refused before a claim"
    );
    assert!(request.vod_only && request.passive_vod);
    request.request_id = None;
    assert!(policy.bind_policy(&mut request, "intent".into()).is_err());
}
