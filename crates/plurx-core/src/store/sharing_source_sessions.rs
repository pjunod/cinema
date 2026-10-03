//! Candidate Source reservation ledger. No worker admission or schema installer.
use super::{
    sharing::{Backend, Statement, Value},
    sharing_catalogue_details::{file_revision_guarded_projection_sql, SourceDetailsRead},
    SharingSourceDetailsStore,
};
use crate::{
    cluster::membership::SourceAdmissionMembers,
    error::StoreError,
    playback_principal::PlaybackPrincipal,
    secrets::CredentialKey,
    sharing::{invalid, is_hash, SharingIdentity, SourceId},
    sharing_catalogue_details::{CatalogueRevisionKey, FileRevision},
    sharing_source_sessions::*,
};
use async_trait::async_trait;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

pub const CANDIDATE_SCHEMA: &str = include_str!("sharing_source_session_bindings_schema.sql");
const MAX_SOURCE_ROWS: i64 = 4096;
const MAX_RESPONSE_BYTES: usize = 128 * 1024;

/// Candidate fixture/activation statements only. This never installs itself.
pub fn candidate_statements() -> Vec<String> {
    CANDIDATE_SCHEMA
        .split("-- next statement")
        .map(|part| {
            let part = &part[part.find("CREATE ").expect("candidate CREATE statement")..];
            part.trim().trim_end_matches(';').to_owned()
        })
        .collect()
}

fn now_ms() -> Result<i64, StoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .filter(|value| *value > 0)
        .ok_or_else(invalid)
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Every Source write checks the candidate shape again. Missing tables are
/// observed before preparing application SQL, not repaired on a read path.
fn schema_guard() -> String {
    let mut guards = candidate_statements()
        .iter()
        .map(|statement| {
            let words = statement.split_whitespace().collect::<Vec<_>>();
            let kind = words[1].to_ascii_lowercase();
            let name = words[2];
            format!(
                "EXISTS(SELECT 1 FROM sqlite_master WHERE type={} AND name={} AND sql={})",
                quote(&kind),
                quote(name),
                quote(statement)
            )
        })
        .collect::<Vec<_>>();
    for statement in super::sharing_catalogue_source::candidate_statements()
        .into_iter()
        .chain(super::sharing_catalogue_source::candidate_item_identity_statements())
        .chain(std::iter::once(
            super::sharing_catalogue_source::CANDIDATE_REVISION_KEY_SCHEMA.to_owned(),
        ))
    {
        let Some(start) = statement.find("CREATE ") else {
            continue;
        };
        let statement = statement[start..].trim().trim_end_matches(';');
        let words = statement.split_whitespace().collect::<Vec<_>>();
        guards.push(format!(
            "EXISTS(SELECT 1 FROM sqlite_master WHERE type={} AND name={} AND sql={})",
            quote(&words[1].to_ascii_lowercase()),
            quote(words[2]),
            quote(statement)
        ));
    }
    for table in [
        "media_session_requests",
        "media_sessions",
        "media_playback_pointers",
        "media_session_preparations",
        "media_playback_desired",
        "media_session_producer_recovery",
        "library_channel_session_recipes",
    ] {
        guards.push(format!("(SELECT count(*) FROM pragma_table_info('{table}') WHERE name IN('owner_key','principal_kind','user_id','share_grant_id','share_viewer_key'))=5"));
        guards.push(format!(
            "NOT EXISTS(SELECT 1 FROM pragma_foreign_key_list('{table}'))"
        ));
        let marker = format!("CREATE TABLE {table}_principal_new (");
        let original = super::MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
            .split("-- next statement\n")
            .find_map(|part| {
                part.find(&marker)
                    .map(|start| part[start..].trim().trim_end_matches(';').to_owned())
            })
            .expect("frozen candidate table");
        let renamed = original.replacen(&marker, &format!("CREATE TABLE \"{table}\" ("), 1);
        guards.push(format!(
            "EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name={} AND sql={})",
            quote(table),
            quote(&renamed)
        ));
    }
    let required = [
        "settings",
        "sharing_identity",
        "sharing_exports",
        "sharing_export_libraries",
        "sharing_catalogue_keys",
        "item_identity_watermark",
        "libraries",
        "items",
        "files",
        "job_leases",
        "cache_consumer_pins",
        "cluster_nodes",
        "cluster_node_capabilities",
        "cluster_join_tokens",
        "cluster_node_join_staging",
        "cluster_node_removals",
        "cluster_node_removal_attempts",
        "cluster_sharing_membership_intents",
        "cluster_sharing_join_intents",
        "cluster_sharing_join_declarations",
        "cluster_sharing_membership_generation",
    ];
    guards.push(format!(
        "(SELECT count(*) FROM sqlite_master WHERE type='table' AND name IN({}))={}",
        required
            .iter()
            .map(|name| quote(name))
            .collect::<Vec<_>>()
            .join(","),
        required.len()
    ));
    for (table, second) in [
        ("media_session_requests", "request_id"),
        ("media_playback_pointers", "playback_id"),
        ("media_session_preparations", "playback_id"),
        ("media_playback_desired", "playback_id"),
        ("media_session_producer_recovery", "playback_id"),
        ("library_channel_session_recipes", "request_id"),
    ] {
        guards.push(format!("EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='owner_key' AND type='TEXT' AND \"notnull\"=1 AND pk=1) AND EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name='{second}' AND pk=2)"));
    }
    guards.join(" AND ")
}

async fn present<T: Backend>(backend: &T) -> Result<bool, StoreError> {
    let rows = backend
        .sharing_read(
            &format!(
                "SELECT json_quote(CASE WHEN {} THEN 1 ELSE 0 END) AS payload",
                schema_guard()
            ),
            vec![],
        )
        .await?;
    Ok(rows.first().map(String::as_str) == Some("1"))
}

const BINDING_JSON: &str = "json_object('incarnation',b.incarnation_id,'owner_key',b.owner_key,'grant',b.share_grant_id,'viewer',b.share_viewer_key,'request',b.request_id,'fingerprint',b.request_fingerprint,'playback',b.playback_id,'source',b.source_server_id,'epoch',b.catalogue_epoch,'library',b.library_id,'item',b.item_id,'file',b.file_id,'revision',b.file_revision,'reservation',b.reservation_state,'start_resolved',b.start_resolved_at_ms,'dispatch',b.dispatch_generation,'request_state',r.state,'request_owner',r.owner_node_id,'response',CASE WHEN length(CAST(r.response_json AS BLOB))<=131072 THEN r.response_json ELSE NULL END)";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BindingRow {
    incarnation: String,
    owner_key: String,
    grant: String,
    viewer: String,
    request: String,
    fingerprint: String,
    playback: String,
    source: String,
    epoch: String,
    library: SourceId,
    item: SourceId,
    file: SourceId,
    revision: FileRevision,
    reservation: String,
    start_resolved: Option<i64>,
    dispatch: i64,
    request_state: Option<String>,
    request_owner: Option<String>,
    response: Option<String>,
}
fn canonical_uuid(value: &str) -> Result<Uuid, StoreError> {
    let uuid = Uuid::parse_str(value).map_err(|_| invalid())?;
    if uuid.to_string() != value {
        return Err(invalid());
    }
    Ok(uuid)
}
impl BindingRow {
    fn handle(&self) -> Result<SourceBindingHandle, StoreError> {
        if !is_hash(&self.fingerprint)
            || !(1..=128).contains(&self.request.len())
            || !(1..=128).contains(&self.playback.len())
            || self.dispatch < 0
            || self.start_resolved.is_some_and(|value| value <= 0)
            || !matches!(self.reservation.as_str(), "held" | "released")
        {
            return Err(invalid());
        }
        let principal = PlaybackPrincipal::from_projection(
            "sharing",
            None,
            Some(&self.grant),
            Some(&self.viewer),
            &self.owner_key,
        )
        .map_err(|_| invalid())?;
        Ok(SourceBindingHandle {
            incarnation_id: canonical_uuid(&self.incarnation)?,
            principal,
            request_id: self.request.clone(),
            request_fingerprint: self.fingerprint.clone(),
            playback_id: self.playback.clone(),
            source_server_id: canonical_uuid(&self.source)?,
            catalogue_epoch: canonical_uuid(&self.epoch)?,
            library_id: self.library.clone(),
            item_id: self.item.clone(),
            file_id: self.file.clone(),
            file_revision: self.revision.clone(),
            released: self.reservation == "released",
        })
    }
}
fn agrees(binding: &SourceBindingHandle, intent: &SourceSessionIntent) -> bool {
    binding.principal == intent.request.principal
        && binding.request_id == intent.request.request_id
        && binding.request_fingerprint == intent.request.request_fingerprint
        && binding.playback_id == intent.request.playback_id
        && binding.source_server_id == intent.witness.server
        && binding.catalogue_epoch == intent.witness.epoch
        && binding.library_id == intent.witness.library
        && binding.item_id == intent.witness.item
        && binding.file_id == intent.witness.file
        && binding.file_revision == intent.request.file_revision
}

async fn binding_row<T: Backend>(
    backend: &T,
    owner: &str,
    request: &str,
) -> Result<Option<BindingRow>, StoreError> {
    let rows = backend
        .sharing_read(
            &format!(
                "SELECT {BINDING_JSON} AS payload FROM sharing_source_session_bindings b
         LEFT JOIN media_session_requests r ON r.owner_key=b.owner_key
           AND r.request_id=b.request_id AND r.incarnation_id=b.incarnation_id
           AND r.request_fingerprint=b.request_fingerprint AND r.playback_id=b.playback_id
         WHERE b.owner_key=$1 AND b.request_id=$2"
            ),
            vec![owner.to_owned().into(), request.to_owned().into()],
        )
        .await?;
    if rows.len() > 1 {
        return Err(invalid());
    }
    rows.first()
        .map(|row| serde_json::from_str(row).map_err(|_| invalid()))
        .transpose()
}

const OCCUPANCY: &str = "WITH obligations(incarnation_id,grant_id) AS (
 SELECT incarnation_id,share_grant_id FROM sharing_source_session_bindings WHERE reservation_state='held'
 UNION SELECT incarnation_id,share_grant_id FROM media_session_requests WHERE principal_kind='sharing' AND state='starting'
 UNION SELECT incarnation_id,share_grant_id FROM media_sessions WHERE principal_kind='sharing' AND state IN('starting','active')
 UNION SELECT staged_incarnation_id,share_grant_id FROM media_session_preparations WHERE principal_kind='sharing'),
 counts AS(SELECT count(*) AS source_slots,coalesce(sum(grant_id=$5),0) AS grant_slots FROM obligations),
 pending AS(SELECT count(*) AS grant_starts FROM sharing_source_session_bindings WHERE share_grant_id=$5 AND reservation_state='held' AND start_resolved_at_ms IS NULL)";

fn coherent_bindings() -> &'static str {
    "NOT EXISTS(SELECT 1 FROM media_session_requests r LEFT JOIN sharing_source_session_bindings b ON b.incarnation_id=r.incarnation_id WHERE r.principal_kind='sharing' AND (b.incarnation_id IS NULL OR b.owner_key<>r.owner_key OR b.share_grant_id<>r.share_grant_id OR b.share_viewer_key<>r.share_viewer_key OR b.request_id<>r.request_id OR b.request_fingerprint<>r.request_fingerprint OR b.playback_id<>r.playback_id))
 AND NOT EXISTS(SELECT 1 FROM media_sessions s LEFT JOIN sharing_source_session_bindings b ON b.incarnation_id=s.incarnation_id WHERE s.principal_kind='sharing' AND (b.incarnation_id IS NULL OR b.owner_key<>s.owner_key OR b.share_grant_id<>s.share_grant_id OR b.share_viewer_key<>s.share_viewer_key OR b.request_fingerprint<>s.request_fingerprint OR b.playback_id<>s.playback_id))
 AND NOT EXISTS(SELECT 1 FROM media_session_preparations p LEFT JOIN sharing_source_session_bindings b ON b.incarnation_id=p.staged_incarnation_id WHERE p.principal_kind='sharing' AND (b.incarnation_id IS NULL OR b.owner_key<>p.owner_key OR b.share_grant_id<>p.share_grant_id OR b.share_viewer_key<>p.share_viewer_key OR b.playback_id<>p.playback_id))"
}

fn authority_guard(member_guard: &str) -> String {
    format!("({member_guard}) AND ({}) AND ({})
 AND EXISTS(SELECT 1 FROM settings WHERE key='sharing_enabled' AND CASE WHEN typeof(value)='text' AND length(CAST(value AS BLOB))<=64 THEN lower(trim(value)) IN('1','true','yes','on') ELSE 0 END)
 AND $4='share:'||$5||':'||$6 AND length($7) BETWEEN 1 AND 128
 AND length($8)=64 AND length($9) BETWEEN 1 AND 128 AND length($10)=36
 AND length($16)=64 AND $19>0
 AND EXISTS(SELECT 1 FROM sharing_exports e JOIN sharing_identity s ON s.singleton=1
 JOIN sharing_export_libraries x ON x.grant_id=e.id JOIN libraries l ON l.id=x.library_id
 JOIN items i ON i.library_id=l.id JOIN files f ON f.item_id=i.id
 JOIN sharing_catalogue_keys k ON k.singleton=1 AND k.server_id=s.server_id AND k.catalogue_epoch=s.catalogue_epoch
 WHERE e.id=$5 AND e.token_hash=$17 AND e.state='active' AND s.server_id=$11 AND s.catalogue_epoch=$12
 AND CAST(i.library_id AS TEXT)=$13 AND CAST(i.id AS TEXT)=$14 AND CAST(f.id AS TEXT)=$15
 AND i.kind IN('movie','episode') AND l.kind IN('movies','shows')
 AND EXISTS(SELECT 1 FROM item_identity_watermark WHERE singleton=1 AND importing=0)
 AND ({})=$18 AND k.revision_envelope=$20)",schema_guard(),coherent_bindings(),file_revision_guarded_projection_sql())
}

fn claim_values(
    intent: &SourceSessionIntent,
    members: String,
    cutoff: i64,
    now: i64,
) -> Vec<Value> {
    let PlaybackPrincipal::Sharing {
        grant_id,
        viewer_key,
    } = &intent.request.principal
    else {
        unreachable!("sealed Sharing intent")
    };
    vec![
        members.into(),
        cutoff.into(),
        now.into(),
        intent.request.principal.owner_key().into(),
        (*grant_id).into(),
        viewer_key.as_str().to_owned().into(),
        intent.request.request_id.clone().into(),
        intent.request.request_fingerprint.clone().into(),
        intent.request.playback_id.clone().into(),
        intent.request.incarnation_id.into(),
        intent.witness.server.into(),
        intent.witness.epoch.into(),
        intent.witness.library.as_str().to_owned().into(),
        intent.witness.item.as_str().to_owned().into(),
        intent.witness.file.as_str().to_owned().into(),
        intent.request.file_revision.as_str().to_owned().into(),
        intent.request.credential_hash.clone().into(),
        intent.witness.canonical_projection().to_owned().into(),
        intent.request.claim_expires_at_ms.into(),
        intent.revision_key_envelope.as_stored().to_owned().into(),
    ]
}

async fn authorized<T: Backend>(
    backend: &T,
    guard: &str,
    values: Vec<Value>,
) -> Result<bool, StoreError> {
    let rows = backend
        .sharing_read(
            &format!("SELECT json_quote(CASE WHEN {guard} THEN 1 ELSE 0 END) AS payload"),
            values,
        )
        .await?;
    Ok(rows.first().map(String::as_str) == Some("1"))
}

fn fresh_statements(guard: &str, values: Vec<Value>) -> Vec<Statement> {
    let exact="b.incarnation_id=$10 AND b.owner_key=$4 AND b.share_grant_id=$5 AND b.share_viewer_key=$6 AND b.request_id=$7 AND b.request_fingerprint=$8 AND b.playback_id=$9 AND b.source_server_id=$11 AND b.catalogue_epoch=$12 AND b.library_id=$13 AND b.item_id=$14 AND b.file_id=$15 AND b.file_revision=$16 AND b.reservation_state='held' AND b.dispatch_generation=0 AND b.start_resolved_at_ms IS NULL";
    vec![
      (format!("{OCCUPANCY} INSERT INTO sharing_source_session_bindings(incarnation_id,owner_key,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,source_server_id,catalogue_epoch,library_id,item_id,file_id,file_revision,reservation_state,start_resolved_at_ms,dispatch_generation,created_at_ms)
 SELECT $10,$4,$5,$6,$7,$8,$9,$11,$12,$13,$14,$15,$16,'held',NULL,0,$3 FROM counts,pending
 WHERE {guard} AND $19>$3 AND source_slots<8 AND grant_slots<4 AND grant_starts<2
 AND (SELECT count(*) FROM sharing_source_session_bindings)<{MAX_SOURCE_ROWS}
 AND (SELECT count(*) FROM media_session_requests WHERE principal_kind='sharing')<{MAX_SOURCE_ROWS}
 AND NOT EXISTS(SELECT 1 FROM sharing_source_session_bindings WHERE incarnation_id=$10 OR(owner_key=$4 AND request_id=$7))
 AND NOT EXISTS(SELECT 1 FROM media_session_requests WHERE owner_key=$4 AND request_id=$7)
 AND NOT EXISTS(SELECT 1 FROM media_sessions WHERE incarnation_id=$10)
 AND NOT EXISTS(SELECT 1 FROM job_leases WHERE resource='session:'||$10)
 AND NOT EXISTS(SELECT 1 FROM cache_consumer_pins WHERE consumer_kind='media_session' AND consumer_id=$10)
 AND NOT EXISTS(SELECT 1 FROM media_session_preparations WHERE staged_incarnation_id=$10)
 AND NOT EXISTS(SELECT 1 FROM library_channel_session_recipes WHERE incarnation_id=$10)
 AND NOT EXISTS(SELECT 1 FROM media_playback_pointers WHERE current_incarnation_id=$10)"),values.clone()),
      (format!("INSERT INTO media_session_requests(owner_key,principal_kind,user_id,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,state,claim_expires_at_ms,incarnation_id,owner_node_id,response_json,updated_at_ms)
 SELECT b.owner_key,'sharing',NULL,b.share_grant_id,b.share_viewer_key,b.request_id,b.request_fingerprint,b.playback_id,'starting',$19,b.incarnation_id,NULL,NULL,$3 FROM sharing_source_session_bindings b
 WHERE {exact} AND {guard} AND $19>$3 ON CONFLICT(owner_key,request_id) DO NOTHING"),values.clone()),
      // Only this named NOT NULL failure is recognized as a lost guarded
      // proposal. Every other database failure remains an error.
      (format!("INSERT INTO sharing_source_session_bindings(incarnation_id,owner_key,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,source_server_id,catalogue_epoch,library_id,item_id,file_id,file_revision,reservation_state,start_resolved_at_ms,dispatch_generation,created_at_ms)
 SELECT NULL,$4,$5,$6,$7,$8,$9,$11,$12,$13,$14,$15,$16,'held',NULL,0,$3
 WHERE NOT EXISTS(SELECT 1 FROM sharing_source_session_bindings b JOIN media_session_requests r ON r.owner_key=b.owner_key AND r.request_id=b.request_id AND r.incarnation_id=b.incarnation_id AND r.request_fingerprint=b.request_fingerprint AND r.playback_id=b.playback_id AND r.principal_kind='sharing' AND r.user_id IS NULL AND r.share_grant_id=b.share_grant_id AND r.share_viewer_key=b.share_viewer_key AND r.state='starting' AND r.owner_node_id IS NULL AND r.claim_expires_at_ms=$19 WHERE {exact} AND {guard})"),values),
    ]
}

fn lost_proposal(error: &StoreError) -> bool {
    error
        .to_string()
        .contains("NOT NULL constraint failed: sharing_source_session_bindings.incarnation_id")
}

/// Must be first in the existing atomic lifecycle transaction. This command
/// writes nothing on success and aborts the transaction on authority refusal.
pub(crate) fn source_activation_guard(
    authority: &SourceSessionWriteAuthority,
    activation: &crate::domain::MediaSessionActivation,
) -> Result<Option<Statement>, StoreError> {
    let binding = &authority.assignment.binding;
    let now = now_ms()?;
    if activation.principal != binding.principal
        || activation.incarnation_id != binding.incarnation_id.to_string()
        || activation.playback_id != binding.playback_id
        || activation.request_id.as_deref() != Some(&binding.request_id)
        || activation.request_fingerprint != binding.request_fingerprint
        || activation.owner_node_id != authority.assignment.owner_node_id
        || !activation.fence_predecessor
        || activation.expected_predecessor_incarnation_id.is_some()
        || activation.lease_expires_at_ms <= now
        || activation.now_ms <= 0
        || activation.now_ms > now
        || now.saturating_sub(activation.now_ms) > 5000
    {
        return Ok(None);
    }
    let members = &authority.assignment.members;
    let Ok((floor, roster, cutoff, observed)) = members.write_guard(now, 1, 2, 3) else {
        return Ok(None);
    };
    let Ok(raft_id) = i64::try_from(members.actual_local_raft_id()) else {
        return Ok(None);
    };
    let mut values = claim_values(&authority.intent, roster, cutoff, observed);
    values.extend([
        raft_id.into(),
        authority.assignment.owner_node_id.clone().into(),
        authority.assignment.dispatch_generation.into(),
        crate::cluster::coordination::removed_job_owner_key(&authority.assignment.owner_node_id)
            .into(),
        activation.session_id.clone().into(),
    ]);
    let guard = authority_guard(&floor);
    let exact = "b.incarnation_id=$10 AND b.owner_key=$4 AND b.share_grant_id=$5 AND b.share_viewer_key=$6 AND b.request_id=$7 AND b.request_fingerprint=$8 AND b.playback_id=$9 AND b.source_server_id=$11 AND b.catalogue_epoch=$12 AND b.library_id=$13 AND b.item_id=$14 AND b.file_id=$15 AND b.file_revision=$16 AND b.reservation_state='held' AND b.dispatch_generation=$23 AND $23=1 AND b.start_resolved_at_ms IS NULL";
    let request = "r.incarnation_id=b.incarnation_id AND r.owner_key=b.owner_key AND r.request_id=b.request_id AND r.request_fingerprint=b.request_fingerprint AND r.playback_id=b.playback_id AND r.principal_kind='sharing' AND r.user_id IS NULL AND r.share_grant_id=b.share_grant_id AND r.share_viewer_key=b.share_viewer_key AND r.state='starting' AND r.claim_expires_at_ms=$19 AND r.claim_expires_at_ms>$3 AND r.owner_node_id=$22";
    let local = "EXISTS(SELECT 1 FROM cluster_nodes WHERE raft_id=$21 AND node_id=$22 AND removed_at IS NULL) AND NOT EXISTS(SELECT 1 FROM settings WHERE key=$24)";
    let route = "s.incarnation_id=$10 AND s.owner_key=$4 AND s.principal_kind='sharing' AND s.user_id IS NULL AND s.share_grant_id=$5 AND s.share_viewer_key=$6 AND s.playback_id=$9 AND s.request_fingerprint=$8 AND s.owner_node_id=$22 AND s.owner_epoch=1 AND s.session_id=$25 AND s.state='active' AND s.lease_expires_at_ms>$3";
    let lineage = format!("NOT EXISTS(SELECT 1 FROM media_sessions s WHERE s.incarnation_id=$10 AND NOT ({route})) AND NOT EXISTS(SELECT 1 FROM job_leases j WHERE j.resource='session:'||$10 AND NOT EXISTS(SELECT 1 FROM media_sessions s WHERE {route} AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms=s.lease_expires_at_ms AND j.revision>0)) AND NOT EXISTS(SELECT 1 FROM cache_consumer_pins p WHERE p.consumer_kind='media_session' AND p.consumer_id=$10 AND NOT EXISTS(SELECT 1 FROM media_sessions s JOIN job_leases j ON j.resource='session:'||s.incarnation_id AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms=s.lease_expires_at_ms WHERE {route})) AND NOT EXISTS(SELECT 1 FROM media_session_preparations WHERE staged_incarnation_id=$10)");
    Ok(Some((format!("INSERT INTO sharing_source_session_bindings(incarnation_id,owner_key,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,source_server_id,catalogue_epoch,library_id,item_id,file_id,file_revision,reservation_state,start_resolved_at_ms,dispatch_generation,created_at_ms) SELECT NULL,$4,$5,$6,$7,$8,$9,$11,$12,$13,$14,$15,$16,'held',NULL,0,$3 WHERE NOT EXISTS(SELECT 1 FROM sharing_source_session_bindings b JOIN media_session_requests r ON {request} WHERE {exact} AND {guard} AND {local} AND ({lineage}))"),values)))
}

pub(crate) fn source_write_refused(error: &StoreError) -> bool {
    lost_proposal(error)
}

async fn replay<T: Backend>(
    backend: &T,
    row: BindingRow,
    intent: &SourceSessionIntent,
    guard: &str,
    values: Vec<Value>,
) -> Result<SourceClaimOutcome, StoreError> {
    let binding = row.handle()?;
    if !authorized(backend, guard, values).await? {
        return Ok(SourceClaimOutcome::Unavailable);
    }
    if !agrees(&binding, intent) {
        return Ok(SourceClaimOutcome::Conflict);
    }
    if binding.released {
        return Ok(SourceClaimOutcome::Retired(binding));
    }
    match row.request_state.as_deref() {
        Some("starting") => Ok(SourceClaimOutcome::InFlight(binding)),
        Some("resolved") => {
            let Some(response_json) = row.response else {
                return Ok(SourceClaimOutcome::Unavailable);
            };
            if response_json.len() > MAX_RESPONSE_BYTES
                || serde_json::from_str::<serde_json::Value>(&response_json).is_err()
            {
                return Err(invalid());
            }
            let rows=backend.sharing_read("SELECT json_quote(count(*)) AS payload FROM media_sessions WHERE incarnation_id=$1 AND owner_key=$2 AND principal_kind='sharing' AND share_grant_id=$3 AND share_viewer_key=$4 AND user_id IS NULL AND playback_id=$5 AND request_fingerprint=$6 AND state='active' AND owner_node_id=$7 AND lease_expires_at_ms>$8 AND publication_ready_at_ms=0",vec![binding.incarnation_id.into(),binding.principal.owner_key().into(),row.grant.clone().into(),row.viewer.clone().into(),binding.playback_id.clone().into(),binding.request_fingerprint.clone().into(),row.request_owner.clone().unwrap_or_default().into(),now_ms()?.into()]).await?;
            if rows.first().map(String::as_str) != Some("1") {
                return Ok(SourceClaimOutcome::Unavailable);
            }
            Ok(SourceClaimOutcome::Resolved {
                binding,
                response_json,
            })
        }
        Some("failed") | None => Ok(SourceClaimOutcome::Unavailable),
        _ => Err(invalid()),
    }
}

#[derive(Deserialize)]
struct OccupancyRow {
    source_slots: i64,
    grant_slots: i64,
    grant_starts: i64,
    bindings: i64,
    requests: i64,
}
async fn capacity<T: Backend>(
    backend: &T,
    grant: Uuid,
) -> Result<Option<SourceCapacity>, StoreError> {
    let sql = OCCUPANCY.replace("$5", "$1");
    let rows=backend.sharing_read(&format!("{sql} SELECT json_object('source_slots',source_slots,'grant_slots',grant_slots,'grant_starts',grant_starts,'bindings',(SELECT count(*) FROM sharing_source_session_bindings),'requests',(SELECT count(*) FROM media_session_requests WHERE principal_kind='sharing')) AS payload FROM counts,pending"),vec![grant.into()]).await?;
    let [row] = rows.as_slice() else {
        return Err(invalid());
    };
    let row: OccupancyRow = serde_json::from_str(row).map_err(|_| invalid())?;
    Ok(if row.source_slots >= 8 {
        Some(SourceCapacity::SourceSlots)
    } else if row.grant_slots >= 4 {
        Some(SourceCapacity::GrantSlots)
    } else if row.grant_starts >= 2 {
        Some(SourceCapacity::PendingStarts)
    } else if row.bindings >= MAX_SOURCE_ROWS {
        Some(SourceCapacity::RetainedBindings)
    } else if row.requests >= MAX_SOURCE_ROWS {
        Some(SourceCapacity::RetainedRequests)
    } else {
        None
    })
}

struct DispatchPrepared {
    assignment: SourceDispatchAssignment,
    intent: Box<SourceSessionIntent>,
}
enum DispatchPreparedRead {
    Ready(Box<DispatchPrepared>),
    Unavailable,
    Capacity,
}
async fn assign_dispatch_prepared<T: Backend>(
    backend: &T,
    binding: &SourceBindingHandle,
    credential: &CredentialKey,
    members: &SourceAdmissionMembers,
) -> Result<DispatchPreparedRead, StoreError> {
    if binding.released || !present(backend).await? {
        return Ok(DispatchPreparedRead::Unavailable);
    }
    let now = now_ms()?;
    if members.write_guard(now, 1, 2, 3).is_err() {
        return Ok(DispatchPreparedRead::Unavailable);
    }
    let Ok(raft_id) = i64::try_from(members.actual_local_raft_id()) else {
        return Ok(DispatchPreparedRead::Unavailable);
    };
    let owner = binding.principal.owner_key();
    let Some(row) = binding_row(backend, &owner, &binding.request_id).await? else {
        return Ok(DispatchPreparedRead::Unavailable);
    };
    if row.incarnation != binding.incarnation_id.to_string()
        || row.reservation != "held"
        || row.start_resolved.is_some()
        || !matches!(row.dispatch, 0 | 1)
        || row.request_state.as_deref() != Some("starting")
    {
        return Ok(DispatchPreparedRead::Unavailable);
    }
    let PlaybackPrincipal::Sharing { grant_id, .. } = &binding.principal else {
        return Err(invalid());
    };
    #[derive(Deserialize)]
    struct CurrentRequest {
        hash: String,
        expires: i64,
        node: String,
    }
    let rows = backend.sharing_read("SELECT json_object('hash',e.token_hash,'expires',r.claim_expires_at_ms,'node',n.node_id) AS payload FROM media_session_requests r JOIN sharing_exports e ON e.id=r.share_grant_id JOIN cluster_nodes n ON n.raft_id=$1 AND n.removed_at IS NULL WHERE r.owner_key=$2 AND r.request_id=$3 AND r.incarnation_id=$4 AND r.request_fingerprint=$5 AND r.playback_id=$6 AND r.principal_kind='sharing' AND r.user_id IS NULL AND r.share_grant_id=$7 AND r.share_viewer_key=$8 AND r.state='starting' AND r.claim_expires_at_ms>$9 AND e.state='active' AND length(e.token_hash)=64 AND length(n.node_id) BETWEEN 1 AND 256 AND (r.owner_node_id IS NULL OR r.owner_node_id=n.node_id)", vec![raft_id.into(),owner.clone().into(),binding.request_id.clone().into(),binding.incarnation_id.into(),binding.request_fingerprint.clone().into(),binding.playback_id.clone().into(),(*grant_id).into(),row.viewer.clone().into(),now.into()]).await?;
    let [request] = rows.as_slice() else {
        return Ok(DispatchPreparedRead::Unavailable);
    };
    let current: CurrentRequest = serde_json::from_str(request).map_err(|_| invalid())?;
    if !is_hash(&current.hash) {
        return Ok(DispatchPreparedRead::Unavailable);
    }
    if (row.dispatch == 0 && row.request_owner.is_some())
        || (row.dispatch == 1 && row.request_owner.as_deref() != Some(&current.node))
    {
        return Ok(DispatchPreparedRead::Unavailable);
    }
    let request = SourceSessionRequest {
        principal: binding.principal.clone(),
        request_id: binding.request_id.clone(),
        request_fingerprint: binding.request_fingerprint.clone(),
        playback_id: binding.playback_id.clone(),
        incarnation_id: binding.incarnation_id,
        now_ms: now,
        claim_expires_at_ms: current.expires,
        credential_hash: current.hash,
        item_id: binding.item_id.clone(),
        file_id: binding.file_id.clone(),
        file_revision: binding.file_revision.clone(),
    };
    let intent = match prepare_intent(backend, request, credential, false).await? {
        SourceIntentRead::Ready(intent) => intent,
        SourceIntentRead::Unavailable => return Ok(DispatchPreparedRead::Unavailable),
        SourceIntentRead::Capacity => return Ok(DispatchPreparedRead::Capacity),
    };
    if !agrees(binding, &intent) || !agrees(&row.handle()?, &intent) {
        return Ok(DispatchPreparedRead::Unavailable);
    }
    let Ok((floor, roster, cutoff, observed)) = members.write_guard(now_ms()?, 1, 2, 3) else {
        return Ok(DispatchPreparedRead::Unavailable);
    };
    let guard = authority_guard(&floor);
    let mut values = claim_values(&intent, roster, cutoff, observed);
    values.extend([
        raft_id.into(),
        current.node.clone().into(),
        row.dispatch.into(),
        crate::cluster::coordination::removed_job_owner_key(&current.node).into(),
    ]);
    let exact = "b.incarnation_id=$10 AND b.owner_key=$4 AND b.share_grant_id=$5 AND b.share_viewer_key=$6 AND b.request_id=$7 AND b.request_fingerprint=$8 AND b.playback_id=$9 AND b.source_server_id=$11 AND b.catalogue_epoch=$12 AND b.library_id=$13 AND b.item_id=$14 AND b.file_id=$15 AND b.file_revision=$16 AND b.reservation_state='held' AND b.start_resolved_at_ms IS NULL";
    let request_exact = "r.incarnation_id=b.incarnation_id AND r.owner_key=b.owner_key AND r.request_id=b.request_id AND r.request_fingerprint=b.request_fingerprint AND r.playback_id=b.playback_id AND r.principal_kind='sharing' AND r.user_id IS NULL AND r.share_grant_id=b.share_grant_id AND r.share_viewer_key=b.share_viewer_key AND r.state='starting' AND r.claim_expires_at_ms=$19 AND r.claim_expires_at_ms>$3";
    let owned_route = "s.incarnation_id=$10 AND s.owner_key=$4 AND s.principal_kind='sharing' AND s.user_id IS NULL AND s.share_grant_id=$5 AND s.share_viewer_key=$6 AND s.playback_id=$9 AND s.request_fingerprint=$8 AND s.owner_node_id=$22 AND s.owner_epoch=1 AND s.state='active' AND s.lease_expires_at_ms>$3";
    let local = format!("EXISTS(SELECT 1 FROM cluster_nodes WHERE raft_id=$21 AND node_id=$22 AND removed_at IS NULL) AND $23 IN(0,1) AND NOT EXISTS(SELECT 1 FROM settings WHERE key=$24) AND NOT EXISTS(SELECT 1 FROM media_sessions s WHERE s.incarnation_id=$10 AND ($23=0 OR NOT ({owned_route}))) AND NOT EXISTS(SELECT 1 FROM job_leases j WHERE j.resource='session:'||$10 AND ($23=0 OR NOT EXISTS(SELECT 1 FROM media_sessions s WHERE {owned_route} AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms=s.lease_expires_at_ms AND j.revision>0))) AND NOT EXISTS(SELECT 1 FROM cache_consumer_pins p WHERE p.consumer_kind='media_session' AND p.consumer_id=$10 AND ($23=0 OR NOT EXISTS(SELECT 1 FROM media_sessions s JOIN job_leases j ON j.resource='session:'||s.incarnation_id AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms=s.lease_expires_at_ms WHERE {owned_route}))) AND NOT EXISTS(SELECT 1 FROM media_session_preparations WHERE staged_incarnation_id=$10)");
    let statements = vec![
            (format!("UPDATE sharing_source_session_bindings AS b SET dispatch_generation=1 WHERE {exact} AND b.dispatch_generation=0 AND {guard} AND {local} AND EXISTS(SELECT 1 FROM media_session_requests r WHERE {request_exact} AND r.owner_node_id IS NULL)"),values.clone()),
            (format!("UPDATE media_session_requests AS r SET owner_node_id=$22,updated_at_ms=$3 WHERE $23=0 AND r.owner_node_id IS NULL AND EXISTS(SELECT 1 FROM sharing_source_session_bindings b WHERE {exact} AND b.dispatch_generation=1 AND {request_exact} AND {guard} AND {local})"),values.clone()),
            (format!("INSERT INTO sharing_source_session_bindings(incarnation_id,owner_key,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,source_server_id,catalogue_epoch,library_id,item_id,file_id,file_revision,reservation_state,start_resolved_at_ms,dispatch_generation,created_at_ms) SELECT NULL,$4,$5,$6,$7,$8,$9,$11,$12,$13,$14,$15,$16,'held',NULL,0,$3 WHERE NOT EXISTS(SELECT 1 FROM sharing_source_session_bindings b JOIN media_session_requests r ON {request_exact} WHERE {exact} AND b.dispatch_generation=1 AND r.owner_node_id=$22 AND {guard} AND {local})"),values),
        ];
    match backend.sharing_txn(statements).await {
        Ok(counts) if counts.as_slice() == [1, 1, 0] || counts.as_slice() == [0, 0, 0] => {
            Ok(DispatchPreparedRead::Ready(Box::new(DispatchPrepared {
                intent,
                assignment: SourceDispatchAssignment {
                    binding: binding.clone(),
                    owner_node_id: current.node,
                    dispatch_generation: 1,
                    members: members.clone(),
                },
            })))
        }
        Ok(_) => Err(invalid()),
        Err(error) if lost_proposal(&error) => Ok(DispatchPreparedRead::Unavailable),
        Err(error) => Err(error),
    }
}

fn owned_route_condition(
    authority: &SourceOwnedRouteAuthority,
    now: i64,
) -> Result<Option<Statement>, StoreError> {
    route_condition(authority, SourcePublicationPhase::Published, now)
}
fn route_condition(
    authority: &SourceOwnedRouteAuthority,
    phase: SourcePublicationPhase,
    now: i64,
) -> Result<Option<Statement>, StoreError> {
    let assignment = &authority.assignment;
    let members = &assignment.members;
    let Ok((floor, roster, cutoff, observed)) = members.write_guard(now, 1, 2, 3) else {
        return Ok(None);
    };
    let Ok(raft) = i64::try_from(members.actual_local_raft_id()) else {
        return Ok(None);
    };
    let mut values = claim_values(&authority.intent, roster, cutoff, observed);
    values.extend([
        raft.into(),
        assignment.owner_node_id.clone().into(),
        assignment.dispatch_generation.into(),
        crate::cluster::coordination::removed_job_owner_key(&assignment.owner_node_id).into(),
        authority.session_id.clone().into(),
        authority.lease_expires_at_ms.into(),
        authority.lease_revision.into(),
    ]);
    let guard = authority_guard(&floor);
    let mut exact=String::from("b.incarnation_id=$10 AND b.owner_key=$4 AND b.share_grant_id=$5 AND b.share_viewer_key=$6 AND b.request_id=$7 AND b.request_fingerprint=$8 AND b.playback_id=$9 AND b.source_server_id=$11 AND b.catalogue_epoch=$12 AND b.library_id=$13 AND b.item_id=$14 AND b.file_id=$15 AND b.file_revision=$16 AND b.reservation_state='held' AND b.dispatch_generation=$23 AND $23=1 AND b.start_resolved_at_ms IS NOT NULL");
    let mut request=String::from("r.incarnation_id=b.incarnation_id AND r.owner_key=b.owner_key AND r.request_id=b.request_id AND r.request_fingerprint=b.request_fingerprint AND r.playback_id=b.playback_id AND r.principal_kind='sharing' AND r.user_id IS NULL AND r.share_grant_id=b.share_grant_id AND r.share_viewer_key=b.share_viewer_key AND r.state='resolved' AND r.claim_expires_at_ms=$19 AND r.owner_node_id=$22");
    let mut route=String::from("s.incarnation_id=b.incarnation_id AND s.owner_key=b.owner_key AND s.principal_kind='sharing' AND s.user_id IS NULL AND s.share_grant_id=b.share_grant_id AND s.share_viewer_key=b.share_viewer_key AND s.playback_id=b.playback_id AND s.request_fingerprint=b.request_fingerprint AND s.session_id=$25 AND s.owner_node_id=$22 AND s.owner_epoch=1 AND s.state='active' AND s.lease_expires_at_ms=$26 AND s.lease_expires_at_ms>$3 AND s.publication_ready_at_ms=0 AND s.response_json=r.response_json AND length(CAST(s.recipe_json AS BLOB))<=32768 AND length(CAST(s.response_json AS BLOB))<=65536");
    if phase == SourcePublicationPhase::Pending {
        exact = exact.replace(
            "b.start_resolved_at_ms IS NOT NULL",
            "b.start_resolved_at_ms IS NULL",
        );
        request = request.replace(
            "r.state='resolved'",
            "r.state='starting' AND r.claim_expires_at_ms>$3",
        );
        route = route
            .replace(
                "s.publication_ready_at_ms=0",
                &format!(
                    "s.publication_ready_at_ms={}",
                    crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
                ),
            )
            .replace(" AND s.response_json=r.response_json", "");
    }
    let pointer="EXISTS(SELECT 1 FROM media_playback_pointers p WHERE p.owner_key=b.owner_key AND p.principal_kind='sharing' AND p.user_id IS NULL AND p.share_grant_id=b.share_grant_id AND p.share_viewer_key=b.share_viewer_key AND p.playback_id=b.playback_id AND p.current_incarnation_id=b.incarnation_id)";
    let lease="j.resource='session:'||b.incarnation_id AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms=s.lease_expires_at_ms AND j.revision=$27 AND j.revision>0 AND j.revision<9223372036854775807";
    Ok(Some((format!("EXISTS(SELECT 1 FROM sharing_source_session_bindings b JOIN media_session_requests r ON {request} JOIN media_sessions s ON {route} JOIN job_leases j ON {lease} WHERE {exact} AND {guard} AND {pointer} AND EXISTS(SELECT 1 FROM cluster_nodes WHERE raft_id=$21 AND node_id=$22 AND removed_at IS NULL) AND NOT EXISTS(SELECT 1 FROM settings WHERE key=$24) AND NOT EXISTS(SELECT 1 FROM media_session_preparations WHERE staged_incarnation_id=b.incarnation_id))"),values)))
}

pub(crate) fn source_owned_renewal_guard(
    authority: &SourceOwnedRouteAuthority,
    renewal: &crate::domain::MediaSessionRenewal,
    at: i64,
    expires: i64,
) -> Result<Option<Statement>, StoreError> {
    let now = now_ms()?;
    if renewal.incarnation_id != authority.assignment.binding.incarnation_id.to_string()
        || renewal.owner_epoch != 1
        || at <= 0
        || at > now
        || now.saturating_sub(at) > 5000
        || expires <= now
        || expires <= authority.lease_expires_at_ms
    {
        return Ok(None);
    }
    let Some((condition, values)) = owned_route_condition(authority, now)? else {
        return Ok(None);
    };
    Ok(Some((format!("INSERT INTO sharing_source_session_bindings(incarnation_id,owner_key,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,source_server_id,catalogue_epoch,library_id,item_id,file_id,file_revision,reservation_state,start_resolved_at_ms,dispatch_generation,created_at_ms) SELECT NULL,$4,$5,$6,$7,$8,$9,$11,$12,$13,$14,$15,$16,'held',NULL,0,$3 WHERE NOT ({condition})"),values)))
}

async fn prepare_owned_route<T: Backend>(
    backend: &T,
    assignment: &SourceDispatchAssignment,
    credential: &CredentialKey,
    members: &SourceAdmissionMembers,
) -> Result<SourceOwnedRouteAuthorityRead, StoreError> {
    Ok(
        match prepare_publication_route(backend, assignment, credential, members).await? {
            SourcePublicationAuthorityRead::Ready(authority)
                if authority.phase == SourcePublicationPhase::Published =>
            {
                SourceOwnedRouteAuthorityRead::Ready(Box::new(authority.owned))
            }
            SourcePublicationAuthorityRead::Ready(_)
            | SourcePublicationAuthorityRead::Unavailable => {
                SourceOwnedRouteAuthorityRead::Unavailable
            }
            SourcePublicationAuthorityRead::Capacity => SourceOwnedRouteAuthorityRead::Capacity,
        },
    )
}

async fn prepare_publication_route<T: Backend>(
    backend: &T,
    assignment: &SourceDispatchAssignment,
    credential: &CredentialKey,
    members: &SourceAdmissionMembers,
) -> Result<SourcePublicationAuthorityRead, StoreError> {
    let now = now_ms()?;
    if assignment.binding.released
        || assignment.dispatch_generation != 1
        || !present(backend).await?
        || members.write_guard(now, 1, 2, 3).is_err()
    {
        return Ok(SourcePublicationAuthorityRead::Unavailable);
    }
    let Ok(raft) = i64::try_from(members.actual_local_raft_id()) else {
        return Ok(SourcePublicationAuthorityRead::Unavailable);
    };
    let binding = &assignment.binding;
    #[derive(Deserialize)]
    struct Current {
        hash: String,
        expires: i64,
        session: String,
        lease: i64,
        revision: i64,
        pending: i64,
    }
    let rows=backend.sharing_read("SELECT json_object('hash',e.token_hash,'expires',r.claim_expires_at_ms,'session',s.session_id,'lease',s.lease_expires_at_ms,'revision',j.revision,'pending',CASE WHEN r.state='starting' AND b.start_resolved_at_ms IS NULL AND s.publication_ready_at_ms=9223372036854775807 THEN 1 WHEN r.state='resolved' AND b.start_resolved_at_ms IS NOT NULL AND s.publication_ready_at_ms=0 THEN 0 ELSE -1 END) AS payload FROM sharing_source_session_bindings b JOIN media_session_requests r ON r.owner_key=b.owner_key AND r.request_id=b.request_id AND r.incarnation_id=b.incarnation_id JOIN media_sessions s ON s.incarnation_id=b.incarnation_id JOIN job_leases j ON j.resource='session:'||s.incarnation_id AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms=s.lease_expires_at_ms JOIN sharing_exports e ON e.id=b.share_grant_id JOIN cluster_nodes n ON n.raft_id=$1 AND n.node_id=s.owner_node_id AND n.removed_at IS NULL WHERE b.incarnation_id=$2 AND b.owner_key=$3 AND s.owner_node_id=$4 AND b.reservation_state='held' AND b.dispatch_generation=1 AND s.state='active' AND s.owner_epoch=1 AND s.lease_expires_at_ms>$5 AND length(s.session_id) BETWEEN 1 AND 256 AND length(e.token_hash)=64 AND j.revision>0 AND j.revision<9223372036854775807",vec![raft.into(),binding.incarnation_id.into(),binding.principal.owner_key().into(),assignment.owner_node_id.clone().into(),now.into()]).await?;
    let [row] = rows.as_slice() else {
        return Ok(SourcePublicationAuthorityRead::Unavailable);
    };
    let current: Current = serde_json::from_str(row).map_err(|_| invalid())?;
    if !matches!(current.pending, 0 | 1) || !is_hash(&current.hash) {
        return Ok(SourcePublicationAuthorityRead::Unavailable);
    }
    let request = SourceSessionRequest {
        principal: binding.principal.clone(),
        request_id: binding.request_id.clone(),
        request_fingerprint: binding.request_fingerprint.clone(),
        playback_id: binding.playback_id.clone(),
        incarnation_id: binding.incarnation_id,
        now_ms: now,
        claim_expires_at_ms: current.expires,
        credential_hash: current.hash,
        item_id: binding.item_id.clone(),
        file_id: binding.file_id.clone(),
        file_revision: binding.file_revision.clone(),
    };
    let intent = match prepare_intent(backend, request, credential, true).await? {
        SourceIntentRead::Ready(i) => i,
        SourceIntentRead::Unavailable => return Ok(SourcePublicationAuthorityRead::Unavailable),
        SourceIntentRead::Capacity => return Ok(SourcePublicationAuthorityRead::Capacity),
    };
    if !agrees(binding, &intent) {
        return Ok(SourcePublicationAuthorityRead::Unavailable);
    }
    let authority = SourceOwnedRouteAuthority {
        assignment: SourceDispatchAssignment {
            binding: binding.clone(),
            owner_node_id: assignment.owner_node_id.clone(),
            dispatch_generation: assignment.dispatch_generation,
            members: members.clone(),
        },
        intent,
        session_id: current.session,
        lease_expires_at_ms: current.lease,
        lease_revision: current.revision,
    };
    let phase = if current.pending == 1 {
        SourcePublicationPhase::Pending
    } else {
        SourcePublicationPhase::Published
    };
    let Some((condition, values)) = route_condition(&authority, phase, now_ms()?)? else {
        return Ok(SourcePublicationAuthorityRead::Unavailable);
    };
    let rows = backend
        .sharing_read(
            &format!("SELECT json_quote(CASE WHEN {condition} THEN 1 ELSE 0 END) AS payload"),
            values,
        )
        .await?;
    if rows.first().map(String::as_str) != Some("1") {
        return Ok(SourcePublicationAuthorityRead::Unavailable);
    }
    Ok(SourcePublicationAuthorityRead::Ready(Box::new(
        SourcePublicationAuthority {
            owned: authority,
            phase,
        },
    )))
}

async fn complete_publication<T: Backend + super::MediaSessionStore>(
    backend: &T,
    authority: &SourcePublicationAuthority,
) -> Result<Option<crate::domain::MediaSessionRoute>, StoreError> {
    let now = now_ms()?;
    let Some((condition, values)) = route_condition(&authority.owned, authority.phase, now)? else {
        return Ok(None);
    };
    let assertion=(format!("INSERT INTO sharing_source_session_bindings(incarnation_id,owner_key,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,source_server_id,catalogue_epoch,library_id,item_id,file_id,file_revision,reservation_state,start_resolved_at_ms,dispatch_generation,created_at_ms) SELECT NULL,$4,$5,$6,$7,$8,$9,$11,$12,$13,$14,$15,$16,'held',NULL,0,$3 WHERE NOT ({condition})"),values);
    let binding = &authority.owned.assignment.binding;
    let node = &authority.owned.assignment.owner_node_id;
    let owner = binding.principal.owner_key();
    let mut statements = vec![assertion];
    if authority.phase == SourcePublicationPhase::Pending {
        let args = vec![
            binding.incarnation_id.into(),
            owner.clone().into(),
            authority.owned.session_id.clone().into(),
            node.clone().into(),
            authority.owned.lease_expires_at_ms.into(),
            now.into(),
        ];
        statements.push((format!("UPDATE media_sessions SET publication_ready_at_ms=0,updated_at_ms=$6 WHERE incarnation_id=$1 AND owner_key=$2 AND session_id=$3 AND owner_node_id=$4 AND owner_epoch=1 AND state='active' AND lease_expires_at_ms=$5 AND publication_ready_at_ms={}",crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED),args));
        statements.push(("UPDATE media_session_requests SET state='resolved',response_json=(SELECT response_json FROM media_sessions WHERE incarnation_id=$1 AND owner_key=$2 AND session_id=$3 AND owner_node_id=$4 AND owner_epoch=1 AND state='active' AND publication_ready_at_ms=0),claim_expires_at_ms=$5,updated_at_ms=$6 WHERE incarnation_id=$1 AND owner_key=$2 AND owner_node_id=$4 AND state='starting'".into(),vec![binding.incarnation_id.into(),owner.clone().into(),authority.owned.session_id.clone().into(),node.clone().into(),authority.owned.lease_expires_at_ms.into(),now.into()]));
        statements.push(("UPDATE sharing_source_session_bindings SET start_resolved_at_ms=$3 WHERE incarnation_id=$1 AND owner_key=$2 AND reservation_state='held' AND dispatch_generation=1 AND start_resolved_at_ms IS NULL".into(),vec![binding.incarnation_id.into(),owner.into(),now.into()]));
    }
    if authority.phase == SourcePublicationPhase::Pending {
        let mut published = authority.owned.clone();
        published.intent.request.claim_expires_at_ms = published.lease_expires_at_ms;
        let Some((condition, values)) =
            route_condition(&published, SourcePublicationPhase::Published, now)?
        else {
            return Ok(None);
        };
        statements.push((format!("INSERT INTO sharing_source_session_bindings(incarnation_id,owner_key,share_grant_id,share_viewer_key,request_id,request_fingerprint,playback_id,source_server_id,catalogue_epoch,library_id,item_id,file_id,file_revision,reservation_state,start_resolved_at_ms,dispatch_generation,created_at_ms) SELECT NULL,$4,$5,$6,$7,$8,$9,$11,$12,$13,$14,$15,$16,'held',NULL,0,$3 WHERE NOT ({condition})"), values));
    }
    let expected = if authority.phase == SourcePublicationPhase::Pending {
        vec![0, 1, 1, 1, 0]
    } else {
        vec![0]
    };
    match backend.sharing_txn(statements).await {
        Ok(counts) if counts == expected => {}
        Ok(_) => return Err(invalid()),
        Err(error) if source_write_refused(&error) => return Ok(None),
        Err(error) => return Err(error),
    }
    Ok(backend
        .media_session_route_by_incarnation(&binding.incarnation_id.to_string())
        .await?
        .filter(|route| {
            route.principal == binding.principal
                && route.session_id == authority.owned.session_id
                && route.playback_id == binding.playback_id
                && route.request_fingerprint == binding.request_fingerprint
                && route.owner_node_id == *node
                && route.owner_epoch == 1
                && route.state == "active"
                && route.publication_ready_at_ms == 0
                && route.lease_expires_at_ms == authority.owned.lease_expires_at_ms
        }))
}

#[async_trait]
pub trait SharingSourceSessionStore: Send + Sync {
    async fn prepare_source_publication_authority(
        &self,
        assignment: &SourceDispatchAssignment,
        credential: &CredentialKey,
        members: &SourceAdmissionMembers,
    ) -> Result<SourcePublicationAuthorityRead, StoreError>;
    /// SQL permission only. The actual worker owns the registered producer
    /// readiness barrier before invoking this coupled publication write.
    async fn complete_source_media_session_publication(
        &self,
        authority: &SourcePublicationAuthority,
    ) -> Result<Option<crate::domain::MediaSessionRoute>, StoreError>;
    async fn prepare_source_owned_route_authority(
        &self,
        assignment: &SourceDispatchAssignment,
        credential: &CredentialKey,
        members: &SourceAdmissionMembers,
    ) -> Result<SourceOwnedRouteAuthorityRead, StoreError>;
    async fn prepare_source_activation_authority(
        &self,
        assignment: &SourceDispatchAssignment,
        credential: &CredentialKey,
        members: &SourceAdmissionMembers,
    ) -> Result<SourceWriteAuthorityRead, StoreError>;
    /// Commit this actual local worker's ownership before queueing. This is
    /// not producer admission and does not enable remote scheduling.
    async fn assign_source_dispatch(
        &self,
        binding: &SourceBindingHandle,
        credential: &CredentialKey,
        members: &SourceAdmissionMembers,
    ) -> Result<Option<SourceDispatchAssignment>, StoreError>;
    /// Existing identity/key and current grant-authorized witness only.
    async fn prepare_source_session_intent(
        &self,
        request: SourceSessionRequest,
        credential: &CredentialKey,
    ) -> Result<SourceIntentRead, StoreError>;
    /// Durable database reservation only; this does not dispatch a worker.
    async fn claim_source_media_session(
        &self,
        intent: &SourceSessionIntent,
        members: &SourceAdmissionMembers,
    ) -> Result<SourceClaimOutcome, StoreError>;
    /// This callback is limited to the immutable, provably undispatched CAS.
    async fn release_source_never_dispatched(
        &self,
        binding: &SourceBindingHandle,
    ) -> Result<SourceReleaseOutcome, StoreError>;
}

#[async_trait]
impl<T: Backend + super::MediaSessionStore> SharingSourceSessionStore for T {
    async fn prepare_source_publication_authority(
        &self,
        assignment: &SourceDispatchAssignment,
        credential: &CredentialKey,
        members: &SourceAdmissionMembers,
    ) -> Result<SourcePublicationAuthorityRead, StoreError> {
        prepare_publication_route(self, assignment, credential, members).await
    }
    async fn complete_source_media_session_publication(
        &self,
        authority: &SourcePublicationAuthority,
    ) -> Result<Option<crate::domain::MediaSessionRoute>, StoreError> {
        complete_publication(self, authority).await
    }
    async fn prepare_source_owned_route_authority(
        &self,
        assignment: &SourceDispatchAssignment,
        credential: &CredentialKey,
        members: &SourceAdmissionMembers,
    ) -> Result<SourceOwnedRouteAuthorityRead, StoreError> {
        prepare_owned_route(self, assignment, credential, members).await
    }
    async fn assign_source_dispatch(
        &self,
        binding: &SourceBindingHandle,
        credential: &CredentialKey,
        members: &SourceAdmissionMembers,
    ) -> Result<Option<SourceDispatchAssignment>, StoreError> {
        Ok(
            match assign_dispatch_prepared(self, binding, credential, members).await? {
                DispatchPreparedRead::Ready(prepared) => Some(prepared.assignment),
                DispatchPreparedRead::Unavailable | DispatchPreparedRead::Capacity => None,
            },
        )
    }

    async fn prepare_source_activation_authority(
        &self,
        assignment: &SourceDispatchAssignment,
        credential: &CredentialKey,
        members: &SourceAdmissionMembers,
    ) -> Result<SourceWriteAuthorityRead, StoreError> {
        match assign_dispatch_prepared(self, &assignment.binding, credential, members).await? {
            DispatchPreparedRead::Ready(prepared)
                if prepared.assignment.owner_node_id == assignment.owner_node_id
                    && prepared.assignment.dispatch_generation
                        == assignment.dispatch_generation =>
            {
                Ok(SourceWriteAuthorityRead::Ready(Box::new(
                    SourceSessionWriteAuthority {
                        assignment: prepared.assignment,
                        intent: prepared.intent,
                    },
                )))
            }
            DispatchPreparedRead::Ready(_) | DispatchPreparedRead::Unavailable => {
                Ok(SourceWriteAuthorityRead::Unavailable)
            }
            DispatchPreparedRead::Capacity => Ok(SourceWriteAuthorityRead::Capacity),
        }
    }

    async fn prepare_source_session_intent(
        &self,
        request: SourceSessionRequest,
        credential: &CredentialKey,
    ) -> Result<SourceIntentRead, StoreError> {
        prepare_intent(self, request, credential, false).await
    }

    async fn claim_source_media_session(
        &self,
        intent: &SourceSessionIntent,
        members: &SourceAdmissionMembers,
    ) -> Result<SourceClaimOutcome, StoreError> {
        if !present(self).await? {
            return Ok(SourceClaimOutcome::Unavailable);
        }
        let now = now_ms()?;
        let Ok((floor, roster, cutoff, observed)) = members.write_guard(now, 1, 2, 3) else {
            return Ok(SourceClaimOutcome::Unavailable);
        };
        let guard = authority_guard(&floor);
        let values = claim_values(intent, roster, cutoff, observed);
        let owner = intent.request.principal.owner_key();
        if let Some(row) = binding_row(self, &owner, &intent.request.request_id).await? {
            return replay(self, row, intent, &guard, values).await;
        }
        match self
            .sharing_txn(fresh_statements(&guard, values.clone()))
            .await
        {
            Ok(counts) if counts.as_slice() == [1, 1, 0] => {
                let row = binding_row(self, &owner, &intent.request.request_id)
                    .await?
                    .ok_or_else(invalid)?;
                let binding = row.handle()?;
                if !agrees(&binding, intent)
                    || binding.incarnation_id != intent.request.incarnation_id
                {
                    return Err(invalid());
                }
                Ok(SourceClaimOutcome::Acquired(binding))
            }
            Ok(_) => Err(invalid()),
            Err(error) if lost_proposal(&error) => {
                if let Some(row) = binding_row(self, &owner, &intent.request.request_id).await? {
                    return replay(self, row, intent, &guard, values).await;
                }
                if !authorized(self, &guard, values).await? {
                    return Ok(SourceClaimOutcome::Unavailable);
                }
                let PlaybackPrincipal::Sharing { grant_id, .. } = intent.request.principal else {
                    return Err(invalid());
                };
                Ok(capacity(self, grant_id)
                    .await?
                    .map(SourceClaimOutcome::Capacity)
                    .unwrap_or(SourceClaimOutcome::Unavailable))
            }
            Err(error) => Err(error),
        }
    }

    async fn release_source_never_dispatched(
        &self,
        binding: &SourceBindingHandle,
    ) -> Result<SourceReleaseOutcome, StoreError> {
        if !present(self).await? {
            return Ok(SourceReleaseOutcome::Refused);
        }
        let PlaybackPrincipal::Sharing {
            grant_id,
            viewer_key,
        } = &binding.principal
        else {
            return Err(invalid());
        };
        let receipt = format!(
            "{:x}",
            Sha256::digest(
                format!(
                    "sharing-source-never-dispatched-v1:{}:{}:{}:{}",
                    binding.incarnation_id,
                    binding.principal.owner_key(),
                    binding.request_id,
                    binding.request_fingerprint
                )
                .as_bytes()
            )
        );
        let now = now_ms()?;
        let values = vec![
            binding.incarnation_id.into(),
            binding.principal.owner_key().into(),
            (*grant_id).into(),
            viewer_key.as_str().to_owned().into(),
            binding.request_id.clone().into(),
            binding.request_fingerprint.clone().into(),
            binding.playback_id.clone().into(),
            binding.source_server_id.into(),
            binding.catalogue_epoch.into(),
            binding.library_id.as_str().to_owned().into(),
            binding.item_id.as_str().to_owned().into(),
            binding.file_id.as_str().to_owned().into(),
            binding.file_revision.as_str().to_owned().into(),
            now.into(),
            receipt.clone().into(),
        ];
        let exact="incarnation_id=$1 AND owner_key=$2 AND share_grant_id=$3 AND share_viewer_key=$4 AND request_id=$5 AND request_fingerprint=$6 AND playback_id=$7 AND source_server_id=$8 AND catalogue_epoch=$9 AND library_id=$10 AND item_id=$11 AND file_id=$12 AND file_revision=$13";
        let release=format!("UPDATE sharing_source_session_bindings SET reservation_state='released',start_resolved_at_ms=$14,released_at_ms=$14,release_fingerprint=$15 WHERE {exact} AND reservation_state='held' AND dispatch_generation=0 AND start_resolved_at_ms IS NULL AND ({})
 AND NOT EXISTS(SELECT 1 FROM media_sessions WHERE incarnation_id=$1)
 AND NOT EXISTS(SELECT 1 FROM job_leases WHERE resource='session:'||$1)
 AND NOT EXISTS(SELECT 1 FROM cache_consumer_pins WHERE consumer_kind='media_session' AND consumer_id=$1)
 AND NOT EXISTS(SELECT 1 FROM media_session_preparations WHERE staged_incarnation_id=$1)
 AND NOT EXISTS(SELECT 1 FROM library_channel_session_recipes WHERE incarnation_id=$1)
 AND NOT EXISTS(SELECT 1 FROM media_playback_pointers WHERE current_incarnation_id=$1)
 AND NOT EXISTS(SELECT 1 FROM media_session_requests WHERE (incarnation_id=$1 OR(owner_key=$2 AND request_id=$5)) AND NOT(owner_key=$2 AND principal_kind='sharing' AND user_id IS NULL AND share_grant_id=$3 AND share_viewer_key=$4 AND request_id=$5 AND request_fingerprint=$6 AND playback_id=$7 AND incarnation_id=$1 AND owner_node_id IS NULL AND state IN('starting','failed')))",schema_guard());
        let settle=format!("UPDATE media_session_requests SET state='failed',updated_at_ms=$14 WHERE owner_key=$2 AND request_id=$5 AND incarnation_id=$1 AND state='starting' AND owner_node_id IS NULL AND EXISTS(SELECT 1 FROM sharing_source_session_bindings WHERE {exact} AND reservation_state='released' AND released_at_ms=$14 AND release_fingerprint=$15)");
        let counts = self
            .sharing_txn(vec![(release, values.clone()), (settle, values.clone())])
            .await?;
        if counts.first() == Some(&1) {
            return Ok(SourceReleaseOutcome::Released);
        }
        let rows=self.sharing_read(&format!("SELECT json_quote(count(*)) AS payload FROM sharing_source_session_bindings WHERE {exact} AND reservation_state='released' AND release_fingerprint=$15 AND $14>0"),values).await?;
        Ok(if rows.first().map(String::as_str) == Some("1") {
            SourceReleaseOutcome::ExactReplay
        } else {
            SourceReleaseOutcome::Refused
        })
    }
}

async fn prepare_intent<T: Backend>(
    backend: &T,
    request: SourceSessionRequest,
    credential: &CredentialKey,
    resolved: bool,
) -> Result<SourceIntentRead, StoreError> {
    let PlaybackPrincipal::Sharing { grant_id, .. } = &request.principal else {
        return Err(invalid());
    };
    if !present(backend).await? {
        return Ok(SourceIntentRead::Unavailable);
    }
    let witness = match backend
        .source_item_file_witness(
            &request.credential_hash,
            *grant_id,
            request.item_id.clone(),
            request.file_id.clone(),
        )
        .await?
    {
        SourceDetailsRead::Authorized(witness) => witness,
        SourceDetailsRead::Unavailable => return Ok(SourceIntentRead::Unavailable),
        SourceDetailsRead::Capacity => return Ok(SourceIntentRead::Capacity),
    };
    let Some(envelope) = backend
        .source_catalogue_revision_key(witness.server, witness.epoch)
        .await?
    else {
        return Ok(SourceIntentRead::Unavailable);
    };
    let rows = backend.sharing_read("SELECT json_object('server_id',server_id,'catalogue_epoch',catalogue_epoch,'created_at_ms',created_at_ms) AS payload FROM sharing_identity WHERE singleton=1",vec![]).await?;
    let [row] = rows.as_slice() else {
        return Ok(SourceIntentRead::Unavailable);
    };
    let identity: SharingIdentity = serde_json::from_str(row).map_err(|_| invalid())?;
    if identity.server_id != witness.server || identity.catalogue_epoch != witness.epoch {
        return Ok(SourceIntentRead::Unavailable);
    }
    let key = CatalogueRevisionKey::open(credential, identity, &envelope)?;
    if key.file_revision(&witness)? != request.file_revision {
        return Ok(SourceIntentRead::Unavailable);
    }
    Ok(SourceIntentRead::Ready(Box::new(if resolved {
        SourceSessionIntent::from_current_owned_witness(request, witness, &key, envelope)?
    } else {
        SourceSessionIntent::from_current_witness(request, witness, &key, envelope)?
    })))
}

#[cfg(test)]
#[path = "sharing_source_session_tests.rs"]
mod tests;
