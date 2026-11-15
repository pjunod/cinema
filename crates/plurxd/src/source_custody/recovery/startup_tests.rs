use super::tests::{Fixture, NEW_BOOT, NODE, OLD_BOOT};
use super::*;
use std::os::unix::fs::PermissionsExt;

fn identity() -> Identity {
    Identity {
        machine_id: "a".repeat(32),
        boot_id: NEW_BOOT.into(),
    }
}

fn private_write(path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).expect("write reboot fixture");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .expect("private fixture");
}

fn poisoned_fixture() -> (Fixture, Record) {
    let fixture = Fixture::new();
    let record = fixture.background("job");
    let Record::Background { token } = &record else {
        unreachable!()
    };
    let activation_id = uuid::Uuid::new_v4().to_string();
    super::write_active_at(&fixture.dir, record.clone(), None).expect("old empty identity");
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
    // Raw launch invalidated this anchor. Same-kernel replay must not mistake
    // it for an untouched same-kernel lifetime lock or rewrite it.
    private_write(
        &witnesses.join(format!("background-{}.anchor", token.custody_id)),
        b"!poisoned",
    );
    (fixture, record)
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
async fn valid_host_identity_recovers_a_poisoned_reader_after_real_boot_change() {
    let (fixture, record) = poisoned_fixture();
    let path = fixture.dir.join(record.name().expect("name"));
    let envelope = Envelope {
        version: 1,
        machine_id: identity().machine_id,
        boot_id: OLD_BOOT.into(),
        node_id: NODE.into(),
        records: vec![record.clone()],
    };
    private_write(
        &path,
        &serde_json::to_vec(&envelope).expect("valid fallback"),
    );
    super::replay_at(&fixture.store, &fixture.dir, &identity(), NODE)
        .await
        .expect("normal reboot recovery");
    let count: i64 = fixture
        .connection()
        .query_row("SELECT count(*) FROM background_source_custody", [], |r| {
            r.get(0)
        })
        .expect("custody count");
    assert_eq!(count, 0);
}
