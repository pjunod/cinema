//! Compatibility adapters for cluster identity and legacy SQLite import.
//! Native storage and application identity live in [`crate::storage`].

use crate::config::Config;
use crate::error::StoreError;
use crate::secrets::CredentialKey;
#[cfg(all(test, unix))]
use crate::storage::identity::mode_is_owner_only;
#[cfg(test)]
use crate::storage::identity::publish_with_rename_fallback;
use crate::store::Store;
#[cfg(test)]
use crate::store::{SharingStore, SqliteStore};
#[cfg(all(test, unix))]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

pub mod clock;
pub mod coordination;
#[cfg(feature = "hiqlite-store")]
pub mod membership;
pub mod migration;

pub use crate::storage::identity::NODE_ID_FILENAME;
pub const SINGLE_VOTER_RAFT_ID: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterIdentity {
    pub cluster_id: String,
    pub node_id: String,
    pub raft_id: u64,
}

impl From<ClusterIdentity> for crate::storage::LocalIdentity {
    fn from(identity: ClusterIdentity) -> Self {
        Self {
            instance_id: identity.cluster_id,
            node_id: identity.node_id,
        }
    }
}

impl From<&ClusterIdentity> for crate::storage::LocalIdentity {
    fn from(identity: &ClusterIdentity) -> Self {
        Self {
            instance_id: identity.cluster_id.clone(),
            node_id: identity.node_id.clone(),
        }
    }
}

/// Legacy import/recovery result. The coordinator owns the directory lock.
pub struct StoreHandle {
    pub store: Arc<dyn Store>,
    pub identity: ClusterIdentity,
    pub credential_key: Arc<CredentialKey>,
}

/// Compatibility wrapper for the existing import/recovery coordinator.
/// Its caller already holds the exclusive data-directory lock.
pub async fn open_store(config: &Config) -> Result<StoreHandle, StoreError> {
    let native = crate::storage::native::open_store_parts(config).await?;
    Ok(StoreHandle {
        store: native.store,
        identity: ClusterIdentity {
            cluster_id: native.identity.instance_id,
            node_id: native.identity.node_id,
            raft_id: SINGLE_VOTER_RAFT_ID,
        },
        credential_key: native.credential_key,
    })
}

pub fn initialize_identity(
    data_dir: &Path,
    cluster_id: &str,
) -> Result<ClusterIdentity, StoreError> {
    let local = crate::storage::identity::initialize_local_identity(data_dir, cluster_id)?;
    Ok(ClusterIdentity {
        cluster_id: local.instance_id,
        node_id: local.node_id,
        raft_id: SINGLE_VOTER_RAFT_ID,
    })
}

#[cfg(feature = "hiqlite-store")]
pub(crate) fn initialize_join_identity(
    data_dir: &Path,
    cluster_id: &str,
    raft_id: u64,
) -> Result<ClusterIdentity, StoreError> {
    if raft_id == 0 {
        return Err(StoreError::Identity(
            "a joining node received Raft id zero".to_owned(),
        ));
    }
    let local = crate::storage::identity::initialize_join_node_identity(data_dir, cluster_id)?;
    Ok(ClusterIdentity {
        cluster_id: local.instance_id,
        node_id: local.node_id,
        raft_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_directories_seed_distinct_node_ids_from_their_cluster_ids() {
        let first_dir = tempfile::tempdir().expect("first data dir");
        let second_dir = tempfile::tempdir().expect("second data dir");
        let first_cluster = uuid::Uuid::new_v4().to_string();
        let second_cluster = uuid::Uuid::new_v4().to_string();

        let first = initialize_identity(first_dir.path(), &first_cluster).expect("first identity");
        let second =
            initialize_identity(second_dir.path(), &second_cluster).expect("second identity");

        assert_eq!(first.node_id, first.cluster_id);
        assert_eq!(second.node_id, second.cluster_id);
        assert_ne!(first.node_id, second.node_id);
        assert_eq!(first.raft_id, SINGLE_VOTER_RAFT_ID);
    }

    #[test]
    fn restart_preserves_an_existing_local_node_id() {
        let dir = tempfile::tempdir().expect("data dir");
        let cluster_id = uuid::Uuid::new_v4().to_string();
        let first = initialize_identity(dir.path(), &cluster_id).expect("initialize");
        let second = initialize_identity(dir.path(), &cluster_id).expect("restart");

        assert_eq!(first, second);
        assert_eq!(
            std::fs::read_to_string(dir.path().join(NODE_ID_FILENAME))
                .expect("node id file")
                .trim(),
            cluster_id
        );

        #[cfg(unix)]
        assert_eq!(
            std::fs::metadata(dir.path().join(NODE_ID_FILENAME))
                .expect("node id metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn an_existing_joiner_identity_is_not_replaced_by_the_cluster_id() {
        let dir = tempfile::tempdir().expect("data dir");
        let cluster_id = uuid::Uuid::new_v4().to_string();
        let node_id = uuid::Uuid::new_v4().to_string();
        std::fs::write(dir.path().join(NODE_ID_FILENAME), format!("{node_id}\n"))
            .expect("preseed node id");

        let identity = initialize_identity(dir.path(), &cluster_id).expect("identity");
        assert_eq!(identity.cluster_id, cluster_id);
        assert_eq!(identity.node_id, node_id);
    }

    #[test]
    fn equivalent_uuid_spellings_preserve_the_exact_legacy_ownership_key() {
        let dir = tempfile::tempdir().expect("data dir");
        let cluster_id = uuid::Uuid::new_v4().to_string();
        std::fs::write(
            dir.path().join(NODE_ID_FILENAME),
            format!("{}\n", cluster_id.to_uppercase()),
        )
        .expect("preseed non-canonical node id");

        let identity = initialize_identity(dir.path(), &cluster_id).expect("identity");
        assert_eq!(identity.node_id, cluster_id);
    }

    #[test]
    fn legacy_non_uuid_instance_ids_are_seeded_byte_for_byte() {
        let dir = tempfile::tempdir().expect("data dir");
        let identity = initialize_identity(dir.path(), "fixture-instance").expect("identity");

        assert_eq!(identity.cluster_id, "fixture-instance");
        assert_eq!(identity.node_id, "fixture-instance");
        assert_eq!(
            std::fs::read_to_string(dir.path().join(NODE_ID_FILENAME)).expect("node id"),
            "fixture-instance\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn stricter_owner_only_modes_are_accepted() {
        for mode in [0o600, 0o400, 0o200, 0o000] {
            assert!(mode_is_owner_only(mode));
        }
        for mode in [0o640, 0o604, 0o666] {
            assert!(!mode_is_owner_only(mode));
        }
    }

    #[test]
    fn rename_fallback_publishes_complete_bytes_without_replacing_a_winner() {
        let dir = tempfile::tempdir().expect("data dir");
        let temporary = dir.path().join("candidate");
        let destination = dir.path().join(NODE_ID_FILENAME);
        std::fs::write(&temporary, "candidate\n").expect("candidate");

        publish_with_rename_fallback(dir.path(), &temporary, &destination).expect("publish");
        assert_eq!(
            std::fs::read_to_string(&destination).expect("published identity"),
            "candidate\n"
        );

        let loser = dir.path().join("loser");
        std::fs::write(&loser, "loser\n").expect("loser");
        publish_with_rename_fallback(dir.path(), &loser, &destination)
            .expect("existing winner is accepted");
        assert_eq!(
            std::fs::read_to_string(&destination).expect("preserved winner"),
            "candidate\n"
        );
    }

    #[test]
    fn concurrent_initializers_converge_without_overwriting() {
        let dir = tempfile::tempdir().expect("data dir");
        let cluster_id = uuid::Uuid::new_v4().to_string();
        let handles = (0..8)
            .map(|_| {
                let path = dir.path().to_owned();
                let seed = cluster_id.clone();
                std::thread::spawn(move || initialize_identity(&path, &seed))
            })
            .collect::<Vec<_>>();

        for handle in handles {
            let identity = handle.join().expect("thread").expect("identity");
            assert_eq!(identity.node_id, cluster_id);
        }
        let leftovers = std::fs::read_dir(dir.path())
            .expect("list data dir")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(leftovers, 0, "temporary identity files must be removed");
    }

    // ---------------------------------------------------------------------
    // Trakt credential encryption at startup (CLUSTERING-PLAN.md §3.2, §6.6)
    // ---------------------------------------------------------------------

    fn config_for(data_dir: &Path) -> Config {
        Config {
            storage: crate::config::StorageConfig {
                data_dir: data_dir.to_owned(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    /// Write the row a pre-encryption install has on disk: a Trakt bearer pair
    /// stored as plain text, written the way that build wrote it.
    async fn seed_cleartext_trakt_row(data_dir: &Path) {
        use crate::store::UserStore;

        let store = SqliteStore::open(&data_dir.join("plurx.db")).expect("open");
        let user = store
            .create_user("legacy", "hash", true)
            .await
            .expect("user");
        store
            .seed_cleartext_trakt_auth(user.id, "legacy-access-token", "legacy-refresh-token")
            .await
            .expect("legacy cleartext link");
    }

    async fn only_trakt_row(store: &Arc<dyn Store>) -> crate::domain::TraktAuth {
        store
            .list_trakt_auth()
            .await
            .expect("list")
            .into_iter()
            .next()
            .expect("one linked account")
    }

    #[cfg(feature = "hiqlite-store")]
    #[tokio::test]
    async fn sharing_purpose_startup_preserves_keys_and_retains_failed_census_without_minting() {
        use crate::store::{sharing::Backend, SharingStore};
        let directory = tempfile::tempdir().expect("startup directory");
        let config = config_for(directory.path());
        let handle = open_store(&config).await.expect("real fresh startup");
        let master = handle.credential_key.clone();
        drop(handle);
        let store =
            SqliteStore::open(&directory.path().join("plurx.db")).expect("explicit fixture writer");
        let identity = store.sharing_identity(1000).await.expect("Source identity");
        let source = crate::sharing_catalogue_details::CatalogueRevisionKey::generate_sealed(
            &master,
            identity.clone(),
        )
        .expect("Source key");
        let receiver =
            crate::sharing_file_locators::FileLocatorKey::generate_sealed(&master, &identity)
                .expect("B key");
        let mut statements = Vec::new();
        for schema in [
            crate::store::sharing_purpose_keys::SHARING_PURPOSE_KEYS_SCHEMA,
            crate::store::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
            crate::store::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA,
        ] {
            let schema = schema
                .lines()
                .filter(|line| !line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n");
            statements.extend(
                schema
                    .split(';')
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(|sql| {
                        (
                            sql.replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS "),
                            vec![],
                        )
                    }),
            );
        }
        statements.extend([
            (
                "INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)".to_owned(),
                vec![
                    identity.server_id.into(),
                    identity.catalogue_epoch.into(),
                    source.as_stored().to_owned().into(),
                ],
            ),
            (
                "INSERT INTO sharing_file_locator_keys VALUES(1,$1,$2,$3)".to_owned(),
                vec![
                    identity.server_id.into(),
                    identity.catalogue_epoch.into(),
                    receiver.as_stored().to_owned().into(),
                ],
            ),
            (
                "INSERT INTO sharing_purpose_key_installation VALUES(1,$1,$2,$3,'ready',1,1000)"
                    .to_owned(),
                vec![
                    identity.server_id.into(),
                    identity.catalogue_epoch.into(),
                    master.id().to_owned().into(),
                ],
            ),
            (
                "INSERT INTO sharing_purpose_transaction_guard VALUES(1,1)".to_owned(),
                vec![],
            ),
        ]);
        store
            .sharing_txn(statements)
            .await
            .expect("candidate provision only");
        assert!(
            migration::prepare_sqlite_import(directory.path()).is_err(),
            "legacy importer refuses installed purpose material"
        );
        assert!(store
            .sharing_revision_key_rows()
            .await
            .expect("unmodified installed ciphertext")[0]
            .contains(source.as_stored()));
        let reopened = open_store(&config)
            .await
            .expect("actual restart opens all purpose material");
        assert_eq!(reopened.credential_key.id(), master.id());
        drop(reopened);
        let path = config.cluster.credential_key_path(directory.path());
        let retained = directory.path().join("retained-master");
        std::fs::rename(&path, &retained).expect("simulate lost master file");
        assert!(open_store(&config).await.is_err());
        assert!(
            !path.exists(),
            "failed purpose census never creates a replacement master"
        );
        assert_eq!(
            store
                .sharing_read(
                    "SELECT json_array(count(*)) AS payload FROM sharing_purpose_census_intents",
                    vec![]
                )
                .await
                .expect("durable failed ownership"),
            ["[1]"]
        );
        std::fs::rename(&retained, &path).expect("restore exact selected master");
        let restarted = open_store(&config)
            .await
            .expect("same real node replaces its failed boot attempt");
        assert_eq!(restarted.credential_key.id(), master.id());
        assert_eq!(
            store
                .sharing_read(
                    "SELECT json_array(count(*)) AS payload FROM sharing_purpose_census_intents",
                    vec![]
                )
                .await
                .expect("exact successful release"),
            ["[0]"]
        );
        assert!(store
            .sharing_revision_key_rows()
            .await
            .expect("preserved Source row")[0]
            .contains(source.as_stored()));
        drop(restarted);
        std::fs::rename(&path, &retained).expect("lost file plus partial purpose image");
        store
            .sharing_txn(vec![(
                "DELETE FROM sharing_file_locator_keys".into(),
                vec![],
            )])
            .await
            .expect("partial fixture");
        assert!(
            migration::prepare_sqlite_import(directory.path()).is_err(),
            "legacy importer also refuses partial purpose material"
        );
        assert!(store
            .sharing_revision_key_rows()
            .await
            .expect("partial ciphertext retained")[0]
            .contains(source.as_stored()));
        assert!(open_store(&config).await.is_err());
        assert!(
            !path.exists(),
            "layout refusal precedes possible file creation"
        );
    }

    #[tokio::test]
    async fn upgrading_an_install_seals_its_cleartext_trakt_row() {
        let dir = tempfile::tempdir().expect("data dir");
        seed_cleartext_trakt_row(dir.path()).await;

        let handle = open_store(&config_for(dir.path())).await.expect("upgrade");
        let auth = only_trakt_row(&handle.store).await;

        assert!(auth.is_wrapped(), "the upgrade must seal both columns");
        assert!(
            !auth
                .access_token
                .as_stored()
                .contains("legacy-access-token")
                && !auth
                    .refresh_token
                    .as_stored()
                    .contains("legacy-refresh-token"),
            "no cleartext bearer credential may survive in the durable row"
        );
        // Migrating is only half of it: the link has to keep working.
        assert_eq!(
            auth.reveal_access_token(&handle.credential_key)
                .expect("open access")
                .expose(),
            "legacy-access-token"
        );
        assert_eq!(
            auth.reveal_refresh_token(&handle.credential_key)
                .expect("open refresh")
                .expose(),
            "legacy-refresh-token"
        );

        let key_path = dir.path().join(crate::secrets::CREDENTIAL_KEY_FILENAME);
        assert!(key_path.exists(), "the upgrade must publish its key file");
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(&key_path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the credential key must be owner-only");
        }
    }

    /// The bytes on disk are also the compare-and-set operand, so a second boot
    /// re-sealing them would silently invalidate every in-flight rotation — and
    /// sealing an envelope a second time would bury the credential a layer
    /// deeper than anything knows how to open.
    #[tokio::test]
    async fn a_later_boot_leaves_an_already_sealed_row_byte_for_byte_alone() {
        let dir = tempfile::tempdir().expect("data dir");
        seed_cleartext_trakt_row(dir.path()).await;

        let first = open_store(&config_for(dir.path())).await.expect("upgrade");
        let sealed = only_trakt_row(&first.store).await;
        drop(first);

        let second = open_store(&config_for(dir.path())).await.expect("restart");
        let reopened = only_trakt_row(&second.store).await;

        assert_eq!(
            reopened.access_token.as_stored(),
            sealed.access_token.as_stored(),
            "a boot with nothing to migrate must not rewrite the row"
        );
        assert_eq!(
            reopened
                .reveal_access_token(&second.credential_key)
                .expect("open access")
                .expose(),
            "legacy-access-token"
        );
    }

    /// The refusal §3.2 asks for. Both silent alternatives are worse than
    /// stopping: minting a replacement key locks the household out of Trakt
    /// with no error to search for, and reading the row as cleartext undoes the
    /// encryption the operator is relying on.
    #[tokio::test]
    async fn a_missing_key_refuses_to_start_instead_of_falling_back_to_cleartext() {
        let dir = tempfile::tempdir().expect("data dir");
        seed_cleartext_trakt_row(dir.path()).await;

        let migrated = open_store(&config_for(dir.path())).await.expect("upgrade");
        let sealed = only_trakt_row(&migrated.store).await;
        drop(migrated);

        let key_path = dir.path().join(crate::secrets::CREDENTIAL_KEY_FILENAME);
        std::fs::remove_file(&key_path).expect("simulate a lost key file");

        let error = open_store(&config_for(dir.path()))
            .await
            .err()
            .expect("startup must refuse a wrapped row it cannot open");
        let message = error.to_string();
        assert!(
            message.contains("credential key file") && message.contains("refusing to start"),
            "the refusal must name the missing key file: {message}"
        );
        assert!(
            !key_path.exists(),
            "a refusal must not mint a key that cannot open the existing rows"
        );

        // And the credential is still there, still sealed, waiting for the
        // operator to restore the key rather than quietly destroyed.
        let store = SqliteStore::open(&dir.path().join("plurx.db")).expect("reopen");
        let store: Arc<dyn Store> = Arc::new(store);
        let untouched = only_trakt_row(&store).await;
        assert_eq!(
            untouched.access_token.as_stored(),
            sealed.access_token.as_stored()
        );
    }

    #[tokio::test]
    async fn sharing_credentials_alone_refuse_missing_and_wrong_startup_keys() {
        use crate::{secrets::SharingSecretPurpose, sharing::*};
        let dir = tempfile::tempdir().expect("synthetic sharing data dir");
        let handle = open_store(&config_for(dir.path()))
            .await
            .expect("initial startup");
        let identity = handle
            .store
            .sharing_identity(1000)
            .await
            .expect("sharing identity");
        let import = uuid::Uuid::new_v4();
        let credential = handle
            .credential_key
            .seal_sharing(
                SharingSecretPurpose::Credential,
                identity.server_id,
                import,
                "synthetic-sharing-credential",
            )
            .expect("seal sharing credential");
        let claim_secret = handle
            .credential_key
            .seal_sharing(
                SharingSecretPurpose::Claim,
                identity.server_id,
                import,
                "synthetic-sharing-bootstrap",
            )
            .expect("seal sharing bootstrap");
        handle
            .store
            .create_share_import(NewImport {
                id: import,
                source: SharingIdentity {
                    server_id: uuid::Uuid::new_v4(),
                    catalogue_epoch: uuid::Uuid::new_v4(),
                    created_at_ms: 1000,
                },
                source_name: "Synthetic source".into(),
                claim_id: uuid::Uuid::new_v4(),
                credential,
                claim_secret,
                endpoints: vec![Endpoint {
                    ipv4: "100.101.102.103"
                        .parse()
                        .expect("synthetic tailnet address"),
                    ipv6: None,
                    ts_fqdn: "source.example.ts.net".into(),
                    port: 32443,
                    spki_sha256: "a".repeat(64),
                }],
                now_ms: 1000,
            })
            .await
            .expect("persist synthetic import");
        let key_id = handle.credential_key.id().to_owned();
        drop(handle);
        let key_path = dir.path().join(crate::secrets::CREDENTIAL_KEY_FILENAME);
        std::fs::remove_file(&key_path).expect("lose synthetic key");
        let error = open_store(&config_for(dir.path()))
            .await
            .err()
            .expect("missing sharing key refuses startup");
        assert!(error.to_string().contains("refusing to start"));
        assert!(!key_path.exists(), "startup must not replace a missing key");
        let replacement = crate::secrets::open_credential_key(&key_path, &Default::default())
            .expect("synthetic replacement key");
        let error = open_store(&config_for(dir.path()))
            .await
            .err()
            .expect("wrong sharing key refuses startup");
        let message = error.to_string();
        assert!(message.contains(&key_id) && message.contains(replacement.id()));
        assert!(!message.contains("synthetic-sharing"));
        let store = SqliteStore::open(&dir.path().join("plurx.db")).expect("preserved database");
        assert_eq!(
            store
                .sharing_sealed_census()
                .await
                .expect("preserved census")
                .sealed_rows(),
            1
        );
    }

    /// Restoring a database beside a *replaced* key is the same accident as
    /// losing the key, and it must land in the same place. A key that opens
    /// nothing is not a working install: without this the daemon reaches
    /// `listening`, every Trakt call fails as "not linked", and the household
    /// has no error to search for.
    #[tokio::test]
    async fn a_wrong_but_present_key_refuses_to_start_instead_of_opening_nothing() {
        let dir = tempfile::tempdir().expect("data dir");
        seed_cleartext_trakt_row(dir.path()).await;

        let migrated = open_store(&config_for(dir.path())).await.expect("upgrade");
        let sealed = only_trakt_row(&migrated.store).await;
        let original_key_id = migrated.credential_key.id().to_owned();
        drop(migrated);

        // The operator restores the database, then hands the node a key file
        // that is not the one that sealed these rows.
        let key_path = dir.path().join(crate::secrets::CREDENTIAL_KEY_FILENAME);
        let real_key_file = std::fs::read_to_string(&key_path).expect("read the real key");
        std::fs::remove_file(&key_path).expect("replace the key file");
        let replacement =
            crate::secrets::open_credential_key(&key_path, &Default::default()).expect("mint");
        assert_ne!(replacement.id(), original_key_id);

        let error = open_store(&config_for(dir.path()))
            .await
            .err()
            .expect("startup must refuse a key that opens none of its rows");
        let message = error.to_string();
        assert!(
            message.contains(&original_key_id) && message.contains(replacement.id()),
            "the refusal must name the key wanted and the key held: {message}"
        );
        assert!(
            !message.contains(sealed.access_token.as_stored()),
            "the refusal must not print the credential it could not open"
        );

        // The wrong key is still there for the operator to swap out, and the
        // credential is intact rather than re-sealed under a key nobody wanted.
        let store: Arc<dyn Store> =
            Arc::new(SqliteStore::open(&dir.path().join("plurx.db")).expect("reopen"));
        let untouched = only_trakt_row(&store).await;
        assert_eq!(
            untouched.access_token.as_stored(),
            sealed.access_token.as_stored()
        );

        // Restoring the right key makes the same install start and read again,
        // which is what makes the refusal a recoverable stop rather than a wall.
        std::fs::write(&key_path, &real_key_file).expect("restore the real key");
        #[cfg(unix)]
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .expect("owner-only");
        let recovered = open_store(&config_for(dir.path()))
            .await
            .expect("the right key starts");
        assert_eq!(recovered.credential_key.id(), original_key_id);
        assert_eq!(
            only_trakt_row(&recovered.store)
                .await
                .reveal_access_token(&recovered.credential_key)
                .expect("open access")
                .expose(),
            "legacy-access-token"
        );
    }

    /// A key file anyone on the box can read is not the node-local key
    /// `docs/SECURITY.md` promises. The mode has to be checked on the file the
    /// operator supplied, not only on the one we minted.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_group_or_world_readable_key_file_refuses_to_start() {
        let dir = tempfile::tempdir().expect("data dir");
        seed_cleartext_trakt_row(dir.path()).await;

        let migrated = open_store(&config_for(dir.path())).await.expect("upgrade");
        let key_id = migrated.credential_key.id().to_owned();
        drop(migrated);

        let key_path = dir.path().join(crate::secrets::CREDENTIAL_KEY_FILENAME);
        let material = std::fs::read_to_string(&key_path).expect("read key");
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o644))
            .expect("expose the key");

        let error = open_store(&config_for(dir.path()))
            .await
            .err()
            .expect("startup must refuse a world-readable credential key");
        let message = error.to_string();
        assert!(
            message.contains("0644") && message.contains("chmod 600"),
            "the refusal must name the mode and the fix: {message}"
        );
        assert!(
            !message.contains(material.trim()),
            "the refusal must not print the key it just read"
        );

        // Fixing the mode is the whole remedy: same key, same rows.
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))
            .expect("restore");
        let handle = open_store(&config_for(dir.path()))
            .await
            .expect("an owner-only key starts");
        assert_eq!(handle.credential_key.id(), key_id);
        assert_eq!(
            only_trakt_row(&handle.store)
                .await
                .reveal_access_token(&handle.credential_key)
                .expect("open access")
                .expose(),
            "legacy-access-token"
        );
    }

    #[tokio::test]
    async fn a_first_boot_with_nothing_linked_mints_its_key_without_complaint() {
        let dir = tempfile::tempdir().expect("data dir");

        let handle = open_store(&config_for(dir.path()))
            .await
            .expect("first boot");

        assert!(dir
            .path()
            .join(crate::secrets::CREDENTIAL_KEY_FILENAME)
            .exists());
        assert_eq!(handle.credential_key.id().len(), 8);
    }

    #[tokio::test]
    async fn an_operator_named_key_file_is_used_instead_of_the_default() {
        let dir = tempfile::tempdir().expect("data dir");
        let keys = tempfile::tempdir().expect("key dir");
        let key_path = keys.path().join("plurx-credentials.key");
        seed_cleartext_trakt_row(dir.path()).await;

        let mut config = config_for(dir.path());
        config.cluster.credential_key_file = key_path.clone();
        let handle = open_store(&config).await.expect("upgrade");

        assert!(key_path.exists(), "the configured path must be honoured");
        assert!(
            !dir.path()
                .join(crate::secrets::CREDENTIAL_KEY_FILENAME)
                .exists(),
            "configuring a key file must not also seed the default one"
        );
        assert_eq!(
            only_trakt_row(&handle.store)
                .await
                .reveal_access_token(&handle.credential_key)
                .expect("open access")
                .expose(),
            "legacy-access-token"
        );
    }

    #[test]
    fn invalid_identity_files_fail_closed() {
        let dir = tempfile::tempdir().expect("data dir");
        let cluster_id = uuid::Uuid::new_v4().to_string();
        std::fs::write(dir.path().join(NODE_ID_FILENAME), "copied-node\n")
            .expect("write invalid id");

        let error = initialize_identity(dir.path(), &cluster_id).expect_err("invalid node id");
        assert!(error.to_string().contains("node.id is not a UUID"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join(NODE_ID_FILENAME)).expect("preserved"),
            "copied-node\n",
            "startup must not silently replace a copied or corrupt identity"
        );
    }
}
