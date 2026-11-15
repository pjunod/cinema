use super::*;
use crate::source_custody::recovery::{tests::Fixture, write_active_at};
use serde_json::{json, Value};

fn proof(token: &JobToken, custody_id: &str) -> Value {
    let pass =
        json!({"tasks":12,"executables":1,"matched_readers":0,"snapshot_sha256":"1".repeat(64)});
    json!({
        "version":1, "model":"legacy_dv_title_reader_class_zero_v1",
        "kernel_instance":"linux:fixture", "observed_at_ms":5000, "activation_sha256":ACTIVATION,
        "owner":{"custody_id":custody_id,"job_id":token.job_id,"node_id":token.node_id,
            "boot_id":token.boot_id,"claim_id":token.claim_id,"fence":token.fence},
        "closure":{"source_commit":REFERENCE_COMMIT,"native_source_sha256":SOURCE,
            "manifest_sha256":MANIFEST,"daemon_sha256":DAEMON,"scanner_sha256":SCANNER,
            "renderer_sha256":RENDERER},
        "census":{"backend":"linux_initial_pidns_executable_markers_v1","passes":[pass,pass],
            "images":[{"device":1,"inode":2,"bytes":450,"sha256":"2".repeat(64),"matched_roles":[]}]}
    })
}

#[test]
fn complete_census_requires_two_matching_passes_and_no_known_reader_binary() {
    let f = Fixture::new();
    let Record::Background { token } = f.background("job") else {
        unreachable!()
    };
    let BackgroundSourceOwner::Job { token: job } = &token.owner else {
        unreachable!()
    };
    let valid = proof(job, &token.custody_id);
    let accepts = |value| {
        serde_json::from_value::<Proof>(value)
            .is_ok_and(|proof| proof.verified(&job.node_id, "linux:fixture", 5000))
    };
    assert!(accepts(valid.clone()));
    for (path, change) in [
        ("/census/passes/1/tasks", json!(13)),
        ("/census/passes/0/matched_readers", json!(1)),
        ("/census/images/0/sha256", json!(DAEMON)),
        ("/census/images/0/sha256", json!(SCANNER)),
        ("/census/images/0/sha256", json!(RENDERER)),
        ("/census/images/0/matched_roles", json!(["scanner"])),
        ("/closure/manifest_sha256", json!("not reviewed")),
        ("/kernel_instance", json!("linux:other")),
        ("/model", json!("daemon_mediated_v1")),
        ("/observed_at_ms", json!(5001)),
    ] {
        let mut value = valid.clone();
        *value.pointer_mut(path).expect("proof field") = change;
        assert!(!accepts(value), "{path}");
    }
}

#[tokio::test]
async fn only_the_expired_native_dv_owner_is_released_after_acknowledgement() {
    let f = Fixture::new();
    let record = f.background("job");
    let Record::Background { token } = &record else {
        unreachable!()
    };
    let BackgroundSourceOwner::Job { token: job } = &token.owner else {
        unreachable!()
    };
    write_active_at(&f.dir, record.clone(), None).expect("legacy activation intent");
    let path = f.dir.join(record.name().expect("record name"));
    let proof: Proof = serde_json::from_value(proof(job, &token.custody_id)).expect("typed proof");
    assert!(
        apply(&f.store, &f.dir, &job.node_id, "linux:fixture", &proof)
            .await
            .is_err(),
        "a different journal cannot reuse the historical activation's audit"
    );
    let activation = f.envelope(vec![record.clone()]);
    let run = || settle_activation(&f.store, &f.dir, &job.node_id, &proof, &activation);
    assert!(
        run().await.is_err(),
        "missing historical job is insufficient"
    );
    let accepted = JobPayload::DvTitleScan {
        file_id: 12,
        source_size: 1200,
        source_mtime: 100,
        probe_revision: 4,
        scanner_digest: TITLE_SCANNER.into(),
    };
    f.connection().execute(
        "INSERT INTO background_jobs(id,kind,payload_version,payload_json,dedupe_key,priority,state,owner_node_id,owner_boot_id,claim_id,fence,revision,lease_expires_ms,not_before_ms,created_at_ms,updated_at_ms)
        VALUES(?1,'dv_title_scan',1,?2,'fixture',2,'running',?3,?4,?5,?6,?7,6000,1,1,1)",
        rusqlite::params![job.job_id,serde_json::to_string(&accepted).expect("payload"),job.node_id,job.boot_id,job.claim_id,job.fence,job.revision],
    ).expect("real native title job");
    assert!(
        run().await.is_err(),
        "renewed lease beyond census must refuse"
    );
    f.connection()
        .execute(
            "UPDATE background_jobs SET lease_expires_ms=1000,fence=fence+1",
            [],
        )
        .expect("replacement generation");
    assert!(
        run().await.is_err(),
        "different live fence cannot be consumed"
    );
    f.connection()
        .execute("UPDATE background_jobs SET fence=fence-1", [])
        .expect("restore exact fixture owner");
    f.connection().execute_batch("CREATE TRIGGER refuse_physical_release BEFORE DELETE ON background_source_custody BEGIN SELECT RAISE(ABORT,'injected acknowledgement failure'); END").expect("failed ack fixture");
    assert!(run().await.is_err());
    assert!(path.exists());
    assert!(f
        .store
        .resolve_background_source_custody(token.clone())
        .await
        .expect("old owner")
        .is_some());
    f.connection()
        .execute_batch("DROP TRIGGER refuse_physical_release")
        .expect("recover store");
    run().await.expect("acknowledged exact release");
    assert!(!path.exists());
    assert!(f
        .store
        .resolve_background_source_custody(token.clone())
        .await
        .expect("read after ack")
        .is_none());
    apply(&f.store, &f.dir, &job.node_id, "linux:fixture", &proof)
        .await
        .expect("read-only admin proof replay is idempotent");
}
