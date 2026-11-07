use super::*;
use crate::{
    installation::REPOSITORY,
    update::{
        select_cohort, verify_accepted_installed_manifest, verify_manifest, AcceptedReleasesV1,
        Component, TrustKey,
    },
};
use ring::{
    rand::SystemRandom,
    signature::{Ed25519KeyPair, KeyPair},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const NOW: u64 = 1_500;
pub(crate) fn installation() -> InstallationV1 {
    serde_json::from_value(json!({
        "schema_version":1,"installation_id":"00000000-0000-4000-8000-000000000001",
        "method":"linux-deb","owner_uid":0,"service_uid":995,"architecture":"amd64",
        "desktop_installed":false,"repository":REPOSITORY,"current_version":"0.7.0",
        "current_tag":"v0.7.0","current_commit":"a".repeat(40),"controller_protocol":1,
        "trust_key_ids":["fixture-only"],"last_transaction_id":null,
        "paths":{"state":"/var/lib/plurx-control","socket":"/run/plurx/desktop-control.sock",
        "runtime":"/usr/lib/plurx","config":"/etc/plurx/plurx.toml","data":"/var/lib/plurx",
        "log":"/var/log/plurxd.log","canonical_plist":null,"login_plist":null},
        "service":{"kind":"systemd","name":"plurxd.service","domain":"system"},
        "browser_origin":"http://127.0.0.1:32400"
    }))
    .expect("ordinary installed fixture")
}
fn build(target: &str) -> MigrationBuildV1 {
    MigrationBuildV1 {
        commit: "b".repeat(40),
        tree: "c".repeat(40),
        target: target.into(),
    }
}
fn cohorts() -> Vec<MigrationSourceCohortV1> {
    vec![
        MigrationSourceCohortV1 {
            auth_version: 81,
            canonical_layout_fingerprint: "e".repeat(64),
        },
        MigrationSourceCohortV1 {
            auth_version: 82,
            canonical_layout_fingerprint: "f".repeat(64),
        },
    ]
}
pub(crate) fn helper(target: &str) -> MigrationHelperV1 {
    let build = build(target);
    let (platform, architecture) = build_target(&build).expect("fixture target");
    let bundle = hex::encode(Sha256::digest(b"separately selected helper archive"));
    MigrationHelperV1 {
        artifact: MigrationHelperArtifactV1 {
            name: format!("plurx-storage-migrate-0.7.10-{target}.tar.gz"),
            platform,
            architecture,
            format: Format::TarGz,
            size: 33,
            sha256: bundle.clone(),
        },
        daemon_build: build.clone(),
        helper_build: build,
        helper_version: "0.7.10".into(),
        profile: MigrationHelperProfile::StorageMigration,
        executable_name: "plurx-storage-migrate".into(),
        executable_sha256: hex::encode(Sha256::digest(b"actual helper executable fixture")),
        bundle_sha256: bundle,
        helper_protocol: 1,
        request_format: 1,
        receipt_format: 1,
        source_cohorts: cohorts(),
        output: MigrationOutputV1 {
            native_schema_version: 124,
            manifest_format: 1,
            transition_format: 1,
        },
        converter_sha256: "1".repeat(64),
    }
}
pub(crate) fn manifest() -> UpdateManifestV1 {
    let mut artifacts = Vec::new();
    for architecture in [Architecture::Amd64, Architecture::Arm64] {
        for format in [Format::Deb, Format::Rpm] {
            for (component, package) in [
                (Component::Server, "plurxd"),
                (Component::Desktop, "plurx-desktop"),
            ] {
                let extension = if format == Format::Deb { "deb" } else { "rpm" };
                artifacts.push(ArtifactV1 {
                    name: format!("{package}-0.7.10-{architecture:?}.{extension}"),
                    platform: Platform::Linux,
                    architecture: architecture.clone(),
                    format,
                    component,
                    package_name: package.into(),
                    package_version: "0.7.10".into(),
                    size: 7,
                    sha256: "2".repeat(64),
                });
            }
        }
    }
    artifacts.push(ArtifactV1 {
        name: "plurx-native-0.7.10-universal.tar.gz".into(),
        platform: Platform::Macos,
        architecture: Architecture::Universal,
        format: Format::TarGz,
        component: Component::NativeRuntime,
        package_name: "plurx-native".into(),
        package_version: "0.7.10".into(),
        size: 7,
        sha256: "3".repeat(64),
    });
    UpdateManifestV1 {
        schema_version: 1,
        repository: REPOSITORY.into(),
        channel: "stable".into(),
        tag: "v0.7.10".into(),
        version: "0.7.10".into(),
        sequence: 10,
        source_commit: "b".repeat(40),
        source_tree: "c".repeat(40),
        packaging_commit: "d".repeat(40),
        created_unix: 1_000,
        expires_unix: 2_000,
        upgrade_from: vec![">=0.7.0, <0.7.10".into()],
        data_compatibility: "native-schema-124".into(),
        cluster_compatibility: "auth-81-82".into(),
        minimum_controller: "0.7.0".into(),
        rollback: Rollback::ManualRecovery,
        release_notes_url: format!("https://github.com/{REPOSITORY}/releases/tag/v0.7.10"),
        artifacts,
    }
}
fn v2() -> Value {
    let mut value = serde_json::to_value(manifest()).expect("base DTO");
    value["schema_version"] = json!(2);
    value["migration_helpers"] = json!([helper("x86_64-unknown-linux-gnu")]);
    value
}
pub(crate) fn signer() -> (Ed25519KeyPair, Vec<TrustKey>) {
    let key = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("ephemeral fixture key");
    let pair = Ed25519KeyPair::from_pkcs8(key.as_ref()).expect("fixture key encoding");
    let mut public_key = [0; 32];
    public_key.copy_from_slice(pair.public_key().as_ref());
    (
        pair,
        vec![TrustKey {
            id: "fixture-only".into(),
            public_key,
        }],
    )
}
fn signed(value: &Value) -> (Vec<u8>, Vec<u8>, Vec<TrustKey>) {
    let (pair, keys) = signer();
    let bytes = serde_json::to_vec(value).expect("exact fixture manifest bytes");
    let signature = pair.sign(&bytes).as_ref().to_vec();
    (bytes, signature, keys)
}
fn verify(value: &Value) -> Result<VerifiedManifest> {
    let (bytes, signature, keys) = signed(value);
    verify_manifest(
        &bytes,
        &signature,
        &keys,
        &installation(),
        &AcceptedReleasesV1::default(),
        NOW,
    )
}

#[test]
fn v1_nine_asset_cohort_stays_exact_and_rejects_v2_fields() {
    let value = serde_json::to_value(manifest()).expect("V1 fixture");
    let verified = verify(&value).expect("unchanged signed V1 cohort");
    assert_eq!(verified.manifest().artifacts.len(), 9);
    assert_eq!(
        select_cohort(&verified, &installation())
            .expect("default server")
            .len(),
        1
    );
    for helper_value in [
        Value::Null,
        json!([]),
        json!([helper("x86_64-unknown-linux-gnu")]),
    ] {
        let mut extended = value.clone();
        extended["migration_helpers"] = helper_value;
        assert!(
            verify(&extended).is_err(),
            "V1 cannot silently acquire V2 fields"
        );
    }
    assert!(select_migration_helper(
        &verified,
        &installation(),
        MigrationHelperPurpose::LegacyDataUpgrade,
        &build("x86_64-unknown-linux-gnu"),
        &cohorts()[0]
    )
    .is_err());
}

#[test]
fn signed_v2_helpers_are_explicit_and_bound_to_the_actual_release_tuple() {
    let verified = verify(&v2()).expect("authenticated V2");
    assert_eq!(verified.manifest().artifacts.len(), 9);
    let default = select_cohort(&verified, &installation()).expect("unchanged default install");
    assert_eq!(default.len(), 1);
    assert_eq!(default[0].package_name, "plurxd");
    assert!(!default.iter().any(|a| a.name.contains("storage-migrate")));
    for purpose in [
        MigrationHelperPurpose::LegacyDataUpgrade,
        MigrationHelperPurpose::ExplicitTransition,
    ] {
        let selected = select_migration_helper(
            &verified,
            &installation(),
            purpose,
            &build("x86_64-unknown-linux-gnu"),
            &cohorts()[0],
        )
        .expect("checked separate selection");
        assert_eq!(selected.purpose(), purpose);
        assert_eq!(selected.bundle_sha256(), selected.artifact().sha256);
        assert_eq!(selected.manifest_digest(), verified.digest());
        assert_eq!(selected.key_id(), "fixture-only");
        assert_eq!(selected.release_tag(), "v0.7.10");
        assert_eq!(selected.helper_build(), selected.daemon_build());
        assert_eq!(
            selected.executable_sha256(),
            helper("x86_64-unknown-linux-gnu").executable_sha256
        );
        assert_eq!(selected.converter_sha256(), "1".repeat(64));
        assert_eq!(selected.output().manifest_format, 1);
        assert_eq!(selected.source_cohort(), &cohorts()[0]);
    }
}

#[test]
fn v2_optional_missing_helper_refuses_only_explicit_helper_selection() {
    let mut value = v2();
    value
        .as_object_mut()
        .expect("object")
        .remove("migration_helpers");
    let verified = verify(&value).expect("standalone cohort needs no helper");
    assert_eq!(
        select_cohort(&verified, &installation())
            .expect("default cohort")
            .len(),
        1
    );
    assert!(select_migration_helper(
        &verified,
        &installation(),
        MigrationHelperPurpose::LegacyDataUpgrade,
        &build("x86_64-unknown-linux-gnu"),
        &cohorts()[0]
    )
    .is_err());
    value["migration_helpers"] = json!([]);
    assert!(
        verify(&value).is_err(),
        "an advertised helper cohort cannot be empty"
    );
}

#[test]
fn v2_signed_wrong_build_profile_protocol_platform_and_unknown_cohorts_refuse() {
    let cases = [
        (
            "/migration_helpers/0/helper_build/commit",
            json!("a".repeat(40)),
        ),
        (
            "/migration_helpers/0/daemon_build/tree",
            json!("d".repeat(40)),
        ),
        ("/migration_helpers/0/helper_version", json!("0.7.9")),
        ("/migration_helpers/0/profile", json!("standalone")),
        ("/migration_helpers/0/artifact/platform", json!("macos")),
        ("/migration_helpers/0/artifact/architecture", json!("arm64")),
        ("/migration_helpers/0/artifact/format", json!("deb")),
        ("/migration_helpers/0/bundle_sha256", json!("0".repeat(64))),
        (
            "/migration_helpers/0/executable_name",
            json!("../plurx-storage-migrate"),
        ),
        ("/migration_helpers/0/helper_protocol", json!(2)),
        ("/migration_helpers/0/request_format", json!(2)),
        ("/migration_helpers/0/receipt_format", json!(2)),
        (
            "/migration_helpers/0/source_cohorts/1/auth_version",
            json!(83),
        ),
        (
            "/migration_helpers/0/source_cohorts/0/canonical_layout_fingerprint",
            json!("not-a-canonical-hash"),
        ),
        ("/migration_helpers/0/output/manifest_format", json!(2)),
        ("/migration_helpers/0/output/transition_format", json!(2)),
        (
            "/migration_helpers/0/output/native_schema_version",
            json!(0),
        ),
    ];
    for (pointer, replacement) in cases {
        let mut value = v2();
        *value.pointer_mut(pointer).expect("existing fixture field") = replacement;
        assert!(
            verify(&value).is_err(),
            "{pointer} must refuse despite a valid new signature"
        );
    }
    let mut value = v2();
    value["migration_helpers"][0]["caller_verified"] = json!(true);
    assert!(
        verify(&value).is_err(),
        "unknown tuple assertions never gain authority"
    );
    let mut value = v2();
    value["unknown_root_field"] = json!(true);
    assert!(verify(&value).is_err());
}

#[test]
fn duplicate_or_cross_cohort_asset_identity_and_wrong_selection_refuse() {
    let mut value = v2();
    let mut duplicate = value["migration_helpers"][0].clone();
    duplicate["artifact"]["name"] = json!("same-target-different-name.tar.gz");
    value["migration_helpers"]
        .as_array_mut()
        .expect("list")
        .push(duplicate);
    assert!(
        verify(&value).is_err(),
        "duplicate target is ambiguous even with distinct names"
    );
    let mut value = v2();
    value["migration_helpers"][0]["artifact"]["name"] = value["artifacts"][8]["name"].clone();
    assert!(
        verify(&value).is_err(),
        "helper and ordinary install namespaces cannot collide"
    );
    let verified = verify(&v2()).expect("signed helper");
    let mut wrong = build("x86_64-unknown-linux-gnu");
    wrong.commit = "a".repeat(40);
    assert!(select_migration_helper(
        &verified,
        &installation(),
        MigrationHelperPurpose::ExplicitTransition,
        &wrong,
        &cohorts()[0]
    )
    .is_err());
    let mut wrong = cohorts()[0].clone();
    wrong.canonical_layout_fingerprint = "0".repeat(64);
    assert!(select_migration_helper(
        &verified,
        &installation(),
        MigrationHelperPurpose::ExplicitTransition,
        &build("x86_64-unknown-linux-gnu"),
        &wrong
    )
    .is_err());
    let mut wrong_install = installation();
    wrong_install.architecture = Architecture::Arm64;
    assert!(select_migration_helper(
        &verified,
        &wrong_install,
        MigrationHelperPurpose::ExplicitTransition,
        &build("x86_64-unknown-linux-gnu"),
        &cohorts()[0]
    )
    .is_err());
}

#[test]
fn universal_native_default_cohort_selects_only_the_exact_helper_slice() {
    let mut value = v2();
    value["migration_helpers"] = json!([helper("aarch64-apple-darwin")]);
    let mut installed = installation();
    installed.method = InstallMethod::MacosNative;
    installed.architecture = Architecture::Universal;
    let (bytes, signature, keys) = signed(&value);
    let verified = verify_manifest(
        &bytes,
        &signature,
        &keys,
        &installed,
        &AcceptedReleasesV1::default(),
        NOW,
    )
    .expect("signed universal native cohort");
    assert_eq!(
        select_cohort(&verified, &installed).expect("native cohort")[0].architecture,
        Architecture::Universal
    );
    assert!(select_migration_helper(
        &verified,
        &installed,
        MigrationHelperPurpose::ExplicitTransition,
        &build("aarch64-apple-darwin"),
        &cohorts()[0]
    )
    .is_ok());
    assert!(select_migration_helper(
        &verified,
        &installed,
        MigrationHelperPurpose::ExplicitTransition,
        &build("x86_64-apple-darwin"),
        &cohorts()[0]
    )
    .is_err());
}

#[test]
fn accepted_installed_manifest_requires_exact_highwater_digest_and_actual_daemon() {
    let value = v2();
    let (bytes, signature, keys) = signed(&value);
    let mut installed = installation();
    let mut ledger = AcceptedReleasesV1::default();
    let upgrade = verify_manifest(&bytes, &signature, &keys, &installed, &ledger, NOW)
        .expect("actual signed upgrade");
    upgrade.accept(&mut ledger);
    installed.current_version = "0.7.10".into();
    installed.current_tag = "v0.7.10".into();
    installed.current_commit = "b".repeat(40);
    assert!(
        verify_manifest(&bytes, &signature, &keys, &installed, &ledger, NOW).is_err(),
        "upgrade API still refuses same-version replay"
    );
    let actual = build("x86_64-unknown-linux-gnu");
    let accepted = verify_accepted_installed_manifest(
        &bytes, &signature, &keys, &installed, &ledger, NOW, &actual,
    )
    .expect("exact accepted installed release");
    assert_eq!(accepted.digest(), upgrade.digest());
    assert!(select_migration_helper(
        &accepted,
        &installed,
        MigrationHelperPurpose::LegacyDataUpgrade,
        &actual,
        &cohorts()[0]
    )
    .is_ok());
    assert!(
        verify_accepted_installed_manifest(
            &bytes,
            &signature,
            &keys,
            &installed,
            &AcceptedReleasesV1::default(),
            NOW,
            &actual
        )
        .is_err(),
        "signature alone is not past acceptance"
    );
    let mut wrong = actual.clone();
    wrong.tree = "d".repeat(40);
    assert!(
        verify_accepted_installed_manifest(
            &bytes, &signature, &keys, &installed, &ledger, NOW, &wrong
        )
        .is_err(),
        "same version/wrong binary refuses"
    );
    let mut newer = ledger.clone();
    newer.highest_sequence += 1;
    assert!(
        verify_accepted_installed_manifest(
            &bytes, &signature, &keys, &installed, &newer, NOW, &actual
        )
        .is_ok(),
        "offering a newer update must not revoke the exact accepted installed build"
    );
    let mut too_old = ledger.clone();
    too_old.highest_sequence -= 1;
    assert!(
        verify_accepted_installed_manifest(
            &bytes, &signature, &keys, &installed, &too_old, NOW, &actual
        )
        .is_err(),
        "a signed sequence above the accepted highwater is not installed acceptance"
    );
    let mut older_digest = ledger.clone();
    older_digest
        .manifests
        .insert(installed.current_tag.clone(), "0".repeat(64));
    assert!(
        verify_accepted_installed_manifest(
            &bytes,
            &signature,
            &keys,
            &installed,
            &older_digest,
            NOW,
            &actual
        )
        .is_err(),
        "a different ledger digest cannot authorize the installed manifest"
    );
    let mut untrusted_install = installed.clone();
    untrusted_install.trust_key_ids.clear();
    assert!(verify_accepted_installed_manifest(
        &bytes,
        &signature,
        &keys,
        &untrusted_install,
        &ledger,
        NOW,
        &actual
    )
    .is_err());
}

#[test]
fn accepted_installed_manifest_rejects_mutated_tuple_and_unknown_signed_bytes() {
    let value = v2();
    let (bytes, signature, keys) = signed(&value);
    let mut installed = installation();
    let mut ledger = AcceptedReleasesV1::default();
    verify_manifest(&bytes, &signature, &keys, &installed, &ledger, NOW)
        .expect("upgrade")
        .accept(&mut ledger);
    installed.current_version = "0.7.10".into();
    installed.current_tag = "v0.7.10".into();
    installed.current_commit = "b".repeat(40);
    let mut altered = value.clone();
    altered["migration_helpers"][0]["converter_sha256"] = json!("4".repeat(64));
    let modified = serde_json::to_vec(&altered).expect("modified tuple");
    assert!(
        verify_accepted_installed_manifest(
            &modified,
            &signature,
            &keys,
            &installed,
            &ledger,
            NOW,
            &build("x86_64-unknown-linux-gnu")
        )
        .is_err(),
        "modified exact bytes fail the original signature"
    );
    // A valid signature from the same installed trust key still cannot replace
    // the digest that the acceptance ledger already pinned.
    let (pair, keys) = signer();
    let original = serde_json::to_vec(&value).expect("original");
    let old_sig = pair.sign(&original);
    let mut ledger = AcceptedReleasesV1::default();
    let mut before = installed.clone();
    before.current_version = "0.7.0".into();
    before.current_tag = "v0.7.0".into();
    before.current_commit = "a".repeat(40);
    verify_manifest(&original, old_sig.as_ref(), &keys, &before, &ledger, NOW)
        .expect("original signer accepted")
        .accept(&mut ledger);
    let altered_sig = pair.sign(&modified);
    assert!(
        verify_accepted_installed_manifest(
            &modified,
            altered_sig.as_ref(),
            &keys,
            &installed,
            &ledger,
            NOW,
            &build("x86_64-unknown-linux-gnu")
        )
        .is_err(),
        "validly signed replacement digest is unaccepted"
    );
    let mut older = installed.clone();
    older.current_version = "0.7.11".into();
    older.current_tag = "v0.7.11".into();
    assert!(
        verify_accepted_installed_manifest(
            &original,
            old_sig.as_ref(),
            &keys,
            &older,
            &ledger,
            NOW,
            &build("x86_64-unknown-linux-gnu")
        )
        .is_err(),
        "old manifest cannot replay across an installed version"
    );
}
