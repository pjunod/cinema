use super::*;
use plurx_core::{
    auth,
    store::{
        invitations::*,
        remote::{NewRemoteGrant, NewRemoteReceiver, RemoteGrant, RemoteReceiver},
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
            "INSERT INTO invitation_schema VALUES(1,1)",
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
}
