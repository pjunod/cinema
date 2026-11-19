//! Exercise actual activation helpers in a fresh process, so their journal
//! OnceLock and fault point cannot interfere with parallel daemon unit tests.
use super::tests::{Fixture, OLD_BOOT};
use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

static FAIL_CLEAR: AtomicBool = AtomicBool::new(false);

pub(super) fn fail_clear() -> bool {
    FAIL_CLEAR.swap(false, Ordering::SeqCst)
}

struct OwnedFixture(std::process::Child);

impl Drop for OwnedFixture {
    fn drop(&mut self) {
        // Retain the exact handle on timeout, polling error, and unwind.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
#[allow(clippy::disallowed_methods)] // A real re-exec isolates process-global production journals.
fn activation_abort_cleanup_and_successful_activation_preserve_the_correct_evidence() {
    let child = std::process::Command::new(std::env::current_exe().expect("test binary"))
        .args([
            "--exact",
            "source_custody::recovery::abort_tests::activation_abort_fixture",
            "--nocapture",
        ])
        .env("PLURX_TEST_SOURCE_ABORT_FIXTURE", "1")
        .spawn()
        .expect("isolated activation fixture");
    let mut child = OwnedFixture(child);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(status) = child.0.try_wait().expect("fixture status") {
            assert!(status.success(), "activation fixture failed: {status}");
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!("activation cleanup fixture exceeded its deadline");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn prepare_record(fixture: &Fixture, foreground: bool) -> Record {
    let record = if foreground {
        fixture.foreground()
    } else {
        fixture.background("job")
    };
    match &record {
        Record::Foreground { token } => {
            fixture.connection().execute("UPDATE foreground_source_owners SET settlement_proof='no_reader', activation_id=NULL WHERE owner_id=?1", [&token.owner.owner_id]).expect("pre-read foreground");
        }
        Record::Background { token } => {
            fixture.connection().execute("UPDATE background_source_custody SET settlement_proof='no_reader', activation_id=NULL WHERE custody_id=?1", [&token.custody_id]).expect("pre-read background");
        }
    }
    record
}

async fn activate(fixture: &Fixture, record: &Record) -> Result<(), String> {
    match record {
        Record::Foreground { token } => {
            super::super::activate_foreground_with_journal(&fixture.store, token.clone()).await
        }
        Record::Background { token } => {
            super::super::activate_background_with_journal(&fixture.store, token.clone()).await
        }
    }
}

fn activation_receipt(record: &Record, activation_id: String) -> SettlementReceipt {
    match record {
        Record::Foreground { token } => SettlementReceipt::ForegroundActivation {
            token: token.clone(),
            activation_id,
        },
        Record::Background { token } => SettlementReceipt::BackgroundActivation {
            token: token.clone(),
            activation_id,
        },
    }
}

#[tokio::test]
async fn recovery_context_cannot_consume_another_stores_evidence() {
    let owner = Fixture::new();
    let other = Fixture::new();
    configure_backend(&owner.store, owner.root.path(), true).expect("owner context");
    for foreground_record in [false, true] {
        let record = prepare_record(&owner, foreground_record);
        let persist = |store: &Arc<dyn Store>| match &record {
            Record::Foreground { token } => foreground(store, token, OLD_BOOT),
            Record::Background { token } => background(store, token, OLD_BOOT),
        };
        let joined = match &record {
            Record::Foreground { token } => SettlementReceipt::Foreground(token.clone()),
            Record::Background { token } => SettlementReceipt::Background(token.clone()),
        };
        let activation = activation_receipt(&record, uuid::Uuid::new_v4().to_string());
        persist(&owner.store).expect("owner evidence");
        let path = owner.dir.join(record.name().expect("record name"));
        let original = std::fs::read(&path).expect("original evidence");

        // Even with the same data path, a fallback Store has no authority to
        // rewrite, clear, or replay this replicated owner's physical evidence.
        configure_backend(&other.store, owner.root.path(), false).expect("fallback context");
        persist(&other.store).expect("fallback persistence is a no-op");
        clear(&other.store, &joined).expect("fallback join cleanup is a no-op");
        assert!(clear_aborted_if_absent(&other.store, &activation)
            .await
            .expect("fallback cleanup"));
        replay(&other.store, super::tests::NODE)
            .await
            .expect("fallback replay");
        assert_eq!(std::fs::read(&path).expect("retained evidence"), original);

        // A different authoritative Store uses its own directory, including
        // when its exact-owner absence query succeeds.
        configure_backend(&other.store, other.root.path(), true).expect("other context");
        persist(&other.store).expect("independent evidence");
        clear(&other.store, &joined).expect("independent cleanup");
        assert!(clear_aborted_if_absent(&other.store, &activation)
            .await
            .expect("other absence"));
        assert_eq!(
            std::fs::read(&path).expect("owner evidence survives"),
            original
        );
        assert!(!other.dir.join(record.name().expect("record name")).exists());

        abort_and_release(&owner, &activation).await;
        assert!(clear_aborted_if_absent(&owner.store, &activation)
            .await
            .expect("owner cleanup"));
        assert!(!path.exists());
    }
}

async fn abort_and_release(fixture: &Fixture, receipt: &SettlementReceipt) {
    match receipt {
        SettlementReceipt::ForegroundActivation {
            token,
            activation_id,
        } => {
            assert!(fixture
                .store
                .abort_foreground_source_activation(token.clone(), activation_id.clone())
                .await
                .expect("foreground abort"));
            let current = fixture
                .store
                .resolve_foreground_source(token.clone())
                .await
                .expect("current foreground")
                .expect("exact foreground");
            assert!(fixture
                .store
                .release_foreground_source(current, 10_000)
                .await
                .expect("foreground release"));
        }
        SettlementReceipt::BackgroundActivation {
            token,
            activation_id,
        } => {
            assert!(fixture
                .store
                .abort_background_source_activation(token.clone(), activation_id.clone())
                .await
                .expect("background abort"));
            assert!(fixture
                .store
                .release_background_source_custody(token.clone(), 10_000)
                .await
                .expect("background release"));
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn activation_abort_fixture() {
    if std::env::var("PLURX_TEST_SOURCE_ABORT_FIXTURE").as_deref() != Ok("1") {
        return;
    }
    let fixture = Fixture::new();
    super::super::configure_settlement_journal(fixture.root.path());
    super::super::configure_recovery_backend(&fixture.store, fixture.root.path(), true)
        .expect("scoped recovery journal");
    let legacy_dir = fixture.root.path().join("source-custody-settlements");
    for foreground in [false, true] {
        // A failed unlink after a real activation forces the confirmed abort
        // path. The helper never returns a guard that could read source input.
        let aborted = prepare_record(&fixture, foreground);
        FAIL_CLEAR.store(true, Ordering::SeqCst);
        assert!(activate(&fixture, &aborted).await.is_err());
        assert!(!fixture
            .dir
            .join(aborted.name().expect("record name"))
            .exists());

        // Clearing the successful pre-read intent must retain active evidence.
        let live = prepare_record(&fixture, foreground);
        activate(&fixture, &live)
            .await
            .expect("successful activation");
        let live_path = fixture.dir.join(live.name().expect("record name"));
        assert!(live_path.exists());
        // A duplicate activation has a different nonce and cannot delete an
        // earlier successful reader's evidence, even though its owner matches.
        assert!(activate(&fixture, &live).await.is_err());
        assert!(live_path.exists());
        match &live {
            Record::Foreground { token } => {
                assert!(fixture
                    .store
                    .mark_foreground_source_settled(token.clone())
                    .await
                    .expect("known no-input foreground join"));
                let current = fixture
                    .store
                    .resolve_foreground_source(token.clone())
                    .await
                    .expect("resolve foreground")
                    .expect("foreground");
                assert!(fixture
                    .store
                    .release_foreground_source(current, 10_000)
                    .await
                    .expect("release foreground"));
                super::super::clear_settlement_receipt(
                    &fixture.store,
                    &SettlementReceipt::Foreground(token.clone()),
                )
                .expect("joined cleanup");
            }
            Record::Background { token } => {
                assert!(fixture
                    .store
                    .mark_background_source_settled(token.clone())
                    .await
                    .expect("known no-input background join"));
                assert!(fixture
                    .store
                    .release_background_source_custody(token.clone(), 10_000)
                    .await
                    .expect("release background"));
                super::super::clear_settlement_receipt(
                    &fixture.store,
                    &SettlementReceipt::Background(token.clone()),
                )
                .expect("joined cleanup");
            }
        }
        assert!(!live_path.exists());

        // Restart replay must remove active evidence after an exact abort,
        // including an earlier successful release whose acknowledgement was lost.
        for already_released in [false, true] {
            let record = prepare_record(&fixture, foreground);
            write_active_at(
                &fixture.dir,
                record.clone(),
                Some(Identity {
                    machine_id: "a".repeat(32),
                    boot_id: OLD_BOOT.into(),
                }),
            )
            .expect("active evidence");
            let receipt = activation_receipt(&record, uuid::Uuid::new_v4().to_string());
            super::super::write_settlement_receipt(&receipt).expect("activation intent");
            if already_released {
                abort_and_release(&fixture, &receipt).await;
            }
            super::super::replay_local_settlements_at(&fixture.store, &legacy_dir)
                .await
                .expect("abort replay");
            assert!(!fixture
                .dir
                .join(record.name().expect("record name"))
                .exists());
            assert!(!legacy_dir
                .join(super::super::settlement_name(&receipt))
                .exists());
        }
    }
    assert!(fixture
        .store
        .source_io_holders(10_000)
        .await
        .expect("no unproved readers")
        .is_empty());
}
