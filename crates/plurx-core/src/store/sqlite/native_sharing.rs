//! Native Source schema and boot ownership. No replication or peer roster is
//! constructed here; shared layout statements contain application facts only.
use super::{housekeeping, SqliteStore};
use crate::{
    error::StoreError,
    secrets::CredentialKey,
    sharing::invalid,
    sharing_source_sessions::LocalSourceAuthority,
    store::{sharing_purpose_keys, sharing_source_schema as schema},
};
use rusqlite::{Connection, OptionalExtension};
use std::{
    path::Path,
    sync::{atomic::AtomicBool, Arc},
};

fn exact_or_create(connection: &Connection, sql: &str) -> Result<(), StoreError> {
    let words = sql.split_whitespace().collect::<Vec<_>>();
    let name = words[2].split('(').next().ok_or_else(invalid)?;
    let existing = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name=?1",
            [name],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match existing {
        None => {
            connection.execute_batch(sql)?;
        }
        Some(existing) => {
            let normalize = |value: &str| {
                value
                    .chars()
                    .filter(|c| !c.is_ascii_whitespace() && *c != ';')
                    .collect::<String>()
            };
            if normalize(&existing) != normalize(sql) {
                return Err(StoreError::Migration(format!(
                    "native Source object {name} has an incompatible layout"
                )));
            }
        }
    }
    Ok(())
}

/// All schema dependencies are audited before the transaction removes any.
fn detached_dependencies(connection: &Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut statement = connection.prepare(
        "SELECT type,name,tbl_name,sql FROM sqlite_master WHERE type IN ('trigger','view') AND sql IS NOT NULL",
    )?;
    let objects = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    schema::validated_external_rebuild_dependencies(objects)
}

fn install_layout(connection: &Connection) -> Result<(), StoreError> {
    let principal: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('media_sessions') WHERE name='owner_key')",
        [],
        |row| row.get(0),
    )?;
    if !principal {
        let expected: bool = connection.query_row(
            &format!("SELECT ({})", schema::native_predecessor_layout_guard()),
            [],
            |row| row.get(0),
        )?;
        if !expected {
            return Err(StoreError::Migration(
                "native Source predecessor is ambiguous".into(),
            ));
        }
        let detached = detached_dependencies(connection)?;
        for (name, _) in &detached {
            // Closed allowlist above; names never come from arbitrary input.
            connection.execute_batch(&format!("DROP TRIGGER {name}"))?;
        }
        for statement in schema::layout_statements() {
            connection.execute_batch(&statement)?;
        }
        for (_, sql) in detached {
            connection.execute_batch(&sql)?;
        }
    }
    for statement in sharing_purpose_keys::native_material_statements() {
        exact_or_create(connection, &statement)?;
    }
    for statement in [
        schema::NATIVE_BOOT_SCHEMA,
        schema::INSTALLATION_SCHEMA,
        schema::TRANSACTION_SCHEMA,
        schema::DISPATCH_GUARD_SCHEMA,
    ] {
        exact_or_create(connection, statement)?;
    }
    let shape: bool = connection.query_row(
        &format!("SELECT ({})", schema::native_layout_shape_guard()),
        [],
        |row| row.get(0),
    )?;
    if !shape {
        return Err(StoreError::Migration(
            "native Source schema failed complete shape validation".into(),
        ));
    }
    Ok(())
}

impl SqliteStore {
    /// Builds a new native import target without minting a logical identity,
    /// credentials or a startup authority. An existing file is never reused.
    pub fn create_native_import_target(path: &Path) -> Result<(), StoreError> {
        Self::create_native_import_target_layout(path, true)
    }

    /// Schema-only predecessor target. Retains the seven local session
    /// families until ordinary native startup performs its audited rebuild.
    /// Empty purpose material tables admit retained keys without minting any.
    pub fn create_native_pre_source_import_target(path: &Path) -> Result<(), StoreError> {
        Self::create_native_import_target_layout(path, false)
    }

    fn create_native_import_target_layout(
        path: &Path,
        current_source: bool,
    ) -> Result<(), StoreError> {
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(path)
            .and_then(|file| file.sync_all())
            .map_err(|error| {
                StoreError::Migration(format!("creating native import target: {error}"))
            })?;
        let connection = Connection::open(path)?;
        housekeeping::configure_writer(&connection)?;
        Self::migrate_schema(&connection, false)?;
        connection.pragma_update(None, "foreign_keys", "OFF")?;
        let transaction = connection.unchecked_transaction()?;
        if current_source {
            install_layout(&transaction)?;
        } else {
            for statement in sharing_purpose_keys::native_material_statements() {
                exact_or_create(&transaction, &statement)?;
            }
            let expected: bool = transaction.query_row(
                &format!("SELECT ({})", schema::native_predecessor_layout_guard()),
                [],
                |row| row.get(0),
            )?;
            if !expected {
                return Err(StoreError::Migration(
                    "native predecessor import target failed its complete layout guard".into(),
                ));
            }
        }
        let errors: i64 =
            transaction.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })?;
        if errors != 0 {
            return Err(invalid());
        }
        transaction.commit()?;
        connection.pragma_update(None, "foreign_keys", "ON")?;
        connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        drop(connection);
        crate::storage::durability::sync_regular(path).map_err(|error| {
            StoreError::Migration(format!("syncing native import target: {error}"))
        })?;
        Ok(())
    }

    /// Ordinary native startup, under the caller's exclusive root lock.
    /// Persisted row and generation checks remain in every Source mutation.
    pub async fn prepare_native_source_before_serving(
        &self,
        node_id: &str,
        master: &CredentialKey,
    ) -> Result<LocalSourceAuthority, StoreError> {
        if self
            .install_native_sharing_purpose_keys(
                master,
                node_id,
                crate::store::sharing::wall_clock_ms()?,
            )
            .await?
            != sharing_purpose_keys::PurposeKeyInstallation::Ready
        {
            return Err(StoreError::Migration(
                "native Source purpose material requires explicit restore settlement".into(),
            ));
        }
        let native_node = node_id.to_owned();
        let node_id = native_node.clone();
        let fingerprint = master.sharing_purpose_master_fingerprint();
        let boot_id = uuid::Uuid::new_v4();
        let (storage_generation,generation)=self.with_conn(move |connection| {
            connection.pragma_update(None,"foreign_keys","OFF")?;
            let result=(|| {
                let transaction=connection.unchecked_transaction()?;
                let storage_generation:String=transaction.query_row("SELECT generation FROM storage_identity WHERE singleton=1 AND node_id=?1 AND instance_id=(SELECT value FROM settings WHERE key='instance.id')",[&node_id],|row|row.get(0))?;
                install_layout(&transaction)?;
                let prior:Option<String>=transaction.query_row("SELECT master_fingerprint FROM sharing_source_schema_installation WHERE singleton=1",[],|row|row.get(0)).optional()?;
                if prior.as_ref().is_some_and(|prior|prior!=&fingerprint) {return Err(StoreError::Migration("native Source master fingerprint changed without a key migration".into()));}
                let changed=transaction.execute("INSERT INTO sharing_native_source_boot VALUES(1,?1,?2,?3,1,?4) ON CONFLICT(singleton) DO UPDATE SET node_id=excluded.node_id,storage_generation=excluded.storage_generation,boot_id=excluded.boot_id,generation=generation+1,master_fingerprint=excluded.master_fingerprint WHERE generation<9223372036854775807",rusqlite::params![node_id,storage_generation,boot_id.to_string(),fingerprint])?;
                if changed!=1 {return Err(invalid());}
                transaction.execute("INSERT INTO sharing_source_schema_installation VALUES(1,71,?1,?2) ON CONFLICT(singleton) DO NOTHING",rusqlite::params![fingerprint,crate::store::sharing::wall_clock_ms()?])?;
                transaction.execute("INSERT INTO sharing_source_schema_transaction_guard VALUES(1,1) ON CONFLICT(singleton) DO NOTHING",[])?;
                let valid:bool=transaction.query_row(&format!("SELECT ({})",schema::native_installed_guard()),[],|row|row.get(0))?;
                if !valid {return Err(StoreError::Migration("native Source startup guard refused".into()));}
                let generation:i64=transaction.query_row("SELECT generation FROM sharing_native_source_boot WHERE singleton=1",[],|row|row.get(0))?;
                transaction.commit()?;
                Ok((storage_generation,generation))
            })();
            connection.pragma_update(None,"foreign_keys","ON")?;
            result
        }).await?;
        // The values passed to the transaction above were owned copies.
        Ok(LocalSourceAuthority {
            node_id: native_node,
            storage_generation,
            boot_id,
            generation,
            master_fingerprint: master.sharing_purpose_master_fingerprint(),
            active: Arc::new(AtomicBool::new(true)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SharingStore;
    use crate::store::{sharing::Backend, SettingsStore};

    async fn native_store() -> (SqliteStore, String) {
        let store = SqliteStore::open_in_memory().expect("native SQLite");
        let instance = store.instance_id().await.expect("actual instance identity");
        let node = "native-test-owner".to_owned();
        store
            .sharing_txn(vec![(
                "INSERT INTO storage_identity VALUES(1,$1,$2,$3)".into(),
                vec![
                    instance.into(),
                    node.clone().into(),
                    uuid::Uuid::new_v4().to_string().into(),
                ],
            )])
            .await
            .expect("selected native storage binding");
        (store, node)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_blocking_sqlite_write_retains_native_owner_until_completion() {
        use crate::storage::{open_native_store_at, NativeDirectoryLock};
        let directory = tempfile::tempdir().expect("native directory");
        let handle =
            open_native_store_at(directory.path(), &directory.path().join("credentials.key"))
                .await
                .expect("native startup");
        let sqlite = Arc::clone(&handle.sqlite);
        let (started, observed_start) = tokio::sync::oneshot::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let (finished, observed_finish) = tokio::sync::oneshot::channel();
        let caller = tokio::spawn(async move {
            sqlite.with_conn(move |connection| {
                let _ = started.send(());
                wait_release.recv().map_err(|error|StoreError::Task(error.to_string()))?;
                connection.execute("INSERT INTO settings(key,value) VALUES('lease.cancelled','committed while owned')",[])?;
                let _ = finished.send(());
                Ok(())
            }).await
        });
        observed_start.await.expect("blocking writer started");
        caller.abort();
        let _ = caller.await;
        drop(handle);
        let excluded = NativeDirectoryLock::try_acquire(directory.path()).is_err();
        // Always unblock the worker before asserting, including on regression.
        release.send(()).expect("release original writer");
        observed_finish.await.expect("original write completed");
        assert!(
            excluded,
            "cancelled async caller cannot release a running writer's lease"
        );
        let next = NativeDirectoryLock::acquire(directory.path())
            .await
            .expect("finished blocking operation releases final owner");
        let connection =
            Connection::open(directory.path().join("plurx.db")).expect("next exclusive owner");
        let value: String = connection
            .query_row(
                "SELECT value FROM settings WHERE key='lease.cancelled'",
                [],
                |row| row.get(0),
            )
            .expect("accepted old write retained");
        assert_eq!(value, "committed while owned");
        drop(connection);
        drop(next);
    }

    #[tokio::test]
    async fn native_schema_only_conversion_without_prior_sharing_opens_and_installs_real_material()
    {
        let directory = tempfile::tempdir().expect("converted native directory");
        crate::storage::mode::begin_native_adoption(directory.path())
            .expect("owned conversion intent before creating native schema");
        let path = directory.path().join("plurx.db");
        SqliteStore::create_native_import_target(&path).expect("complete schema-only target");
        let identity = uuid::Uuid::new_v4().to_string();
        let connection = Connection::open(&path).expect("simulated verified copy");
        connection
            .execute(
                "INSERT INTO settings(key,value) VALUES('instance.id',?1)",
                [&identity],
            )
            .expect("retained old singleton identity");
        drop(connection);
        let handle = crate::storage::open_native_store_at(
            directory.path(),
            &directory.path().join("credentials.key"),
        )
        .await
        .expect("native census accepts exact empty purpose schema");
        assert_eq!(handle.identity.instance_id, identity);
        let source = handle
            .sqlite
            .prepare_native_source_before_serving(&handle.identity.node_id, &handle.credential_key)
            .await
            .expect("first real native factory owns schema without generating a vote");
        source.revoke();
        assert_eq!(
            handle
                .sqlite
                .sharing_revision_key_rows()
                .await
                .expect("first sealed material")
                .len(),
            1
        );
        assert_eq!(handle.sqlite.sharing_read("SELECT json_array(count(*)) AS payload FROM sqlite_master WHERE name IN('cluster_meta','cluster_nodes','sharing_purpose_census_intents')",vec![]).await.expect("no replicated factory state"),["[0]"]);
    }

    #[tokio::test]
    async fn native_restore_pending_settles_new_identity_and_retains_original_archived_ciphertext()
    {
        let (store, node) = native_store().await;
        let master = CredentialKey::from_bytes([96; 32]);
        let original = store
            .prepare_native_source_before_serving(&node, &master)
            .await
            .expect("original native Source");
        original.revoke();
        let old_identity = store
            .sharing_identity(1000)
            .await
            .expect("original sharing identity");
        store.with_conn(|connection| {
            let transaction=connection.unchecked_transaction()?;
            sharing_purpose_keys::archive_native_for_restore(&transaction,&CredentialKey::from_bytes([96;32]))?;
            transaction.execute("DELETE FROM sharing_identity",[])?;
            transaction.execute("INSERT INTO settings(key,value) VALUES('sharing_enabled','false') ON CONFLICT(key) DO UPDATE SET value=excluded.value",[])?;
            transaction.execute("UPDATE storage_identity SET generation=?1 WHERE singleton=1",[uuid::Uuid::new_v4().to_string()])?;
            transaction.commit()?;
            Ok(())
        }).await.expect("explicit restore archive and generation disposition");
        let archived = store
            .sharing_purpose_archive_rows()
            .await
            .expect("retained archive before settlement");
        assert_eq!(archived.len(), 2);
        assert!(
            store
                .prepare_native_source_before_serving(&node, &CredentialKey::from_bytes([97; 32]))
                .await
                .is_err(),
            "a lost master cannot settle or replace archived keys"
        );
        assert_eq!(
            store
                .sharing_read(
                    "SELECT json_array(count(*)) AS payload FROM sharing_identity",
                    vec![]
                )
                .await
                .expect("no replacement identity on a wrong master"),
            ["[0]"]
        );
        let restored = store
            .prepare_native_source_before_serving(&node, &master)
            .await
            .expect("disabled Sharing does not block local daemon boot after explicit restore");
        restored.revoke();
        let new_identity = store
            .sharing_identity(1001)
            .await
            .expect("new sharing identity");
        assert_ne!(new_identity.server_id, old_identity.server_id);
        assert_ne!(new_identity.catalogue_epoch, old_identity.catalogue_epoch);
        assert_eq!(
            store
                .sharing_purpose_archive_rows()
                .await
                .expect("retained original AAD and ciphertext"),
            archived
        );
        crate::store::SharingPurposeKeyStore::verify_sharing_purpose_material(&store, &master)
            .await
            .expect("new active and archived material both authenticate under original master");
        assert_eq!(store.sharing_read("SELECT json_array(state,master_key_id) AS payload FROM sharing_purpose_key_installation",vec![]).await.expect("ready installation"),[serde_json::to_string(&["ready",master.id()]).expect("marker tuple")]);
        assert_eq!(
            store
                .get_setting("sharing_enabled")
                .await
                .expect("restored switch")
                .as_deref(),
            Some("false")
        );
    }

    #[test]
    fn native_source_rebuild_preserves_external_interest_trigger_and_execution() {
        let connection = Connection::open_in_memory().expect("native SQLite");
        housekeeping::configure_writer(&connection).expect("writer configuration");
        SqliteStore::migrate_schema(&connection, false).expect("legacy application schema");
        let original: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name='playback_analysis_restore_effects'",
                [],
                |row| row.get(0),
            )
            .expect("external interest trigger before rebuild");
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .expect("rebuild foreign-key mode");
        let transaction = connection.unchecked_transaction().expect("atomic rebuild");
        install_layout(&transaction).expect("all cross-table dependencies accounted for");
        transaction.commit().expect("publish complete layout");
        connection
            .pragma_update(None, "foreign_keys", "ON")
            .expect("restore foreign keys");
        let restored: String = connection
            .query_row(
                "SELECT sql FROM sqlite_master WHERE name='playback_analysis_restore_effects'",
                [],
                |row| row.get(0),
            )
            .expect("external interest trigger after rebuild");
        assert_eq!(restored, original, "interest semantics retained verbatim");
        connection.execute("INSERT INTO background_job_commands VALUES('native-interest','source_restore_interest','{\"receipt_id\":\"absent\",\"now_ms\":1000}','true')",[]).expect("trigger body resolves rebuilt families and consumes the command");
        let commands: i64 = connection
            .query_row(
                "SELECT count(*) FROM background_job_commands WHERE id='native-interest'",
                [],
                |row| row.get(0),
            )
            .expect("interest command effects");
        assert_eq!(commands, 0);
        let violations: i64 = connection
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .expect("foreign-key audit");
        assert_eq!(violations, 0);
    }

    #[test]
    fn native_source_rebuild_refuses_unknown_external_dependency_without_partial_layout() {
        let connection = Connection::open_in_memory().expect("native SQLite");
        housekeeping::configure_writer(&connection).expect("writer configuration");
        SqliteStore::migrate_schema(&connection, false).expect("legacy application schema");
        connection
            .execute_batch("CREATE VIEW unknown_session_reader AS SELECT * FROM media_sessions")
            .expect("unrecognized external dependency");
        let transaction = connection.unchecked_transaction().expect("atomic rebuild");
        let error = install_layout(&transaction).expect_err("ambiguous dependency must refuse");
        assert!(error.to_string().contains("unknown_session_reader"));
        drop(transaction);
        let principal: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM pragma_table_info('media_sessions') WHERE name='owner_key')",
                [],
                |row| row.get(0),
            )
            .expect("predecessor remains intact");
        assert!(!principal);
        connection
            .prepare("SELECT * FROM unknown_session_reader")
            .expect("old dependent view still resolves");
        let detached = detached_dependencies(&connection).expect_err("view was preserved");
        assert!(detached.to_string().contains("unknown_session_reader"));
    }

    #[test]
    fn native_import_target_has_complete_schema_without_minting_identity_or_votes() {
        let directory = tempfile::tempdir().expect("import directory");
        let path = directory.path().join("native.db");
        SqliteStore::create_native_import_target(&path).expect("native import schema");
        let connection = Connection::open(&path).expect("inspect native target");
        let count: i64 = connection
            .query_row(
                "SELECT count(*) FROM settings WHERE key='instance.id'",
                [],
                |row| row.get(0),
            )
            .expect("identity census");
        assert_eq!(count, 0);
        let cluster:i64=connection.query_row("SELECT count(*) FROM sqlite_master WHERE name IN('cluster_meta','cluster_nodes','sharing_source_boot_intents','sharing_purpose_census_intents')",[],|row|row.get(0)).expect("replication census");
        assert_eq!(cluster, 0);
        for table in [
            "storage_identity",
            "sharing_native_source_boot",
            "sharing_source_schema_installation",
            "sharing_purpose_key_installation",
            "sharing_catalogue_keys",
            "sharing_file_locator_keys",
        ] {
            let count: i64 = connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("empty import-only table");
            assert_eq!(count, 0, "{table} must not mint import authority/material");
        }
        assert!(
            SqliteStore::create_native_import_target(&path).is_err(),
            "existing target never reused"
        );
    }

    #[tokio::test]
    async fn native_source_boot_is_locally_fenced_and_preserves_sealed_purpose_keys() {
        let (store, node) = native_store().await;
        let master = CredentialKey::from_bytes([93; 32]);
        let first = store
            .prepare_native_source_before_serving(&node, &master)
            .await
            .expect("actual local Source bootstrap");
        let registry = uuid::Uuid::new_v4();
        let admission = first
            .admission(registry)
            .expect("registry-bound local admission");
        let (guard, captured, cutoff, observed) =
            admission.write_guard(10_000, 1, 2, 3).expect("local guard");
        assert!(!guard.contains("cluster_"));
        let read = |guard: &str| {
            format!("SELECT json_quote(CASE WHEN ({guard}) THEN 1 ELSE 0 END) AS payload")
        };
        let args = || vec![captured.clone().into(), cutoff.into(), observed.into()];
        assert_eq!(
            store
                .sharing_read(&read(&guard), args())
                .await
                .expect("local assertion"),
            ["1"]
        );
        let before = store
            .sharing_revision_key_rows()
            .await
            .expect("sealed key before restart");
        let second = store
            .prepare_native_source_before_serving(&node, &master)
            .await
            .expect("replacement local boot");
        assert_eq!(
            store
                .sharing_revision_key_rows()
                .await
                .expect("sealed key after restart"),
            before
        );
        assert_eq!(
            store
                .sharing_read(&read(&guard), args())
                .await
                .expect("stale local assertion"),
            ["0"]
        );
        second.revoke();
        assert!(second.admission(registry).is_err());
        let count=store.sharing_read("SELECT json_quote(count(*)) AS payload FROM sqlite_master WHERE name IN('cluster_meta','cluster_nodes','sharing_source_boot_intents','sharing_purpose_census_intents')",vec![]).await.expect("native authority does not create replication state");
        assert_eq!(count, ["0"]);
        assert!(
            store
                .prepare_native_source_before_serving(&node, &CredentialKey::from_bytes([94; 32]))
                .await
                .is_err(),
            "boot cannot replace retained key material"
        );
    }

    #[tokio::test]
    async fn native_census_stale_attempt_cannot_release_replacement_without_raft_identity() {
        let (store, node) = native_store().await;
        let old = sharing_purpose_keys::begin_native_census(&store, &node, 100)
            .await
            .expect("first local census");
        let current = sharing_purpose_keys::begin_native_census(&store, &node, 101)
            .await
            .expect("replacement local census");
        assert!(sharing_purpose_keys::finish_census(&store, old)
            .await
            .is_err());
        sharing_purpose_keys::finish_census(&store, current)
            .await
            .expect("current census release");
        assert_eq!(
            store
                .sharing_read(
                    "SELECT json_quote(count(*)) AS payload FROM sharing_local_census",
                    vec![]
                )
                .await
                .expect("census released"),
            ["0"]
        );
    }
}
