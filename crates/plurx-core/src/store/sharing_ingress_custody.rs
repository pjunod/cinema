//! Shared adjunct CAS helpers. Source/B adapters supply their own exact
//! principal/fence guards in the SAME write transaction as these statements.
use super::sharing::{Backend, Statement};
use crate::{error::StoreError, sharing::invalid, sharing_ingress_custody::IngressCustodyState};
use async_trait::async_trait;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Clone)]
pub struct IngressCustodySnapshot {
    pub principal_kind: String,
    pub incarnation_id: Uuid,
    pub owner_identity: String,
    pub revision: i64,
    pub state: IngressCustodyState,
}
fn identity(kind: &str, incarnation: Uuid, owner: &str) -> Result<(), StoreError> {
    if !matches!(kind, "source" | "receiver")
        || incarnation.is_nil()
        || !crate::sharing::is_hash(owner)
    {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) async fn read<T: Backend>(
    store: &T,
    kind: &str,
    incarnation: Uuid,
    owner: &str,
) -> Result<Option<IngressCustodySnapshot>, StoreError> {
    identity(kind, incarnation, owner)?;
    let rows = store.sharing_read(
        "SELECT json_object('revision',revision,'custody',custody_json) AS payload FROM sharing_ingress_custody WHERE principal_kind=$1 AND incarnation_id=$2 AND owner_identity=$3 LIMIT 2",
        vec![kind.to_owned().into(), incarnation.into(), owner.to_owned().into()],
    ).await?;
    let [row] = rows.as_slice() else {
        return if rows.is_empty() {
            Ok(None)
        } else {
            Err(invalid())
        };
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Row {
        revision: i64,
        custody: String,
    }
    let row: Row = serde_json::from_str(row).map_err(|_| invalid())?;
    if row.revision <= 0 {
        return Err(invalid());
    }
    let state = IngressCustodyState::decode(&row.custody)?;
    if kind == "receiver" && state.source_routing().is_some() {
        return Err(invalid());
    }
    Ok(Some(IngressCustodySnapshot {
        principal_kind: kind.into(),
        incarnation_id: incarnation,
        owner_identity: owner.into(),
        revision: row.revision,
        state,
    }))
}
#[allow(dead_code)] // Complete CAS helper; Source/B same-write principal adapters integrate next.
pub(crate) fn create(kind: &str, incarnation: Uuid, owner: &str) -> Result<Statement, StoreError> {
    identity(kind, incarnation, owner)?;
    Ok(("INSERT INTO sharing_ingress_custody(principal_kind,incarnation_id,owner_identity,custody_json,revision) VALUES($1,$2,$3,$4,1) ON CONFLICT(principal_kind,incarnation_id) DO NOTHING".into(),
        vec![kind.to_owned().into(), incarnation.into(), owner.to_owned().into(), IngressCustodyState::default().encode()?.into()]))
}
#[allow(dead_code)] // Complete CAS helper; Source/B same-write principal adapters integrate next.
pub(crate) fn compare_and_swap(
    snapshot: &IngressCustodySnapshot,
    next: &IngressCustodyState,
) -> Result<Statement, StoreError> {
    if snapshot.revision <= 0 || snapshot.revision == i64::MAX {
        return Err(invalid());
    }
    Ok(("UPDATE sharing_ingress_custody SET custody_json=$1,revision=revision+1 WHERE principal_kind=$2 AND incarnation_id=$3 AND owner_identity=$4 AND revision=$5 AND custody_json=$6".into(),
        vec![next.encode()?.into(), snapshot.principal_kind.clone().into(), snapshot.incarnation_id.into(),
            snapshot.owner_identity.clone().into(), snapshot.revision.into(), snapshot.state.encode()?.into()]))
}
#[async_trait]
pub trait SharingIngressCustodyStore: Send + Sync {
    /// Read-only registration observation. Driver closure remains independent.
    async fn registered_ingress_driver(
        &self,
        kind: &str,
        incarnation: Uuid,
        owner: &str,
        registration: &crate::sharing_ingress_custody::IngressRegistration,
    ) -> Result<bool, StoreError>;
}
#[async_trait]
impl<T: Backend> SharingIngressCustodyStore for T {
    async fn registered_ingress_driver(
        &self,
        kind: &str,
        incarnation: Uuid,
        owner: &str,
        registration: &crate::sharing_ingress_custody::IngressRegistration,
    ) -> Result<bool, StoreError> {
        if !registration.valid() {
            return Ok(false);
        }
        Ok(read(self, kind, incarnation, owner)
            .await?
            .is_some_and(|snapshot| snapshot.state.contains_driver(registration)))
    }
}

pub const SCHEMA: &str = include_str!("sharing_ingress_custody_schema.sql");
pub(crate) fn declaration() -> &'static str {
    SCHEMA[SCHEMA
        .find("CREATE TABLE")
        .expect("closed custody declaration")..]
        .trim()
        .trim_end_matches(';')
}
pub fn schema_guard() -> String {
    let declaration = declaration().replace('\'', "''");
    format!("EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='sharing_ingress_custody' AND sql='{declaration}') AND NOT EXISTS(SELECT 1 FROM sqlite_master WHERE type='trigger' AND tbl_name='sharing_ingress_custody')")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{SqliteStore, SQLITE_SCHEMA_VERSION};
    #[test]
    fn sharing_ingress_custody_migration_preserves_unresolved_debt_without_route_cascade() {
        let connection = rusqlite::Connection::open_in_memory().expect("database");
        SqliteStore::apply_migrations_for_test(&connection, 92)
            .expect("actual previous SQLite migration set");
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='sharing_ingress_custody'",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("previous census"),
            0
        );
        SqliteStore::apply_migrations_for_test(&connection, SQLITE_SCHEMA_VERSION)
            .expect("actual custody migration");
        let incarnation = uuid::Uuid::new_v4().to_string();
        let json = "{\"version\":1,\"sealed\":true,\"slots\":[{\"closed_confirmation\":null}],\"highwater\":[]}";
        connection
            .execute(
                "INSERT INTO sharing_ingress_custody VALUES('source',?1,?2,?3,1)",
                rusqlite::params![incarnation, "a".repeat(64), json],
            )
            .expect("unresolved diagnostic debt");
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM pragma_foreign_key_list('sharing_ingress_custody')",
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("foreign-key census"),
            0
        );
        connection
            .execute("DELETE FROM media_sessions", [])
            .expect("old route cleanup");
        assert_eq!(connection.query_row("SELECT custody_json FROM sharing_ingress_custody WHERE principal_kind='source' AND incarnation_id=?1",[incarnation],|row| row.get::<_,String>(0)).expect("debt preserved"),json);
        assert!(connection
            .execute(
                "UPDATE sharing_ingress_custody SET custody_json='invalid-json'",
                []
            )
            .is_err());
        assert!(connection
            .execute(
                "UPDATE sharing_ingress_custody SET custody_json=?1",
                [serde_json::json!("x".repeat(65536)).to_string()]
            )
            .is_err());
        assert!(
            connection
                .query_row(
                    &format!("SELECT CASE WHEN {} THEN 1 ELSE 0 END", schema_guard()),
                    [],
                    |row| row.get::<_, i64>(0)
                )
                .expect("exact adjunct")
                == 1
        );
    }
}
