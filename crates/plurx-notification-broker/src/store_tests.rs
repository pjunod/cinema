use super::*;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
type TestResult = std::result::Result<(), Box<dyn std::error::Error>>;
const NOW: i64 = 1_800_000_000;
fn publisher() -> PublisherRecord {
    PublisherRecord {
        credential: "synthetic-signing-key".into(),
        id: uuid::Uuid::new_v4().to_string(),
        proof_hash: "11".repeat(32),
        server: "synthetic-home".into(),
        apple: Some("TEAM:tv.plurx:production".into()),
        android: Some("synthetic-project".into()),
    }
}
fn ticket(platform: Platform) -> Ticket {
    Ticket {
        version: VERSION.into(),
        ticket_id: uuid::Uuid::new_v4().to_string(),
        ticket_secret_hash: "22".repeat(32),
        server_instance_id: "synthetic-home".into(),
        installation_id: uuid::Uuid::new_v4().to_string(),
        receiver_id: uuid::Uuid::new_v4().to_string(),
        consent_id: uuid::Uuid::new_v4().to_string(),
        phone_generation: 1,
        consent_generation: 1,
        transport_generation: 1,
        platform,
    }
}
fn delivery(ticket: &Ticket, now: i64) -> Delivery {
    let mut bytes = Vec::from(
        uuid::Uuid::parse_str(&ticket.installation_id)
            .expect("synthetic UUID")
            .as_bytes()
            .as_slice(),
    );
    bytes.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
    Delivery {
        version: VERSION.into(),
        enrollment_id: ticket.ticket_id.clone(),
        installation_id: ticket.installation_id.clone(),
        phone_generation: 1,
        consent_generation: 1,
        transport_generation: 1,
        invitation_id: URL_SAFE_NO_PAD.encode(bytes),
        expires_at: now + 120,
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    path: std::path::PathBuf,
    generation: String,
    publisher: PublisherRecord,
    store: Store,
}
impl Fixture {
    fn new() -> std::result::Result<Self, Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("broker.sqlite");
        let generation = uuid::Uuid::new_v4().to_string();
        let publisher = publisher();
        Store::initialize(
            &path,
            &generation,
            &[7; 32],
            crate::config::STORAGE_REALM,
            std::slice::from_ref(&publisher),
            NOW,
        )?;
        let store = Store::open(&path, &generation, &[7; 32], crate::config::STORAGE_REALM)?;
        Ok(Self {
            _dir: dir,
            path,
            generation,
            publisher,
            store,
        })
    }
    fn enroll(&mut self, ticket: &Ticket, now: i64) -> Result<()> {
        self.store.issue(&self.publisher.id, ticket, now)?;
        self.store.claim(
            &ticket.ticket_id,
            &[0x22; 32],
            ticket.platform,
            "aabb",
            std::slice::from_ref(&self.publisher.id),
            now,
        )
    }
}
#[test]
fn ticket_retry_single_use_expiry_and_preemptive_tombstone() -> TestResult {
    let mut f = Fixture::new()?;
    let t = ticket(Platform::Apple);
    let first = f.store.issue(&f.publisher.id, &t, NOW)?;
    let retry = f.store.issue(&f.publisher.id, &t, NOW + 100)?;
    assert_eq!(first.expires_at, retry.expires_at);
    assert!(f
        .store
        .claim(
            &t.ticket_id,
            &[0x22; 32],
            Platform::Apple,
            "aabb",
            std::slice::from_ref(&f.publisher.id),
            NOW + 120
        )
        .is_err());
    assert_eq!(
        f.store
            .status(&f.publisher.id, &t.ticket_id, NOW + 120)?
            .status,
        "expired"
    );
    let t = ticket(Platform::Apple);
    f.enroll(&t, NOW + 120)?;
    assert!(f
        .store
        .claim(
            &t.ticket_id,
            &[0x22; 32],
            Platform::Apple,
            "aabb",
            std::slice::from_ref(&f.publisher.id),
            NOW + 121
        )
        .is_err());
    assert_eq!(
        f.store
            .status(&f.publisher.id, &t.ticket_id, NOW + 500)?
            .status,
        "claimed"
    );
    let blocked = ticket(Platform::Apple);
    f.store
        .revoke(&f.publisher.id, &blocked.ticket_id, NOW + 500)?;
    assert!(f.store.issue(&f.publisher.id, &blocked, NOW + 501).is_err());
    assert!(f
        .store
        .status(&publisher().id, &t.ticket_id, NOW + 501)
        .is_err());
    let other = publisher();
    f.store
        .reconcile(&[f.publisher.clone(), other.clone()], NOW + 501)?;
    f.store.revoke(&other.id, &t.ticket_id, NOW + 501)?;
    assert_eq!(
        f.store
            .status(&f.publisher.id, &t.ticket_id, NOW + 501)?
            .status,
        "claimed"
    );
    Ok(())
}
#[test]
fn durable_attempt_duplicate_cooldown_and_revoke() -> TestResult {
    let mut f = Fixture::new()?;
    let t = ticket(Platform::Apple);
    f.enroll(&t, NOW)?;
    let d = delivery(&t, NOW);
    assert!(matches!(
        f.store.admit(&f.publisher.id, &d, NOW)?,
        Admission::Attempt(_)
    ));
    let mut reopened = Store::open(
        &f.path,
        &f.generation,
        &[7; 32],
        crate::config::STORAGE_REALM,
    )?;
    assert!(matches!(
        reopened.admit(&f.publisher.id, &d, NOW)?,
        Admission::Duplicate
    ));
    let next = delivery(&t, NOW);
    assert!(matches!(
        reopened.admit(&f.publisher.id, &next, NOW)?,
        Admission::Denied
    ));
    assert!(reopened.admit(&f.publisher.id, &d, NOW + 120).is_err());
    reopened.revoke(&f.publisher.id, &t.ticket_id, NOW + 120)?;
    let mut d = d;
    d.expires_at = NOW + 240;
    assert!(reopened.admit(&f.publisher.id, &d, NOW + 120).is_err());
    Ok(())
}
#[test]
fn bounded_capacity_reserves_known_revoke_and_never_erases_expired_ids() -> TestResult {
    let mut f = Fixture::new()?;
    f.store.limits = Limits {
        identities: 2,
        pending: 1,
        active: 1,
        deliveries: 1,
    };
    let t = ticket(Platform::Apple);
    f.store.issue(&f.publisher.id, &t, NOW)?;
    assert!(f
        .store
        .issue(&f.publisher.id, &ticket(Platform::Apple), NOW)
        .is_err());
    let second = ticket(Platform::Apple);
    f.enroll(&second, NOW + 120)?;
    assert!(f
        .store
        .issue(&f.publisher.id, &ticket(Platform::Apple), NOW + 120)
        .is_err());
    f.store
        .revoke(&f.publisher.id, &second.ticket_id, NOW + 120)?;
    assert!(f.store.issue(&f.publisher.id, &t, NOW + 120).is_ok());
    assert!(f
        .store
        .claim(
            &t.ticket_id,
            &[0x22; 32],
            Platform::Apple,
            "aabb",
            std::slice::from_ref(&f.publisher.id),
            NOW + 120
        )
        .is_err());
    Ok(())
}
#[test]
fn ciphertext_and_full_binding_tampering_fails_closed() -> TestResult {
    let mut f = Fixture::new()?;
    let t = ticket(Platform::Apple);
    f.enroll(&t, NOW)?;
    let mut altered = t.clone();
    altered.receiver_id = uuid::Uuid::new_v4().to_string();
    sql(f.store.connection.execute(
        "UPDATE capabilities SET binding=?1 WHERE id=?2",
        params![encoded(&altered)?, t.ticket_id],
    ))?;
    assert!(f
        .store
        .admit(&f.publisher.id, &delivery(&t, NOW), NOW)
        .is_err());
    sql(f.store.connection.execute(
        "UPDATE capabilities SET binding=?1,ciphertext=zeroblob(20) WHERE id=?2",
        params![encoded(&t)?, t.ticket_id],
    ))?;
    assert!(f
        .store
        .admit(&f.publisher.id, &delivery(&t, NOW), NOW)
        .is_err());
    Ok(())
}
#[test]
fn compatible_rotation_rejects_old_process_and_realm_changes_do_not_resurrect() -> TestResult {
    let mut f = Fixture::new()?;
    let apple = ticket(Platform::Apple);
    let android = ticket(Platform::Android);
    f.enroll(&apple, NOW)?;
    f.enroll(&android, NOW)?;
    let old = f.publisher.clone();
    let mut rotated = old.clone();
    rotated.proof_hash = "33".repeat(32);
    rotated.credential = "rotated-signing-key".into();
    let mut second = Store::open(
        &f.path,
        &f.generation,
        &[7; 32],
        crate::config::STORAGE_REALM,
    )?;
    second.reconcile(std::slice::from_ref(&rotated), NOW)?;
    assert!(f.store.authorized(&old, NOW).is_err());
    assert!(f.store.eligible(&rotated, &delivery(&apple, NOW), NOW)?);
    rotated.apple = Some("TEAM:new-topic:production".into());
    second.reconcile(std::slice::from_ref(&rotated), NOW)?;
    assert!(!f.store.eligible(&rotated, &delivery(&apple, NOW), NOW)?);
    assert!(f.store.eligible(&rotated, &delivery(&android, NOW), NOW)?);
    second.reconcile(&[], NOW)?;
    second.reconcile(std::slice::from_ref(&rotated), NOW)?;
    assert!(!f.store.eligible(&rotated, &delivery(&android, NOW), NOW)?);
    assert!(second.issue(&rotated.id, &android, NOW).is_err());
    let fresh = ticket(Platform::Android);
    f.publisher = rotated.clone();
    f.store.reconcile(std::slice::from_ref(&rotated), NOW)?;
    f.enroll(&fresh, NOW)?;
    rotated.android = Some("other-project".into());
    second.reconcile(std::slice::from_ref(&rotated), NOW)?;
    assert!(!f.store.eligible(&rotated, &delivery(&fresh, NOW), NOW)?);
    let fresh = ticket(Platform::Apple);
    f.publisher = rotated.clone();
    f.store.reconcile(std::slice::from_ref(&rotated), NOW)?;
    f.enroll(&fresh, NOW)?;
    rotated.server = "other-home".into();
    second.reconcile(std::slice::from_ref(&rotated), NOW)?;
    assert!(!f.store.eligible(&rotated, &delivery(&fresh, NOW), NOW)?);
    Ok(())
}
#[test]
fn external_restore_fence_requires_generation_master_and_proof_rotation() -> TestResult {
    let f = Fixture::new()?;
    assert!(Store::open(
        &f.path,
        &uuid::Uuid::new_v4().to_string(),
        &[7; 32],
        crate::config::STORAGE_REALM
    )
    .is_err());
    assert!(Store::open(
        &f.path,
        &f.generation,
        &[8; 32],
        crate::config::STORAGE_REALM
    )
    .is_err());
    let generation = uuid::Uuid::new_v4().to_string();
    assert!(Store::restore_fence(
        &f.path,
        &generation,
        &[8; 32],
        crate::config::STORAGE_REALM,
        std::slice::from_ref(&f.publisher),
        NOW
    )
    .is_err());
    let mut publisher = f.publisher;
    publisher.proof_hash = "44".repeat(32);
    Store::restore_fence(
        &f.path,
        &generation,
        &[8; 32],
        crate::config::STORAGE_REALM,
        &[publisher],
        NOW,
    )?;
    assert!(Store::open(
        &f.path,
        &f.generation,
        &[7; 32],
        crate::config::STORAGE_REALM
    )
    .is_err());
    Ok(())
}

#[test]
fn publisher_retention_counts_inactive_and_rejects_atomically() -> TestResult {
    let mut f = Fixture::new()?;
    let all = (0..63)
        .map(|_| publisher())
        .chain(std::iter::once(f.publisher.clone()))
        .collect::<Vec<_>>();
    f.store.reconcile(&all, NOW)?;
    f.store.reconcile(std::slice::from_ref(&f.publisher), NOW)?;
    assert_eq!(
        f.store
            .reconcile(&[publisher()], NOW)
            .err()
            .map(|error| error.code),
        Some("retention_limit")
    );
    f.store.authorized(&f.publisher, NOW)?;
    assert_eq!(
        f.store
            .connection
            .query_row("SELECT count(*) FROM publishers", [], |row| row
                .get::<_, i64>(0))?,
        64
    );
    Ok(())
}

#[test]
fn reconciliation_between_auth_and_mutation_rejects_stale_authority_atomically() -> TestResult {
    let mut f = Fixture::new()?;
    let expected = f.publisher.clone();
    f.store.authorized(&expected, NOW)?;
    let mut other = Store::open(
        &f.path,
        &f.generation,
        &[7; 32],
        crate::config::STORAGE_REALM,
    )?;
    let mut changed = expected.clone();
    changed.apple = Some("TEAM:changed-topic:production".into());
    other.reconcile(&[changed], NOW)?;
    let t = ticket(Platform::Apple);
    assert!(f.store.issue(&expected.id, &t, NOW).is_err());
    assert_eq!(
        other
            .connection
            .query_row("SELECT count(*) FROM capabilities", [], |row| row
                .get::<_, i64>(0))?,
        0
    );
    assert!(f.store.revoke(&expected.id, &t.ticket_id, NOW).is_err());
    assert_eq!(
        other
            .connection
            .query_row("SELECT count(*) FROM capabilities", [], |row| row
                .get::<_, i64>(0))?,
        0
    );
    Ok(())
}

#[test]
fn concurrent_duplicate_admission_commits_one_attempt() -> TestResult {
    let mut f = Fixture::new()?;
    let ticket = ticket(Platform::Apple);
    f.enroll(&ticket, NOW)?;
    let delivery = delivery(&ticket, NOW);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let mut store = Store::open(
            &f.path,
            &f.generation,
            &[7; 32],
            crate::config::STORAGE_REALM,
        )?;
        let delivery = delivery.clone();
        let publisher = f.publisher.id.clone();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            store
                .admit(&publisher, &delivery, NOW)
                .map(|admission| matches!(admission, Admission::Attempt(_)))
        }));
    }
    let mut attempts = 0;
    for handle in handles {
        if handle.join().map_err(|_| "thread")?? {
            attempts += 1;
        }
    }
    assert_eq!(attempts, 1);
    Ok(())
}
#[test]
fn active_delivery_quota_and_observed_clock_rollback_are_fenced() -> TestResult {
    let mut f = Fixture::new()?;
    f.store.limits.active = 1;
    f.store.limits.deliveries = 1;
    let first = ticket(Platform::Apple);
    f.enroll(&first, NOW)?;
    let second = ticket(Platform::Apple);
    f.store.issue(&f.publisher.id, &second, NOW)?;
    assert_eq!(
        f.store
            .claim(
                &second.ticket_id,
                &[0x22; 32],
                Platform::Apple,
                "aabb",
                std::slice::from_ref(&f.publisher.id),
                NOW
            )
            .err()
            .map(|error| error.code),
        Some("retention_limit")
    );
    let delivery = delivery(&first, NOW);
    f.store.admit(&f.publisher.id, &delivery, NOW)?;
    let another = self::delivery(&first, NOW);
    assert_eq!(
        f.store
            .admit(&f.publisher.id, &another, NOW)
            .err()
            .map(|error| error.code),
        Some("retention_limit")
    );
    f.store.revoke(&f.publisher.id, &first.ticket_id, NOW + 1)?;
    assert_eq!(
        f.store
            .rate(&f.publisher.id, NOW)
            .err()
            .map(|error| error.code),
        Some("restore_fence_required")
    );
    Ok(())
}
