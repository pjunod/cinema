use super::*;
#[cfg(feature = "hiqlite-contract-tests")]
use plurx_core::store::JellyfinLoginStore;
use plurx_core::store::{
    JellyfinClientFamily as Family, JellyfinLoginWrite, TokenAudience, TokenAuthentication,
};

/// The login a token authenticates as under the audience that issued it.
/// `user_for_token` is the native audience, which refuses every
/// compatibility login by design, so these liveness checks read both.
trait LoginLookup {
    async fn login_for_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<plurx_core::domain::User>, plurx_core::error::StoreError>;
}
impl<S: Store + ?Sized> LoginLookup for S {
    async fn login_for_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<plurx_core::domain::User>, plurx_core::error::StoreError> {
        for audience in [TokenAudience::JellyfinCompatibility, TokenAudience::Native] {
            if let TokenAuthentication::Authenticated(user) =
                self.authenticate_token_for(token_hash, audience).await?
            {
                return Ok(Some(user));
            }
        }
        Ok(None)
    }
}
fn digest(label: &str) -> String {
    plurx_core::auth::hash_token(label)
}
fn write(user_id: i64, token: &str, device: &str, family: Family) -> JellyfinLoginWrite {
    JellyfinLoginWrite {
        token_hash: digest(token),
        user_id,
        device_digest: digest(device),
        client_family: family,
        device_label: Some("same visible label".into()),
        expected_password_hash: "hash".into(),
        created_at: unix_seconds(),
    }
}
#[tokio::test]
async fn jellyfin_login_replacement_is_atomic_scoped_and_password_fenced() {
    for_each_backend(|store, backend| async move {
        let (uid, file) = seed_file(&store, "jellyfin-login").await;
        let other = store
            .create_user("other-login-user", "hash", false)
            .await
            .expect("other user");
        let native = digest("native-reader");
        store
            .create_token(&native, uid, Some("same visible label"))
            .await
            .expect("native reader");
        let grant = digest("native-reader-grant");
        store
            .create_file_grant(plurx_core::store::NewFileGrant {
                id: "native-reader".into(),
                token_hash: grant.clone(),
                file_id: file,
                user_id: uid,
                source_token_hash: native.clone(),
                created_at: unix_seconds(),
                expires_at: unix_seconds() + 3600,
            })
            .await
            .expect("native reader grant");
        for w in [
            write(uid, "old", "shared-device", Family::Infuse),
            write(uid, "android", "shared-device", Family::AndroidTv),
            write(uid, "other-device", "other-device", Family::Infuse),
            write(other.id, "other-user", "shared-device", Family::Infuse),
        ] {
            assert!(store
                .replace_jellyfin_login(w, None)
                .await
                .expect("initial scoped login"));
        }
        assert!(store
            .replace_jellyfin_login(write(uid, "new", "shared-device", Family::Infuse), None)
            .await
            .expect("replace own scope"));
        assert!(store
            .login_for_token(&digest("old"))
            .await
            .expect("old lookup")
            .is_none());
        for token in [
            "native-reader",
            "android",
            "other-device",
            "other-user",
            "new",
        ] {
            assert!(
                store
                    .login_for_token(&digest(token))
                    .await
                    .expect("preserved lookup")
                    .is_some(),
                "{backend}: replacement preserves {token}"
            );
        }
        let (a, b) = tokio::join!(
            store.replace_jellyfin_login(
                write(uid, "race-a", "shared-device", Family::Infuse),
                None
            ),
            store.replace_jellyfin_login(
                write(uid, "race-b", "shared-device", Family::Infuse),
                None
            )
        );
        assert!(a.expect("race a"));
        assert!(b.expect("race b"));
        let alive_a = store
            .login_for_token(&digest("race-a"))
            .await
            .expect("race a lookup")
            .is_some();
        let alive_b = store
            .login_for_token(&digest("race-b"))
            .await
            .expect("race b lookup")
            .is_some();
        assert_ne!(
            alive_a, alive_b,
            "{backend}: concurrent replacements retain one token"
        );
        let winner = if alive_a { "race-a" } else { "race-b" };
        assert!(store
            .login_for_token(&digest("new"))
            .await
            .expect("replaced lookup")
            .is_none());
        // Duplicate incoming digest must roll back without deleting the scope.
        assert!(store
            .replace_jellyfin_login(
                write(uid, "native-reader", "shared-device", Family::Infuse),
                None
            )
            .await
            .is_err());
        assert!(store
            .login_for_token(&digest(winner))
            .await
            .expect("rollback winner")
            .is_some());
        let mut stale = write(uid, "stale-proof", "shared-device", Family::Infuse);
        stale.expected_password_hash = "stale".into();
        assert!(!store
            .replace_jellyfin_login(stale, None)
            .await
            .expect("stale password no-op"));
        assert!(store
            .login_for_token(&digest("stale-proof"))
            .await
            .expect("stale lookup")
            .is_none());
        assert!(store
            .set_password(uid, "changed-hash")
            .await
            .expect("change password"));
        assert!(!store
            .replace_jellyfin_login(
                write(uid, "old-password", "shared-device", Family::Infuse),
                None
            )
            .await
            .expect("password change fences verification"));
        assert!(store
            .login_for_token(&digest(winner))
            .await
            .expect("failed replacement preserves scope")
            .is_some());
        let reader = store
            .file_grant_by_hash(&grant)
            .await
            .expect("reader lookup")
            .expect("reader row");
        assert!(reader.source_active);
        assert_eq!(reader.revoked_at, None);
        // Highest user integer can be reused; its deleted scope must not survive.
        assert!(store
            .delete_user(other.id)
            .await
            .expect("delete other user"));
        let replacement = store
            .create_user("reused-login-user", "hash", false)
            .await
            .expect("reused user");
        assert_eq!(replacement.id, other.id);
        assert!(store
            .replace_jellyfin_login(
                write(
                    replacement.id,
                    "reused-user",
                    "shared-device",
                    Family::Infuse
                ),
                None
            )
            .await
            .expect("new user scope"));
        assert!(store
            .login_for_token(&digest("other-user"))
            .await
            .expect("deleted user token")
            .is_none());
    })
    .await;
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_login_replacement_requires_the_exact_live_cluster_claim() {
    let _case = HIQLITE_CASE.lock().await;
    let cluster = ContractCluster::start().await;
    let client = Client::remote(
        cluster.addresses.clone(),
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("claim client");
    let store = open_contract_hiqlite_store(&cluster).await;
    client.execute("CREATE TABLE IF NOT EXISTS cluster_cache_admin_revocation_leases (singleton INTEGER PRIMARY KEY CHECK(singleton=1),node_id TEXT NOT NULL,claim_id TEXT NOT NULL UNIQUE,expires_at INTEGER NOT NULL,retain_release_receipt INTEGER NOT NULL CHECK(retain_release_receipt IN (0,1))) STRICT",hiqlite::params!()).await.expect("membership-owned claim schema fixture");
    let user = store
        .create_user("claim-login-user", "hash", false)
        .await
        .expect("claim user");
    assert!(store
        .replace_jellyfin_login(
            write(user.id, "before-claim", "device", Family::Infuse),
            None
        )
        .await
        .expect("initial login"));
    let claim =
        plurx_core::cluster::membership::MembershipManager::prepare_cache_admin_revocation_claim(
            "contract-node",
            Duration::from_secs(15),
        )
        .expect("prepare absent claim");
    assert!(!store
        .replace_jellyfin_login(
            write(user.id, "absent-claim", "device", Family::Infuse),
            Some(claim.mutation_claim())
        )
        .await
        .expect("absent claim no-op"));
    assert!(store
        .login_for_token(&digest("before-claim"))
        .await
        .expect("old authority")
        .is_some());
    client.execute("INSERT INTO cluster_cache_admin_revocation_leases(singleton,node_id,claim_id,expires_at,retain_release_receipt) VALUES(1,$1,$2,9000000000000,0)",hiqlite::params!("contract-node",claim.claim_id())).await.expect("install exact claim");
    assert!(store
        .replace_jellyfin_login(
            write(user.id, "under-claim", "device", Family::Infuse),
            Some(claim.mutation_claim())
        )
        .await
        .expect("exact claim replacement"));
    assert!(store
        .login_for_token(&digest("before-claim"))
        .await
        .expect("replaced authority")
        .is_none());
    store
        .set_jellyfin_compatibility(true)
        .await
        .expect("enable guarded claim login");
    let generation = store
        .jellyfin_compatibility_state()
        .await
        .expect("claim switch")
        .generation
        .expect("claim generation");
    assert!(store
        .replace_jellyfin_login_if_enabled(
            write(user.id, "guarded-claim", "device", Family::Infuse),
            Some(claim.mutation_claim()),
            &generation,
        )
        .await
        .expect("enabled exact claim replacement"));
    store
        .set_jellyfin_compatibility(false)
        .await
        .expect("disable guarded claim login");
    assert!(!store
        .replace_jellyfin_login_if_enabled(
            write(user.id, "disabled-claim", "device", Family::Infuse),
            Some(claim.mutation_claim()),
            &generation,
        )
        .await
        .expect("disabled exact claim refused"));
    assert!(store
        .login_for_token(&digest("guarded-claim"))
        .await
        .expect("guarded authority preserved")
        .is_some());
    client
        .execute(
            "DELETE FROM cluster_cache_admin_revocation_leases WHERE claim_id=$1",
            hiqlite::params!(claim.claim_id()),
        )
        .await
        .expect("claim cleanup");
    assert!(!store
        .replace_jellyfin_login(
            write(user.id, "late-claim", "device", Family::Infuse),
            Some(claim.mutation_claim())
        )
        .await
        .expect("cleaned-up claim no-op"));
    assert!(store
        .login_for_token(&digest("guarded-claim"))
        .await
        .expect("current authority")
        .is_some());
    assert!(store
        .login_for_token(&digest("late-claim"))
        .await
        .expect("late authority")
        .is_none());
}

#[cfg(feature = "hiqlite-contract-tests")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn jellyfin_login_import_preserves_replacement_scope_on_another_node() {
    let _case = HIQLITE_CASE.lock().await;
    let source = tempfile::tempdir().expect("import source");
    let path = source
        .path()
        .join(plurx_core::cluster::migration::SQLITE_FILENAME);
    let local = SqliteStore::open(&path).expect("local store");
    local
        .put_setting(plurx_core::store::keys::INSTANCE_ID, CONTRACT_INSTANCE_ID)
        .await
        .expect("instance ID");
    let user = local
        .create_user("import-login-user", "hash", false)
        .await
        .expect("source user");
    local
        .create_token(
            &digest("import-native"),
            user.id,
            Some("same visible label"),
        )
        .await
        .expect("native token");
    assert!(local
        .replace_jellyfin_login(
            write(user.id, "import-old", "import-device", Family::Infuse),
            None
        )
        .await
        .expect("source login"));
    drop(local);
    let prepared = prepare_sqlite_import(source.path()).expect("prepare import");
    let cluster = ContractCluster::start().await;
    let store = open_contract_hiqlite_store(&cluster).await;
    store
        .import_sqlite_backup(
            &prepared.backup_path,
            &prepared.backup_sha256,
            prepared.schema_version,
        )
        .await
        .expect("import login mappings");
    drop(store);
    let mut addresses = cluster.addresses.clone();
    addresses.rotate_left(1);
    let client = Client::remote(
        addresses,
        true,
        true,
        CONTRACT_API_SECRET.to_owned(),
        false,
        None,
    )
    .await
    .expect("alternate node client");
    let store = HiqliteAuthStore::open(
        client,
        &cluster
            ._root
            .path()
            .join("jellyfin-login-import-telemetry.db"),
    )
    .await
    .expect("alternate node store");
    assert!(store
        .replace_jellyfin_login(
            write(user.id, "import-new", "import-device", Family::Infuse),
            None
        )
        .await
        .expect("replace imported login"));
    assert!(store
        .login_for_token(&digest("import-old"))
        .await
        .expect("retired lookup")
        .is_none());
    for token in ["import-native", "import-new"] {
        assert_eq!(
            store
                .login_for_token(&digest(token))
                .await
                .expect("current lookup")
                .expect("current authority")
                .id,
            user.id
        );
    }
}

#[tokio::test]
async fn jellyfin_public_login_requires_exact_enabled_generation_across_off_on() {
    for_each_backend(|store, backend| async move {
        let (uid, _) = seed_file(&store, "jellyfin-generation").await;
        let initial = store
            .jellyfin_compatibility_state()
            .await
            .expect("initial switch");
        assert!(!initial.enabled);
        assert!(initial.generation.is_none());
        store
            .set_jellyfin_compatibility(true)
            .await
            .expect("explicit enable");
        let enabled = store
            .jellyfin_compatibility_state()
            .await
            .expect("enabled switch");
        let generation = enabled.generation.expect("saved generation");
        assert!(enabled.enabled);
        assert!(store
            .replace_jellyfin_login_if_enabled(
                write(uid, "first-enabled", "device", Family::Infuse),
                None,
                &generation
            )
            .await
            .expect("enabled login"));
        store
            .set_jellyfin_compatibility(false)
            .await
            .expect("explicit disable");
        assert!(
            !store
                .replace_jellyfin_login_if_enabled(
                    write(uid, "late-disabled", "device", Family::Infuse),
                    None,
                    &generation
                )
                .await
                .expect("disabled late login"),
            "{backend}"
        );
        assert!(store
            .login_for_token(&digest("late-disabled"))
            .await
            .expect("no disabled mint")
            .is_none());
        store
            .set_jellyfin_compatibility(true)
            .await
            .expect("reenable");
        let current = store
            .jellyfin_compatibility_state()
            .await
            .expect("reenabled switch")
            .generation
            .expect("new generation");
        assert_ne!(generation, current);
        assert!(!store
            .replace_jellyfin_login_if_enabled(
                write(uid, "late-old-cycle", "device", Family::Infuse),
                None,
                &generation
            )
            .await
            .expect("old cycle refused"));
        assert!(store
            .login_for_token(&digest("first-enabled"))
            .await
            .expect("failed replacement preserved prior scope")
            .is_some());
        assert!(store
            .replace_jellyfin_login_if_enabled(
                write(uid, "current-cycle", "device", Family::Infuse),
                None,
                &current
            )
            .await
            .expect("current login"));
        assert!(store
            .login_for_token(&digest("first-enabled"))
            .await
            .expect("current replacement")
            .is_none());
        assert!(store
            .login_for_token(&digest("current-cycle"))
            .await
            .expect("current token")
            .is_some());
    })
    .await;
}

#[tokio::test]
async fn jellyfin_login_token_audience_separates_native_and_compatibility() {
    for_each_backend(|store, backend| async move {
        let (uid, _) = seed_file(&store, "jellyfin-audience").await;
        let native = digest("audience-native");
        store
            .create_token(&native, uid, Some("native"))
            .await
            .expect("native token");
        assert!(store
            .replace_jellyfin_login(
                write(uid, "audience-compat", "audience-device", Family::Infuse),
                None
            )
            .await
            .expect("compatibility login"));
        let compat = digest("audience-compat");
        for (token, audience, admitted) in [
            (&native, TokenAudience::Native, true),
            (&native, TokenAudience::JellyfinCompatibility, false),
            (&compat, TokenAudience::Native, false),
            (&compat, TokenAudience::JellyfinCompatibility, true),
        ] {
            let result = store
                .authenticate_token_for(token, audience)
                .await
                .expect("audience lookup");
            assert_eq!(
                matches!(result, TokenAuthentication::Authenticated(ref user) if user.id == uid),
                admitted,
                "{backend}: {audience:?}"
            );
        }
        assert!(
            store
                .user_for_token(&compat)
                .await
                .expect("native default")
                .is_none(),
            "{backend}: the native default refuses a compatibility login"
        );
    })
    .await;
}
