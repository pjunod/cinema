use super::*;
use plurx_core::{
    auth,
    store::{
        invitations::*,
        remote::{NewRemoteGrant, NewRemoteReceiver, RemoteGrant, RemoteReceiver},
        RemoteStore, SettingsStore,
    },
};
fn id() -> String {
    uuid::Uuid::new_v4().to_string()
}
struct Fixture {
    user: i64,
    receiver: String,
    phone: String,
    grant: String,
    enrollment: String,
    rhash: String,
    phash: String,
    ghash: String,
    phone_digest: String,
    tv_digest: String,
    now: i64,
}
impl Fixture {
    async fn new(s: &dyn Store) -> Self {
        let user = s
            .create_user("invitation-contract", "hash", false)
            .await
            .expect("user")
            .id;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_secs() as i64;
        let phone_digest = auth::hash_token("phone native");
        let tv_digest = auth::hash_token("TV native");
        s.create_token(&phone_digest, user, None)
            .await
            .expect("phone login");
        s.create_token(&tv_digest, user, None)
            .await
            .expect("TV login");
        let (receiver, phone, grant, enrollment) = (id(), id(), id(), id());
        let (rhash, phash, ghash) = (
            auth::hash_token("receiver"),
            auth::hash_token("phone"),
            auth::hash_token("grant"),
        );
        s.create_remote_receiver(NewRemoteReceiver {
            receiver: RemoteReceiver {
                id: receiver.clone(),
                user_id: user,
                name: "TV".into(),
                platform: "web".into(),
                created_at: now,
            },
            secret_hash: rhash.clone(),
        })
        .await
        .expect("receiver");
        s.create_remote_grant(NewRemoteGrant {
            grant: RemoteGrant {
                id: grant.clone(),
                receiver_id: receiver.clone(),
                name: "phone".into(),
                created_at: now,
            },
            user_id: user,
            secret_hash: ghash.clone(),
        })
        .await
        .expect("grant");
        assert!(s
            .create_invitation_phone(NewInvitationPhone {
                phone: InvitationPhone {
                    id: phone.clone(),
                    user_id: user,
                    name: "Phone".into(),
                    platform: "android".into(),
                    generation: 1,
                    created_at: now,
                    permission_granted: false,
                    resident_active: false
                },
                secret_hash: phash.clone(),
                token_digest: phone_digest.clone()
            })
            .await
            .expect("phone"));
        Self {
            user,
            receiver,
            phone,
            grant,
            enrollment,
            rhash,
            phash,
            ghash,
            phone_digest,
            tv_digest,
            now,
        }
    }
    fn consent(&self, expected: i64, enabled: bool) -> SaveInvitationConsent {
        self.consent_at(expected, enabled, 1)
    }
    fn consent_at(
        &self,
        expected: i64,
        enabled: bool,
        phone_generation: i64,
    ) -> SaveInvitationConsent {
        SaveInvitationConsent {
            id: self.enrollment.clone(),
            phone_id: self.phone.clone(),
            receiver_id: self.receiver.clone(),
            user_id: self.user,
            phone_hash: self.phash.clone(),
            expected_generation: expected,
            expected_phone_generation: phone_generation,
            enable: enabled.then(|| {
                (
                    self.grant.clone(),
                    self.ghash.clone(),
                    InvitationTransport::AndroidResident,
                )
            }),
        }
    }
    async fn candidate(&self, s: &dyn Store, foreground: String, now: i64) -> AdmitInvitation {
        let c = s
            .invitation_consent(&self.phone, &self.receiver, self.user)
            .await
            .expect("consent")
            .expect("consent row");
        AdmitInvitation {
            id: id(),
            enrollment_id: self.enrollment.clone(),
            foreground_id: foreground,
            receiver_hash: self.rhash.clone(),
            phone_generation: s
                .invitation_phone(&self.phone, self.user)
                .await
                .expect("phone metadata")
                .expect("phone")
                .generation,
            consent_generation: c.generation,
            transport_generation: c.transport_generation,
            phone_login: s
                .invitation_login(&self.phone_digest, now)
                .await
                .expect("no-touch login")
                .expect("live phone"),
            receiver_login: s
                .invitation_login(&self.tv_digest, now)
                .await
                .expect("no-touch TV")
                .expect("live TV"),
            now,
        }
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_consent_off_and_no_touch_authority_fail_closed() {
    for_each_backend(|s, _| async move {
        let f = Fixture::new(s.as_ref()).await;
        assert!(s
            .invitation_phone_authority(&f.phone, f.user, "")
            .await
            .is_err());
        assert!(s
            .invitation_phone_authority(&f.phone, f.user, &auth::hash_token("wrong"))
            .await
            .expect("wrong proof")
            .is_none());
        let original = s.list_tokens_for_user(f.user).await.expect("inventory");
        let before = s
            .invitation_login(&f.phone_digest, f.now)
            .await
            .expect("login")
            .expect("live");
        let after = s
            .invitation_login(&f.phone_digest, f.now + 600)
            .await
            .expect("login later")
            .expect("live");
        assert_eq!(before.last_seen_at, after.last_seen_at);
        assert_eq!(
            original,
            s.list_tokens_for_user(f.user)
                .await
                .expect("unchanged inventory")
        );
        let mut request = f.consent(0, true);
        request.enable.as_mut().expect("enable").1 = String::new();
        assert!(s.save_invitation_consent(request).await.is_err());
        assert!(s
            .save_invitation_consent(f.consent(0, true))
            .await
            .expect("on"));
        assert!(!s
            .save_invitation_consent(f.consent(0, true))
            .await
            .expect("stale write"));
        s.revoke_remote_grant(&f.grant, f.user, f.now)
            .await
            .expect("revoke grant");
        s.revoke_remote_receiver(&f.receiver, f.user, f.now)
            .await
            .expect("revoke receiver");
        s.delete_token(&f.phone_digest)
            .await
            .expect("logout original login");
        assert!(s
            .save_invitation_consent(f.consent(1, false))
            .await
            .expect("OFF survives revoked authorities/global off"));
        assert!(
            !s.invitation_consent(&f.phone, &f.receiver, f.user)
                .await
                .expect("consent")
                .expect("row")
                .enabled
        );
        assert!(!s
            .save_invitation_consent(f.consent(2, true))
            .await
            .expect("ON needs grant"));
        assert!(s
            .invitation_login(&f.phone_digest, f.now)
            .await
            .expect("revoked login")
            .is_none());
        s.revoke_invitation_phone(&f.phone, f.user)
            .await
            .expect("lost-phone deletion");
        assert!(s
            .invitation_phone(&f.phone, f.user)
            .await
            .expect("deleted")
            .is_none());
        assert!(s
            .invitation_consent(&f.phone, &f.receiver, f.user)
            .await
            .expect("cascade")
            .is_none());
    })
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_atomic_foreground_dedupe_cooldown_and_revocation() {
    for_each_backend(|s, _| async move {
        let f = Fixture::new(s.as_ref()).await;
        assert!(s
            .save_invitation_consent(f.consent(0, true))
            .await
            .expect("enable consent"));
        let foreground = id();
        assert!(s
            .set_invitation_availability(&f.phone, f.user, &f.phash, 1, true, true)
            .await
            .expect("explicit OS permission and resident Start"));
        let first = f.candidate(s.as_ref(), foreground.clone(), f.now).await;
        assert_eq!(
            s.admit_invitation(first.clone())
                .await
                .expect("global switches off"),
            InvitationAdmission::Refused
        );
        s.put_setting("cinema.remote_control", "1")
            .await
            .expect("remote feature");
        s.put_setting("cinema.remote_invitations", "1")
            .await
            .expect("invitation feature");
        assert!(!s
            .save_invitation_consent(f.consent(1, false))
            .await
            .expect("stale phone generation"));
        assert!(s
            .set_invitation_availability(&f.phone, f.user, &f.phash, 2, false, false)
            .await
            .expect("permission denied"));
        assert!(
            s.invitation_consent(&f.phone, &f.receiver, f.user)
                .await
                .expect("saved consent")
                .expect("consent")
                .enabled
        );
        let denied = f.candidate(s.as_ref(), foreground.clone(), f.now).await;
        assert_eq!(
            s.admit_invitation(denied).await.expect("permission gate"),
            InvitationAdmission::Refused
        );
        assert!(s
            .set_invitation_availability(&f.phone, f.user, &f.phash, 3, true, true)
            .await
            .expect("explicit restored readiness"));
        let first = f.candidate(s.as_ref(), foreground.clone(), f.now).await;
        let mut concurrent = first.clone();
        concurrent.id = id();
        let (a, b) = tokio::join!(
            s.admit_invitation(first.clone()),
            s.admit_invitation(concurrent)
        );
        let outcomes = [a.expect("admit A"), b.expect("admit B")];
        assert_eq!(
            outcomes
                .iter()
                .filter(|x| **x == InvitationAdmission::Admitted)
                .count(),
            1
        );
        let cooldown = f.candidate(s.as_ref(), id(), f.now + 1799).await;
        assert_eq!(
            s.admit_invitation(cooldown).await.expect("cooldown"),
            InvitationAdmission::Refused
        );
        let later = f.candidate(s.as_ref(), id(), f.now + 1800).await;
        assert_eq!(
            s.admit_invitation(later).await.expect("new foreground"),
            InvitationAdmission::Admitted
        );
        let retry = f.candidate(s.as_ref(), foreground, f.now + 3600).await;
        assert_eq!(
            s.admit_invitation(retry)
                .await
                .expect("old foreground remains deduped"),
            InvitationAdmission::Refused
        );
        let queued = f.candidate(s.as_ref(), id(), f.now + 3600).await;
        assert!(s
            .save_invitation_consent(f.consent_at(1, false, 4))
            .await
            .expect("OFF"));
        assert_eq!(
            s.admit_invitation(queued)
                .await
                .expect("concurrent consent fencing"),
            InvitationAdmission::Refused
        );
        assert!(s
            .save_invitation_consent(f.consent_at(2, true, 4))
            .await
            .expect("reenable"));
        let revoked = f.candidate(s.as_ref(), id(), f.now + 3600).await;
        s.delete_token(&f.phone_digest).await.expect("logout");
        assert_eq!(
            s.admit_invitation(revoked)
                .await
                .expect("current login required at admission"),
            InvitationAdmission::Refused
        );
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_adjunct_schema_repeats_at_81_82_and_refuses_denied_partial_shape() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("client");
    // Recreate the exact previous adjunct, independently of Source81/82.
    for (name, ddl) in objects().into_iter().rev() {
        let kind = if ddl.starts_with("CREATE TABLE") {
            "TABLE"
        } else if ddl.starts_with("CREATE TRIGGER") {
            "TRIGGER"
        } else {
            "INDEX"
        };
        client
            .execute(format!("DROP {kind} {name}"), hiqlite::params!())
            .await
            .expect("old adjunct fixture");
    }
    for (_, ddl) in schema_objects(SCHEMA_V1) {
        client
            .execute(ddl, hiqlite::params!())
            .await
            .expect("exact v1 objects");
    }
    client
        .execute(
            "INSERT INTO invitation_schema VALUES(1,1)",
            hiqlite::params!(),
        )
        .await
        .expect("v1 marker");
    assert!(
        store
            .validation_install_invitation_schema(true)
            .await
            .is_err(),
        "upgrade needs admission"
    );
    store
        .validation_install_invitation_schema(false)
        .await
        .expect("v1 to v2 upgrade");
    for version in [81_i64, 82] {
        client
            .execute(
                "UPDATE cluster_meta SET schema_version=$1 WHERE singleton=1",
                hiqlite::params!(version),
            )
            .await
            .expect("baseline");
        store
            .validation_install_invitation_schema(false)
            .await
            .expect("repeat adjunct install");
        store
            .validation_install_invitation_schema(false)
            .await
            .expect("repeat startup");
    }
    assert!(
        store
            .validation_install_invitation_schema(true)
            .await
            .is_err(),
        "even current adjunct needs migration admission"
    );
    client
        .execute("DELETE FROM invitation_schema", hiqlite::params!())
        .await
        .expect("bad marker");
    assert!(store
        .validation_install_invitation_schema(false)
        .await
        .is_err());
    client
        .execute(
            "INSERT INTO invitation_schema VALUES(1,4)",
            hiqlite::params!(),
        )
        .await
        .expect("restore marker");
    client
        .execute("DROP INDEX invitation_events_pending", hiqlite::params!())
        .await
        .expect("partial shape");
    assert!(store
        .validation_install_invitation_schema(false)
        .await
        .is_err());
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_cooldown_failure_rolls_back_event_on_real_voters() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("client");
    let f = Fixture::new(&store).await;
    store
        .put_setting("cinema.remote_control", "1")
        .await
        .expect("remote");
    store
        .put_setting("cinema.remote_invitations", "1")
        .await
        .expect("invitations");
    assert!(store
        .save_invitation_consent(f.consent(0, true))
        .await
        .expect("consent"));
    client.execute("CREATE TRIGGER invitation_fixture_cooldown_abort BEFORE INSERT ON invitation_cooldowns BEGIN SELECT RAISE(ABORT,'fixture cooldown denied'); END",hiqlite::params!()).await.expect("paused accounting failure fixture");
    assert!(store
        .set_invitation_availability(&f.phone, f.user, &f.phash, 1, true, true)
        .await
        .expect("resident Start"));
    let candidate = f.candidate(&store, id(), f.now).await;
    assert!(
        store.admit_invitation(candidate.clone()).await.is_err(),
        "second statement failure cannot report Admitted"
    );
    let mut rows = client
        .query_consistent(
            "SELECT last_revision FROM invitation_phones WHERE id=$1",
            hiqlite::params!(f.phone.clone()),
        )
        .await
        .expect("rolled-back cursor");
    assert_eq!(
        rows[0].get::<i64>("last_revision"),
        0,
        "cooldown failure also rolls back high water"
    );
    client
        .execute(
            "DROP TRIGGER invitation_fixture_cooldown_abort",
            hiqlite::params!(),
        )
        .await
        .expect("remove failure");
    assert_eq!(
        store
            .admit_invitation(candidate)
            .await
            .expect("retry after rollback"),
        InvitationAdmission::Admitted,
        "event insertion rolled back with cooldown failure"
    );
    let mut rows = client
        .query_consistent(
            "SELECT last_revision FROM invitation_phones WHERE id=$1",
            hiqlite::params!(f.phone),
        )
        .await
        .expect("committed cursor");
    assert_eq!(
        rows[0].get::<i64>("last_revision"),
        1,
        "only successful admission advances high water"
    );
}

#[tokio::test]
async fn invitations_receiver_cleanup_does_not_rewind_live_phone_cursor() {
    let directory = tempfile::tempdir().expect("database directory");
    let path = directory.path().join("cursor.db");
    let store = SqliteStore::open(&path).expect("store");
    let f = Fixture::new(&store).await;
    store
        .put_setting("cinema.remote_control", "1")
        .await
        .expect("remote enabled");
    store
        .put_setting("cinema.remote_invitations", "1")
        .await
        .expect("invitations enabled");
    assert!(store
        .save_invitation_consent(f.consent(0, true))
        .await
        .expect("consent"));
    assert!(store
        .set_invitation_availability(&f.phone, f.user, &f.phash, 1, true, true)
        .await
        .expect("readiness"));
    assert_eq!(
        store
            .admit_invitation(f.candidate(&store, id(), f.now).await)
            .await
            .expect("first"),
        InvitationAdmission::Admitted
    );
    let raw = rusqlite::Connection::open(path).expect("controlled cleanup");
    raw.execute_batch("PRAGMA foreign_keys=ON;")
        .expect("FK cleanup");
    assert_eq!(
        raw.query_row(
            "SELECT last_revision FROM invitation_phones WHERE id=$1",
            [&f.phone],
            |r| r.get::<_, i64>(0)
        )
        .expect("cursor"),
        1
    );
    raw.execute("DELETE FROM remote_receivers WHERE id=$1", [&f.receiver])
        .expect("physical receiver cleanup");
    assert_eq!(
        raw.query_row("SELECT count(*) FROM invitation_events", [], |r| r
            .get::<_, i64>(0))
            .expect("events removed"),
        0
    );
    assert_eq!(
        raw.query_row(
            "SELECT last_revision FROM invitation_phones WHERE id=$1",
            [&f.phone],
            |r| r.get::<_, i64>(0)
        )
        .expect("surviving cursor"),
        1
    );
    store
        .create_remote_receiver(NewRemoteReceiver {
            receiver: RemoteReceiver {
                id: f.receiver.clone(),
                user_id: f.user,
                name: "TV".into(),
                platform: "web".into(),
                created_at: f.now,
            },
            secret_hash: f.rhash.clone(),
        })
        .await
        .expect("replacement synthetic receiver");
    store
        .create_remote_grant(NewRemoteGrant {
            grant: RemoteGrant {
                id: f.grant.clone(),
                receiver_id: f.receiver.clone(),
                name: "phone".into(),
                created_at: f.now,
            },
            user_id: f.user,
            secret_hash: f.ghash.clone(),
        })
        .await
        .expect("new grant");
    assert!(store
        .save_invitation_consent(f.consent_at(0, true, 2))
        .await
        .expect("new consent"));
    assert_eq!(
        store
            .admit_invitation(f.candidate(&store, id(), f.now + 1800).await)
            .await
            .expect("next"),
        InvitationAdmission::Admitted
    );
    assert_eq!(
        raw.query_row(
            "SELECT revision FROM invitation_events WHERE phone_id=$1",
            [&f.phone],
            |r| r.get::<_, i64>(0)
        )
        .expect("next event cursor"),
        2
    );
    assert_eq!(
        raw.query_row(
            "SELECT last_revision FROM invitation_phones WHERE id=$1",
            [&f.phone],
            |r| r.get::<_, i64>(0)
        )
        .expect("phone cursor"),
        2
    );
    store
        .put_setting("cinema.remote_invitations", "0")
        .await
        .expect("disabled");
    assert_eq!(
        store
            .admit_invitation(f.candidate(&store, id(), f.now + 3600).await)
            .await
            .expect("refused"),
        InvitationAdmission::Refused
    );
    assert_eq!(
        raw.query_row(
            "SELECT last_revision FROM invitation_phones WHERE id=$1",
            [&f.phone],
            |r| r.get::<_, i64>(0)
        )
        .expect("refusal preserves cursor"),
        2
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_explicit_rebind_fences_consent_and_preserves_history() {
    for_each_backend(|store, _| async move {
        let f = Fixture::new(store.as_ref()).await;
        assert!(store
            .save_invitation_consent(f.consent(0, true))
            .await
            .expect("consent"));
        assert!(store
            .set_invitation_availability(&f.phone, f.user, &f.phash, 1, true, true)
            .await
            .expect("readiness"));
        store
            .put_setting("cinema.remote_control", "1")
            .await
            .expect("remote enabled");
        store
            .put_setting("cinema.remote_invitations", "1")
            .await
            .expect("invitation enabled");
        let event = f.candidate(store.as_ref(), id(), f.now).await;
        let event_id = event.id.clone();
        assert_eq!(
            store.admit_invitation(event).await.expect("admit"),
            InvitationAdmission::Admitted
        );
        assert_eq!(
            store
                .invitation_revision(&f.phone, f.user)
                .await
                .expect("cursor"),
            Some(1)
        );
        assert_eq!(
            store
                .invitation_phone_binding(&f.phone, f.user, &f.phash)
                .await
                .expect("binding"),
            Some(f.phone_digest.clone())
        );
        assert!(store
            .invitation_phone_binding(&f.phone, f.user, "")
            .await
            .is_err());
        assert!(store
            .invitation_phone_binding(&f.phone, f.user, &auth::hash_token("wrong"))
            .await
            .expect("wrong proof")
            .is_none());
        let scopes = store
            .invitation_receiver_scopes(&f.receiver, f.user)
            .await
            .expect("bounded scopes");
        assert_eq!(scopes.len(), 1);
        assert!(scopes[0].consent.enabled);
        assert_eq!(scopes[0].phone_digest, f.phone_digest);
        assert!(store
            .invitation_event(&f.phone, f.user, &event_id, f.now)
            .await
            .expect("current event")
            .is_some());
        let digest = auth::hash_token("explicit replacement human login");
        store
            .create_token(&digest, f.user, None)
            .await
            .expect("replacement Native login");
        let inventory = store
            .list_tokens_for_user(f.user)
            .await
            .expect("activity before");
        assert!(store
            .rebind_invitation_phone(&f.phone, f.user, &f.phash, 2, &digest)
            .await
            .expect("explicit rebind"));
        assert!(!store
            .rebind_invitation_phone(&f.phone, f.user, &f.phash, 2, &f.phone_digest)
            .await
            .expect("stale rebind"));
        let phone = store
            .invitation_phone(&f.phone, f.user)
            .await
            .expect("phone")
            .expect("row");
        assert_eq!(phone.generation, 3);
        assert!(!phone.permission_granted);
        assert!(!phone.resident_active);
        let consent = store
            .invitation_consents(&f.phone, f.user, "")
            .await
            .expect("consent page");
        assert_eq!(consent.len(), 1);
        assert!(!consent[0].enabled);
        assert_eq!(consent[0].generation, 2);
        assert_eq!(
            store
                .invitation_revision(&f.phone, f.user)
                .await
                .expect("surviving high water"),
            Some(1)
        );
        assert!(store
            .invitation_event(&f.phone, f.user, &event_id, f.now)
            .await
            .expect("old event fenced")
            .is_none());
        assert!(store
            .invitation_events(&f.phone, f.user, 0, f.now)
            .await
            .expect("fenced page")
            .is_empty());
        assert_eq!(
            store
                .invitation_phone_binding(&f.phone, f.user, &f.phash)
                .await
                .expect("new binding"),
            Some(digest)
        );
        assert_eq!(
            inventory,
            store
                .list_tokens_for_user(f.user)
                .await
                .expect("no-touch runtime reads")
        );
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_transport_reference_survives_account_cleanup() {
    for_each_backend(|store, _| async move {
        let f = Fixture::new(store.as_ref()).await;
        let mut consent = f.consent(0, true);
        consent.enable = Some((f.grant.clone(), f.ghash.clone(), InvitationTransport::Fcm));
        assert!(store
            .save_invitation_consent(consent)
            .await
            .expect("provider consent"));
        let reference = BrokerReference {
            ticket_id: id(),
            scope_hash: "b".repeat(64),
        };
        let start = StartInvitationTransport {
            phone_id: f.phone.clone(),
            receiver_id: f.receiver.clone(),
            user_id: f.user,
            phone_hash: f.phash.clone(),
            grant_id: f.grant.clone(),
            grant_hash: f.ghash.clone(),
            login_digest: f.phone_digest.clone(),
            expected_phone_generation: 1,
            expected_consent_generation: 1,
            reference: reference.clone(),
            provider_available: true,
            now: f.now,
        };
        let mut wrong = start.clone();
        wrong.phone_hash = "c".repeat(64);
        assert!(!store
            .start_invitation_transport(wrong)
            .await
            .expect("wrong proof rejected"));
        assert!(store
            .start_invitation_transport(start.clone())
            .await
            .expect("reserve before unknown external issuance"));
        assert!(!store
            .start_invitation_transport(start)
            .await
            .expect("stale start cannot rotate"));
        assert_eq!(
            store
                .invitation_cleanup_budget(f.user)
                .await
                .expect("one reserved ref"),
            1
        );
        let rows = store
            .invitation_consents(&f.phone, f.user, "")
            .await
            .expect("consent metadata");
        assert_eq!(rows[0].generation, 2);
        assert_eq!(rows[0].transport_generation, 1);
        assert_eq!(
            rows[0].broker_ticket,
            Some(reference.encode().expect("reference"))
        );
        assert!(store
            .delete_user(f.user)
            .await
            .expect("account deletion with provider offline"));
        let work = store
            .invitation_revocations()
            .await
            .expect("global durable cleanup");
        assert_eq!(work.len(), 1);
        assert_eq!(work[0].enrollment_id, reference.ticket_id);
        store
            .record_invitation_revocation(&work[0].id, false)
            .await
            .expect("unknown broker response retains cleanup");
        assert_eq!(
            store.invitation_revocations().await.expect("retained work")[0].attempts,
            1
        );
        store
            .record_invitation_revocation(&work[0].id, true)
            .await
            .expect("tombstone acknowledgement");
        assert!(store
            .invitation_revocations()
            .await
            .expect("ack cleanup")
            .is_empty());
    })
    .await;
}

#[tokio::test]
async fn invitations_reserved_limit_refuses_start_but_off_and_delete_complete() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("capacity.db");
    let store = SqliteStore::open(&path).expect("store");
    let f = Fixture::new(&store).await;
    let mut consent = f.consent(0, true);
    consent.enable = Some((f.grant.clone(), f.ghash.clone(), InvitationTransport::Fcm));
    assert!(store
        .save_invitation_consent(consent)
        .await
        .expect("consent"));
    let reference = BrokerReference {
        ticket_id: id(),
        scope_hash: "b".repeat(64),
    };
    let mut start = StartInvitationTransport {
        phone_id: f.phone.clone(),
        receiver_id: f.receiver.clone(),
        user_id: f.user,
        phone_hash: f.phash.clone(),
        grant_id: f.grant.clone(),
        grant_hash: f.ghash.clone(),
        login_digest: f.phone_digest.clone(),
        expected_phone_generation: 1,
        expected_consent_generation: 1,
        reference: reference.clone(),
        provider_available: true,
        now: f.now,
    };
    assert!(store
        .start_invitation_transport(start.clone())
        .await
        .expect("reserve live identity"));
    let raw = rusqlite::Connection::open(path).expect("bounded capacity setup");
    raw.execute("WITH RECURSIVE n(x) AS(SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<99999) INSERT INTO invitation_broker_revocations SELECT json_object('ticket_id',printf('00000000-0000-4000-8000-%012x',x),'scope_hash',$2),$1,printf('00000000-0000-4000-8000-%012x',x),1,0,9223372036854775807,0 FROM n",rusqlite::named_params!{"$1":f.user,"$2":"b".repeat(64)}).expect("reserved cleanup capacity");
    assert_eq!(
        store.invitation_cleanup_budget(f.user).await.expect("cap"),
        100000
    );
    // Execute the exact reservation statements at capacity to model the budget
    // filling after Hiqlite's advisory read and before its replicated txn.
    let replacement = BrokerReference {
        ticket_id: id(),
        scope_hash: "b".repeat(64),
    }
    .encode()
    .expect("reference");
    let values = rusqlite::params![
        f.phone,
        f.receiver,
        f.user,
        f.phash,
        f.grant,
        f.ghash,
        1,
        2,
        replacement,
        "pending",
        f.phone_digest,
        f.now
    ];
    let tx = raw
        .unchecked_transaction()
        .expect("capacity race transaction");
    assert_eq!(
        tx.execute(&start_cleanup_query(), values)
            .expect("cleanup refuses at cap"),
        0
    );
    assert_eq!(
        tx.execute(&start_query(), values)
            .expect("reserve refuses at cap"),
        0
    );
    tx.commit().expect("no-effect transaction");
    start.expected_consent_generation = 2;
    start.reference.ticket_id = id();
    assert!(!store
        .start_invitation_transport(start)
        .await
        .expect("new issuance refused"));
    let saved = store
        .invitation_consents(&f.phone, f.user, "")
        .await
        .expect("saved preference");
    assert!(saved[0].enabled);
    assert_eq!(
        saved[0].broker_ticket,
        Some(reference.encode().expect("original reference"))
    );
    assert!(store
        .save_invitation_consent(f.consent(2, false))
        .await
        .expect("OFF at cap"));
    store
        .revoke_invitation_phone(&f.phone, f.user)
        .await
        .expect("DELETE while broker offline at cap");
    assert_eq!(
        store
            .invitation_cleanup_budget(f.user)
            .await
            .expect("transferred reservation"),
        100000
    );
    assert_eq!(
        raw.query_row(
            "SELECT enrollment_id FROM invitation_broker_revocations WHERE id=$1",
            [reference.encode().expect("reference")],
            |r| r.get::<_, String>(0)
        )
        .expect("known cleanup retained"),
        reference.ticket_id
    );
    assert_eq!(
        raw.query_row("SELECT count(*) FROM invitation_phones", [], |r| r
            .get::<_, i64>(0))
            .expect("phone removed"),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_attempt_is_single_use_and_rechecks_current_authority() {
    for_each_backend(|store, _| async move {
        let f = Fixture::new(store.as_ref()).await;
        store
            .put_setting("cinema.remote_control", "1")
            .await
            .expect("control");
        store
            .put_setting("cinema.remote_invitations", "1")
            .await
            .expect("invitations");
        assert!(store
            .save_invitation_consent(f.consent(0, true))
            .await
            .expect("resident consent"));
        assert!(store
            .set_invitation_availability(&f.phone, f.user, &f.phash, 1, true, true)
            .await
            .expect("resident ready"));
        let first = f.candidate(store.as_ref(), id(), f.now).await;
        assert_eq!(
            store.admit_invitation(first.clone()).await.expect("admit"),
            InvitationAdmission::Admitted
        );
        assert!(store
            .attempt_invitation(first.clone())
            .await
            .expect("persist attempt before effect"));
        assert!(!store
            .attempt_invitation(first.clone())
            .await
            .expect("unknown never retries"));
        store
            .finish_invitation(&first.id, InvitationDispatchOutcome::Unknown)
            .await
            .expect("unknown outcome");
        store
            .finish_invitation(&first.id, InvitationDispatchOutcome::Accepted)
            .await
            .expect("later result cannot rewrite unknown");
        assert_eq!(
            store
                .invitation_event(&f.phone, f.user, &first.id, f.now)
                .await
                .expect("event")
                .expect("live event")
                .outcome
                .as_deref(),
            Some("unknown")
        );
        let second = f.candidate(store.as_ref(), id(), f.now + 1800).await;
        assert_eq!(
            store
                .admit_invitation(second.clone())
                .await
                .expect("next foreground admit"),
            InvitationAdmission::Admitted
        );
        store
            .delete_token(&f.phone_digest)
            .await
            .expect("logout after admission");
        assert!(!store
            .attempt_invitation(second)
            .await
            .expect("authoritative logout fences effect"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_scope_replacement_fences_issuance_and_retains_cleanup() {
    for_each_backend(|store, _| async move {
        let f = Fixture::new(store.as_ref()).await;
        let mut consent = f.consent(0, true);
        consent.enable = Some((f.grant.clone(), f.ghash.clone(), InvitationTransport::Fcm));
        assert!(store
            .save_invitation_consent(consent)
            .await
            .expect("consent"));
        let reference = BrokerReference {
            ticket_id: id(),
            scope_hash: "b".repeat(64),
        };
        let start = StartInvitationTransport {
            phone_id: f.phone.clone(),
            receiver_id: f.receiver.clone(),
            user_id: f.user,
            phone_hash: f.phash.clone(),
            grant_id: f.grant.clone(),
            grant_hash: f.ghash.clone(),
            login_digest: f.phone_digest.clone(),
            expected_phone_generation: 1,
            expected_consent_generation: 1,
            reference: reference.clone(),
            provider_available: true,
            now: f.now,
        };
        assert!(store
            .start_invitation_transport(start.clone())
            .await
            .expect("reserve"));
        let health = store
            .invitation_broker_health(&reference.scope_hash)
            .await
            .expect("scope snapshot");
        assert_eq!(
            (health.budget, health.invalid, health.mismatched),
            (1, 0, 0)
        );
        let replacement = "c".repeat(64);
        let mut other = start;
        other.expected_consent_generation = 2;
        other.reference = BrokerReference {
            ticket_id: id(),
            scope_hash: replacement.clone(),
        };
        assert!(!store
            .start_invitation_transport(other)
            .await
            .expect("replacement refused"));
        assert!(store
            .invitation_revocations()
            .await
            .expect("no side effect on refused replacement")
            .is_empty());
        let old = store
            .invitation_consent(&f.phone, &f.receiver, f.user)
            .await
            .expect("old consent")
            .expect("row");
        assert_eq!(old.generation, 2);
        assert_eq!(
            old.broker_ticket.as_deref(),
            Some(reference.encode().expect("reference").as_str())
        );
        store
            .revoke_invitation_phone(&f.phone, f.user)
            .await
            .expect("delete transfers reservation");
        let health = store
            .invitation_broker_health(&replacement)
            .await
            .expect("replacement still fenced");
        assert_eq!(
            (health.budget, health.invalid, health.mismatched),
            (1, 0, 1)
        );
        assert_eq!(
            store
                .invitation_revocations()
                .await
                .expect("cleanup retained")
                .len(),
            1
        );
        assert!(store.invitation_broker_health("bad").await.is_err());
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invitations_late_cleanup_cannot_clear_a_newer_ticket() {
    for_each_backend(|store, _| async move {
        let f = Fixture::new(store.as_ref()).await;
        let mut consent = f.consent(0, true);
        consent.enable = Some((f.grant.clone(), f.ghash.clone(), InvitationTransport::Fcm));
        assert!(store
            .save_invitation_consent(consent)
            .await
            .expect("consent"));
        let old = BrokerReference {
            ticket_id: id(),
            scope_hash: "b".repeat(64),
        };
        let mut start = StartInvitationTransport {
            phone_id: f.phone.clone(),
            receiver_id: f.receiver.clone(),
            user_id: f.user,
            phone_hash: f.phash.clone(),
            grant_id: f.grant.clone(),
            grant_hash: f.ghash.clone(),
            login_digest: f.phone_digest.clone(),
            expected_phone_generation: 1,
            expected_consent_generation: 1,
            reference: old.clone(),
            provider_available: true,
            now: f.now,
        };
        assert!(store
            .start_invitation_transport(start.clone())
            .await
            .expect("first ticket"));
        let newer = BrokerReference {
            ticket_id: id(),
            scope_hash: old.scope_hash.clone(),
        };
        start.expected_consent_generation = 2;
        start.reference = newer.clone();
        assert!(store
            .start_invitation_transport(start)
            .await
            .expect("new ticket"));
        assert!(!store
            .queue_invitation_reference(&f.enrollment, f.user, old, f.now)
            .await
            .expect("late old failure"));
        let current = store
            .invitation_consent(&f.phone, &f.receiver, f.user)
            .await
            .expect("current")
            .expect("row");
        assert_eq!(
            current.broker_ticket,
            Some(newer.encode().expect("reference"))
        );
        assert_eq!(current.transport_status, "pending");
        assert!(store
            .queue_invitation_reference(&f.enrollment, f.user, newer, f.now)
            .await
            .expect("current failure"));
        let current = store
            .invitation_consent(&f.phone, &f.receiver, f.user)
            .await
            .expect("current")
            .expect("row");
        assert!(current.broker_ticket.is_none());
        assert!(current.enabled);
        let work = store.invitation_revocations().await.expect("durable work");
        assert_eq!(work.len(), 2);
        assert!(work
            .iter()
            .all(|w| w.attempts == 0 && w.created_at == f.now));
    })
    .await;
}

#[tokio::test]
async fn invitations_cleanup_refuses_mismatched_legacy_held_reference() {
    let directory = tempfile::tempdir().expect("fixture");
    let path = directory.path().join("legacy.db");
    let store = SqliteStore::open(&path).expect("store");
    let f = Fixture::new(&store).await;
    let mut consent = f.consent(0, true);
    consent.enable = Some((f.grant.clone(), f.ghash.clone(), InvitationTransport::Fcm));
    assert!(store
        .save_invitation_consent(consent)
        .await
        .expect("consent"));
    let reference = BrokerReference {
        ticket_id: id(),
        scope_hash: "b".repeat(64),
    };
    let encoded = reference.encode().expect("reference");
    let other = id();
    let raw = rusqlite::Connection::open(path).expect("legacy seam");
    raw.execute(
        "UPDATE invitation_consents SET broker_ticket=?1,broker_enrollment=?2 WHERE id=?3",
        rusqlite::params![encoded, other, f.enrollment],
    )
    .expect("legacy mismatch");
    assert!(store
        .queue_invitation_cleanup(&f.enrollment, f.user, f.now)
        .await
        .expect_err("legacy cleanup refuses")
        .to_string()
        .contains("migration_remediation"));
    assert!(store
        .queue_invitation_reference(&f.enrollment, f.user, reference, f.now)
        .await
        .expect_err("CAS cleanup refuses")
        .to_string()
        .contains("migration_remediation"));
    let current = store
        .invitation_consent(&f.phone, &f.receiver, f.user)
        .await
        .expect("retained")
        .expect("row");
    assert_eq!(current.broker_ticket, Some(encoded));
    assert_eq!(current.broker_enrollment, Some(other));
    assert!(store
        .invitation_revocations()
        .await
        .expect("no wrong tombstone")
        .is_empty());
}
