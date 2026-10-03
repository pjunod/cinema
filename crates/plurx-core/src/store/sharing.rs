//! Backend-neutral sharing transactions. The SQL and outcome rules are shared.
use crate::{
    error::StoreError,
    secrets::{SealedRowCensus, SealedSecret},
    sharing::*,
};
use async_trait::async_trait;
use serde::de::DeserializeOwned;
use uuid::Uuid;

pub const SCHEMA: &str = include_str!("sharing_schema.sql");
#[derive(Clone)]
pub(crate) enum Value {
    Text(String),
    Integer(i64),
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Self::Text(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Self::Integer(v)
    }
}
impl From<Uuid> for Value {
    fn from(v: Uuid) -> Self {
        Self::Text(v.to_string())
    }
}
pub(crate) type Statement = (String, Vec<Value>);
fn stmt(sql: &str, values: Vec<Value>) -> Statement {
    (sql.into(), values)
}
/// SQLite numbers named dollar parameters by first appearance, not suffix.
/// Canonicalize the shared statements and bind in that same order on both stores.
pub(crate) fn ordered(sql: &str, values: Vec<Value>) -> Result<Statement, StoreError> {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| regex::Regex::new(r"\$(\d+)").expect("static parameter grammar"));
    let mut indices = Vec::new();
    let mut bad = false;
    let sql = re
        .replace_all(sql, |c: &regex::Captures<'_>| {
            let n = c[1].parse::<usize>().unwrap_or(0);
            if n == 0 || n > values.len() {
                bad = true;
            }
            let pos = indices.iter().position(|i| *i == n).unwrap_or_else(|| {
                indices.push(n);
                indices.len() - 1
            });
            format!("${}", pos + 1)
        })
        .into_owned();
    if bad || indices.len() != values.len() {
        return Err(invalid());
    }
    Ok((
        sql,
        indices.into_iter().map(|i| values[i - 1].clone()).collect(),
    ))
}
#[async_trait]
pub(crate) trait Backend: Send + Sync {
    async fn sharing_read(&self, sql: &str, values: Vec<Value>) -> Result<Vec<String>, StoreError>;
    async fn sharing_txn(&self, statements: Vec<Statement>) -> Result<Vec<usize>, StoreError>;
    /// Optional candidate table. SQLite reads its shape and rows in one
    /// snapshot; Hiqlite currently requires coordinated schema/rewrap quiescence.
    async fn sharing_revision_key_rows(&self) -> Result<Vec<String>, StoreError>;
    async fn sharing_file_locator_key_rows(&self) -> Result<Vec<String>, StoreError>;
}
pub(super) const REVISION_KEY_COLUMNS_SQL: &str = "SELECT json_array(name,type,\"notnull\",pk) AS payload FROM pragma_table_info('sharing_catalogue_keys') ORDER BY cid";
pub(super) const REVISION_KEY_ROWS_SQL: &str = "SELECT json_object('singleton',singleton,'server_id',substr(server_id,1,37),'catalogue_epoch',substr(catalogue_epoch,1,37),'envelope',CASE WHEN length(revision_envelope)<=4096 THEN revision_envelope ELSE NULL END,'source_matches',EXISTS(SELECT 1 FROM sharing_identity s WHERE s.singleton=1 AND s.server_id=sharing_catalogue_keys.server_id AND s.catalogue_epoch=sharing_catalogue_keys.catalogue_epoch)) AS payload FROM sharing_catalogue_keys LIMIT 2";
pub(super) fn revision_key_columns(rows: Vec<String>) -> Result<(), StoreError> {
    let columns: Vec<(String, String, i64, i64)> = rows
        .into_iter()
        .map(|r| decode(&r))
        .collect::<Result<_, _>>()?;
    if columns
        != [
            ("singleton".into(), "INTEGER".into(), 1, 1),
            ("server_id".into(), "TEXT".into(), 1, 0),
            ("catalogue_epoch".into(), "TEXT".into(), 1, 0),
            ("revision_envelope".into(), "TEXT".into(), 1, 0),
        ]
    {
        return Err(invalid());
    }
    Ok(())
}
pub(super) fn revision_key_envelopes(rows: Vec<String>) -> Result<Vec<SealedSecret>, StoreError> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Row {
        singleton: i64,
        server_id: String,
        catalogue_epoch: String,
        envelope: String,
        source_matches: i64,
    }
    if rows.len() > 1 {
        return Err(invalid());
    }
    rows.into_iter()
        .map(|row| {
            let row: Row = decode(&row)?;
            if row.singleton != 1
                || row.source_matches != 1
                || [row.server_id, row.catalogue_epoch].iter().any(|id| {
                    Uuid::parse_str(id)
                        .ok()
                        .is_none_or(|uuid| uuid.to_string() != *id)
                })
            {
                return Err(invalid());
            }
            let envelope = SealedSecret::from_stored(row.envelope);
            if !envelope.is_wrapped() {
                return Err(invalid());
            }
            Ok(envelope)
        })
        .collect()
}
fn decode<T: DeserializeOwned>(s: &str) -> Result<T, StoreError> {
    serde_json::from_str(s).map_err(|_| invalid())
}
fn json<T: serde::Serialize>(v: &T) -> Result<String, StoreError> {
    serde_json::to_string(v).map_err(|_| invalid())
}
fn libraries(ids: &[i64]) -> Result<(), StoreError> {
    if ids.len() > MAX_LIBRARIES
        || ids.iter().any(|id| *id < 0)
        || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
    {
        return Err(invalid());
    }
    Ok(())
}
const GRANT_JSON: &str =
    "json_object('id',e.id,'recipient_server_id',e.recipient_server_id,'state',e.state,
    'scope_generation',e.scope_generation,'credential_generation',e.credential_generation,
    'catalogue_generation',e.catalogue_generation,'mutation_generation',e.mutation_generation,
    'pending_expires_at_ms',e.pending_expires_at_ms)";

/// Consistent authority reads and atomic mutations; no foreign account identity.
#[async_trait]
pub trait SharingStore: Send + Sync {
    /// Canonical UUID keyset; at most 33 records (32 plus the next-page sentinel).
    async fn sharing_exports(&self, after: Option<Uuid>) -> Result<Vec<ExportSummary>, StoreError>;
    /// Credential-scoped status only, including terminal and expired pending grants.
    async fn sharing_grant_status(&self, hash: &str) -> Result<Option<ExportSummary>, StoreError>;
    async fn sharing_imports(&self) -> Result<Vec<ImportSummary>, StoreError>;
    async fn sharing_import(&self, id: Uuid) -> Result<Option<StoredImport>, StoreError>;
    async fn sharing_endpoint_manifest(&self) -> Result<Option<EndpointManifest>, StoreError>;
    async fn set_sharing_endpoint_manifest(
        &self,
        generation: i64,
        endpoints: Vec<Endpoint>,
    ) -> Result<MutationOutcome, StoreError>;
    async fn set_sharing_import_endpoints(
        &self,
        id: Uuid,
        generation: i64,
        endpoints: Vec<Endpoint>,
        source_revision: Option<i64>,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn sharing_identity(&self, now_ms: i64) -> Result<SharingIdentity, StoreError>;
    async fn create_share_invitation(
        &self,
        invitation: InvitationRecord,
    ) -> Result<MutationOutcome, StoreError>;
    async fn cancel_share_invitation(&self, id: Uuid) -> Result<(), StoreError>;
    async fn claim_share(&self, claim: ShareClaim) -> Result<ClaimOutcome, StoreError>;
    async fn approve_share(
        &self,
        id: Uuid,
        generation: i64,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn share_scope(
        &self,
        id: Uuid,
        generation: i64,
        libraries: Vec<i64>,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn revoke_share(&self, id: Uuid, now_ms: i64) -> Result<(), StoreError>;
    async fn authorize_share(
        &self,
        hash: &str,
        library: Option<i64>,
        now_ms: i64,
    ) -> Result<Authorization, StoreError>;
    async fn rotate_share(
        &self,
        id: Uuid,
        request: Uuid,
        old_hash: &str,
        new_hash: &str,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn share_rotation_status(
        &self,
        id: Uuid,
        request: Uuid,
        hash: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError>;
    async fn create_share_import(&self, import: NewImport) -> Result<ImportOutcome, StoreError>;
    async fn re_pair_share_import(
        &self,
        import: NewImport,
        expected_lifecycle: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn settle_share_claim(
        &self,
        id: Uuid,
        grant: Uuid,
        active: bool,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn settle_current_share_claim(
        &self,
        id: Uuid,
        claim: Uuid,
        lifecycle: i64,
        grant: Uuid,
        active: bool,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn fail_current_share_claim(
        &self,
        id: Uuid,
        claim: Uuid,
        lifecycle: i64,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn assign_share_viewers(
        &self,
        id: Uuid,
        generation: i64,
        assignments: Vec<Assignment>,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn authorize_share_viewer(
        &self,
        id: Uuid,
        library: SourceId,
        user_id: i64,
    ) -> Result<Option<String>, StoreError>;
    async fn disable_share_import(&self, id: Uuid, now_ms: i64) -> Result<(), StoreError>;
    async fn begin_share_import_rotation(
        &self,
        rotation: ImportRotation,
    ) -> Result<MutationOutcome, StoreError>;
    async fn sharing_import_rotation(
        &self,
        id: Uuid,
    ) -> Result<Option<StoredImportRotation>, StoreError>;
    async fn settle_share_import_response(
        &self,
        receipt: ImportClaimReceipt,
    ) -> Result<MutationOutcome, StoreError>;
    async fn commit_share_import_rotation(
        &self,
        id: Uuid,
        request: Uuid,
        lifecycle: i64,
        credential_generation: i64,
        credential: SealedSecret,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError>;
    async fn sharing_sealed_census(&self) -> Result<SealedRowCensus, StoreError>;
}

#[async_trait]
impl<T: Backend> SharingStore for T {
    async fn sharing_exports(&self, after: Option<Uuid>) -> Result<Vec<ExportSummary>, StoreError> {
        let rows = self.sharing_read(&format!("SELECT {EXPORT_SUMMARY_JSON} AS payload FROM sharing_exports e JOIN sharing_invitations i ON i.id=e.invitation_id JOIN sharing_identity s ON s.singleton=1 WHERE e.id>$1 ORDER BY e.id LIMIT 33"), vec![after.map(|id| id.to_string()).unwrap_or_default().into()]).await?;
        rows.iter().map(|row| export_summary(row)).collect()
    }
    async fn sharing_grant_status(&self, hash: &str) -> Result<Option<ExportSummary>, StoreError> {
        if !is_hash(hash) {
            return Ok(None);
        }
        let rows = self.sharing_read(&format!("SELECT {EXPORT_SUMMARY_JSON} AS payload FROM sharing_exports e JOIN sharing_invitations i ON i.id=e.invitation_id JOIN sharing_identity s ON s.singleton=1 WHERE e.token_hash=$1 LIMIT 1"), vec![hash.to_owned().into()]).await?;
        rows.first().map(|row| export_summary(row)).transpose()
    }
    async fn sharing_imports(&self) -> Result<Vec<ImportSummary>, StoreError> {
        let rows = self.sharing_read(&format!("SELECT {IMPORT_SUMMARY_JSON} AS payload FROM sharing_imports i ORDER BY i.id LIMIT 32"), vec![]).await?;
        rows.iter().map(|row| decode(row)).collect()
    }
    async fn sharing_import(&self, id: Uuid) -> Result<Option<StoredImport>, StoreError> {
        #[derive(serde::Deserialize)]
        struct Row {
            summary: ImportSummary,
            credential: String,
            claim: Option<String>,
        }
        let rows = self.sharing_read(&format!("SELECT json_object('summary',{IMPORT_SUMMARY_JSON},'credential',i.credential_envelope,'claim',i.claim_envelope) AS payload FROM sharing_imports i WHERE i.id=$1"), vec![id.into()]).await?;
        rows.first()
            .map(|row| {
                let row: Row = decode(row)?;
                Ok(StoredImport {
                    summary: row.summary,
                    credential: SealedSecret::from_stored(row.credential),
                    claim: row.claim.map(SealedSecret::from_stored),
                })
            })
            .transpose()
    }
    async fn sharing_endpoint_manifest(&self) -> Result<Option<EndpointManifest>, StoreError> {
        let rows = self.sharing_read("SELECT json_object('revision',revision,'endpoints',json(endpoints_json)) AS payload FROM sharing_endpoint_manifest WHERE singleton=1", vec![]).await?;
        rows.first().map(|row| decode(row)).transpose()
    }
    async fn set_sharing_endpoint_manifest(
        &self,
        generation: i64,
        endpoints: Vec<Endpoint>,
    ) -> Result<MutationOutcome, StoreError> {
        validate_endpoints(&endpoints)?;
        if generation < 0 || generation == i64::MAX {
            return Err(invalid());
        }
        let body = json(&endpoints)?;
        let counts = if generation == 0 {
            self.sharing_txn(vec![stmt("INSERT INTO sharing_endpoint_manifest(singleton,endpoints_json,revision) VALUES(1,$1,1) ON CONFLICT(singleton) DO NOTHING", vec![body.into()])]).await?
        } else {
            self.sharing_txn(vec![stmt("UPDATE sharing_endpoint_manifest SET endpoints_json=$1,revision=revision+1 WHERE singleton=1 AND revision=$2 AND revision<9223372036854775807", vec![body.into(), generation.into()])]).await?
        };
        Ok(if counts[0] == 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn set_sharing_import_endpoints(
        &self,
        id: Uuid,
        generation: i64,
        endpoints: Vec<Endpoint>,
        source_revision: Option<i64>,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError> {
        validate_endpoints(&endpoints)?;
        if generation < 1 || now_ms < 0 || source_revision.is_some_and(|r| r < 1) {
            return Err(invalid());
        }
        let revision = json(&source_revision)?;
        let counts = self.sharing_txn(vec![stmt("UPDATE sharing_imports SET endpoints_json=$1,endpoint_generation=endpoint_generation+1,observed_endpoint_revision=CASE WHEN json_extract($2,'$') IS NULL THEN observed_endpoint_revision ELSE json_extract($2,'$') END,updated_at_ms=$3 WHERE id=$4 AND endpoint_generation=$5 AND endpoint_generation<9223372036854775807 AND state IN ('claiming','pending','active') AND (json_extract($2,'$') IS NULL OR json_extract($2,'$')>coalesce(observed_endpoint_revision,0))", vec![json(&endpoints)?.into(), revision.into(), now_ms.into(), id.into(), generation.into()])]).await?;
        Ok(if counts[0] == 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn sharing_identity(&self, now_ms: i64) -> Result<SharingIdentity, StoreError> {
        if now_ms < 0 {
            return Err(invalid());
        }
        let existing = self
            .sharing_read(
                "SELECT json_object('server_id',server_id,'catalogue_epoch',catalogue_epoch,
            'created_at_ms',created_at_ms) AS payload FROM sharing_identity WHERE singleton=1",
                vec![],
            )
            .await?;
        if let Some(row) = existing.first() {
            return decode(row);
        }
        self.sharing_txn(vec![stmt(
            "INSERT INTO sharing_identity(singleton,server_id,catalogue_epoch,created_at_ms)
            VALUES(1,$1,$2,$3) ON CONFLICT(singleton) DO NOTHING",
            vec![Uuid::new_v4().into(), Uuid::new_v4().into(), now_ms.into()],
        )])
        .await?;
        let rows = self
            .sharing_read(
                "SELECT json_object('server_id',server_id,'catalogue_epoch',catalogue_epoch,
            'created_at_ms',created_at_ms) AS payload FROM sharing_identity WHERE singleton=1",
                vec![],
            )
            .await?;
        decode(rows.first().ok_or_else(invalid)?)
    }
    async fn create_share_invitation(
        &self,
        i: InvitationRecord,
    ) -> Result<MutationOutcome, StoreError> {
        libraries(&i.library_ids)?;
        if i.library_ids.is_empty() {
            return Err(invalid());
        }
        if !is_hash(&i.token_hash)
            || i.created_at_ms < 0
            || i.expires_at_ms <= i.created_at_ms
            || i.expires_at_ms - i.created_at_ms > MAX_INVITATION_TTL_MS
        {
            return Err(invalid());
        }
        let counts = self.sharing_txn(vec![stmt("INSERT INTO sharing_invitations
            (id,token_hash,library_ids_json,created_at_ms,expires_at_ms,state)
            SELECT $1,$2,$3,$4,$5,'open'
            WHERE (SELECT count(*) FROM sharing_invitations WHERE state='open' AND expires_at_ms>$4)<32
            AND NOT EXISTS(SELECT 1 FROM json_each($3) j LEFT JOIN libraries l ON l.id=j.value
                WHERE l.id IS NULL OR l.kind NOT IN ('movies','shows'))
            ON CONFLICT(id) DO NOTHING", vec![i.id.into(), i.token_hash.into(), json(&i.library_ids)?.into(),
                i.created_at_ms.into(), i.expires_at_ms.into()])]).await?;
        if counts[0] == 1 {
            return Ok(MutationOutcome::Applied);
        }
        // The write itself enforces the limit atomically. This read only names
        // the refusal; it never grants admission from a pre-count.
        let full = !self.sharing_read(
            "SELECT '{}' AS payload WHERE (SELECT count(*) FROM sharing_invitations WHERE state='open' AND expires_at_ms>$1)>=32 AND NOT EXISTS(SELECT 1 FROM sharing_invitations WHERE id=$2)",
            vec![i.created_at_ms.into(), i.id.into()],
        ).await?.is_empty();
        Ok(if full {
            MutationOutcome::Capacity
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn cancel_share_invitation(&self, id: Uuid) -> Result<(), StoreError> {
        self.sharing_txn(vec![stmt(
            "UPDATE sharing_invitations SET state='cancelled' WHERE id=$1 AND state='open'",
            vec![id.into()],
        )])
        .await?;
        Ok(())
    }
    async fn claim_share(&self, c: ShareClaim) -> Result<ClaimOutcome, StoreError> {
        c.validate()?;
        let digest = c.digest();
        let counts = self.sharing_txn(vec![
            stmt("UPDATE sharing_exports SET state='revoked',scope_generation=scope_generation+1,mutation_generation=mutation_generation+1,updated_at_ms=$1
                WHERE state='pending' AND pending_expires_at_ms<=$1",vec![c.now_ms.into()]),
            stmt("INSERT INTO sharing_exports(id,invitation_id,recipient_server_id,recipient_name,token_hash,
                scope_generation,credential_generation,catalogue_generation,mutation_generation,state,
                pending_expires_at_ms,created_at_ms,updated_at_ms)
                SELECT $1,id,$2,$3,$4,1,1,1,1,'pending',$5,$6,$6 FROM sharing_invitations
                WHERE id=$7 AND token_hash=$8 AND state='open' AND expires_at_ms>$6
                AND (SELECT count(*) FROM sharing_exports WHERE state!='revoked')<32
                ON CONFLICT DO NOTHING", vec![c.grant_id.into(), c.recipient_server_id.into(), c.recipient_name.into(),
                    c.credential_hash.clone().into(), (c.now_ms + PENDING_TTL_MS).into(), c.now_ms.into(),
                    c.invitation_id.into(), c.invitation_hash.clone().into()]),
            stmt("UPDATE sharing_invitations SET state='consumed',claim_id=$1,claim_digest=$2 WHERE id=$3
                AND state='open' AND EXISTS(SELECT 1 FROM sharing_exports WHERE id=$4 AND invitation_id=$3)",
                vec![c.claim_id.into(), digest.clone().into(), c.invitation_id.into(), c.grant_id.into()]),
            stmt("INSERT INTO sharing_export_libraries(grant_id,library_id)
                SELECT e.id,j.value FROM sharing_invitations i JOIN sharing_exports e ON e.invitation_id=i.id,
                    json_each(i.library_ids_json) j
                WHERE i.id=$1 AND i.claim_id=$2 AND i.claim_digest=$3 AND e.state='pending'
                AND EXISTS(SELECT 1 FROM libraries WHERE id=j.value AND kind IN ('movies','shows'))
                ON CONFLICT DO NOTHING", vec![c.invitation_id.into(), c.claim_id.into(), digest.clone().into()]),
        ]).await?;
        let sql = format!(
            "SELECT json_object('state',i.state,'expires',i.expires_at_ms,
            'matches',i.claim_id=$1 AND i.claim_digest=$2,'grant',{}) AS payload
            FROM sharing_invitations i LEFT JOIN sharing_exports e ON e.invitation_id=i.id
            WHERE i.id=$3 AND i.token_hash=$4",
            GRANT_JSON
        );
        let rows = self
            .sharing_read(
                &sql,
                vec![
                    c.claim_id.into(),
                    digest.into(),
                    c.invitation_id.into(),
                    c.invitation_hash.into(),
                ],
            )
            .await?;
        let Some(row) = rows.first() else {
            return Ok(ClaimOutcome::NotFound);
        };
        let v: serde_json::Value = decode(row)?;
        if v["matches"] == 1 {
            let g = serde_json::from_value(v["grant"].clone()).map_err(|_| invalid())?;
            return Ok(if counts[1] == 1 {
                ClaimOutcome::Created(g)
            } else {
                ClaimOutcome::Replay(g)
            });
        }
        Ok(match v["state"].as_str() {
            Some("consumed") => ClaimOutcome::Consumed,
            Some("cancelled") => ClaimOutcome::Cancelled,
            _ if v["expires"].as_i64().is_some_and(|t| t <= c.now_ms) => ClaimOutcome::Expired,
            _ => ClaimOutcome::Capacity,
        })
    }
    async fn approve_share(
        &self,
        id: Uuid,
        generation: i64,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError> {
        let counts = self.sharing_txn(vec![stmt("UPDATE sharing_exports SET state='active',mutation_generation=mutation_generation+1,
            updated_at_ms=$1 WHERE id=$2 AND mutation_generation=$3 AND mutation_generation<9223372036854775807
            AND state='pending' AND pending_expires_at_ms>$1
            AND NOT EXISTS(SELECT 1 FROM sharing_invitations i,json_each(i.library_ids_json) j
                LEFT JOIN libraries l ON l.id=j.value WHERE i.id=sharing_exports.invitation_id
                AND (l.id IS NULL OR l.kind NOT IN ('movies','shows')))", vec![now_ms.into(), id.into(), generation.into()])]).await?;
        Ok(if counts[0] == 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn share_scope(
        &self,
        id: Uuid,
        generation: i64,
        ids: Vec<i64>,
        now_ms: i64,
    ) -> Result<MutationOutcome, StoreError> {
        libraries(&ids)?;
        let ids = json(&ids)?;
        // Compute authority loss before replacing links; mutation CAS remains
        // unchanged until the final statement. All four steps commit together.
        let guard =
            "EXISTS(SELECT 1 FROM sharing_exports e WHERE e.id=$1 AND e.mutation_generation=$2
            AND e.state='active' AND e.mutation_generation<9223372036854775807)
            AND NOT EXISTS(SELECT 1 FROM json_each($3) j LEFT JOIN libraries l ON l.id=j.value
                WHERE l.id IS NULL OR l.kind NOT IN ('movies','shows'))";
        let counts = self.sharing_txn(vec![
            stmt(&format!("UPDATE sharing_exports SET scope_generation=scope_generation+
                EXISTS(SELECT 1 FROM sharing_export_libraries l WHERE l.grant_id=$1 AND l.library_id NOT IN (SELECT value FROM json_each($3))),
                catalogue_generation=catalogue_generation+(
                  EXISTS(SELECT 1 FROM sharing_export_libraries l WHERE l.grant_id=$1 AND l.library_id NOT IN (SELECT value FROM json_each($3)))
                  OR EXISTS(SELECT 1 FROM json_each($3) j WHERE NOT EXISTS(SELECT 1 FROM sharing_export_libraries l WHERE l.grant_id=$1 AND l.library_id=j.value)))
                WHERE id=$1 AND {guard}"),vec![id.into(),generation.into(),ids.clone().into()]),
            stmt(&format!("DELETE FROM sharing_export_libraries WHERE grant_id=$1 AND {guard}
                AND library_id NOT IN (SELECT value FROM json_each($3))"), vec![id.into(),generation.into(),ids.clone().into()]),
            stmt(&format!("INSERT INTO sharing_export_libraries(grant_id,library_id) SELECT $1,value FROM json_each($3)
                WHERE {guard} ON CONFLICT DO NOTHING"), vec![id.into(),generation.into(),ids.clone().into()]),
            stmt(&format!("UPDATE sharing_exports SET mutation_generation=mutation_generation+1,updated_at_ms=$4
                WHERE id=$1 AND {guard}"),vec![id.into(),generation.into(),ids.into(),now_ms.into()]),
        ]).await?;
        Ok(if counts[3] == 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn revoke_share(&self, id: Uuid, now_ms: i64) -> Result<(), StoreError> {
        self.sharing_txn(vec![stmt("UPDATE sharing_exports SET state='revoked',scope_generation=scope_generation+1,
            mutation_generation=mutation_generation+1,updated_at_ms=$1 WHERE id=$2 AND state!='revoked'",vec![now_ms.into(),id.into()])]).await?;
        Ok(())
    }
    async fn authorize_share(
        &self,
        hash: &str,
        library: Option<i64>,
        now_ms: i64,
    ) -> Result<Authorization, StoreError> {
        if !is_hash(hash) || library.is_some_and(|id| id < 0) {
            return Ok(Authorization::Denied);
        }
        let rows = self.sharing_read(&format!("SELECT {GRANT_JSON} AS payload FROM sharing_exports e WHERE e.token_hash=$1
            AND ($2<0 OR EXISTS(SELECT 1 FROM sharing_export_libraries l JOIN libraries b ON b.id=l.library_id
                WHERE l.grant_id=e.id AND l.library_id=$2 AND b.kind IN ('movies','shows')))"),
            vec![hash.to_owned().into(),library.unwrap_or(-1).into()]).await?;
        let Some(row) = rows.first() else {
            return Ok(Authorization::Denied);
        };
        let g: ExportGrant = decode(row)?;
        Ok(match g.state {
            GrantState::Active => Authorization::Allowed(g),
            GrantState::Pending if library.is_none() && g.pending_expires_at_ms > now_ms => {
                Authorization::Pending
            }
            _ => Authorization::Denied,
        })
    }
    async fn rotate_share(
        &self,
        id: Uuid,
        request: Uuid,
        old: &str,
        new: &str,
        now: i64,
    ) -> Result<MutationOutcome, StoreError> {
        if !is_hash(old)
            || !is_hash(new)
            || old == new
            || now.checked_add(ROTATION_TTL_MS).is_none()
        {
            return Err(invalid());
        }
        let counts = self.sharing_txn(vec![
            stmt("DELETE FROM sharing_rotations WHERE grant_id=$1 AND expires_at_ms<=$2", vec![id.into(),now.into()]),
            stmt("INSERT INTO sharing_rotations(grant_id,request_id,old_hash,new_hash,created_at_ms,expires_at_ms)
                SELECT $1,$2,$3,$4,$5,$6 WHERE EXISTS(SELECT 1 FROM sharing_exports WHERE id=$1 AND token_hash=$3
                AND state='active' AND max(credential_generation,mutation_generation)<9223372036854775807)
                ON CONFLICT DO NOTHING",vec![id.into(),request.into(),old.to_owned().into(),new.to_owned().into(),now.into(),(now+ROTATION_TTL_MS).into()]),
            stmt("UPDATE sharing_exports SET token_hash=$1,credential_generation=credential_generation+1,
                mutation_generation=mutation_generation+1,updated_at_ms=$2 WHERE id=$3 AND token_hash=$4 AND state='active'
                AND EXISTS(SELECT 1 FROM sharing_rotations WHERE grant_id=$3 AND request_id=$5 AND old_hash=$4 AND new_hash=$1 AND expires_at_ms>$2)",
                vec![new.to_owned().into(),now.into(),id.into(),old.to_owned().into(),request.into()]),
        ]).await?;
        if counts[2] == 1 || self.share_rotation_status(id, request, new, now).await? {
            Ok(MutationOutcome::Applied)
        } else {
            Ok(MutationOutcome::Conflict)
        }
    }
    async fn share_rotation_status(
        &self,
        id: Uuid,
        request: Uuid,
        hash: &str,
        now: i64,
    ) -> Result<bool, StoreError> {
        if !is_hash(hash) {
            return Ok(false);
        }
        Ok(!self.sharing_read("SELECT '{}' AS payload FROM sharing_rotations r JOIN sharing_exports e ON e.id=r.grant_id
            WHERE r.grant_id=$1 AND r.request_id=$2 AND (r.old_hash=$3 OR r.new_hash=$3)
            AND r.expires_at_ms>$4 AND e.state='active' AND e.token_hash=r.new_hash",vec![id.into(),request.into(),hash.to_owned().into(),now.into()]).await?.is_empty())
    }
    async fn create_share_import(&self, i: NewImport) -> Result<ImportOutcome, StoreError> {
        validate_endpoints(&i.endpoints)?;
        if i.source_name.len() > 128 || i.source_name.chars().any(char::is_control) || i.now_ms < 0
        {
            return Err(invalid());
        }
        let credential = i.credential.to_persist().map_err(|_| invalid())?.to_owned();
        let claim = i
            .claim_secret
            .to_persist()
            .map_err(|_| invalid())?
            .to_owned();
        let counts=self.sharing_txn(vec![stmt("INSERT INTO sharing_imports(id,source_server_id,catalogue_epoch,source_name,claim_id,
            credential_envelope,claim_envelope,endpoints_json,assignment_generation,lifecycle_generation,endpoint_generation,
            state,created_at_ms,updated_at_ms) SELECT $1,$2,$3,$4,$5,$6,$7,$8,1,1,1,'claiming',$9,$9
            WHERE (SELECT count(*) FROM sharing_imports)<32 ON CONFLICT(source_server_id,catalogue_epoch) DO NOTHING",
            vec![i.id.into(),i.source.server_id.into(),i.source.catalogue_epoch.into(),i.source_name.into(),i.claim_id.into(),
                credential.into(),claim.into(),json(&i.endpoints)?.into(),i.now_ms.into()])]).await?;
        if counts[0] == 1 {
            return Ok(ImportOutcome::Created);
        }
        let rows=self.sharing_read("SELECT json_quote(id) AS payload FROM sharing_imports WHERE source_server_id=$1 AND catalogue_epoch=$2",
            vec![i.source.server_id.into(),i.source.catalogue_epoch.into()]).await?;
        match rows.first() {
            Some(row) => Ok(ImportOutcome::AlreadyImported(decode(row)?)),
            None => Ok(ImportOutcome::Capacity),
        }
    }
    async fn re_pair_share_import(
        &self,
        i: NewImport,
        generation: i64,
    ) -> Result<MutationOutcome, StoreError> {
        validate_endpoints(&i.endpoints)?;
        if generation < 1
            || i.now_ms < 0
            || i.source_name.len() > 128
            || i.source_name.chars().any(char::is_control)
        {
            return Err(invalid());
        }
        let credential = i.credential.to_persist().map_err(|_| invalid())?.to_owned();
        let claim = i
            .claim_secret
            .to_persist()
            .map_err(|_| invalid())?
            .to_owned();
        let counts = self.sharing_txn(vec![
            stmt("DELETE FROM sharing_import_rotations WHERE import_id=$1 AND EXISTS(SELECT 1 FROM sharing_imports WHERE id=$1 AND lifecycle_generation=$2 AND lifecycle_generation<9223372036854775807 AND endpoint_generation<9223372036854775807 AND source_server_id=$3 AND catalogue_epoch=$4)", vec![i.id.into(), generation.into(), i.source.server_id.into(), i.source.catalogue_epoch.into()]),
            stmt("UPDATE sharing_imports SET source_name=$1,claim_id=$2,credential_envelope=$3,claim_envelope=$4,endpoints_json=$5,remote_grant_id=NULL,state='claiming',lifecycle_generation=lifecycle_generation+1,endpoint_generation=endpoint_generation+1,observed_scope_generation=NULL,observed_credential_generation=NULL,observed_catalogue_generation=NULL,observed_endpoint_revision=NULL,updated_at_ms=$6 WHERE id=$7 AND lifecycle_generation=$8 AND lifecycle_generation<9223372036854775807 AND endpoint_generation<9223372036854775807 AND source_server_id=$9 AND catalogue_epoch=$10", vec![i.source_name.into(), i.claim_id.into(), credential.into(), claim.into(), json(&i.endpoints)?.into(), i.now_ms.into(), i.id.into(), generation.into(), i.source.server_id.into(), i.source.catalogue_epoch.into()]),
        ]).await?;
        Ok(if counts[1] == 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn settle_share_claim(
        &self,
        id: Uuid,
        grant: Uuid,
        active: bool,
        now: i64,
    ) -> Result<MutationOutcome, StoreError> {
        let counts=self.sharing_txn(vec![stmt("UPDATE sharing_imports SET remote_grant_id=$1,state=$2,
            claim_envelope=NULL,updated_at_ms=$3 WHERE id=$4 AND state IN ('claiming','pending') AND (remote_grant_id IS NULL OR remote_grant_id=$1)",
            vec![grant.into(),if active { "active".to_owned() } else { "pending".to_owned() }.into(),now.into(),id.into()])]).await?;
        Ok(if counts[0] == 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn settle_share_import_response(
        &self,
        r: ImportClaimReceipt,
    ) -> Result<MutationOutcome, StoreError> {
        let credential = r.credential.to_persist().map_err(|_| invalid())?.to_owned();
        let counts=self.sharing_txn(vec![stmt("UPDATE sharing_imports SET remote_grant_id=$1,state=$2,claim_envelope=NULL,credential_envelope=$7,updated_at_ms=$3 WHERE id=$4 AND claim_id=$5 AND lifecycle_generation=$6 AND state IN ('claiming','pending') AND (remote_grant_id IS NULL OR remote_grant_id=$1)",vec![r.grant_id.into(),if r.active {"active".to_owned()}else{"pending".to_owned()}.into(),r.now_ms.into(),r.import_id.into(),r.claim_id.into(),r.lifecycle_generation.into(),credential.into()])]).await?;
        Ok(if counts[0] == 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn settle_current_share_claim(
        &self,
        id: Uuid,
        claim: Uuid,
        lifecycle: i64,
        grant: Uuid,
        active: bool,
        now: i64,
    ) -> Result<MutationOutcome, StoreError> {
        let counts = self.sharing_txn(vec![stmt("UPDATE sharing_imports SET remote_grant_id=$1,state=$2,claim_envelope=NULL,updated_at_ms=$3 WHERE id=$4 AND claim_id=$5 AND lifecycle_generation=$6 AND state IN ('claiming','pending') AND (remote_grant_id IS NULL OR remote_grant_id=$1)",vec![grant.into(),if active {"active".to_owned()}else{"pending".to_owned()}.into(),now.into(),id.into(),claim.into(),lifecycle.into()])]).await?;
        Ok(if counts[0] == 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn fail_current_share_claim(
        &self,
        id: Uuid,
        claim: Uuid,
        lifecycle: i64,
        now: i64,
    ) -> Result<MutationOutcome, StoreError> {
        let counts = self.sharing_txn(vec![stmt("UPDATE sharing_imports SET state='revoked',claim_envelope=NULL,lifecycle_generation=lifecycle_generation+1,updated_at_ms=$1 WHERE id=$2 AND claim_id=$3 AND lifecycle_generation=$4 AND lifecycle_generation<9223372036854775807 AND state IN ('claiming','pending')",vec![now.into(),id.into(),claim.into(),lifecycle.into()])]).await?;
        Ok(if counts[0] == 1 {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn assign_share_viewers(
        &self,
        id: Uuid,
        generation: i64,
        assignments: Vec<Assignment>,
        now: i64,
    ) -> Result<MutationOutcome, StoreError> {
        if assignments.len() > MAX_LIBRARIES * 256
            || assignments.iter().any(|a| a.user_id < 0)
            || assignments
                .iter()
                .map(|a| (&a.library_id, a.user_id))
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != assignments.len()
            || assignments
                .iter()
                .map(|a| &a.library_id)
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                > MAX_LIBRARIES
        {
            return Err(invalid());
        }
        let data = json(&assignments)?;
        let guard="EXISTS(SELECT 1 FROM sharing_imports WHERE id=$1 AND assignment_generation=$2 AND assignment_generation<9223372036854775807
            AND state IN ('active','pending')) AND NOT EXISTS(SELECT 1 FROM json_each($3) j LEFT JOIN users u ON u.id=json_extract(j.value,'$.user_id') WHERE u.id IS NULL)";
        let mut statements = Vec::new();
        // Generate viewer identity once for the current user lifetime; deletion cascades.
        for a in &assignments {
            statements.push(stmt(&format!("INSERT INTO sharing_viewers(user_id,viewer_id) SELECT $4,$5 WHERE {guard} ON CONFLICT(user_id) DO NOTHING"),
                vec![id.into(),generation.into(),data.clone().into(),a.user_id.into(),Uuid::new_v4().into()]));
        }
        statements.push(stmt(
            &format!("DELETE FROM sharing_assignments WHERE import_id=$1 AND {guard}"),
            vec![id.into(), generation.into(), data.clone().into()],
        ));
        statements.push(stmt(&format!("INSERT INTO sharing_assignments(import_id,remote_library_id,user_id,enabled)
            SELECT $1,json_extract(value,'$.library_id'),json_extract(value,'$.user_id'),1 FROM json_each($3) WHERE {guard}"),
            vec![id.into(),generation.into(),data.clone().into()]));
        statements.push(stmt(&format!("UPDATE sharing_imports SET assignment_generation=assignment_generation+1,updated_at_ms=$4 WHERE id=$1 AND {guard}"),
            vec![id.into(),generation.into(),data.into(),now.into()]));
        let counts = self.sharing_txn(statements).await?;
        Ok(if counts.last() == Some(&1) {
            MutationOutcome::Applied
        } else {
            MutationOutcome::Conflict
        })
    }
    async fn authorize_share_viewer(
        &self,
        id: Uuid,
        library: SourceId,
        user: i64,
    ) -> Result<Option<String>, StoreError> {
        let rows=self.sharing_read("SELECT json_quote(v.viewer_id) AS payload FROM sharing_assignments a JOIN sharing_imports i ON i.id=a.import_id
            JOIN sharing_viewers v ON v.user_id=a.user_id JOIN users u ON u.id=v.user_id
            WHERE a.import_id=$1 AND a.remote_library_id=$2 AND a.user_id=$3 AND a.enabled=1 AND i.state='active'",
            vec![id.into(),library.as_str().to_owned().into(),user.into()]).await?;
        match rows.first() {
            Some(row) => Ok(Some(viewer_key(id, decode(row)?))),
            None => Ok(None),
        }
    }
    async fn disable_share_import(&self, id: Uuid, now: i64) -> Result<(), StoreError> {
        self.sharing_txn(vec![stmt("UPDATE sharing_imports SET state='disabled',lifecycle_generation=lifecycle_generation+1,
            updated_at_ms=$1 WHERE id=$2 AND state NOT IN ('disabled','revoked')",vec![now.into(),id.into()])]).await?;
        Ok(())
    }
    async fn sharing_import_rotation(
        &self,
        id: Uuid,
    ) -> Result<Option<StoredImportRotation>, StoreError> {
        #[derive(serde::Deserialize)]
        struct Row {
            request_id: Uuid,
            credential: String,
            expires_at_ms: i64,
        }
        let rows = self.sharing_read("SELECT json_object('request_id',r.request_id,'credential',r.credential_envelope,'expires_at_ms',r.expires_at_ms) AS payload FROM sharing_import_rotations r JOIN sharing_imports i ON i.id=r.import_id WHERE r.import_id=$1 AND i.state='active'",vec![id.into()]).await?;
        rows.first()
            .map(|row| {
                let row: Row = decode(row)?;
                Ok(StoredImportRotation {
                    request_id: row.request_id,
                    credential: SealedSecret::from_stored(row.credential),
                    expires_at_ms: row.expires_at_ms,
                })
            })
            .transpose()
    }
    async fn begin_share_import_rotation(
        &self,
        r: ImportRotation,
    ) -> Result<MutationOutcome, StoreError> {
        if r.now_ms < 0
            || r.now_ms.checked_add(ROTATION_TTL_MS).is_none()
            || r.lifecycle_generation < 1
        {
            return Err(invalid());
        }
        let envelope = r.credential.to_persist().map_err(|_| invalid())?.to_owned();
        self.sharing_txn(vec![stmt("INSERT INTO sharing_import_rotations(import_id,request_id,credential_envelope,created_at_ms,expires_at_ms)
            SELECT $1,$2,$3,$4,$5 WHERE EXISTS(SELECT 1 FROM sharing_imports WHERE id=$1 AND lifecycle_generation=$6 AND state='active')
            ON CONFLICT(import_id) DO NOTHING",vec![r.import_id.into(),r.request_id.into(),envelope.clone().into(),r.now_ms.into(),(r.now_ms+ROTATION_TTL_MS).into(),r.lifecycle_generation.into()])]).await?;
        let rows=self.sharing_read("SELECT '{}' AS payload FROM sharing_import_rotations r JOIN sharing_imports i ON i.id=r.import_id
            WHERE r.import_id=$1 AND r.request_id=$2 AND r.credential_envelope=$3 AND i.lifecycle_generation=$4 AND i.state='active' AND r.expires_at_ms>$5",
            vec![r.import_id.into(),r.request_id.into(),envelope.into(),r.lifecycle_generation.into(),r.now_ms.into()]).await?;
        Ok(if rows.is_empty() {
            MutationOutcome::Conflict
        } else {
            MutationOutcome::Applied
        })
    }
    async fn commit_share_import_rotation(
        &self,
        id: Uuid,
        request: Uuid,
        lifecycle: i64,
        credential_generation: i64,
        credential: SealedSecret,
        now: i64,
    ) -> Result<MutationOutcome, StoreError> {
        if credential_generation < 1 {
            return Err(invalid());
        }
        let envelope = credential.to_persist().map_err(|_| invalid())?.to_owned();
        let counts=self.sharing_txn(vec![
            stmt("UPDATE sharing_imports SET credential_envelope=$6,
                observed_credential_generation=$3,updated_at_ms=$4 WHERE id=$1 AND lifecycle_generation=$5 AND state='active'
                AND (observed_credential_generation IS NULL OR observed_credential_generation<$3)
                AND EXISTS(SELECT 1 FROM sharing_import_rotations WHERE import_id=$1 AND request_id=$2)",
                vec![id.into(),request.into(),credential_generation.into(),now.into(),lifecycle.into(),envelope.into()]),
            stmt("DELETE FROM sharing_import_rotations WHERE import_id=$1 AND request_id=$2
                AND EXISTS(SELECT 1 FROM sharing_imports WHERE id=$1 AND lifecycle_generation=$3 AND state='active' AND observed_credential_generation=$4)",
                vec![id.into(),request.into(),lifecycle.into(),credential_generation.into()]),
        ]).await?;
        if counts[0] == 1 {
            return Ok(MutationOutcome::Applied);
        }
        // Confirming a recovered receipt is idempotent. The caller supplies a
        // verified remote generation, never a locally invented rotation result.
        let rows=self.sharing_read("SELECT '{}' AS payload FROM sharing_imports WHERE id=$1 AND lifecycle_generation=$2 AND state='active' AND observed_credential_generation=$3",
            vec![id.into(),lifecycle.into(),credential_generation.into()]).await?;
        Ok(if rows.is_empty() {
            MutationOutcome::Conflict
        } else {
            MutationOutcome::Applied
        })
    }
    async fn sharing_sealed_census(&self) -> Result<SealedRowCensus, StoreError> {
        let rows = self.sharing_read("SELECT json_object('purpose','credential','values',json_array(credential_envelope,claim_envelope)) AS payload FROM sharing_imports
            UNION ALL SELECT json_object('purpose','rotation','values',json_array(credential_envelope)) AS payload FROM sharing_import_rotations
            UNION ALL SELECT json_object('purpose','upstream','values',json_array(capability_envelope)) AS payload FROM sharing_relay_upstream WHERE capability_envelope IS NOT NULL",vec![]).await?;
        let mut census = SealedRowCensus::default();
        for row in rows {
            let v: serde_json::Value = decode(&row)?;
            let envelopes: Vec<_> = v["values"]
                .as_array()
                .ok_or_else(invalid)?
                .iter()
                .filter_map(|s| s.as_str())
                .map(SealedSecret::from_stored)
                .collect();
            census.observe_envelopes(
                v["purpose"].as_str().ok_or_else(invalid)?,
                &envelopes.iter().collect::<Vec<_>>(),
            );
        }
        for envelope in revision_key_envelopes(self.sharing_revision_key_rows().await?)? {
            census.observe_envelopes("catalogue-revision", &[&envelope]);
        }
        for envelope in revision_key_envelopes(self.sharing_file_locator_key_rows().await?)? {
            census.observe_envelopes("file-locator", &[&envelope]);
        }
        Ok(census)
    }
}

const EXPORT_SUMMARY_JSON: &str = "json_object('grant',json_object('id',e.id,'recipient_server_id',e.recipient_server_id,'state',e.state,'scope_generation',e.scope_generation,'credential_generation',e.credential_generation,'catalogue_generation',e.catalogue_generation,'mutation_generation',e.mutation_generation,'pending_expires_at_ms',e.pending_expires_at_ms),'recipient_name',e.recipient_name,'invitation_id',e.invitation_id,'claim_id',i.claim_id,'library_ids',json((SELECT json_group_array(CAST(library_id AS TEXT)) FROM (SELECT library_id FROM sharing_export_libraries WHERE grant_id=e.id ORDER BY library_id))),'source_server_id',s.server_id,'verifier',e.token_hash)";
const IMPORT_SUMMARY_JSON: &str = "json_object('id',i.id,'source_server_id',i.source_server_id,'catalogue_epoch',i.catalogue_epoch,'source_name',i.source_name,'claim_id',i.claim_id,'remote_grant_id',i.remote_grant_id,'state',i.state,'assignment_generation',i.assignment_generation,'lifecycle_generation',i.lifecycle_generation,'endpoint_generation',i.endpoint_generation,'observed_endpoint_revision',i.observed_endpoint_revision,'endpoints',json(i.endpoints_json))";
fn export_summary(row: &str) -> Result<ExportSummary, StoreError> {
    #[derive(serde::Deserialize)]
    struct Row {
        grant: ExportGrant,
        recipient_name: String,
        invitation_id: Uuid,
        claim_id: Uuid,
        library_ids: Vec<SourceId>,
        source_server_id: Uuid,
        verifier: String,
    }
    let row: Row = decode(row)?;
    Ok(ExportSummary {
        pairing_code: pairing_code(
            row.source_server_id,
            row.grant.recipient_server_id,
            row.invitation_id,
            row.claim_id,
            &row.verifier,
        ),
        grant: row.grant,
        recipient_name: row.recipient_name,
        invitation_id: row.invitation_id,
        claim_id: row.claim_id,
        library_ids: row.library_ids,
    })
}

/// A restored image can contain an older revocation state. Retain ciphertext
/// and private history, but fence all traffic and require explicit re-pairing.
#[cfg(feature = "hiqlite-store")]
pub(crate) fn fence_restored_sharing(connection: &rusqlite::Connection) -> Result<(), StoreError> {
    let exists: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='sharing_identity'",
        [],
        |r| r.get(0),
    )?;
    if exists == 0 {
        return Ok(());
    }
    let tx = connection.unchecked_transaction()?;
    tx.execute("INSERT INTO settings(key,value) VALUES('sharing_enabled','false') ON CONFLICT(key) DO UPDATE SET value='false'",[])?;
    tx.execute("UPDATE sharing_exports SET state='revoked',scope_generation=scope_generation+1,mutation_generation=mutation_generation+1 WHERE state!='revoked'",[])?;
    tx.execute(
        "UPDATE sharing_invitations SET state='cancelled' WHERE state='open'",
        [],
    )?;
    tx.execute("UPDATE sharing_imports SET state='disabled',lifecycle_generation=lifecycle_generation+1 WHERE state NOT IN ('disabled','revoked')",[])?;
    tx.execute("UPDATE sharing_delivery_grants SET state='revoked'", [])?;
    tx.execute("DELETE FROM sharing_identity", [])?;
    tx.commit()?;
    Ok(())
}
