#![cfg(unix)]
use plurx_desktop_control::update::{parse_trust_roots, production_keys, PRODUCTION_ROOTS_JSON};
use serde_json::json;

#[test]
fn production_signing_root_is_embedded_and_not_a_fixture() {
    let roots = parse_trust_roots(PRODUCTION_ROOTS_JSON.as_bytes()).expect("embedded roots schema");
    assert_eq!(roots.repository, "pjunod/cinema");
    assert!(
        !roots.keys.is_empty(),
        "production updates need an approved public root"
    );
    let keys = production_keys().expect("production keys");
    assert_eq!(keys.len(), roots.keys.len());
    for (root, key) in roots.keys.iter().zip(keys.iter()) {
        assert_eq!(root.id, key.id);
        assert_eq!(root.public_key_hex, hex::encode(key.public_key));
        assert_ne!(
            root.public_key_hex,
            "ab".repeat(32),
            "no fixture key may be pinned"
        );
    }
}
#[test]
fn trust_roots_reject_wrong_repository_duplicates_and_malformed_keys() {
    let key = json!({"id":"release-key", "public_key_hex":"ab".repeat(32)});
    let valid = json!({"schema_version":1,"repository":"pjunod/cinema","keys":[key.clone()]});
    assert!(parse_trust_roots(&serde_json::to_vec(&valid).expect("root fixture")).is_ok());
    for case in 0..7 {
        let mut v = valid.clone();
        match case {
            0 => v["repository"] = json!("attacker/cinema"),
            1 => v["schema_version"] = json!(2),
            2 => v["keys"] = json!([key.clone(), key.clone()]),
            3 => v["keys"][0]["public_key_hex"] = json!("AB".repeat(32)),
            4 => v["keys"][0]["public_key_hex"] = json!("00".repeat(32)),
            5 => v["keys"][0]["id"] = json!("bad/key"),
            _ => v["unknown"] = json!(true),
        }
        assert!(
            parse_trust_roots(&serde_json::to_vec(&v).expect("invalid root fixture")).is_err(),
            "case {case}"
        );
    }
}
#[test]
#[allow(
    clippy::disallowed_methods,
    reason = "Executes only the finite test-owned CLI; no daemon or activity store"
)]
fn trust_info_reports_exact_candidate_roots_without_installation() {
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_plurx-control"))
        .arg("trust-info")
        .env_clear()
        .output()
        .expect("candidate CLI");
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let actual: serde_json::Value = serde_json::from_slice(&output.stdout).expect("finite JSON");
    let expected: serde_json::Value =
        serde_json::from_str(PRODUCTION_ROOTS_JSON).expect("embedded roots JSON");
    assert_eq!(actual, expected);
}
