use super::*;
use crate::source_custody::recovery::tests::{Fixture, NEW_BOOT, NODE, OLD_BOOT};
use std::os::unix::fs::PermissionsExt;

fn identity() -> Identity {
    Identity {
        machine_id: "a".repeat(32),
        boot_id: NEW_BOOT.into(),
    }
}

struct Case {
    fixture: Fixture,
    proof: Proof,
    record: Record,
}

fn private_write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("write proof fixture");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("private fixture");
}

impl Case {
    fn new() -> Self {
        let fixture = Fixture::new();
        let record = fixture.background("job");
        let Record::Background { token } = &record else {
            unreachable!()
        };
        let activation_id = uuid::Uuid::new_v4().to_string();
        super::super::write_active_at(&fixture.dir, record.clone(), None)
            .expect("old empty identity");
        let witnesses = fixture.dir.with_file_name("v2");
        std::fs::create_dir(&witnesses).expect("witness directory");
        std::fs::set_permissions(&witnesses, std::fs::Permissions::from_mode(0o700))
            .expect("private directory");
        let bytes = serde_json::to_vec(&serde_json::json!({
            "version":2,"reader_model":"daemon_mediated_v1",
            "kernel_instance":format!("linux:{OLD_BOOT}"),
            "activation_id":activation_id,"node_id":NODE,
            "anchor_nonce":uuid::Uuid::new_v4().to_string(),
            "anchor_device":1,"anchor_inode":2,"anchor_ctime":[1,0],
            "record":record,
        }))
        .expect("old witness");
        private_write(&witnesses.join(record.name().expect("name")), &bytes);
        private_write(
            &witnesses.join(format!("background-{}.armed", token.custody_id)),
            activation_id.as_bytes(),
        );
        // Raw launch invalidated this anchor. Reboot proof must not mistake
        // it for an untouched same-kernel lifetime lock or rewrite it.
        private_write(
            &witnesses.join(format!("background-{}.anchor", token.custody_id)),
            b"!poisoned",
        );
        let legacy = read_bytes(&fixture.dir.join(record.name().expect("name"))).expect("v1");
        let proof = Proof {
            version: 1,
            model: MODEL.into(),
            node_id: NODE.into(),
            custody_id: token.custody_id.clone(),
            activation_id,
            machine_id: identity().machine_id,
            previous_boot_id: OLD_BOOT.into(),
            current_boot_id: NEW_BOOT.into(),
            witness_sha256: digest(&bytes),
            legacy_sha256: digest(&legacy),
            at_ms: 10_000,
            previous_boot_entry_ms: 500,
        };
        Self {
            fixture,
            proof,
            record,
        }
    }

    fn custody_count(&self) -> i64 {
        self.fixture
            .connection()
            .query_row("SELECT count(*) FROM background_source_custody", [], |r| {
                r.get(0)
            })
            .expect("custody count")
    }

    fn evidence_present(&self) -> bool {
        let name = self.record.name().expect("name");
        self.fixture.dir.join(&name).exists()
            && self.fixture.dir.with_file_name("v2").join(name).exists()
    }

    async fn apply(&self, proof: &Proof) -> Result<(), String> {
        apply(
            &self.fixture.store,
            &self.fixture.dir,
            NODE,
            &identity(),
            10_000,
            proof,
        )
        .await
    }
}

#[test]
fn replicated_linux_startup_requires_a_valid_actual_host_reboot_identity() {
    use crate::source_custody::recovery::validate_linux_startup;
    assert!(validate_linux_startup(false, None, None).is_ok());
    assert!(validate_linux_startup(true, None, None).is_err());
    assert!(validate_linux_startup(true, Some(identity()), None).is_err());
    assert!(
        validate_linux_startup(true, Some(identity()), Some(format!("linux:{OLD_BOOT}"))).is_err()
    );
    assert!(validate_linux_startup(
        true,
        Some(Identity {
            machine_id: String::new(),
            boot_id: NEW_BOOT.into()
        }),
        Some(format!("linux:{NEW_BOOT}"))
    )
    .is_err());
    assert!(
        validate_linux_startup(true, Some(identity()), Some(format!("linux:{NEW_BOOT}"))).is_ok()
    );
}

#[tokio::test]
async fn trusted_reboot_proof_settles_exact_missing_identity_poisoned_custody() {
    let case = Case::new();
    assert_eq!(case.custody_count(), 1);
    case.apply(&case.proof)
        .await
        .expect("verified physical closure");
    assert_eq!(case.custody_count(), 0);
    assert!(!case.evidence_present());
    case.apply(&case.proof)
        .await
        .expect("acknowledged absence is idempotent");
}

#[tokio::test]
async fn reboot_proof_refuses_other_host_boot_node_nonce_stale_and_changed_evidence() {
    for variant in 0..12 {
        let case = Case::new();
        let mut proof = case.proof.clone();
        match variant {
            0 => proof.machine_id = "b".repeat(32),
            1 => proof.previous_boot_id = NEW_BOOT.into(),
            2 => proof.current_boot_id = OLD_BOOT.into(),
            3 => proof.node_id = "another-node".into(),
            4 => proof.activation_id = uuid::Uuid::new_v4().to_string(),
            5 => proof.at_ms = 10_000 + MAX_SKEW_MS + 1,
            6 => proof.at_ms = 0,
            7 => proof.previous_boot_entry_ms = proof.at_ms + 1,
            8 => proof.witness_sha256 = "0".repeat(64),
            9 => proof.legacy_sha256 = "0".repeat(64),
            10 => proof.previous_boot_id = uuid::Uuid::new_v4().to_string(),
            11 => proof.version = 2,
            _ => unreachable!(),
        }
        assert!(case.apply(&proof).await.is_err(), "variant {variant}");
        assert_eq!(case.custody_count(), 1, "variant {variant}");
        assert!(case.evidence_present(), "variant {variant}");
    }
    let case = Case::new();
    assert!(apply(
        &case.fixture.store,
        &case.fixture.dir,
        NODE,
        &identity(),
        10_000 + MAX_AGE_MS + 1,
        &case.proof
    )
    .await
    .is_err());
    assert_eq!(case.custody_count(), 1);
}

#[tokio::test]
async fn reboot_proof_refuses_unarmed_and_different_legacy_owner() {
    for unarmed in [true, false] {
        let mut case = Case::new();
        if unarmed {
            std::fs::remove_file(
                case.fixture
                    .dir
                    .with_file_name("v2")
                    .join(format!("background-{}.armed", case.proof.custody_id)),
            )
            .expect("remove marker");
        } else {
            let path = case.fixture.dir.join(case.record.name().expect("name"));
            let mut envelope: Envelope =
                serde_json::from_slice(&read_bytes(&path).expect("legacy")).expect("envelope");
            let Record::Background { token } = &mut envelope.records[0] else {
                unreachable!()
            };
            let BackgroundSourceOwner::Job { token: owner } = &mut token.owner else {
                unreachable!()
            };
            owner.claim_id = uuid::Uuid::new_v4().to_string();
            let bytes = serde_json::to_vec(&envelope).expect("changed owner");
            private_write(&path, &bytes);
            case.proof.legacy_sha256 = digest(&bytes);
        }
        assert!(case.apply(&case.proof).await.is_err());
        assert_eq!(case.custody_count(), 1);
        assert!(case.evidence_present());
    }
}

#[tokio::test]
async fn reboot_proof_cannot_settle_replacement_owner_and_retains_failed_release_for_retry() {
    let case = Case::new();
    let replacement_key = "replacement-owner";
    case.fixture
        .connection()
        .execute(
            "UPDATE background_source_custody SET owner_key=?1",
            [replacement_key],
        )
        .expect("replacement owner");
    case.apply(&case.proof)
        .await
        .expect("old owner is authoritatively absent");
    assert_eq!(case.custody_count(), 1);
    let proof: Option<String> = case
        .fixture
        .connection()
        .query_row(
            "SELECT settlement_proof FROM background_source_custody",
            [],
            |r| r.get(0),
        )
        .expect("replacement proof");
    assert!(proof.is_none());

    let case = Case::new();
    case.fixture.connection().execute_batch("CREATE TRIGGER refuse_reboot_release BEFORE DELETE ON background_source_custody BEGIN SELECT RAISE(ABORT,'injected release outage'); END;").expect("release failure");
    assert!(case.apply(&case.proof).await.is_err());
    assert_eq!(case.custody_count(), 1);
    assert!(case.evidence_present());
    case.fixture
        .connection()
        .execute_batch("DROP TRIGGER refuse_reboot_release;")
        .expect("release restored");
    case.apply(&case.proof)
        .await
        .expect("retry acknowledged release");
    assert_eq!(case.custody_count(), 0);
}

#[tokio::test]
async fn valid_host_identity_recovers_a_poisoned_reader_after_real_boot_change() {
    let case = Case::new();
    let path = case.fixture.dir.join(case.record.name().expect("name"));
    let envelope = Envelope {
        version: 1,
        machine_id: identity().machine_id,
        boot_id: OLD_BOOT.into(),
        node_id: NODE.into(),
        records: vec![case.record.clone()],
    };
    private_write(
        &path,
        &serde_json::to_vec(&envelope).expect("valid fallback"),
    );
    super::super::replay_at(&case.fixture.store, &case.fixture.dir, &identity(), NODE)
        .await
        .expect("normal reboot recovery");
    assert_eq!(case.custody_count(), 0);
}
