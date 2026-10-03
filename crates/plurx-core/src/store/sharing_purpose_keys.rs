//! Explicit factory/restore purpose material; catalogue reads never repair it.
use crate::{
    error::StoreError,
    secrets::{CredentialKey, SealedSecret, SharingSecretPurpose},
    sharing::{invalid, SharingIdentity},
};
use serde::Deserialize;
use uuid::Uuid;

pub const SHARING_PURPOSE_KEYS_SCHEMA: &str = include_str!("sharing_purpose_keys_schema.sql");
pub(crate) const MAX_ARCHIVED_PURPOSE_KEYS: usize = 256;
pub(crate) const ARCHIVE_COLUMNS_SQL: &str = "SELECT json_array(name,type,\"notnull\",pk) AS payload FROM pragma_table_info('sharing_purpose_key_archive') ORDER BY cid";
pub(crate) const ARCHIVE_ROWS_SQL: &str = "SELECT CASE WHEN length(server_id)=36 AND length(catalogue_epoch)=36 AND length(CAST(envelope AS BLOB))<=4096 THEN json_object('purpose',purpose,'server_id',server_id,'catalogue_epoch',catalogue_epoch,'envelope',envelope) ELSE NULL END AS payload FROM sharing_purpose_key_archive ORDER BY purpose,server_id,catalogue_epoch LIMIT 257";

pub(crate) fn archive_columns(rows: Vec<String>) -> Result<(), StoreError> {
    let columns: Vec<(String, String, i64, i64)> = rows
        .into_iter()
        .map(|row| serde_json::from_str(&row).map_err(|_| invalid()))
        .collect::<Result<_, _>>()?;
    if columns
        != [
            ("purpose".into(), "TEXT".into(), 1, 1),
            ("server_id".into(), "TEXT".into(), 1, 2),
            ("catalogue_epoch".into(), "TEXT".into(), 1, 3),
            ("envelope".into(), "TEXT".into(), 1, 0),
        ]
    {
        return Err(invalid());
    }
    Ok(())
}
const TRANSACTION_GUARD_SQL: &str = "SELECT json_array(sql,(SELECT count(*) FROM sqlite_master WHERE type='trigger' AND tbl_name='sharing_purpose_transaction_guard')) AS payload FROM sqlite_master WHERE name='sharing_purpose_transaction_guard' AND type='table'";
fn transaction_guard_shape(rows: Vec<String>) -> Result<(), StoreError> {
    let [raw] = rows.as_slice() else {
        return Err(invalid());
    };
    let (sql, triggers): (String, i64) = serde_json::from_str(raw).map_err(|_| invalid())?;
    if triggers != 0 {
        return Err(invalid());
    }
    let normalized = sql
        .chars()
        .filter(|c| !c.is_ascii_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>();
    if normalized!="createtablesharing_purpose_transaction_guard(singletonintegernotnullprimarykeycheck(singleton=1),passedintegernotnullcheck(passed=1))strict" {return Err(invalid());}
    Ok(())
}
/// No Debug/Serialize: retained ciphertext is never response or log material.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArchivedPurposeKey {
    purpose: String,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    server_id: Uuid,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    catalogue_epoch: Uuid,
    envelope: String,
}
impl ArchivedPurposeKey {
    fn purpose(&self) -> Result<SharingSecretPurpose, StoreError> {
        match self.purpose.as_str() {
            "catalogue_revision" => Ok(SharingSecretPurpose::CatalogueRevision),
            "file_locator" => Ok(SharingSecretPurpose::FileLocator),
            _ => Err(invalid()),
        }
    }
    pub(crate) fn sealed(&self) -> SealedSecret {
        SealedSecret::from_stored(self.envelope.clone())
    }
    #[cfg(feature = "hiqlite-store")]
    fn rewrapped(
        &self,
        old: &CredentialKey,
        replacement: &CredentialKey,
    ) -> Result<SealedSecret, StoreError> {
        self.verify(old)?;
        let purpose = self.purpose()?;
        let clear = old
            .open_sharing(
                purpose,
                self.server_id,
                self.catalogue_epoch,
                &self.sealed(),
            )
            .map_err(|_| invalid())?;
        replacement
            .seal_sharing(
                purpose,
                self.server_id,
                self.catalogue_epoch,
                clear.expose(),
            )
            .map_err(|_| invalid())
    }
    pub(crate) fn verify(&self, master: &CredentialKey) -> Result<(), StoreError> {
        let identity = SharingIdentity {
            server_id: self.server_id,
            catalogue_epoch: self.catalogue_epoch,
            created_at_ms: 0,
        };
        let envelope = self.sealed();
        match self.purpose()? {
            SharingSecretPurpose::CatalogueRevision => {
                crate::sharing_catalogue_details::CatalogueRevisionKey::open(
                    master, identity, &envelope,
                )
                .map_err(|_| invalid())?;
            }
            SharingSecretPurpose::FileLocator => {
                crate::sharing_file_locators::FileLocatorKey::open(master, &identity, &envelope)
                    .map_err(|_| invalid())?;
            }
            _ => return Err(invalid()),
        }
        Ok(())
    }
}
pub(crate) fn archive_rows(rows: Vec<String>) -> Result<Vec<ArchivedPurposeKey>, StoreError> {
    if rows.len() > MAX_ARCHIVED_PURPOSE_KEYS {
        return Err(invalid());
    }
    let mut seen = std::collections::BTreeSet::new();
    let parsed: Vec<_> = rows
        .into_iter()
        .map(|row| {
            let row: ArchivedPurposeKey = serde_json::from_str(&row).map_err(|_| invalid())?;
            row.purpose()?;
            if row.envelope.len() > 4096
                || !row.sealed().is_wrapped()
                || !seen.insert((row.purpose.clone(), row.server_id, row.catalogue_epoch))
            {
                return Err(invalid());
            }
            Ok(row)
        })
        .collect::<Result<_, _>>()?;
    let mut epochs = std::collections::BTreeMap::<(Uuid, Uuid), usize>::new();
    for row in &parsed {
        *epochs
            .entry((row.server_id, row.catalogue_epoch))
            .or_default() += 1;
    }
    if epochs.values().any(|count| *count != 2) {
        return Err(invalid());
    }
    Ok(parsed)
}

#[cfg(feature = "hiqlite-store")]
use super::sharing::Value;
use super::sharing::{Backend, SharingStore};
use async_trait::async_trait;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    singleton: i64,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    server_id: Uuid,
    #[serde(deserialize_with = "crate::sharing::canonical_uuid")]
    catalogue_epoch: Uuid,
    master_key_id: String,
    state: String,
    generation: i64,
    updated_at_ms: i64,
}
const CENSUS_COLUMNS_SQL: &str = "SELECT json_array(name,type,\"notnull\",pk) AS payload FROM pragma_table_info('sharing_purpose_census_intents') ORDER BY cid";
const CENSUS_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS sharing_purpose_census_intents(node_id TEXT NOT NULL PRIMARY KEY CHECK(length(node_id) BETWEEN 1 AND 256),raft_id INTEGER NOT NULL CHECK(raft_id>0),attempt_id TEXT NOT NULL CHECK(length(attempt_id)=36),generation INTEGER NOT NULL CHECK(generation>0),claimed_at_ms INTEGER NOT NULL CHECK(claimed_at_ms>0)) STRICT";
#[cfg(feature = "hiqlite-store")]
const CENSUS_REMOVAL_TRIGGER: &str = "CREATE TRIGGER IF NOT EXISTS sharing_purpose_census_removed_node AFTER UPDATE OF removed_at ON cluster_nodes WHEN NEW.removed_at IS NOT NULL BEGIN DELETE FROM sharing_purpose_census_intents WHERE node_id=NEW.node_id AND raft_id=NEW.raft_id; END";
fn census_columns(rows: Vec<String>) -> Result<(), StoreError> {
    if rows.as_slice()
        != [
            "[\"node_id\",\"TEXT\",1,1]",
            "[\"raft_id\",\"INTEGER\",1,0]",
            "[\"attempt_id\",\"TEXT\",1,0]",
            "[\"generation\",\"INTEGER\",1,0]",
            "[\"claimed_at_ms\",\"INTEGER\",1,0]",
        ]
    {
        return Err(invalid());
    }
    Ok(())
}
/// A census claim is private boot ownership, never a readiness permission.
/// No timer or Drop handler can release it after failed/cancelled startup.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PurposeCensus {
    node_id: String,
    raft_id: i64,
    attempt_id: Uuid,
    generation: i64,
}
#[cfg(feature = "hiqlite-store")]
enum CensusBackend {
    Local,
    Replicated,
}
#[cfg(feature = "hiqlite-store")]
async fn begin_census<T: Backend + ?Sized>(
    store: &T,
    identity: &crate::cluster::ClusterIdentity,
    now_ms: i64,
    mode: CensusBackend,
) -> Result<PurposeCensus, StoreError> {
    let raft_id = i64::try_from(identity.raft_id).map_err(|_| invalid())?;
    if identity.node_id.is_empty() || identity.node_id.len() > 256 || raft_id <= 0 || now_ms <= 0 {
        return Err(invalid());
    }
    let attempt = Uuid::new_v4();
    // Local startup is under its data-directory advisory lock. Replicated
    // startup additionally claims only its real current admitted SQL identity.
    let admitted = if matches!(mode, CensusBackend::Replicated) {
        "EXISTS(SELECT 1 FROM cluster_nodes WHERE node_id=$1 AND raft_id=$2 AND removed_at IS NULL)"
    } else {
        "1"
    };
    let capabilities=store.sharing_read("SELECT json_array(type) AS payload FROM sqlite_master WHERE name='cluster_node_capabilities'",vec![]).await?;
    if !capabilities.is_empty() && capabilities.as_slice() != ["[\"table\"]"] {
        return Err(invalid());
    }
    let mut statements=vec![(CENSUS_SCHEMA.to_owned(),vec![]),(format!("INSERT INTO sharing_purpose_census_intents(node_id,raft_id,attempt_id,generation,claimed_at_ms) SELECT $1,$2,$3,1,$4 WHERE ({admitted}) ON CONFLICT(node_id) DO UPDATE SET attempt_id=excluded.attempt_id,generation=generation+1,claimed_at_ms=excluded.claimed_at_ms WHERE raft_id=excluded.raft_id AND generation<9223372036854775807"),vec![identity.node_id.clone().into(),raft_id.into(),attempt.into(),now_ms.into()])];
    if matches!(mode, CensusBackend::Replicated) {
        statements.push((CENSUS_REMOVAL_TRIGGER.to_owned(), vec![]));
    }
    if !capabilities.is_empty() {
        statements.push(("DELETE FROM cluster_node_capabilities WHERE node_id=$1 AND (capability='sharing_purpose_keys_v1' OR capability GLOB 'sharing_purpose_master_v1:*')".to_owned(),vec![identity.node_id.clone().into()]));
    }
    let changed = store.sharing_txn(statements).await?;
    if changed.get(1) != Some(&1) {
        return Err(invalid());
    }
    census_columns(store.sharing_read(CENSUS_COLUMNS_SQL, vec![]).await?)?;
    if matches!(mode, CensusBackend::Replicated) {
        let removal = store.sharing_read("SELECT json_array(type,sql) AS payload FROM sqlite_master WHERE name='sharing_purpose_census_removed_node'",vec![]).await?;
        let expected = serde_json::to_string(&(
            "trigger",
            CENSUS_REMOVAL_TRIGGER.replace(" IF NOT EXISTS", ""),
        ))
        .map_err(|_| invalid())?;
        if removal.as_slice() != [expected] {
            return Err(invalid());
        }
    }
    let rows=store.sharing_read("SELECT json_object('node_id',node_id,'raft_id',raft_id,'attempt_id',attempt_id,'generation',generation) AS payload FROM sharing_purpose_census_intents WHERE node_id=$1 AND raft_id=$2 AND attempt_id=$3",vec![identity.node_id.clone().into(),raft_id.into(),attempt.into()]).await?;
    let [raw] = rows.as_slice() else {
        return Err(invalid());
    };
    let claim: PurposeCensus = serde_json::from_str(raw).map_err(|_| invalid())?;
    if claim.node_id != identity.node_id
        || claim.raft_id != raft_id
        || claim.attempt_id != attempt
        || claim.generation <= 0
    {
        return Err(invalid());
    }
    Ok(claim)
}
#[cfg(feature = "hiqlite-store")]
pub(crate) async fn begin_local_census(
    store: &crate::store::SqliteStore,
    identity: &crate::cluster::ClusterIdentity,
    now_ms: i64,
) -> Result<PurposeCensus, StoreError> {
    begin_census(store, identity, now_ms, CensusBackend::Local).await
}
#[cfg(feature = "hiqlite-store")]
pub(crate) async fn begin_replicated_census(
    store: &crate::store::HiqliteAuthStore,
    identity: &crate::cluster::ClusterIdentity,
    now_ms: i64,
) -> Result<PurposeCensus, StoreError> {
    begin_census(store, identity, now_ms, CensusBackend::Replicated).await
}
#[cfg(feature = "hiqlite-store")]
pub(crate) async fn finish_census<T: Backend + ?Sized>(
    store: &T,
    claim: PurposeCensus,
) -> Result<(), StoreError> {
    let changed=store.sharing_txn(vec![("DELETE FROM sharing_purpose_census_intents WHERE node_id=$1 AND raft_id=$2 AND attempt_id=$3 AND generation=$4".to_owned(),vec![claim.node_id.into(),claim.raft_id.into(),claim.attempt_id.into(),claim.generation.into()])]).await?;
    if changed.as_slice() != [1] {
        return Err(invalid());
    }
    Ok(())
}
#[cfg(feature = "hiqlite-contract-tests")]
#[doc(hidden)]
pub async fn with_purpose_census_for_contract<F, Fut, R>(
    store: &crate::store::HiqliteAuthStore,
    identity: &crate::cluster::ClusterIdentity,
    now_ms: i64,
    operation: F,
) -> Result<R, StoreError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = R>,
{
    let claim = begin_replicated_census(store, identity, now_ms).await?;
    let result = operation().await;
    finish_census(store, claim).await?;
    Ok(result)
}
enum InstallationState {
    Fresh,
    Installed(Installation),
}
async fn installation_state<T: Backend + ?Sized>(
    store: &T,
) -> Result<InstallationState, StoreError> {
    let tables=store.sharing_read("SELECT json_array(name,type) AS payload FROM sqlite_master WHERE name IN ('sharing_catalogue_keys','sharing_file_locator_keys','sharing_purpose_key_installation','sharing_purpose_key_archive','sharing_purpose_census_intents','sharing_purpose_transaction_guard') ORDER BY name",vec![]).await?;
    let tables: Vec<(String, String)> = tables
        .into_iter()
        .map(|row| serde_json::from_str(&row).map_err(|_| invalid()))
        .collect::<Result<_, _>>()?;
    if tables.is_empty() {
        return Ok(InstallationState::Fresh);
    }
    if tables.as_slice()
        == [(
            "sharing_purpose_census_intents".to_owned(),
            "table".to_owned(),
        )]
    {
        census_columns(store.sharing_read(CENSUS_COLUMNS_SQL, vec![]).await?)?;
        return Ok(InstallationState::Fresh);
    }
    if tables.len() != 6 || tables.iter().any(|(_, kind)| kind != "table") {
        return Err(invalid());
    }
    for (table, expected) in [
        (
            "sharing_purpose_key_installation",
            vec![
                ("singleton", "INTEGER", 1, 1),
                ("server_id", "TEXT", 1, 0),
                ("catalogue_epoch", "TEXT", 1, 0),
                ("master_key_id", "TEXT", 1, 0),
                ("state", "TEXT", 1, 0),
                ("generation", "INTEGER", 1, 0),
                ("updated_at_ms", "INTEGER", 1, 0),
            ],
        ),
        (
            "sharing_purpose_census_intents",
            vec![
                ("node_id", "TEXT", 1, 1),
                ("raft_id", "INTEGER", 1, 0),
                ("attempt_id", "TEXT", 1, 0),
                ("generation", "INTEGER", 1, 0),
                ("claimed_at_ms", "INTEGER", 1, 0),
            ],
        ),
        (
            "sharing_purpose_transaction_guard",
            vec![("singleton", "INTEGER", 1, 1), ("passed", "INTEGER", 1, 0)],
        ),
    ] {
        let rows=store.sharing_read(&format!("SELECT json_array(name,type,\"notnull\",pk) AS payload FROM pragma_table_info('{table}') ORDER BY cid"),vec![]).await?;
        let rows: Vec<(String, String, i64, i64)> = rows
            .into_iter()
            .map(|row| serde_json::from_str(&row).map_err(|_| invalid()))
            .collect::<Result<_, _>>()?;
        let expected: Vec<_> = expected
            .into_iter()
            .map(|(name, kind, notnull, pk)| (name.to_owned(), kind.to_owned(), notnull, pk))
            .collect();
        if rows != expected {
            return Err(invalid());
        }
    }
    transaction_guard_shape(store.sharing_read(TRANSACTION_GUARD_SQL, vec![]).await?)?;
    archive_columns(store.sharing_read(ARCHIVE_COLUMNS_SQL, vec![]).await?)?;
    super::sharing::revision_key_columns(
        store
            .sharing_read(super::sharing::REVISION_KEY_COLUMNS_SQL, vec![])
            .await?,
    )?;
    super::sharing_file_locators::columns(
        store
            .sharing_read(super::sharing_file_locators::COLUMNS_SQL, vec![])
            .await?,
    )?;
    let rows=store.sharing_read("SELECT CASE WHEN length(server_id)=36 AND length(catalogue_epoch)=36 AND length(master_key_id)=8 AND length(state)<=15 THEN json_object('singleton',singleton,'server_id',server_id,'catalogue_epoch',catalogue_epoch,'master_key_id',master_key_id,'state',state,'generation',generation,'updated_at_ms',updated_at_ms) ELSE NULL END AS payload FROM sharing_purpose_key_installation LIMIT 2",vec![]).await?;
    let [row] = rows.as_slice() else {
        return Err(invalid());
    };
    let row: Installation = serde_json::from_str(row).map_err(|_| invalid())?;
    if row.singleton != 1
        || row.generation <= 0
        || row.updated_at_ms <= 0
        || row.master_key_id.len() != 8
        || !row
            .master_key_id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || !matches!(row.state.as_str(), "ready" | "restore_pending")
    {
        return Err(invalid());
    }
    let guard=store.sharing_read("SELECT json_array(singleton,passed) AS payload FROM sharing_purpose_transaction_guard LIMIT 2",vec![]).await?;
    if guard.as_slice() != ["[1,1]"] {
        return Err(invalid());
    }
    Ok(InstallationState::Installed(row))
}
async fn verify_installed<T: Backend + ?Sized>(
    store: &T,
    master: &CredentialKey,
    row: &Installation,
) -> Result<(), StoreError> {
    if row.master_key_id != master.id() {
        return Err(invalid());
    }
    let revision =
        super::sharing::revision_key_envelopes(store.sharing_revision_key_rows().await?)?;
    let locator =
        super::sharing::revision_key_envelopes(store.sharing_file_locator_key_rows().await?)?;
    if row.state == "ready" {
        let ([revision], [locator]) = (revision.as_slice(), locator.as_slice()) else {
            return Err(invalid());
        };
        let identity = SharingIdentity {
            server_id: row.server_id,
            catalogue_epoch: row.catalogue_epoch,
            created_at_ms: 0,
        };
        crate::sharing_catalogue_details::CatalogueRevisionKey::open(
            master,
            identity.clone(),
            revision,
        )?;
        crate::sharing_file_locators::FileLocatorKey::open(master, &identity, locator)?;
    } else if !revision.is_empty() || !locator.is_empty() {
        return Err(invalid());
    }
    let archive = archive_rows(store.sharing_purpose_archive_rows().await?)?;
    if row.state == "restore_pending"
        && archive
            .iter()
            .filter(|key| {
                key.server_id == row.server_id && key.catalogue_epoch == row.catalogue_epoch
            })
            .count()
            != 2
    {
        return Err(invalid());
    }
    for archived in archive {
        archived.verify(master)?;
    }
    Ok(())
}
/// The caller holds a read snapshot or the offline restore transaction. No
/// optional-table reads escape that snapshot, and no key is generated here.
#[cfg(feature = "hiqlite-store")]
pub(crate) struct ConnectionPurposeMaterial {
    marker: Installation,
    active: Vec<ArchivedPurposeKey>,
    archived: Vec<ArchivedPurposeKey>,
}
#[cfg(feature = "hiqlite-store")]
impl ConnectionPurposeMaterial {
    pub(crate) fn observe(&self, census: &mut crate::secrets::SealedRowCensus) {
        for row in self.active.iter().chain(&self.archived) {
            census.observe_envelopes("sharing_purpose_keys", &[&row.sealed()]);
        }
    }
    pub(crate) fn verify(&self, key: &CredentialKey) -> Result<(), StoreError> {
        if self.marker.master_key_id != key.id() {
            return Err(invalid());
        }
        for row in self.active.iter().chain(&self.archived) {
            row.verify(key)?;
        }
        Ok(())
    }
}
#[cfg(feature = "hiqlite-store")]
pub(crate) fn connection_material(
    connection: &rusqlite::Connection,
) -> Result<Option<ConnectionPurposeMaterial>, StoreError> {
    let read = |sql: &str| -> Result<Vec<String>, StoreError> {
        Ok(connection
            .prepare(sql)?
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    };
    let tables = read("SELECT json_array(name,type) FROM sqlite_master WHERE name IN ('sharing_catalogue_keys','sharing_file_locator_keys','sharing_purpose_key_installation','sharing_purpose_key_archive','sharing_purpose_census_intents','sharing_purpose_transaction_guard') ORDER BY name")?;
    if tables.is_empty() {
        return Ok(None);
    }
    if tables.as_slice() == ["[\"sharing_purpose_census_intents\",\"table\"]"] {
        census_columns(read(CENSUS_COLUMNS_SQL)?)?;
        return Ok(None);
    }
    if tables.len() != 6
        || tables.iter().any(|raw| {
            serde_json::from_str::<(String, String)>(raw).map_or(true, |(_, kind)| kind != "table")
        })
    {
        return Err(invalid());
    }
    super::sharing::revision_key_columns(read(super::sharing::REVISION_KEY_COLUMNS_SQL)?)?;
    super::sharing_file_locators::columns(read(super::sharing_file_locators::COLUMNS_SQL)?)?;
    transaction_guard_shape(read(TRANSACTION_GUARD_SQL)?)?;
    archive_columns(read(ARCHIVE_COLUMNS_SQL)?)?;
    for (table, expected) in [
        (
            "sharing_purpose_key_installation",
            r#"[["singleton","INTEGER",1,1],["server_id","TEXT",1,0],["catalogue_epoch","TEXT",1,0],["master_key_id","TEXT",1,0],["state","TEXT",1,0],["generation","INTEGER",1,0],["updated_at_ms","INTEGER",1,0]]"#,
        ),
        (
            "sharing_purpose_census_intents",
            r#"[["node_id","TEXT",1,1],["raft_id","INTEGER",1,0],["attempt_id","TEXT",1,0],["generation","INTEGER",1,0],["claimed_at_ms","INTEGER",1,0]]"#,
        ),
        (
            "sharing_purpose_transaction_guard",
            r#"[["singleton","INTEGER",1,1],["passed","INTEGER",1,0]]"#,
        ),
    ] {
        let columns=read(&format!("SELECT json_array(name,type,\"notnull\",pk) FROM pragma_table_info('{table}') ORDER BY cid"))?;
        let columns: Vec<serde_json::Value> = columns
            .iter()
            .map(|raw| serde_json::from_str(raw).map_err(|_| invalid()))
            .collect::<Result<_, _>>()?;
        if serde_json::Value::Array(columns)
            != serde_json::from_str::<serde_json::Value>(expected).map_err(|_| invalid())?
        {
            return Err(invalid());
        }
    }
    if read("SELECT json_array(singleton,passed) FROM sharing_purpose_transaction_guard LIMIT 2")?
        .as_slice()
        != ["[1,1]"]
    {
        return Err(invalid());
    }
    let rows=read("SELECT CASE WHEN length(server_id)=36 AND length(catalogue_epoch)=36 AND length(master_key_id)=8 AND length(state)<=15 THEN json_object('singleton',singleton,'server_id',server_id,'catalogue_epoch',catalogue_epoch,'master_key_id',master_key_id,'state',state,'generation',generation,'updated_at_ms',updated_at_ms) ELSE NULL END FROM sharing_purpose_key_installation LIMIT 2")?;
    let [raw] = rows.as_slice() else {
        return Err(invalid());
    };
    let marker: Installation = serde_json::from_str(raw).map_err(|_| invalid())?;
    if marker.singleton != 1
        || marker.master_key_id.len() != 8
        || !marker
            .master_key_id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        || !matches!(marker.state.as_str(), "ready" | "restore_pending")
        || marker.generation <= 0
        || marker.updated_at_ms <= 0
    {
        return Err(invalid());
    }
    let mut active = Vec::new();
    for (purpose, sql) in [
        ("catalogue_revision", super::sharing::REVISION_KEY_ROWS_SQL),
        ("file_locator", super::sharing_file_locators::ROWS_SQL),
    ] {
        let rows = read(sql)?;
        let envelopes = super::sharing::revision_key_envelopes(rows.clone())?;
        if marker.state == "ready" {
            let ([raw], [envelope]) = (rows.as_slice(), envelopes.as_slice()) else {
                return Err(invalid());
            };
            let parsed: serde_json::Value = serde_json::from_str(raw).map_err(|_| invalid())?;
            if parsed["server_id"].as_str() != Some(marker.server_id.to_string().as_str())
                || parsed["catalogue_epoch"].as_str()
                    != Some(marker.catalogue_epoch.to_string().as_str())
            {
                return Err(invalid());
            }
            active.push(ArchivedPurposeKey {
                purpose: purpose.to_owned(),
                server_id: marker.server_id,
                catalogue_epoch: marker.catalogue_epoch,
                envelope: envelope.as_stored().to_owned(),
            });
        } else if !rows.is_empty() {
            return Err(invalid());
        }
    }
    let archived = archive_rows(read(ARCHIVE_ROWS_SQL)?)?;
    if marker.state == "restore_pending"
        && archived
            .iter()
            .filter(|row| {
                row.server_id == marker.server_id && row.catalogue_epoch == marker.catalogue_epoch
            })
            .count()
            != 2
    {
        return Err(invalid());
    }
    Ok(Some(ConnectionPurposeMaterial {
        marker,
        active,
        archived,
    }))
}
/// Explicit offline restore disposition, inside the caller's fencing write.
/// Retains old ciphertext under its old AAD identity before retiring that
/// identity; normal startup cannot use this operation to replace keys.
#[cfg(feature = "hiqlite-store")]
pub(crate) fn archive_for_restore(connection: &rusqlite::Connection) -> Result<(), StoreError> {
    let Some(material) = connection_material(connection)? else {
        return Ok(());
    };
    if material.marker.state == "restore_pending" {
        return Ok(());
    }
    if material.marker.generation == i64::MAX
        || material.archived.len() + material.active.len() > MAX_ARCHIVED_PURPOSE_KEYS
    {
        return Err(invalid());
    }
    for row in &material.active {
        connection.execute("INSERT INTO sharing_purpose_key_archive(purpose,server_id,catalogue_epoch,envelope) VALUES(?1,?2,?3,?4)",rusqlite::params![row.purpose,row.server_id.to_string(),row.catalogue_epoch.to_string(),row.envelope])?;
    }
    connection.execute("DELETE FROM sharing_catalogue_keys", [])?;
    connection.execute("DELETE FROM sharing_file_locator_keys", [])?;
    connection.execute("DELETE FROM sharing_purpose_census_intents", [])?;
    connection.execute("UPDATE sharing_purpose_key_installation SET state='restore_pending',generation=generation+1 WHERE singleton=1 AND state='ready'",[])?;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PurposeKeyInstallation {
    Ready,
    NotReady,
}
/// Explicit startup/coordinator operation. Ordinary catalogue/art/file reads
/// never call this trait. A current observation still has to pass its guard in
/// the factory's actual replicated write.
#[async_trait]
pub trait SharingPurposeKeyStore: Send + Sync {
    /// Shape/count census before selecting or creating a sealing master. The
    /// startup coordinator holds its durable census intent across this read.
    async fn inspect_sharing_purpose_material(&self) -> Result<(), StoreError>;
    async fn verify_sharing_purpose_material(
        &self,
        master: &CredentialKey,
    ) -> Result<PurposeKeyInstallation, StoreError>;
    #[cfg(feature = "hiqlite-store")]
    async fn rewrap_sharing_purpose_keys(
        &self,
        old: &CredentialKey,
        replacement: &CredentialKey,
        members: &crate::cluster::membership::PurposeKeyMembers,
        now_ms: i64,
    ) -> Result<(), StoreError>;
    #[cfg(feature = "hiqlite-store")]
    async fn install_sharing_purpose_keys(
        &self,
        master: &CredentialKey,
        members: &crate::cluster::membership::PurposeKeyMembers,
        now_ms: i64,
    ) -> Result<PurposeKeyInstallation, StoreError>;
}
#[async_trait]
impl<T: Backend + SharingStore + ?Sized> SharingPurposeKeyStore for T {
    async fn inspect_sharing_purpose_material(&self) -> Result<(), StoreError> {
        let InstallationState::Installed(row) = installation_state(self).await? else {
            return Ok(());
        };
        let revision =
            super::sharing::revision_key_envelopes(self.sharing_revision_key_rows().await?)?;
        let locator =
            super::sharing::revision_key_envelopes(self.sharing_file_locator_key_rows().await?)?;
        if (row.state == "ready" && (revision.len() != 1 || locator.len() != 1))
            || (row.state == "restore_pending" && (!revision.is_empty() || !locator.is_empty()))
        {
            return Err(invalid());
        }
        let archive = archive_rows(self.sharing_purpose_archive_rows().await?)?;
        if row.state == "restore_pending"
            && archive
                .iter()
                .filter(|key| {
                    key.server_id == row.server_id && key.catalogue_epoch == row.catalogue_epoch
                })
                .count()
                != 2
        {
            return Err(invalid());
        }
        Ok(())
    }
    async fn verify_sharing_purpose_material(
        &self,
        master: &CredentialKey,
    ) -> Result<PurposeKeyInstallation, StoreError> {
        match installation_state(self).await? {
            InstallationState::Fresh => Ok(PurposeKeyInstallation::NotReady),
            InstallationState::Installed(row) => {
                verify_installed(self, master, &row).await?;
                Ok(if row.state == "ready" {
                    PurposeKeyInstallation::Ready
                } else {
                    PurposeKeyInstallation::NotReady
                })
            }
        }
    }
    #[cfg(feature = "hiqlite-store")]
    async fn rewrap_sharing_purpose_keys(
        &self,
        old: &CredentialKey,
        replacement: &CredentialKey,
        members: &crate::cluster::membership::PurposeKeyMembers,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        if now_ms <= 0 || !members.matches_master(old) {
            return Err(invalid());
        }
        let InstallationState::Installed(row) = installation_state(self).await? else {
            return Err(invalid());
        };
        if row.generation == i64::MAX {
            return Err(invalid());
        }
        verify_installed(self, old, &row).await?;
        let archive = archive_rows(self.sharing_purpose_archive_rows().await?)?;
        let mut captured = Vec::new();
        let mut replacement_captured = Vec::new();
        let mut writes = Vec::new();
        for archived in archive {
            let replacement_envelope = archived.rewrapped(old, replacement)?;
            captured.push(serde_json::json!({"purpose":archived.purpose,"server_id":archived.server_id,"catalogue_epoch":archived.catalogue_epoch,"envelope":archived.envelope}));
            replacement_captured.push(serde_json::json!({"purpose":archived.purpose,"server_id":archived.server_id,"catalogue_epoch":archived.catalogue_epoch,"envelope":replacement_envelope.as_stored()}));
            writes.push(("UPDATE sharing_purpose_key_archive SET envelope=$1 WHERE purpose=$2 AND server_id=$3 AND catalogue_epoch=$4 AND envelope=$5".to_owned(),vec![replacement_envelope.as_stored().to_owned().into(),archived.purpose.into(),archived.server_id.into(),archived.catalogue_epoch.into(),archived.envelope.into()]));
        }
        let revision =
            super::sharing::revision_key_envelopes(self.sharing_revision_key_rows().await?)?;
        let locator =
            super::sharing::revision_key_envelopes(self.sharing_file_locator_key_rows().await?)?;
        let mut active_expected = String::new();
        for (purpose, table, column, envelopes) in [
            (
                "catalogue_revision",
                "sharing_catalogue_keys",
                "revision_envelope",
                revision,
            ),
            (
                "file_locator",
                "sharing_file_locator_keys",
                "locator_envelope",
                locator,
            ),
        ] {
            if row.state == "ready" {
                let [envelope] = envelopes.as_slice() else {
                    return Err(invalid());
                };
                let archived = ArchivedPurposeKey {
                    purpose: purpose.to_owned(),
                    server_id: row.server_id,
                    catalogue_epoch: row.catalogue_epoch,
                    envelope: envelope.as_stored().to_owned(),
                };
                let replacement_envelope = archived.rewrapped(old, replacement)?;
                captured.push(serde_json::json!({"purpose":purpose,"server_id":row.server_id,"catalogue_epoch":row.catalogue_epoch,"envelope":envelope.as_stored(),"active":true}));
                replacement_captured.push(serde_json::json!({"purpose":purpose,"server_id":row.server_id,"catalogue_epoch":row.catalogue_epoch,"envelope":replacement_envelope.as_stored(),"active":true}));
                writes.push((format!("UPDATE {table} SET {column}=$1 WHERE singleton=1 AND server_id=$2 AND catalogue_epoch=$3 AND {column}=$4"),vec![replacement_envelope.as_stored().to_owned().into(),row.server_id.into(),row.catalogue_epoch.into(),envelope.as_stored().to_owned().into()]));
                active_expected.push_str(&format!(" AND (SELECT count(*) FROM {table})=1 AND EXISTS(SELECT 1 FROM {table} current JOIN json_each($4) expected WHERE json_extract(expected.value,'$.active')=1 AND json_extract(expected.value,'$.purpose')='{purpose}' AND current.singleton=1 AND current.server_id=json_extract(expected.value,'$.server_id') AND current.catalogue_epoch=json_extract(expected.value,'$.catalogue_epoch') AND current.{column}=json_extract(expected.value,'$.envelope'))"));
            } else {
                active_expected.push_str(&format!(" AND NOT EXISTS(SELECT 1 FROM {table})"));
            }
        }
        let captured = serde_json::to_string(&captured).map_err(|_| invalid())?;
        if captured.len() > 2 * 1024 * 1024 {
            return Err(invalid());
        }
        let (guard, roster, cutoff, observed) = members
            .write_guard(now_ms, 1, 2, 3)
            .map_err(|_| invalid())?;
        let mut statements = vec![(
            "DELETE FROM sharing_purpose_transaction_guard".to_owned(),
            vec![],
        )];
        statements.push((format!("INSERT INTO sharing_purpose_transaction_guard VALUES(1,CASE WHEN ({guard}) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_census_intents) AND EXISTS(SELECT 1 FROM sharing_purpose_key_installation WHERE singleton=1 AND generation={} AND master_key_id='{}' AND state='{}') AND (SELECT count(*) FROM sharing_purpose_key_archive)=(SELECT count(*) FROM json_each($4) WHERE coalesce(json_extract(value,'$.active'),0)=0) AND NOT EXISTS(SELECT 1 FROM json_each($4) expected WHERE coalesce(json_extract(expected.value,'$.active'),0)=0 AND NOT EXISTS(SELECT 1 FROM sharing_purpose_key_archive current WHERE current.purpose=json_extract(expected.value,'$.purpose') AND current.server_id=json_extract(expected.value,'$.server_id') AND current.catalogue_epoch=json_extract(expected.value,'$.catalogue_epoch') AND current.envelope=json_extract(expected.value,'$.envelope'))) {active_expected} THEN 1 ELSE 0 END)",row.generation,old.id(),row.state),vec![roster.clone().into(),cutoff.into(),observed.into(),captured.into()]));
        statements.extend(writes);
        statements.push(("UPDATE sharing_purpose_key_installation SET master_key_id=$1,generation=generation+1,updated_at_ms=$2 WHERE singleton=1".to_owned(),vec![replacement.id().to_owned().into(),now_ms.into()]));
        let replacement_captured =
            serde_json::to_string(&replacement_captured).map_err(|_| invalid())?;
        if replacement_captured.len() > 2 * 1024 * 1024 {
            return Err(invalid());
        }
        let active_expected = active_expected.replace("$4", "$1");
        statements.push((format!("UPDATE sharing_purpose_transaction_guard SET passed=CASE WHEN EXISTS(SELECT 1 FROM sharing_purpose_key_installation WHERE singleton=1 AND generation={} AND master_key_id='{}' AND state='{}') AND (SELECT count(*) FROM sharing_purpose_key_archive)=(SELECT count(*) FROM json_each($1) WHERE coalesce(json_extract(value,'$.active'),0)=0) AND NOT EXISTS(SELECT 1 FROM json_each($1) expected WHERE coalesce(json_extract(expected.value,'$.active'),0)=0 AND NOT EXISTS(SELECT 1 FROM sharing_purpose_key_archive current WHERE current.purpose=json_extract(expected.value,'$.purpose') AND current.server_id=json_extract(expected.value,'$.server_id') AND current.catalogue_epoch=json_extract(expected.value,'$.catalogue_epoch') AND current.envelope=json_extract(expected.value,'$.envelope'))) {active_expected} THEN 1 ELSE 0 END WHERE singleton=1",row.generation+1,replacement.id(),row.state),vec![replacement_captured.into()]));
        self.sharing_txn(statements).await?;
        let InstallationState::Installed(current) = installation_state(self).await? else {
            return Err(invalid());
        };
        verify_installed(self, replacement, &current).await
    }
    #[cfg(feature = "hiqlite-store")]
    async fn install_sharing_purpose_keys(
        &self,
        master: &CredentialKey,
        members: &crate::cluster::membership::PurposeKeyMembers,
        now_ms: i64,
    ) -> Result<PurposeKeyInstallation, StoreError> {
        if now_ms <= 0 || members.actual_local_raft_id() == 0 || !members.matches_master(master) {
            return Err(invalid());
        }
        let state = installation_state(self).await?;
        if let InstallationState::Installed(ref row) = state {
            verify_installed(self, master, row).await?;
            if row.state == "ready" {
                return Ok(PurposeKeyInstallation::Ready);
            }
            if row.generation == i64::MAX {
                return Err(invalid());
            }
        }
        let expected_installation = match &state {
            InstallationState::Fresh=>"NOT EXISTS(SELECT 1 FROM sharing_purpose_key_installation) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_key_archive)".to_owned(),
            InstallationState::Installed(row)=>format!("EXISTS(SELECT 1 FROM sharing_purpose_key_installation WHERE singleton=1 AND state='restore_pending' AND generation={}) AND (SELECT count(*) FROM sharing_purpose_key_archive WHERE server_id='{}' AND catalogue_epoch='{}')=2",row.generation,row.server_id,row.catalogue_epoch),
        };
        let watermark = self.sharing_read("SELECT json_array(type) AS payload FROM sqlite_master WHERE name='item_identity_watermark'",vec![]).await?;
        let import_guard = if watermark.is_empty() {
            "NOT EXISTS(SELECT 1 FROM sqlite_master WHERE name='item_identity_watermark')"
        } else {
            if watermark.as_slice() != ["[\"table\"]"] {
                return Err(invalid());
            }
            let columns=self.sharing_read("SELECT json_array(name,type,\"notnull\",pk) AS payload FROM pragma_table_info('item_identity_watermark') ORDER BY cid",vec![]).await?;
            let expected = [
                "[\"singleton\",\"INTEGER\",1,1]",
                "[\"high_water\",\"INTEGER\",1,0]",
                "[\"importing\",\"INTEGER\",1,0]",
            ];
            if columns.as_slice() != expected {
                return Err(invalid());
            }
            "(SELECT count(*) FROM item_identity_watermark)=1 AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND high_water>=0 AND importing=0)"
        };
        let identity = self.sharing_identity(now_ms).await?;
        let revision = crate::sharing_catalogue_details::CatalogueRevisionKey::generate_sealed(
            master,
            identity.clone(),
        )?;
        let locator =
            crate::sharing_file_locators::FileLocatorKey::generate_sealed(master, &identity)?;
        let (guard, roster, cutoff, observed) = members
            .write_guard(now_ms, 1, 2, 3)
            .map_err(|_| invalid())?;
        let mut statements = Vec::new();
        for schema in [
            SHARING_PURPOSE_KEYS_SCHEMA,
            super::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
            super::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA,
        ] {
            let without_comments = schema
                .lines()
                .filter(|line| !line.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n");
            for statement in without_comments
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if matches!(state, InstallationState::Fresh) {
                    // The census table is the sole qualified pre-factory
                    // table. Every key/marker table must still be absent in
                    // this write; a competing complete winner is reopened.
                    let ddl =
                        if statement.starts_with("CREATE TABLE sharing_purpose_census_intents") {
                            statement.replace("CREATE TABLE ", "CREATE TABLE IF NOT EXISTS ")
                        } else {
                            statement.to_owned()
                        };
                    statements.push((ddl, vec![]));
                }
            }
        }
        statements.push((CENSUS_REMOVAL_TRIGGER.to_owned(), vec![]));
        statements.push((
            "DELETE FROM sharing_purpose_transaction_guard".into(),
            vec![],
        ));
        statements.push((format!("INSERT INTO sharing_purpose_transaction_guard VALUES(1,CASE WHEN ({guard}) AND NOT EXISTS(SELECT 1 FROM sharing_purpose_census_intents) AND ({expected_installation}) AND NOT EXISTS(SELECT 1 FROM sharing_catalogue_keys) AND NOT EXISTS(SELECT 1 FROM sharing_file_locator_keys) AND EXISTS(SELECT 1 FROM sharing_identity WHERE singleton=1 AND server_id=$4 AND catalogue_epoch=$5) AND ({import_guard}) THEN 1 ELSE 0 END)"),vec![Value::Text(roster),Value::Integer(cutoff),Value::Integer(observed),Value::Text(identity.server_id.to_string()),Value::Text(identity.catalogue_epoch.to_string())]));
        statements.push((
            "INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)".into(),
            vec![
                identity.server_id.into(),
                identity.catalogue_epoch.into(),
                revision.as_stored().to_owned().into(),
            ],
        ));
        statements.push((
            "INSERT INTO sharing_file_locator_keys VALUES(1,$1,$2,$3)".into(),
            vec![
                identity.server_id.into(),
                identity.catalogue_epoch.into(),
                locator.as_stored().to_owned().into(),
            ],
        ));
        let marker_sql = match state {
            InstallationState::Fresh=>"INSERT INTO sharing_purpose_key_installation VALUES(1,$1,$2,$3,'ready',1,$4)".to_owned(),
            InstallationState::Installed(row)=>format!("UPDATE sharing_purpose_key_installation SET server_id=$1,catalogue_epoch=$2,master_key_id=$3,state='ready',generation=generation+1,updated_at_ms=$4 WHERE singleton=1 AND state='restore_pending' AND generation={}",row.generation),
        };
        statements.push((
            marker_sql,
            vec![
                identity.server_id.into(),
                identity.catalogue_epoch.into(),
                master.id().to_owned().into(),
                now_ms.into(),
            ],
        ));
        statements.push(("UPDATE sharing_purpose_transaction_guard SET passed=CASE WHEN (SELECT count(*) FROM sharing_catalogue_keys)=1 AND (SELECT count(*) FROM sharing_file_locator_keys)=1 AND EXISTS(SELECT 1 FROM sharing_catalogue_keys WHERE singleton=1 AND server_id=$1 AND catalogue_epoch=$2 AND revision_envelope=$3) AND EXISTS(SELECT 1 FROM sharing_file_locator_keys WHERE singleton=1 AND server_id=$1 AND catalogue_epoch=$2 AND locator_envelope=$4) AND EXISTS(SELECT 1 FROM sharing_purpose_key_installation WHERE singleton=1 AND state='ready' AND server_id=$1 AND catalogue_epoch=$2 AND master_key_id=$5) THEN 1 ELSE 0 END WHERE singleton=1".to_owned(),vec![identity.server_id.into(),identity.catalogue_epoch.into(),revision.as_stored().to_owned().into(),locator.as_stored().to_owned().into(),master.id().to_owned().into()]));
        if let Err(error) = self.sharing_txn(statements).await {
            return match installation_state(self).await? {
                InstallationState::Fresh
                    if error
                        .to_string()
                        .contains("CHECK constraint failed: passed=1")
                        || error
                            .to_string()
                            .contains("CHECK constraint failed: passed = 1") =>
                {
                    Ok(PurposeKeyInstallation::NotReady)
                }
                InstallationState::Fresh => Err(error),
                InstallationState::Installed(row) => {
                    verify_installed(self, master, &row).await?;
                    Ok(if row.state == "ready" {
                        PurposeKeyInstallation::Ready
                    } else {
                        PurposeKeyInstallation::NotReady
                    })
                }
            };
        }
        let InstallationState::Installed(row) = installation_state(self).await? else {
            return Err(invalid());
        };
        verify_installed(self, master, &row).await?;
        Ok(PurposeKeyInstallation::Ready)
    }
}

#[cfg(all(test, feature = "hiqlite-store"))]
mod tests {
    use super::*;
    use crate::store::{sharing::Backend, SqliteStore};

    #[tokio::test]
    async fn sharing_purpose_census_tombstone_retires_only_exact_node_and_raft_claim() {
        let store = SqliteStore::open_in_memory().expect("census transaction store");
        let first_directory = tempfile::tempdir().expect("first identity directory");
        let second_directory = tempfile::tempdir().expect("second identity directory");
        let first = crate::cluster::initialize_identity(first_directory.path(), "census-cluster")
            .expect("actual persisted first identity");
        let second =
            crate::cluster::initialize_join_identity(second_directory.path(), "census-cluster", 2)
                .expect("persisted joined identity for the SQL removal fixture");
        store.sharing_txn(vec![("CREATE TABLE cluster_nodes(node_id TEXT PRIMARY KEY,raft_id INTEGER NOT NULL,removed_at INTEGER)".into(),vec![]),("INSERT INTO cluster_nodes VALUES($1,$2,NULL),($3,$4,NULL)".into(),vec![first.node_id.clone().into(),(first.raft_id as i64).into(),second.node_id.clone().into(),(second.raft_id as i64).into()])]).await.expect("actual identities in removal fixture");
        let first_claim = begin_census(&store, &first, 1000, CensusBackend::Replicated)
            .await
            .expect("first exact durable claim");
        let second_claim = begin_census(&store, &second, 1001, CensusBackend::Replicated)
            .await
            .expect("second exact durable claim");
        store
            .sharing_txn(vec![(
                "UPDATE cluster_nodes SET removed_at=1002 WHERE node_id=$1 AND raft_id=$2".into(),
                vec![first.node_id.clone().into(), (first.raft_id as i64).into()],
            )])
            .await
            .expect("atomic removal tombstone");
        assert!(
            finish_census(&store, first_claim).await.is_err(),
            "retired ownership cannot release another claim"
        );
        assert_eq!(store.sharing_read("SELECT json_array(node_id,raft_id) AS payload FROM sharing_purpose_census_intents",vec![]).await.expect("other node ownership retained"),[serde_json::to_string(&(second.node_id.clone(),second.raft_id)).expect("fixture tuple")]);
        finish_census(&store, second_claim)
            .await
            .expect("unrelated exact owner can still complete");
    }

    #[tokio::test]
    async fn sharing_purpose_census_old_attempt_cannot_release_new_generation() {
        let directory = tempfile::tempdir().expect("node directory");
        let store = SqliteStore::open_in_memory().expect("store");
        let logical = crate::store::SettingsStore::instance_id(&store)
            .await
            .expect("actual logical identity");
        let identity = crate::cluster::initialize_identity(directory.path(), &logical)
            .expect("actual node identity");
        let old = begin_local_census(&store, &identity, 1000)
            .await
            .expect("first boot ownership");
        let new = begin_local_census(&store, &identity, 1001)
            .await
            .expect("same node next generation");
        assert!(finish_census(&store, old).await.is_err());
        assert_eq!(
            store
                .sharing_read(
                    "SELECT json_array(generation) AS payload FROM sharing_purpose_census_intents",
                    vec![]
                )
                .await
                .expect("new ownership survives"),
            ["[2]"]
        );
        finish_census(&store, new)
            .await
            .expect("only current exact attempt releases");
        assert_eq!(
            store
                .sharing_read(
                    "SELECT json_array(count(*)) AS payload FROM sharing_purpose_census_intents",
                    vec![]
                )
                .await
                .expect("settled"),
            ["[0]"]
        );
    }
    #[test]
    fn sharing_purpose_rewrap_preserves_active_and_archived_material_with_original_aad() {
        let old = CredentialKey::from_bytes([46; 32]);
        let replacement = CredentialKey::from_bytes([47; 32]);
        for _ in 0..2 {
            let identity = SharingIdentity {
                server_id: Uuid::new_v4(),
                catalogue_epoch: Uuid::new_v4(),
                created_at_ms: 1,
            };
            for (purpose, label, envelope) in [
                (
                    SharingSecretPurpose::CatalogueRevision,
                    "catalogue_revision",
                    crate::sharing_catalogue_details::CatalogueRevisionKey::generate_sealed(
                        &old,
                        identity.clone(),
                    )
                    .expect("Source key"),
                ),
                (
                    SharingSecretPurpose::FileLocator,
                    "file_locator",
                    crate::sharing_file_locators::FileLocatorKey::generate_sealed(&old, &identity)
                        .expect("B key"),
                ),
            ] {
                let row = ArchivedPurposeKey {
                    purpose: label.to_owned(),
                    server_id: identity.server_id,
                    catalogue_epoch: identity.catalogue_epoch,
                    envelope: envelope.as_stored().to_owned(),
                };
                let rewrapped = row
                    .rewrapped(&old, &replacement)
                    .expect("original AAD rewrap");
                assert_eq!(
                    old.open_sharing(
                        purpose,
                        identity.server_id,
                        identity.catalogue_epoch,
                        &envelope
                    )
                    .expect("original")
                    .expose(),
                    replacement
                        .open_sharing(
                            purpose,
                            identity.server_id,
                            identity.catalogue_epoch,
                            &rewrapped
                        )
                        .expect("replacement")
                        .expose()
                );
                assert!(old
                    .open_sharing(
                        purpose,
                        identity.server_id,
                        identity.catalogue_epoch,
                        &rewrapped
                    )
                    .is_err());
                assert!(replacement
                    .open_sharing(
                        purpose,
                        Uuid::new_v4(),
                        identity.catalogue_epoch,
                        &rewrapped
                    )
                    .is_err());
            }
        }
    }
    #[tokio::test]
    async fn sharing_purpose_restore_archives_exact_keys_and_refuses_partial_or_excess_state() {
        for pooled in [false, true] {
            let directory = tempfile::tempdir().expect("directory");
            let path = directory.path().join("purpose.sqlite");
            let store = if pooled {
                SqliteStore::open_with_read_connections(&path, 2).expect("pooled")
            } else {
                SqliteStore::open(&path).expect("single")
            };
            let master = CredentialKey::from_bytes([44; 32]);
            let identity = store.sharing_identity(1000).await.expect("real identity");
            let source = crate::sharing_catalogue_details::CatalogueRevisionKey::generate_sealed(
                &master,
                identity.clone(),
            )
            .expect("Source purpose");
            let receiver =
                crate::sharing_file_locators::FileLocatorKey::generate_sealed(&master, &identity)
                    .expect("B purpose");
            let mut statements = Vec::new();
            for schema in [
                SHARING_PURPOSE_KEYS_SCHEMA,
                super::super::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA,
                super::super::sharing_file_locators::CANDIDATE_FILE_LOCATOR_KEY_SCHEMA,
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
                        .map(|sql| (sql.to_owned(), vec![])),
                );
            }
            statements.push((
                "INSERT INTO sharing_catalogue_keys VALUES(1,$1,$2,$3)".into(),
                vec![
                    identity.server_id.into(),
                    identity.catalogue_epoch.into(),
                    source.as_stored().to_owned().into(),
                ],
            ));
            statements.push((
                "INSERT INTO sharing_file_locator_keys VALUES(1,$1,$2,$3)".into(),
                vec![
                    identity.server_id.into(),
                    identity.catalogue_epoch.into(),
                    receiver.as_stored().to_owned().into(),
                ],
            ));
            statements.push((
                "INSERT INTO sharing_purpose_key_installation VALUES(1,$1,$2,$3,'ready',1,1000)"
                    .into(),
                vec![
                    identity.server_id.into(),
                    identity.catalogue_epoch.into(),
                    master.id().to_owned().into(),
                ],
            ));
            statements.push((
                "INSERT INTO sharing_purpose_transaction_guard VALUES(1,1)".into(),
                vec![],
            ));
            store
                .sharing_txn(statements)
                .await
                .expect("candidate provision fixture");
            assert_eq!(
                store
                    .verify_sharing_purpose_material(&master)
                    .await
                    .expect("current ready"),
                PurposeKeyInstallation::Ready
            );
            let connection = rusqlite::Connection::open(&path).expect("offline snapshot");
            connection
                .pragma_update(None, "foreign_keys", "ON")
                .expect("actual FK enforcement");
            let material = connection_material(&connection)
                .expect("strict snapshot")
                .expect("installed");
            material.verify(&master).expect("both keys open");
            assert!(material
                .verify(&CredentialKey::from_bytes([45; 32]))
                .is_err());
            crate::store::sharing::fence_restored_sharing(&connection)
                .expect("actual restore fencing write");
            assert_eq!(
                store
                    .verify_sharing_purpose_material(&master)
                    .await
                    .expect("archive still opens"),
                PurposeKeyInstallation::NotReady
            );
            let snapshot = connection_material(&connection)
                .expect("restored snapshot")
                .expect("retained marker");
            assert!(snapshot.active.is_empty());
            assert_eq!(snapshot.archived.len(), 2);
            assert_eq!(snapshot.archived[0].envelope, source.as_stored());
            assert_eq!(snapshot.archived[1].envelope, receiver.as_stored());
            assert_eq!(
                store
                    .sharing_sealed_census()
                    .await
                    .expect("archive remains census-visible")
                    .sealed_rows(),
                2
            );
            let new_identity = store
                .sharing_identity(1001)
                .await
                .expect("explicit restored new identity");
            assert_ne!(identity.server_id, new_identity.server_id);
            assert_ne!(identity.catalogue_epoch, new_identity.catalogue_epoch);
            assert_eq!(
                store
                    .sharing_sealed_census()
                    .await
                    .expect("old AAD is retained separately")
                    .sealed_rows(),
                2
            );
            // An interrupted/malformed archived pair cannot be silently replaced.
            connection
                .execute(
                    "DELETE FROM sharing_purpose_key_archive WHERE purpose='file_locator'",
                    [],
                )
                .expect("partial fixture");
            assert!(connection_material(&connection).is_err());
            assert!(store.sharing_sealed_census().await.is_err());
            connection
                .execute(
                    "INSERT INTO sharing_purpose_key_archive VALUES('file_locator',?1,?2,?3)",
                    rusqlite::params![
                        identity.server_id.to_string(),
                        identity.catalogue_epoch.to_string(),
                        receiver.as_stored()
                    ],
                )
                .expect("restore exact counterpart");
            // Bounded archive capacity is a refusal, never eviction of sealed keys.
            for _ in 0..128 {
                let server = Uuid::new_v4().to_string();
                let epoch = Uuid::new_v4().to_string();
                for (purpose, envelope) in [
                    ("catalogue_revision", source.as_stored()),
                    ("file_locator", receiver.as_stored()),
                ] {
                    connection
                        .execute(
                            "INSERT INTO sharing_purpose_key_archive VALUES(?1,?2,?3,?4)",
                            rusqlite::params![purpose, server, epoch, envelope],
                        )
                        .expect("capacity fixture");
                }
            }
            assert!(connection_material(&connection).is_err());
            assert!(store.sharing_sealed_census().await.is_err());
        }
    }
}
