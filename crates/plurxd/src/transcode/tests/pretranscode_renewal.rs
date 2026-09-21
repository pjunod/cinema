
use super::*;

fn job(expires: i64) -> PretranscodeJob {
    PretranscodeJob {
        id: "00000000-0000-4000-8000-000000000301".to_owned(),
        dedupe_key: "renewal-boundary".to_owned(),
        file_id: 1,
        source_size: 1,
        source_mtime: 1,
        target_height: 720,
        policy_generation: "contract-v1".to_owned(),
        requirements_json: "{}".to_owned(),
        reason: "recent".to_owned(),
        priority: 1,
        state: "running".to_owned(),
        owner_node_id: "node-a".to_owned(),
        fence: 7,
        lease_expires_ms: expires,
        attempts: 0,
        not_before_ms: 0,
        created_at_ms: 0,
        updated_at_ms: 0,
    }
}

#[test]
fn delayed_renewal_response_cannot_restore_expired_authority() {
    let previous = job(1_000);
    let replacement = job(2_000);
    assert!(renewal_response_is_authoritative(
        &previous,
        &replacement,
        999
    ));
    assert!(!renewal_response_is_authoritative(
        &previous,
        &replacement,
        1_000
    ));
    assert!(!renewal_response_is_authoritative(
        &previous,
        &replacement,
        1_001
    ));
}
