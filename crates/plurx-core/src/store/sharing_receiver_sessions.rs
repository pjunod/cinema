//! Atomic B authority and remote-upstream binding for ordinary Local sessions.
use super::{
    sharing::{Backend, Statement, Value},
    SharingCatalogueStore,
};
use crate::{
    domain::{MediaSessionActivation, MEDIA_SESSION_PUBLICATION_BLOCKED},
    error::StoreError,
    playback_principal::PlaybackPrincipal,
    sharing::{invalid, is_hash},
    sharing_receiver_sessions::{
        ReceiverSessionIntent, ReceiverSessionWriteAuthority, ReceiverSourceAttachment,
        ReceiverSourcePublication, ReceiverSourceRenewal, ReceiverSourceWrite,
    },
};
use async_trait::async_trait;
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};

fn now_ms() -> Result<i64, StoreError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_millis()).ok())
        .filter(|n| *n > 0)
        .ok_or_else(invalid)
}
fn policy_json_sql() -> String {
    let read = |key: &str| format!("(SELECT value FROM settings WHERE key='{key}')");
    format!(
        "json_object('enabled',{},'days',{},'since',{})",
        read(super::keys::AUTH_TOKEN_EXPIRY_ENABLED),
        read(super::keys::AUTH_TOKEN_IDLE_DAYS),
        read(super::keys::AUTH_TOKEN_EXPIRY_SINCE)
    )
}
const SCOPE: &str = "EXISTS(SELECT 1 FROM sharing_imports i JOIN sharing_viewers v ON v.user_id=u.id WHERE i.id=json_extract($3,'$.import_id') AND i.source_server_id=json_extract($3,'$.source_server_id') AND i.catalogue_epoch=json_extract($3,'$.catalogue_epoch') AND i.lifecycle_generation=json_extract($3,'$.lifecycle_generation') AND i.assignment_generation>=json_extract($3,'$.assignment_generation') AND i.endpoint_generation>=json_extract($3,'$.endpoint_generation') AND i.claim_id=json_extract($3,'$.claim_id') AND i.remote_grant_id=json_extract($3,'$.remote_grant_id') AND i.state='active' AND NOT EXISTS(SELECT 1 FROM json_each($3,'$.libraries') l WHERE NOT EXISTS(SELECT 1 FROM sharing_assignments a WHERE a.import_id=i.id AND a.user_id=u.id AND a.enabled=1 AND a.remote_library_id=l.value)))";
const SWITCH: &str = "EXISTS(SELECT 1 FROM settings WHERE key='sharing_enabled' AND CASE WHEN typeof(value)='text' AND length(CAST(value AS BLOB))<=64 THEN lower(trim(value)) IN('1','true','yes','on') ELSE 0 END)";

#[async_trait]
pub trait SharingReceiverSessionStore: Send + Sync {
    async fn prepare_receiver_session_authority(
        &self,
        intent: ReceiverSessionIntent,
    ) -> Result<Option<ReceiverSessionWriteAuthority>, StoreError>;
    /// Fresh original-login proof; atomically extends the pending request,
    /// blocked owner and matching lease without resolving a Source outcome.
    async fn renew_pending_receiver_session(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        renewal: &crate::sharing_receiver_sessions::ReceiverPendingRenewal,
    ) -> Result<bool, StoreError>;
    async fn attach_receiver_source(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        attachment: &ReceiverSourceAttachment,
    ) -> Result<ReceiverSourceWrite, StoreError>;
    async fn publish_receiver_source(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        publication: &ReceiverSourcePublication,
    ) -> Result<ReceiverSourceWrite, StoreError>;
    async fn renew_receiver_source_session(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        renewal: &ReceiverSourceRenewal,
    ) -> Result<ReceiverSourceWrite, StoreError>;
}
#[async_trait]
impl<T: Backend> SharingReceiverSessionStore for T {
    async fn attach_receiver_source(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        attachment: &ReceiverSourceAttachment,
    ) -> Result<ReceiverSourceWrite, StoreError> {
        attach_source(self, authority, attachment).await
    }
    async fn publish_receiver_source(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        publication: &ReceiverSourcePublication,
    ) -> Result<ReceiverSourceWrite, StoreError> {
        publish_source(self, authority, publication).await
    }
    async fn renew_receiver_source_session(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        renewal: &ReceiverSourceRenewal,
    ) -> Result<ReceiverSourceWrite, StoreError> {
        renew_source(self, authority, renewal).await
    }
    async fn renew_pending_receiver_session(
        &self,
        authority: &ReceiverSessionWriteAuthority,
        renewal: &crate::sharing_receiver_sessions::ReceiverPendingRenewal,
    ) -> Result<bool, StoreError> {
        renew_pending(self, authority, renewal).await
    }

    async fn prepare_receiver_session_authority(
        &self,
        intent: ReceiverSessionIntent,
    ) -> Result<Option<ReceiverSessionWriteAuthority>, StoreError> {
        let now = now_ms()?;
        let reference = &intent.recipe.reference;
        if intent.user_id <= 0
            || !is_hash(&intent.login_hash)
            || intent.recipe.parent_login_hash != intent.login_hash
            || intent.recipe.version != 1
            || intent.recipe.source_request_id.is_nil()
            || intent.source_position_ms < 0
            || intent.scope.import_id != reference.import_id
            || intent.scope.source_server_id != reference.server_id
            || intent.scope.catalogue_epoch != reference.catalogue_epoch
            || intent.scope.lifecycle_generation != intent.recipe.lifecycle_generation
            || intent.scope.libraries.as_slice() != [reference.library_id.clone()]
            || !(2..=24 * 1024).contains(&intent.recipe.request_json.len())
        {
            return Err(invalid());
        }
        let request: serde_json::Value =
            serde_json::from_str(&intent.recipe.request_json).map_err(|_| invalid())?;
        if !request.is_object()
            || serde_json::to_string(&request).map_err(|_| invalid())? != intent.recipe.request_json
        {
            return Err(invalid());
        }
        if !self
            .receiver_catalogue_authorized(
                &intent.login_hash,
                intent.user_id,
                std::slice::from_ref(&intent.scope),
                now / 1000,
            )
            .await?
        {
            return Ok(None);
        }
        let scope = serde_json::to_string(&intent.scope).map_err(|_| invalid())?;
        let sql = format!("SELECT json_object('last_seen',t.last_seen_at,'policy',{}) AS payload FROM tokens t JOIN users u ON u.id=t.user_id WHERE t.token_hash=$1 AND u.id=$2 AND {SCOPE} AND {SWITCH} AND NOT EXISTS(SELECT 1 FROM settings WHERE key IN ('{}','{}','{}') AND (typeof(value)!='text' OR length(CAST(value AS BLOB))>64))", policy_json_sql(), super::keys::AUTH_TOKEN_EXPIRY_ENABLED, super::keys::AUTH_TOKEN_IDLE_DAYS, super::keys::AUTH_TOKEN_EXPIRY_SINCE);
        let rows = self
            .sharing_read(
                &sql,
                vec![
                    intent.login_hash.clone().into(),
                    intent.user_id.into(),
                    scope.into(),
                ],
            )
            .await?;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Policy {
            enabled: Option<String>,
            days: Option<String>,
            since: Option<String>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Row {
            last_seen: i64,
            policy: Policy,
        }
        let [row] = rows.as_slice() else {
            return if rows.is_empty() {
                Ok(None)
            } else {
                Err(invalid())
            };
        };
        let value: serde_json::Value = serde_json::from_str(row).map_err(|_| invalid())?;
        let policy_json = serde_json::to_string(value.get("policy").ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        let row: Row = serde_json::from_value(value).map_err(|_| invalid())?;
        let login_expires_at_s = crate::auth::TokenIdlePolicy::from_settings(
            row.policy.enabled.as_deref(),
            row.policy.days.as_deref(),
            row.policy.since.as_deref(),
        )
        .map(|p| p.expires_at(row.last_seen));
        if login_expires_at_s.is_some_and(|deadline| now / 1000 >= deadline) {
            return Ok(None);
        }
        Ok(Some(ReceiverSessionWriteAuthority {
            intent,
            policy_json,
            last_seen: row.last_seen,
            login_expires_at_s,
            observed_at_ms: now,
        }))
    }
}

/// Pending Source response is an obligation, not permission to replace it.
/// Renew only an existing blocked B owner and its original request together.
async fn renew_pending<T: Backend>(
    store: &T,
    authority: &ReceiverSessionWriteAuthority,
    renewal: &crate::sharing_receiver_sessions::ReceiverPendingRenewal,
) -> Result<bool, StoreError> {
    let actual_now = now_ms()?;
    if renewal.incarnation_id != authority.intent.recipe.source_request_id.to_string()
        || renewal.owner_node_id.is_empty()
        || renewal.owner_node_id.len() > 128
        || renewal.owner_epoch <= 0
        || !(1..=128).contains(&renewal.request_id.len())
        || renewal.request_id.chars().any(char::is_control)
        || renewal.now_ms > actual_now
        || actual_now.saturating_sub(renewal.now_ms) > 5000
        || actual_now.saturating_sub(authority.observed_at_ms) > 5000
        || renewal.lease_expires_at_ms <= actual_now
        || renewal.lease_expires_at_ms > actual_now.saturating_add(30_000)
        || authority
            .login_expires_at_s
            .is_some_and(|deadline| actual_now / 1000 >= deadline)
    {
        return Ok(false);
    }
    let mut values = values(authority)?;
    values.extend([
        renewal.incarnation_id.clone().into(),
        renewal.owner_node_id.clone().into(),
        renewal.owner_epoch.into(),
        renewal.request_id.clone().into(),
        serde_json::to_string(&authority.intent.recipe)
            .map_err(|_| invalid())?
            .into(),
        authority.intent.recipe.request_fingerprint()?.into(),
        renewal.now_ms.into(),
        renewal.lease_expires_at_ms.into(),
        crate::cluster::coordination::removed_job_owner_key(&renewal.owner_node_id).into(),
        authority.intent.source_position_ms.into(),
    ]);
    // Both supported principal layouts retain Local user_id. Exact pending
    // request, pointer, recipe, owner epoch and upstream identity are mandatory.
    let current = "EXISTS(SELECT 1 FROM media_sessions s JOIN sharing_relay_upstream b ON b.incarnation_id=s.incarnation_id JOIN job_leases j ON j.resource='session:'||s.incarnation_id JOIN media_session_requests r ON r.incarnation_id=s.incarnation_id AND r.user_id=s.user_id WHERE s.incarnation_id=$6 AND s.user_id=$2 AND s.owner_node_id=$7 AND s.owner_epoch=$8 AND s.recipe_json=$10 AND s.state='active' AND s.publication_ready_at_ms=9223372036854775807 AND s.media_origin_ms=$15 AND s.lease_expires_at_ms>$12 AND j.owner_node_id=$7 AND j.fence=$8 AND j.expires_at_ms=s.lease_expires_at_ms AND r.request_id=$9 AND r.owner_node_id=$7 AND r.request_fingerprint=$11 AND r.playback_id=s.playback_id AND r.state='starting' AND r.claim_expires_at_ms>$12 AND b.import_id=json_extract($3,'$.import_id') AND b.lifecycle_generation=json_extract($3,'$.lifecycle_generation') AND b.assignment_generation<=json_extract($3,'$.assignment_generation') AND b.endpoint_revision<=json_extract($3,'$.endpoint_generation') AND b.remote_library_id=json_extract($10,'$.reference.library_id') AND b.remote_item_id=json_extract($10,'$.reference.item_id') AND b.remote_file_id=json_extract($10,'$.file_id') AND b.remote_revision=json_extract($10,'$.file_revision') AND b.source_request_id=json_extract($10,'$.source_request_id') AND b.source_position_ms=$15 AND b.source_session_id IS NULL AND b.source_incarnation_id IS NULL AND b.capability_envelope IS NULL AND EXISTS(SELECT 1 FROM media_playback_pointers p WHERE p.user_id=s.user_id AND p.playback_id=s.playback_id AND p.current_incarnation_id=s.incarnation_id))";
    let valid = format!(
        "{} AND {current} AND NOT EXISTS(SELECT 1 FROM settings WHERE key=$14) AND $13>$12",
        authority_predicate()
    );
    let guard = |predicate: String| {
        (format!("INSERT INTO sharing_relay_upstream(incarnation_id,import_id,lifecycle_generation,assignment_generation,remote_library_id,remote_item_id,remote_file_id,remote_revision,source_request_id,endpoint_revision,source_position_ms) SELECT json_extract('receiver_source_authority_refused','$'),'',1,1,'0','0','0','','',1,0 WHERE NOT ({predicate})"), values.clone())
    };
    let statements = vec![
        guard(format!("{valid} AND EXISTS(SELECT 1 FROM job_leases WHERE resource='session:'||$6 AND revision<9223372036854775807 AND expires_at_ms<=$13) AND EXISTS(SELECT 1 FROM media_session_requests WHERE incarnation_id=$6 AND claim_expires_at_ms<=$13)")),
        ("UPDATE media_session_requests SET claim_expires_at_ms=$1,updated_at_ms=$2 WHERE incarnation_id=$3 AND user_id=$4 AND request_id=$5 AND state='starting' AND owner_node_id=$6".into(),vec![renewal.lease_expires_at_ms.into(),renewal.now_ms.into(),renewal.incarnation_id.clone().into(),authority.intent.user_id.into(),renewal.request_id.clone().into(),renewal.owner_node_id.clone().into()]),
        ("UPDATE job_leases SET expires_at_ms=$1,revision=revision+1,updated_at_ms=$2 WHERE resource='session:'||$3 AND owner_node_id=$4 AND fence=$5".into(),vec![renewal.lease_expires_at_ms.into(),renewal.now_ms.into(),renewal.incarnation_id.clone().into(),renewal.owner_node_id.clone().into(),renewal.owner_epoch.into()]),
        ("UPDATE media_sessions SET lease_expires_at_ms=$1,updated_at_ms=$2 WHERE incarnation_id=$3 AND owner_node_id=$4 AND owner_epoch=$5 AND state='active'".into(),vec![renewal.lease_expires_at_ms.into(),renewal.now_ms.into(),renewal.incarnation_id.clone().into(),renewal.owner_node_id.clone().into(),renewal.owner_epoch.into()]),
        guard(format!("{valid} AND EXISTS(SELECT 1 FROM media_sessions WHERE incarnation_id=$6 AND lease_expires_at_ms=$13) AND EXISTS(SELECT 1 FROM media_session_requests WHERE incarnation_id=$6 AND claim_expires_at_ms=$13)")),
    ];
    match store.sharing_txn(statements).await {
        Ok(counts) if counts == [0, 1, 1, 1, 0] => Ok(true),
        Ok(_) => Err(invalid()),
        Err(error) if receiver_write_refused(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

// Every Source-bound writer repeats the actual B login/policy/import predicate
// and exact route/request/lease lineage in its transaction. A Source result is
// caller actor evidence; these guards authorize only B metadata.
fn source_values(
    authority: &ReceiverSessionWriteAuthority,
    attachment: &ReceiverSourceAttachment,
) -> Result<Option<Vec<Value>>, StoreError> {
    let actual = now_ms()?;
    let owner = &attachment.owner;
    let binding = &attachment.binding;
    let recipe = &authority.intent.recipe;
    if binding.capability_envelope.as_stored().len() > 4096 {
        return Err(invalid());
    }
    let envelope = binding
        .capability_envelope
        .to_persist()
        .map_err(|_| invalid())?;
    // The v1 XChaCha envelope must contain its 24-byte nonce and 16-byte tag.
    if envelope
        .rsplit(':')
        .next()
        .is_none_or(|body| body.len() < 80)
    {
        return Err(invalid());
    }
    if owner.incarnation_id != recipe.source_request_id
        || owner.session_id.is_nil()
        || !(1..=256).contains(&owner.owner_node_id.len())
        || owner.owner_node_id.chars().any(char::is_control)
        || owner.owner_epoch <= 0
        || !(1..=128).contains(&owner.request_id.len())
        || owner.request_id.chars().any(char::is_control)
        || owner.now_ms <= 0
        || owner.now_ms > actual
        || actual.saturating_sub(owner.now_ms) > 5000
        || authority.observed_at_ms > actual
        || actual.saturating_sub(authority.observed_at_ms) > 5000
        || owner.lease_expires_at_ms <= actual
        || authority
            .login_expires_at_s
            .is_some_and(|deadline| actual / 1000 >= deadline)
        || binding.reference != recipe.reference
        || binding.file_id != recipe.file_id
        || binding.file_revision != recipe.file_revision
        || binding.source_request_id != recipe.source_request_id
        || binding.source_session_id.is_nil()
        || binding.source_incarnation_id.is_nil()
    {
        return Ok(None);
    }
    let mut values = values(authority)?;
    values.extend([
        owner.incarnation_id.into(),
        owner.session_id.into(),
        owner.owner_node_id.clone().into(),
        owner.owner_epoch.into(),
        owner.request_id.clone().into(),
        serde_json::to_string(recipe).map_err(|_| invalid())?.into(),
        recipe.request_fingerprint()?.into(),
        owner.lease_expires_at_ms.into(),
        actual.into(),
        binding.source_session_id.into(),
        binding.source_incarnation_id.into(),
        envelope.to_owned().into(),
        authority.intent.source_position_ms.into(),
        crate::cluster::coordination::removed_job_owner_key(&owner.owner_node_id).into(),
    ]);
    Ok(Some(values))
}

const ATTACHED: &str =
    "b.source_session_id=$15 AND b.source_incarnation_id=$16 AND b.capability_envelope=$17";
const UNATTACHED: &str = "b.source_session_id IS NULL AND b.source_incarnation_id IS NULL AND b.capability_envelope IS NULL";
const BLOCKED: &str = "s.publication_ready_at_ms=9223372036854775807 AND r.state='starting' AND r.claim_expires_at_ms=s.lease_expires_at_ms AND r.response_json IS NULL";
const PUBLISHED: &str =
    "s.publication_ready_at_ms=0 AND r.state='resolved' AND r.response_json=s.response_json";

fn source_current(binding: &str, state: &str) -> String {
    format!("{} AND NOT EXISTS(SELECT 1 FROM settings WHERE key=$19) AND EXISTS(SELECT 1 FROM media_sessions s JOIN sharing_relay_upstream b ON b.incarnation_id=s.incarnation_id JOIN job_leases j ON j.resource='session:'||s.incarnation_id JOIN media_session_requests r ON r.incarnation_id=s.incarnation_id AND r.user_id=s.user_id WHERE s.incarnation_id=$6 AND s.session_id=$7 AND s.user_id=$2 AND s.owner_node_id=$8 AND s.owner_epoch=$9 AND s.recipe_json=$11 AND s.request_fingerprint=$12 AND s.state='active' AND s.media_origin_ms=$18 AND s.lease_expires_at_ms=$13 AND s.lease_expires_at_ms>$14 AND j.owner_node_id=$8 AND j.fence=$9 AND j.expires_at_ms=s.lease_expires_at_ms AND r.request_id=$10 AND r.owner_node_id=$8 AND r.request_fingerprint=$12 AND r.playback_id=s.playback_id AND b.import_id=json_extract($3,'$.import_id') AND b.lifecycle_generation=json_extract($3,'$.lifecycle_generation') AND b.assignment_generation<=json_extract($3,'$.assignment_generation') AND b.endpoint_revision<=json_extract($3,'$.endpoint_generation') AND b.remote_library_id=json_extract($11,'$.reference.library_id') AND b.remote_item_id=json_extract($11,'$.reference.item_id') AND b.remote_file_id=json_extract($11,'$.file_id') AND b.remote_revision=json_extract($11,'$.file_revision') AND b.source_request_id=json_extract($11,'$.source_request_id') AND b.source_position_ms=$18 AND ({binding}) AND ({state}) AND EXISTS(SELECT 1 FROM media_playback_pointers p WHERE p.user_id=s.user_id AND p.playback_id=s.playback_id AND p.current_incarnation_id=s.incarnation_id))",authority_predicate())
}
fn source_assert(predicate: String, values: Vec<Value>) -> Statement {
    (format!("INSERT INTO sharing_relay_upstream(incarnation_id,import_id,lifecycle_generation,assignment_generation,remote_library_id,remote_item_id,remote_file_id,remote_revision,source_request_id,endpoint_revision,source_position_ms) SELECT json_extract('receiver_source_authority_refused','$'),'',1,1,'0','0','0','','',1,0 WHERE NOT ({predicate})"),values)
}
fn source_write_refused(error: &StoreError) -> bool {
    // source_assert evaluates a fixed malformed-JSON expression only when its
    // closed predicate refuses. SELECT evaluates it before an INSERT trigger
    // can ignore the assertion. All JSON predicate inputs are server-serialized.
    receiver_write_refused(error)
}

async fn source_result<T: Backend>(
    store: &T,
    statements: Vec<Statement>,
    expected: &[usize],
) -> Result<ReceiverSourceWrite, StoreError> {
    match store.sharing_txn(statements).await {
        Ok(counts) if counts == expected => Ok(ReceiverSourceWrite::Applied),
        Ok(counts) if counts.iter().all(|count| *count == 0) && counts.len() == expected.len() => {
            Ok(ReceiverSourceWrite::Replay)
        }
        Ok(_) => Err(invalid()),
        Err(error) if source_write_refused(&error) => Ok(ReceiverSourceWrite::Refused),
        Err(error) => Err(error),
    }
}
async fn attach_source<T: Backend>(
    store: &T,
    authority: &ReceiverSessionWriteAuthority,
    attachment: &ReceiverSourceAttachment,
) -> Result<ReceiverSourceWrite, StoreError> {
    let Some(values) = source_values(authority, attachment)? else {
        return Ok(ReceiverSourceWrite::Refused);
    };
    let state = format!("({BLOCKED}) OR ({PUBLISHED})");
    let accepted = source_current(
        &format!("(({UNATTACHED}) AND ({BLOCKED})) OR ({ATTACHED})"),
        &state,
    );
    let statements=vec![
        source_assert(accepted,values.clone()),
        ("UPDATE sharing_relay_upstream SET source_session_id=$1,source_incarnation_id=$2,capability_envelope=$3 WHERE incarnation_id=$4 AND source_session_id IS NULL AND source_incarnation_id IS NULL AND capability_envelope IS NULL".into(),vec![attachment.binding.source_session_id.into(),attachment.binding.source_incarnation_id.into(),attachment.binding.capability_envelope.to_persist().map_err(|_|invalid())?.to_owned().into(),attachment.owner.incarnation_id.into()]),
        source_assert(source_current(ATTACHED,&state),values),
    ];
    source_result(store, statements, &[0, 1, 0]).await
}
async fn publish_source<T: Backend>(
    store: &T,
    authority: &ReceiverSessionWriteAuthority,
    publication: &ReceiverSourcePublication,
) -> Result<ReceiverSourceWrite, StoreError> {
    if !(2..=64 * 1024).contains(&publication.response_json.len()) {
        return Err(invalid());
    }
    let response: serde_json::Value =
        serde_json::from_str(&publication.response_json).map_err(|_| invalid())?;
    if !response.is_object()
        || serde_json::to_string(&response).map_err(|_| invalid())? != publication.response_json
    {
        return Err(invalid());
    }
    let attachment = &publication.attachment;
    let Some(mut values) = source_values(authority, attachment)? else {
        return Ok(ReceiverSourceWrite::Refused);
    };
    values.push(publication.response_json.clone().into());
    let ready = format!("({PUBLISHED}) AND s.response_json=$20");
    let accepted = source_current(ATTACHED, &format!("({BLOCKED}) OR ({ready})"));
    let owner = &attachment.owner;
    let statements=vec![
        source_assert(accepted,values.clone()),
        ("UPDATE media_sessions SET publication_ready_at_ms=0,response_json=$1,updated_at_ms=$2 WHERE incarnation_id=$3 AND publication_ready_at_ms=9223372036854775807".into(),vec![publication.response_json.clone().into(),owner.now_ms.into(),owner.incarnation_id.into()]),
        ("UPDATE media_session_requests SET state='resolved',response_json=$1,updated_at_ms=$2 WHERE incarnation_id=$3 AND user_id=$4 AND request_id=$5 AND state='starting'".into(),vec![publication.response_json.clone().into(),owner.now_ms.into(),owner.incarnation_id.into(),authority.intent.user_id.into(),owner.request_id.clone().into()]),
        source_assert(source_current(ATTACHED,&ready),values),
    ];
    source_result(store, statements, &[0, 1, 1, 0]).await
}
async fn renew_source<T: Backend>(
    store: &T,
    authority: &ReceiverSessionWriteAuthority,
    renewal: &ReceiverSourceRenewal,
) -> Result<ReceiverSourceWrite, StoreError> {
    let attachment = &renewal.attachment;
    let Some(mut values) = source_values(authority, attachment)? else {
        return Ok(ReceiverSourceWrite::Refused);
    };
    let actual = now_ms()?;
    if renewal.lease_expires_at_ms < attachment.owner.lease_expires_at_ms
        || renewal.lease_expires_at_ms <= actual
        || renewal.lease_expires_at_ms > actual.saturating_add(30_000)
    {
        return Ok(ReceiverSourceWrite::Refused);
    }
    values.push(renewal.lease_expires_at_ms.into());
    let state = format!("({BLOCKED}) OR ({PUBLISHED})");
    let accepted=format!("{} AND EXISTS(SELECT 1 FROM job_leases WHERE resource='session:'||$6 AND revision<9223372036854775807) AND $20 >= $13",source_current(ATTACHED,&state).replace("s.lease_expires_at_ms=$13","(s.lease_expires_at_ms=$13 OR s.lease_expires_at_ms=$20)"));
    let owner = &attachment.owner;
    let statements=vec![
        source_assert(accepted,values.clone()),
        ("UPDATE media_session_requests SET claim_expires_at_ms=$1,updated_at_ms=$2 WHERE incarnation_id=$3 AND user_id=$4 AND request_id=$5 AND state='starting' AND claim_expires_at_ms<$1".into(),vec![renewal.lease_expires_at_ms.into(),owner.now_ms.into(),owner.incarnation_id.into(),authority.intent.user_id.into(),owner.request_id.clone().into()]),
        ("UPDATE job_leases SET expires_at_ms=$1,revision=revision+1,updated_at_ms=$2 WHERE resource='session:'||$3 AND expires_at_ms<$1".into(),vec![renewal.lease_expires_at_ms.into(),owner.now_ms.into(),owner.incarnation_id.into()]),
        ("UPDATE media_sessions SET lease_expires_at_ms=$1,updated_at_ms=$2 WHERE incarnation_id=$3 AND lease_expires_at_ms<$1".into(),vec![renewal.lease_expires_at_ms.into(),owner.now_ms.into(),owner.incarnation_id.into()]),
        source_assert(format!("{} AND $20 >= $13",source_current(ATTACHED,&state).replace("s.lease_expires_at_ms=$13","s.lease_expires_at_ms=$20")),values),
    ];
    match store.sharing_txn(statements).await {
        Ok(counts) if counts == [0, 1, 1, 1, 0] || counts == [0, 0, 1, 1, 0] => {
            Ok(ReceiverSourceWrite::Applied)
        }
        Ok(counts) if counts == [0, 0, 0, 0, 0] => Ok(ReceiverSourceWrite::Replay),
        Ok(_) => Err(invalid()),
        Err(error) if source_write_refused(&error) => Ok(ReceiverSourceWrite::Refused),
        Err(error) => Err(error),
    }
}

fn values(authority: &ReceiverSessionWriteAuthority) -> Result<Vec<Value>, StoreError> {
    let intent = &authority.intent;
    Ok(vec![
        intent.login_hash.clone().into(),
        intent.user_id.into(),
        serde_json::to_string(&intent.scope)
            .map_err(|_| invalid())?
            .into(),
        authority.policy_json.clone().into(),
        authority.last_seen.into(),
    ])
}
fn authority_predicate() -> String {
    // JSON equality uses both directions: textual object member order is not
    // security identity. No policy change may reuse an older expiry decision.
    format!("EXISTS(SELECT 1 FROM tokens t JOIN users u ON u.id=t.user_id WHERE t.token_hash=$1 AND u.id=$2 AND t.last_seen_at>=$5 AND {SCOPE} AND {SWITCH} AND NOT EXISTS(SELECT key,value FROM json_each({}) EXCEPT SELECT key,value FROM json_each($4)) AND NOT EXISTS(SELECT key,value FROM json_each($4) EXCEPT SELECT key,value FROM json_each({})))", policy_json_sql(), policy_json_sql())
}

pub(crate) fn receiver_activation_guard(
    authority: &ReceiverSessionWriteAuthority,
    activation: &MediaSessionActivation,
) -> Result<Option<Statement>, StoreError> {
    let now = now_ms()?;
    if activation.principal
        != (PlaybackPrincipal::LocalUser {
            user_id: authority.intent.user_id,
        })
        || activation.publication_ready_at_ms != MEDIA_SESSION_PUBLICATION_BLOCKED
        || activation.recipe_json
            != serde_json::to_string(&authority.intent.recipe).map_err(|_| invalid())?
        || activation.now_ms > now
        || now.saturating_sub(activation.now_ms) > 5000
        || now.saturating_sub(authority.observed_at_ms) > 5000
        || authority
            .login_expires_at_s
            .is_some_and(|deadline| now / 1000 >= deadline)
        || activation.media_origin_ms != authority.intent.source_position_ms
        || activation.incarnation_id != authority.intent.recipe.source_request_id.to_string()
        || activation.request_fingerprint != authority.intent.recipe.request_fingerprint()?
        || activation.request_id.is_none()
        || !activation.fence_predecessor
    {
        return Ok(None);
    }
    // Named NOT NULL violation aborts the *whole* activation on a lost proof.
    let lineage = "NOT EXISTS(SELECT 1 FROM media_sessions s WHERE s.incarnation_id=$6 AND NOT (s.user_id=$2 AND s.recipe_json=$7 AND s.session_id=$8 AND s.state='active' AND s.publication_ready_at_ms=9223372036854775807 AND EXISTS(SELECT 1 FROM sharing_relay_upstream b WHERE b.incarnation_id=s.incarnation_id AND b.import_id=json_extract($3,'$.import_id') AND b.lifecycle_generation=json_extract($3,'$.lifecycle_generation') AND b.assignment_generation=json_extract($3,'$.assignment_generation') AND b.endpoint_revision=json_extract($3,'$.endpoint_generation') AND b.remote_library_id=json_extract($7,'$.reference.library_id') AND b.remote_item_id=json_extract($7,'$.reference.item_id') AND b.remote_file_id=json_extract($7,'$.file_id') AND b.remote_revision=json_extract($7,'$.file_revision') AND b.source_request_id=json_extract($7,'$.source_request_id') AND b.source_position_ms=$9 AND b.source_session_id IS NULL AND b.source_incarnation_id IS NULL AND b.capability_envelope IS NULL)))";
    let sql = format!("INSERT INTO sharing_relay_upstream(incarnation_id,import_id,lifecycle_generation,assignment_generation,remote_library_id,remote_item_id,remote_file_id,remote_revision,source_request_id,endpoint_revision,source_position_ms) SELECT json_extract('receiver_source_authority_refused','$'),'',1,1,'0','0','0','', '',1,0 WHERE NOT ({} AND {lineage})", authority_predicate());
    let mut values = values(authority)?;
    values.extend([
        activation.incarnation_id.clone().into(),
        activation.recipe_json.clone().into(),
        activation.session_id.clone().into(),
        authority.intent.source_position_ms.into(),
    ]);
    Ok(Some((sql, values)))
}
pub(crate) fn receiver_write_refused(error: &StoreError) -> bool {
    error
        .to_string()
        .contains("NOT NULL constraint failed: sharing_relay_upstream.incarnation_id")
        || error.to_string().contains("malformed JSON")
}

/// Final adjunct statements belong to the same activation transaction. Exact
/// replay is read-only; mismatched or previously Source-bound rows refuse.
pub(crate) fn receiver_activation_binding(
    authority: &ReceiverSessionWriteAuthority,
    activation: &MediaSessionActivation,
) -> Result<Vec<Statement>, StoreError> {
    let intent = &authority.intent;
    let recipe = &intent.recipe;
    let values: Vec<Value> = vec![
        activation.incarnation_id.clone().into(),
        recipe.reference.import_id.into(),
        recipe.lifecycle_generation.into(),
        intent.scope.assignment_generation.into(),
        recipe.reference.library_id.as_str().to_owned().into(),
        recipe.reference.item_id.as_str().to_owned().into(),
        recipe.file_id.as_str().to_owned().into(),
        recipe.file_revision.as_str().to_owned().into(),
        recipe.source_request_id.into(),
        intent.scope.endpoint_generation.into(),
        intent.source_position_ms.into(),
        intent.user_id.into(),
        activation.recipe_json.clone().into(),
        activation.session_id.clone().into(),
    ];
    let exact = "b.incarnation_id=$1 AND b.import_id=$2 AND b.lifecycle_generation=$3 AND b.assignment_generation=$4 AND b.remote_library_id=$5 AND b.remote_item_id=$6 AND b.remote_file_id=$7 AND b.remote_revision=$8 AND b.source_request_id=$9 AND b.endpoint_revision=$10 AND b.source_position_ms=$11 AND b.source_session_id IS NULL AND b.source_incarnation_id IS NULL AND b.capability_envelope IS NULL";
    let route = "EXISTS(SELECT 1 FROM media_sessions s WHERE s.incarnation_id=$1 AND s.user_id=$12 AND s.recipe_json=$13 AND s.session_id=$14 AND s.state='active' AND s.publication_ready_at_ms=9223372036854775807 AND EXISTS(SELECT 1 FROM media_playback_pointers p WHERE p.user_id=$12 AND p.current_incarnation_id=s.incarnation_id))";
    Ok(vec![
        (format!("INSERT INTO sharing_relay_upstream(incarnation_id,import_id,lifecycle_generation,assignment_generation,remote_library_id,remote_item_id,remote_file_id,remote_revision,source_request_id,endpoint_revision,source_position_ms) SELECT $1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11 WHERE {route} ON CONFLICT(incarnation_id) DO NOTHING"), values.clone()),
        (format!("INSERT INTO sharing_relay_upstream(incarnation_id,import_id,lifecycle_generation,assignment_generation,remote_library_id,remote_item_id,remote_file_id,remote_revision,source_request_id,endpoint_revision,source_position_ms) SELECT json_extract('receiver_source_authority_refused','$'),$2,$3,$4,$5,$6,$7,$8,$9,$10,$11 WHERE NOT EXISTS(SELECT 1 FROM sharing_relay_upstream b WHERE {exact} AND {route})"), values),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::MediaSessionRequestClaim,
        sharing::SourceId,
        sharing_catalogue::SharedReference,
        sharing_catalogue_details::FileRevision,
        sharing_receiver_sessions::{ReceiverProducerKind, RemoteSourceRecipe},
        store::{
            sharing_catalogue::ReceiverCatalogueScope, MediaSessionStore, SqliteStore, UserStore,
        },
    };
    use uuid::Uuid;

    #[tokio::test]
    async fn sharing_receiver_activation_atomically_binds_full_remote_recipe_and_current_login_scope(
    ) {
        let directory = tempfile::tempdir().expect("pooled receiver");
        for rebuilt in [false, true] {
            for pooled in [false, true] {
                for refuse in [
                    "none",
                    "assignment",
                    "login",
                    "switch",
                    "policy",
                    "epoch",
                    "recipe",
                    "position",
                    "fingerprint",
                    "incarnation",
                ] {
                    let path = directory
                        .path()
                        .join(format!("{rebuilt}-{pooled}-{refuse}.sqlite"));
                    let store = if pooled {
                        SqliteStore::open(&path).expect("pooled")
                    } else {
                        SqliteStore::open_in_memory().expect("memory")
                    };
                    if rebuilt {
                        let statements = super::super::MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
                            .split("-- next statement\n")
                            .map(|sql| (sql.trim().trim_end_matches(';').to_owned(), vec![]))
                            .collect();
                        store
                            .sharing_txn(statements)
                            .await
                            .expect("frozen principal fixture");
                    }
                    let user = store
                        .create_user("receiver", "fixture-hash", false)
                        .await
                        .expect("B user");
                    let hash = "c".repeat(64);
                    store
                        .create_token(&hash, user.id, None)
                        .await
                        .expect("B login");
                    let scope = ReceiverCatalogueScope {
                        import_id: Uuid::new_v4(),
                        source_server_id: Uuid::new_v4(),
                        catalogue_epoch: Uuid::new_v4(),
                        lifecycle_generation: 1,
                        assignment_generation: 1,
                        endpoint_generation: 1,
                        claim_id: Uuid::new_v4(),
                        remote_grant_id: Uuid::new_v4(),
                        libraries: vec![SourceId::parse("0").expect("zero Source library")],
                    };
                    store.sharing_txn(vec![
                        ("INSERT INTO settings(key,value) VALUES('sharing_enabled','true')".into(), vec![]),
                        ("INSERT INTO sharing_viewers(user_id,viewer_id) VALUES($1,$2)".into(),vec![user.id.into(),Uuid::new_v4().into()]),
                        ("INSERT INTO sharing_imports(id,source_server_id,catalogue_epoch,source_name,claim_id,remote_grant_id,credential_envelope,endpoints_json,assignment_generation,lifecycle_generation,endpoint_generation,state,created_at_ms,updated_at_ms) VALUES($1,$2,$3,'Source',$4,$5,'fixture never opened','[]',1,1,1,'active',1000,1000)".into(),vec![scope.import_id.into(),scope.source_server_id.into(),scope.catalogue_epoch.into(),scope.claim_id.into(),scope.remote_grant_id.into()]),
                        ("INSERT INTO sharing_assignments VALUES($1,'0',$2,1)".into(),vec![scope.import_id.into(),user.id.into()]),
                    ]).await.expect("configured B fixture");
                    let recipe = RemoteSourceRecipe {
                        kind: ReceiverProducerKind::RemoteSource,
                        version: 1,
                        reference: SharedReference {
                            import_id: scope.import_id,
                            server_id: scope.source_server_id,
                            catalogue_epoch: scope.catalogue_epoch,
                            library_id: scope.libraries[0].clone(),
                            item_id: SourceId::parse("9223372036854775807").expect("lossless item"),
                        },
                        lifecycle_generation: 1,
                        file_id: SourceId::parse("0").expect("zero file"),
                        file_revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
                        source_request_id: Uuid::new_v4(),
                        parent_login_hash: hash.clone(),
                        request_json: "{\"caps_v2\":{}}".into(),
                    };
                    let intent = ReceiverSessionIntent {
                        scope: scope.clone(),
                        user_id: user.id,
                        login_hash: hash.clone(),
                        recipe: recipe.clone(),
                        source_position_ms: 1234,
                    };
                    let other_hash = "d".repeat(64);
                    store
                        .create_token(&other_hash, user.id, None)
                        .await
                        .expect("second valid login");
                    let mut other_login = intent.clone();
                    other_login.login_hash = other_hash;
                    assert!(
                        store
                            .prepare_receiver_session_authority(other_login)
                            .await
                            .is_err(),
                        "same user cannot adopt a different parent login"
                    );
                    let authority = store
                        .prepare_receiver_session_authority(intent.clone())
                        .await
                        .expect("snapshot")
                        .expect("authorized");
                    let now = now_ms().expect("clock");
                    let mut activation = MediaSessionActivation {
                        incarnation_id: recipe.source_request_id.to_string(),
                        session_id: Uuid::new_v4().to_string(),
                        principal: PlaybackPrincipal::LocalUser { user_id: user.id },
                        playback_id: "B-playback".into(),
                        recovery_epoch: Uuid::new_v4().to_string(),
                        expected_predecessor_incarnation_id: None,
                        fence_predecessor: true,
                        request_id: Some("B-request".into()),
                        request_fingerprint: recipe
                            .request_fingerprint()
                            .expect("full remote request fingerprint"),
                        owner_node_id: "B-node".into(),
                        recipe_json: serde_json::to_string(&recipe).expect("recipe"),
                        response_json: "{}".into(),
                        publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
                        media_origin_ms: 1234,
                        now_ms: now,
                        lease_expires_at_ms: now + 30_000,
                        expected_desired_revision: None,
                    };
                    assert!(matches!(
                        store
                            .claim_media_session_request(
                                &activation.principal,
                                "B-request",
                                &activation.request_fingerprint,
                                &activation.playback_id,
                                &activation.incarnation_id,
                                now,
                                now + 30_000
                            )
                            .await
                            .expect("claim"),
                        MediaSessionRequestClaim::Acquired { .. }
                    ));
                    assert!(store
                        .assign_media_session_request_owner(
                            &activation.principal,
                            "B-request",
                            &activation.incarnation_id,
                            "B-node",
                            now
                        )
                        .await
                        .expect("owner"));
                    // Ordinary Local activation must not bypass the B guard.
                    assert!(store.activate_media_session(&activation).await.is_err());
                    let mutation = match refuse {
                        "assignment" => Some("DELETE FROM sharing_assignments"),
                        "login" => Some("DELETE FROM tokens"),
                        "switch" => {
                            Some("UPDATE settings SET value='false' WHERE key='sharing_enabled'")
                        }
                        "policy" => Some(
                            "INSERT INTO settings(key,value) VALUES('auth.token_idle_days','1')",
                        ),
                        "epoch" => {
                            Some("UPDATE sharing_imports SET catalogue_epoch='different-epoch'")
                        }
                        "recipe" => {
                            activation.recipe_json.push(' ');
                            None
                        }
                        "position" => {
                            activation.media_origin_ms += 1;
                            None
                        }
                        "fingerprint" => {
                            activation.request_fingerprint = "d".repeat(64);
                            None
                        }
                        "incarnation" => {
                            activation.incarnation_id = Uuid::new_v4().to_string();
                            None
                        }
                        _ => None,
                    };
                    if let Some(sql) = mutation {
                        store
                            .sharing_txn(vec![(sql.into(), vec![])])
                            .await
                            .expect("race");
                    }
                    if refuse != "none" {
                        store.sharing_txn(vec![("CREATE TRIGGER receiver_ignored_old_assertion BEFORE INSERT ON sharing_relay_upstream BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("ignored old assertion fixture");
                    }
                    let outcome = store
                        .activate_receiver_media_session(&authority, &activation)
                        .await
                        .expect("guarded activation");
                    if refuse != "none" {
                        store
                            .sharing_txn(vec![(
                                "DROP TRIGGER receiver_ignored_old_assertion".into(),
                                vec![],
                            )])
                            .await
                            .expect("remove old assertion fixture");
                    }
                    let succeeded = refuse == "none";
                    assert_eq!(outcome.is_some(), succeeded, "{rebuilt}/{pooled}/{refuse}");
                    let rows = store.sharing_read("SELECT json_array((SELECT count(*) FROM media_sessions),(SELECT count(*) FROM media_playback_pointers),(SELECT count(*) FROM job_leases),(SELECT count(*) FROM sharing_relay_upstream),(SELECT count(*) FROM files)) AS payload", vec![]).await.expect("atomic census");
                    assert_eq!(
                        rows,
                        vec![if succeeded {
                            "[1,1,1,1,0]"
                        } else {
                            "[0,0,0,0,0]"
                        }
                        .to_owned()]
                    );
                    if succeeded {
                        assert_eq!(
                            outcome.expect("B route").route.principal,
                            activation.principal
                        );
                        assert!(store
                            .activate_receiver_media_session(&authority, &activation)
                            .await
                            .expect("exact replay")
                            .is_some());
                        let route = store
                            .media_session_route_by_incarnation(&activation.incarnation_id)
                            .await
                            .expect("route")
                            .expect("blocked owner");
                        assert!(store.complete_media_session_handoff(
                            &activation.incarnation_id, &activation.owner_node_id, route.owner_epoch,
                            crate::domain::MediaSessionProjectionCompletion::PredecessorAcknowledged, now_ms().expect("clock")
                        ).await.expect("ordinary handoff refused").is_none());
                        assert!(store
                            .arm_media_session_handoff(
                                &activation.incarnation_id,
                                &activation.owner_node_id,
                                route.owner_epoch,
                                now + crate::domain::MEDIA_SESSION_HANDOFF_SAFETY_WINDOW_MS + 5000,
                                now_ms().expect("clock")
                            )
                            .await
                            .expect("ordinary arm refused")
                            .is_none());
                        let renewal_now = now_ms().expect("current renewal clock");
                        let renewal = crate::sharing_receiver_sessions::ReceiverPendingRenewal {
                            incarnation_id: activation.incarnation_id.clone(),
                            owner_node_id: activation.owner_node_id.clone(),
                            owner_epoch: route.owner_epoch,
                            request_id: "B-request".into(),
                            now_ms: renewal_now,
                            lease_expires_at_ms: renewal_now + 30_000,
                        };
                        let fresh = store
                            .prepare_receiver_session_authority(intent.clone())
                            .await
                            .expect("fresh login")
                            .expect("original login");
                        assert!(store
                            .renew_pending_receiver_session(&fresh, &renewal)
                            .await
                            .expect("pending renewal"));
                        let renewed = store
                            .media_session_route_by_incarnation(&activation.incarnation_id)
                            .await
                            .expect("renewed route")
                            .expect("route retained");
                        assert_eq!(renewed.lease_expires_at_ms, renewal.lease_expires_at_ms);
                        assert_eq!(
                            renewed.publication_ready_at_ms,
                            MEDIA_SESSION_PUBLICATION_BLOCKED
                        );
                        for stale in 0..4 {
                            let mut rejected = renewal.clone();
                            match stale {
                                0 => rejected.owner_epoch += 1,
                                1 => rejected.incarnation_id = Uuid::new_v4().to_string(),
                                2 => rejected.request_id = "other-request".into(),
                                _ => rejected.owner_node_id = "other-node".into(),
                            }
                            assert!(!store
                                .renew_pending_receiver_session(&fresh, &rejected)
                                .await
                                .expect("stale renewal refusal"));
                            assert_eq!(
                                store
                                    .media_session_route_by_incarnation(&activation.incarnation_id)
                                    .await
                                    .expect("unchanged")
                                    .expect("retained")
                                    .lease_expires_at_ms,
                                renewed.lease_expires_at_ms
                            );
                        }
                        for (change, restore) in [
                            ("UPDATE sharing_assignments SET enabled=0", "UPDATE sharing_assignments SET enabled=1"),
                            ("UPDATE settings SET value='false' WHERE key='sharing_enabled'", "UPDATE settings SET value='true' WHERE key='sharing_enabled'"),
                            ("INSERT INTO settings(key,value) VALUES('auth.token_idle_days','1')", "DELETE FROM settings WHERE key='auth.token_idle_days'"),
                            ("UPDATE sharing_relay_upstream SET source_session_id='unverified-source-session'", "UPDATE sharing_relay_upstream SET source_session_id=NULL"),
                            ("UPDATE job_leases SET fence=fence+1", "UPDATE job_leases SET fence=fence-1"),
                            ("UPDATE media_session_requests SET state='failed'", "UPDATE media_session_requests SET state='starting'"),
                        ] {
                            store.sharing_txn(vec![(change.into(),vec![])]).await.expect("renewal race");
                            store.sharing_txn(vec![("CREATE TRIGGER receiver_ignored_pending_assertion BEFORE INSERT ON sharing_relay_upstream BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("ignored pending assertion fixture");

                            let census = "SELECT json_array((SELECT group_concat(claim_expires_at_ms) FROM media_session_requests),(SELECT group_concat(expires_at_ms||':'||revision) FROM job_leases),(SELECT group_concat(lease_expires_at_ms||':'||publication_ready_at_ms) FROM media_sessions)) AS payload";
                            let before = store.sharing_read(census,vec![]).await.expect("before refused write");
                            assert!(!store.renew_pending_receiver_session(&fresh,&renewal).await.expect("current proof race refuses"),"{change}");
                            assert_eq!(store.sharing_read(census,vec![]).await.expect("after refused write"),before,"atomic refusal {change}");
                            store.sharing_txn(vec![("DROP TRIGGER receiver_ignored_pending_assertion".into(),vec![])]).await.expect("remove pending assertion fixture");
                            store.sharing_txn(vec![(restore.into(),vec![])]).await.expect("restore fixture");
                        }
                        let later = now + 7 * 24 * 60 * 60 * 1000;
                        store
                            .maintain_media_sessions(later)
                            .await
                            .expect("expired unresolved upstream retained");
                        assert!(store
                            .media_session_route_by_incarnation(&activation.incarnation_id)
                            .await
                            .expect("retained route")
                            .is_some());
                        assert!(!matches!(
                            store
                                .claim_media_session_request(
                                    &activation.principal,
                                    "B-request",
                                    &activation.request_fingerprint,
                                    &activation.playback_id,
                                    &Uuid::new_v4().to_string(),
                                    later,
                                    later + 30_000
                                )
                                .await
                                .expect("no incarnation replacement"),
                            MediaSessionRequestClaim::Acquired { .. }
                        ));
                        // A missing adjunct after a prior activation is corruption,
                        // never a read-path opportunity to recreate authority.
                        store
                            .sharing_txn(vec![(
                                "DELETE FROM sharing_relay_upstream".into(),
                                vec![],
                            )])
                            .await
                            .expect("corrupt binding fixture");
                        assert!(store
                            .activate_receiver_media_session(&authority, &activation)
                            .await
                            .expect("missing binding refusal")
                            .is_none());
                        assert_eq!(store.sharing_read("SELECT json_array(count(*)) AS payload FROM sharing_relay_upstream",vec![]).await.expect("no repair"), vec!["[0]".to_owned()]);
                    }
                }
            }
        }
    }
    #[tokio::test]
    async fn sharing_receiver_source_binding_publication_and_renewal_are_guarded_and_exact() {
        use crate::secrets::{CredentialKey, SharingSecretPurpose};
        use crate::sharing_receiver_sessions::{ReceiverSourceBinding, ReceiverSourceOwner};
        let directory = tempfile::tempdir().expect("receiver Source binding fixture");
        for rebuilt in [false, true] {
            for pooled in [false, true] {
                let store = if pooled {
                    SqliteStore::open(
                        &directory
                            .path()
                            .join(format!("bound-{rebuilt}-{pooled}.sqlite")),
                    )
                    .expect("pooled")
                } else {
                    SqliteStore::open_in_memory().expect("memory")
                };
                if rebuilt {
                    store
                        .sharing_txn(
                            super::super::MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
                                .split("-- next statement\n")
                                .map(|sql| (sql.trim().trim_end_matches(';').to_owned(), vec![]))
                                .collect(),
                        )
                        .await
                        .expect("principal fixture");
                }
                let user = store
                    .create_user("receiver", "fixture-hash", false)
                    .await
                    .expect("B user");
                let hash = "c".repeat(64);
                store
                    .create_token(&hash, user.id, None)
                    .await
                    .expect("B login");
                let scope = ReceiverCatalogueScope {
                    import_id: Uuid::new_v4(),
                    source_server_id: Uuid::new_v4(),
                    catalogue_epoch: Uuid::new_v4(),
                    lifecycle_generation: 1,
                    assignment_generation: 1,
                    endpoint_generation: 1,
                    claim_id: Uuid::new_v4(),
                    remote_grant_id: Uuid::new_v4(),
                    libraries: vec![SourceId::parse("0").expect("zero Source library")],
                };
                store.sharing_txn(vec![
                        ("INSERT INTO settings(key,value) VALUES('sharing_enabled','true')".into(), vec![]),
                        ("INSERT INTO sharing_viewers(user_id,viewer_id) VALUES($1,$2)".into(),vec![user.id.into(),Uuid::new_v4().into()]),
                        ("INSERT INTO sharing_imports(id,source_server_id,catalogue_epoch,source_name,claim_id,remote_grant_id,credential_envelope,endpoints_json,assignment_generation,lifecycle_generation,endpoint_generation,state,created_at_ms,updated_at_ms) VALUES($1,$2,$3,'Source',$4,$5,'fixture never opened','[]',1,1,1,'active',1000,1000)".into(),vec![scope.import_id.into(),scope.source_server_id.into(),scope.catalogue_epoch.into(),scope.claim_id.into(),scope.remote_grant_id.into()]),
                        ("INSERT INTO sharing_assignments VALUES($1,'0',$2,1)".into(),vec![scope.import_id.into(),user.id.into()]),
                    ]).await.expect("configured B fixture");
                let recipe = RemoteSourceRecipe {
                    kind: ReceiverProducerKind::RemoteSource,
                    version: 1,
                    reference: SharedReference {
                        import_id: scope.import_id,
                        server_id: scope.source_server_id,
                        catalogue_epoch: scope.catalogue_epoch,
                        library_id: scope.libraries[0].clone(),
                        item_id: SourceId::parse("9223372036854775807").expect("lossless item"),
                    },
                    lifecycle_generation: 1,
                    file_id: SourceId::parse("0").expect("zero file"),
                    file_revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
                    source_request_id: Uuid::new_v4(),
                    parent_login_hash: hash.clone(),
                    request_json: "{\"caps_v2\":{}}".into(),
                };
                let intent = ReceiverSessionIntent {
                    scope: scope.clone(),
                    user_id: user.id,
                    login_hash: hash.clone(),
                    recipe: recipe.clone(),
                    source_position_ms: 1234,
                };
                let other_hash = "d".repeat(64);
                store
                    .create_token(&other_hash, user.id, None)
                    .await
                    .expect("second valid login");
                let mut other_login = intent.clone();
                other_login.login_hash = other_hash;
                assert!(
                    store
                        .prepare_receiver_session_authority(other_login)
                        .await
                        .is_err(),
                    "same user cannot adopt a different parent login"
                );
                let authority = store
                    .prepare_receiver_session_authority(intent.clone())
                    .await
                    .expect("snapshot")
                    .expect("authorized");
                let now = now_ms().expect("clock");
                let activation = MediaSessionActivation {
                    incarnation_id: recipe.source_request_id.to_string(),
                    session_id: Uuid::new_v4().to_string(),
                    principal: PlaybackPrincipal::LocalUser { user_id: user.id },
                    playback_id: "B-playback".into(),
                    recovery_epoch: Uuid::new_v4().to_string(),
                    expected_predecessor_incarnation_id: None,
                    fence_predecessor: true,
                    request_id: Some("B-request".into()),
                    request_fingerprint: recipe
                        .request_fingerprint()
                        .expect("full remote request fingerprint"),
                    owner_node_id: "B-node".into(),
                    recipe_json: serde_json::to_string(&recipe).expect("recipe"),
                    response_json: "{}".into(),
                    publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
                    media_origin_ms: 1234,
                    now_ms: now,
                    lease_expires_at_ms: now + 30_000,
                    expected_desired_revision: None,
                };
                assert!(matches!(
                    store
                        .claim_media_session_request(
                            &activation.principal,
                            "B-request",
                            &activation.request_fingerprint,
                            &activation.playback_id,
                            &activation.incarnation_id,
                            now,
                            now + 30_000
                        )
                        .await
                        .expect("claim"),
                    MediaSessionRequestClaim::Acquired { .. }
                ));
                assert!(store
                    .assign_media_session_request_owner(
                        &activation.principal,
                        "B-request",
                        &activation.incarnation_id,
                        "B-node",
                        now
                    )
                    .await
                    .expect("owner"));
                // Ordinary Local activation must not bypass the B guard.
                assert!(store.activate_media_session(&activation).await.is_err());

                let outcome = store
                    .activate_receiver_media_session(&authority, &activation)
                    .await
                    .expect("B activation")
                    .expect("blocked route");
                let envelope = CredentialKey::from_bytes([31; 32])
                    .seal_sharing(
                        SharingSecretPurpose::Upstream,
                        Uuid::new_v4(),
                        scope.import_id,
                        "actual fixture upstream capability",
                    )
                    .expect("sealed upstream");
                let mut attachment = ReceiverSourceAttachment {
                    owner: ReceiverSourceOwner {
                        incarnation_id: recipe.source_request_id,
                        session_id: Uuid::parse_str(&activation.session_id)
                            .expect("B session UUID"),
                        owner_node_id: activation.owner_node_id.clone(),
                        owner_epoch: outcome.route.owner_epoch,
                        request_id: "B-request".into(),
                        lease_expires_at_ms: activation.lease_expires_at_ms,
                        now_ms: now_ms().expect("clock"),
                    },
                    binding: ReceiverSourceBinding {
                        reference: recipe.reference.clone(),
                        file_id: recipe.file_id.clone(),
                        file_revision: recipe.file_revision.clone(),
                        source_request_id: recipe.source_request_id,
                        source_session_id: Uuid::new_v4(),
                        source_incarnation_id: Uuid::new_v4(),
                        capability_envelope: envelope,
                    },
                };
                let census="SELECT json_array((SELECT json_group_array(json_array(session_id,incarnation_id,owner_node_id,owner_epoch,lease_expires_at_ms,publication_ready_at_ms,response_json,updated_at_ms)) FROM media_sessions),(SELECT json_group_array(json_array(state,response_json,claim_expires_at_ms,updated_at_ms)) FROM media_session_requests),(SELECT json_group_array(json_array(owner_node_id,fence,revision,expires_at_ms,updated_at_ms)) FROM job_leases),(SELECT json_group_array(json_array(source_session_id,source_incarnation_id,capability_envelope)) FROM sharing_relay_upstream)) AS payload";
                for stale in 0..7 {
                    let mut wrong = attachment.clone();
                    match stale {
                        0 => wrong.owner.session_id = Uuid::new_v4(),
                        1 => wrong.owner.owner_epoch += 1,
                        2 => wrong.owner.owner_node_id = "foreign-owner".into(),
                        3 => wrong.owner.request_id = "foreign-request".into(),
                        4 => wrong.binding.reference.catalogue_epoch = Uuid::new_v4(),
                        5 => wrong.owner.lease_expires_at_ms += 1,
                        _ => wrong.binding.source_request_id = Uuid::new_v4(),
                    };
                    let before = store
                        .sharing_read(census, vec![])
                        .await
                        .expect("before stale binding");
                    assert_eq!(
                        store
                            .attach_receiver_source(&authority, &wrong)
                            .await
                            .expect("stale refusal"),
                        ReceiverSourceWrite::Refused
                    );
                    assert_eq!(
                        store
                            .sharing_read(census, vec![])
                            .await
                            .expect("after stale binding"),
                        before
                    );
                }
                for bad in ["raw bearer".to_owned(), "plurx-sealed:broken".to_owned()] {
                    let mut wrong = attachment.clone();
                    wrong.binding.capability_envelope =
                        crate::secrets::SealedSecret::from_stored(bad);
                    assert!(store
                        .attach_receiver_source(&authority, &wrong)
                        .await
                        .is_err());
                }
                let mut oversized = attachment.clone();
                oversized.binding.capability_envelope = CredentialKey::from_bytes([31; 32])
                    .seal_sharing(
                        SharingSecretPurpose::Upstream,
                        Uuid::new_v4(),
                        scope.import_id,
                        &"x".repeat(4096),
                    )
                    .expect("oversized envelope fixture");
                assert!(store
                    .attach_receiver_source(&authority, &oversized)
                    .await
                    .is_err());
                let mut expired = attachment.clone();
                expired.owner.lease_expires_at_ms = now_ms().expect("clock") - 1;
                assert_eq!(
                    store
                        .attach_receiver_source(&authority, &expired)
                        .await
                        .expect("no expired owner resurrection"),
                    ReceiverSourceWrite::Refused
                );
                let mut stale = authority.clone();
                stale.observed_at_ms -= 6000;
                assert_eq!(
                    store
                        .attach_receiver_source(&stale, &attachment)
                        .await
                        .expect("fresh authority required"),
                    ReceiverSourceWrite::Refused
                );
                for (change, restore) in [
                    ("UPDATE tokens SET token_hash='other-login' WHERE token_hash='cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc'","UPDATE tokens SET token_hash='cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc' WHERE token_hash='other-login'"),
                    ("INSERT INTO settings(key,value) VALUES('auth.token_idle_days','1')","DELETE FROM settings WHERE key='auth.token_idle_days'"),

                    (
                        "UPDATE sharing_assignments SET enabled=0",
                        "UPDATE sharing_assignments SET enabled=1",
                    ),
                    (
                        "UPDATE sharing_imports SET lifecycle_generation=2",
                        "UPDATE sharing_imports SET lifecycle_generation=1",
                    ),
                    (
                        "UPDATE media_playback_pointers SET playback_id='foreign-playback'",
                        "UPDATE media_playback_pointers SET playback_id='B-playback'",
                    ),
                    (
                        "UPDATE job_leases SET fence=fence+1",
                        "UPDATE job_leases SET fence=fence-1",
                    ),
                    (
                        "UPDATE media_session_requests SET state='failed'",
                        "UPDATE media_session_requests SET state='starting'",
                    ),
                    (
                        "UPDATE sharing_relay_upstream SET source_session_id='partial'",
                        "UPDATE sharing_relay_upstream SET source_session_id=NULL",
                    ),
                ] {
                    store
                        .sharing_txn(vec![(change.into(), vec![])])
                        .await
                        .expect("binding race");
                    let before = store
                        .sharing_read(census, vec![])
                        .await
                        .expect("before race");
                    assert_eq!(
                        store
                            .attach_receiver_source(&authority, &attachment)
                            .await
                            .expect("race refusal"),
                        ReceiverSourceWrite::Refused,
                        "{change}"
                    );
                    assert_eq!(
                        store
                            .sharing_read(census, vec![])
                            .await
                            .expect("after race"),
                        before
                    );
                    store
                        .sharing_txn(vec![(restore.into(), vec![])])
                        .await
                        .expect("restore fixture");
                }
                store.sharing_txn(vec![("CREATE TRIGGER receiver_ignored_assertion BEFORE INSERT ON sharing_relay_upstream BEGIN SELECT RAISE(IGNORE); END".into(),vec![]),("UPDATE sharing_assignments SET enabled=0".into(),vec![])]).await.expect("ignored assertion fixture");
                let before = store
                    .sharing_read(census, vec![])
                    .await
                    .expect("before ignored assertion");
                assert_eq!(
                    store
                        .attach_receiver_source(&authority, &attachment)
                        .await
                        .expect("assertion evaluated before trigger"),
                    ReceiverSourceWrite::Refused
                );
                assert_eq!(
                    store
                        .sharing_read(census, vec![])
                        .await
                        .expect("refused assertion leaves all state unchanged"),
                    before
                );
                store
                    .sharing_txn(vec![
                        ("DROP TRIGGER receiver_ignored_assertion".into(), vec![]),
                        ("UPDATE sharing_assignments SET enabled=1".into(), vec![]),
                    ])
                    .await
                    .expect("restore fixture");
                store.sharing_txn(vec![("CREATE TRIGGER receiver_ignored_attachment BEFORE UPDATE ON sharing_relay_upstream BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("ignored writer fixture");
                let before = store
                    .sharing_read(census, vec![])
                    .await
                    .expect("before ignored attachment");
                assert_eq!(
                    store
                        .attach_receiver_source(&authority, &attachment)
                        .await
                        .expect("ignored write refuses"),
                    ReceiverSourceWrite::Refused
                );
                assert_eq!(
                    store.sharing_read(census, vec![]).await.expect("rollback"),
                    before
                );
                store
                    .sharing_txn(vec![(
                        "DROP TRIGGER receiver_ignored_attachment".into(),
                        vec![],
                    )])
                    .await
                    .expect("remove test trigger");
                assert_eq!(
                    store
                        .attach_receiver_source(&authority, &attachment)
                        .await
                        .expect("attach"),
                    ReceiverSourceWrite::Applied
                );
                let before = store
                    .sharing_read(census, vec![])
                    .await
                    .expect("attached snapshot");
                assert_eq!(
                    store
                        .attach_receiver_source(&authority, &attachment)
                        .await
                        .expect("replay"),
                    ReceiverSourceWrite::Replay
                );
                assert_eq!(
                    store
                        .sharing_read(census, vec![])
                        .await
                        .expect("read-only replay"),
                    before
                );
                let mut replacement = attachment.clone();
                replacement.binding.source_incarnation_id = Uuid::new_v4();
                assert_eq!(
                    store
                        .attach_receiver_source(&authority, &replacement)
                        .await
                        .expect("no rebind"),
                    ReceiverSourceWrite::Refused
                );
                let pending = crate::sharing_receiver_sessions::ReceiverPendingRenewal {
                    incarnation_id: activation.incarnation_id.clone(),
                    owner_node_id: activation.owner_node_id.clone(),
                    owner_epoch: outcome.route.owner_epoch,
                    request_id: "B-request".into(),
                    now_ms: now_ms().expect("clock"),
                    lease_expires_at_ms: now_ms().expect("clock") + 30_000,
                };
                assert!(!store
                    .renew_pending_receiver_session(&authority, &pending)
                    .await
                    .expect("null-only pending refuses attached"));
                let renew = ReceiverSourceRenewal {
                    attachment: attachment.clone(),
                    lease_expires_at_ms: pending.lease_expires_at_ms,
                };
                assert_eq!(
                    store
                        .renew_receiver_source_session(&authority, &renew)
                        .await
                        .expect("blocked bound renewal"),
                    ReceiverSourceWrite::Applied
                );
                assert_eq!(
                    store
                        .renew_receiver_source_session(&authority, &renew)
                        .await
                        .expect("renewal replay"),
                    ReceiverSourceWrite::Replay
                );
                attachment.owner.lease_expires_at_ms = renew.lease_expires_at_ms;
                let response=serde_json::json!({"session_id":attachment.owner.session_id.to_string(),"file_id":"0"}).to_string();
                let publication = ReceiverSourcePublication {
                    attachment: attachment.clone(),
                    response_json: response,
                };
                store.sharing_txn(vec![("CREATE TRIGGER receiver_ignored_publication BEFORE UPDATE ON media_sessions BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("ignored publication fixture");
                let before = store
                    .sharing_read(census, vec![])
                    .await
                    .expect("before ignored publication");
                assert_eq!(
                    store
                        .publish_receiver_source(&authority, &publication)
                        .await
                        .expect("ignored publication refuses"),
                    ReceiverSourceWrite::Refused
                );
                assert_eq!(
                    store
                        .sharing_read(census, vec![])
                        .await
                        .expect("no partial resolved request"),
                    before
                );
                store
                    .sharing_txn(vec![(
                        "DROP TRIGGER receiver_ignored_publication".into(),
                        vec![],
                    )])
                    .await
                    .expect("remove test trigger");
                assert_eq!(
                    store
                        .publish_receiver_source(&authority, &publication)
                        .await
                        .expect("publish"),
                    ReceiverSourceWrite::Applied
                );
                let before = store
                    .sharing_read(census, vec![])
                    .await
                    .expect("published snapshot");
                assert_eq!(
                    store
                        .publish_receiver_source(&authority, &publication)
                        .await
                        .expect("published replay"),
                    ReceiverSourceWrite::Replay
                );
                assert_eq!(
                    store
                        .attach_receiver_source(&authority, &attachment)
                        .await
                        .expect("attachment replay after publication"),
                    ReceiverSourceWrite::Replay
                );
                assert_eq!(
                    store
                        .sharing_read(census, vec![])
                        .await
                        .expect("whole replay unchanged"),
                    before
                );
                assert!(store
                    .publish_media_session_activation(
                        &activation.principal,
                        "B-request",
                        &activation.incarnation_id,
                        now_ms().expect("clock")
                    )
                    .await
                    .expect("generic Local publication refused")
                    .is_none());
                let mut wrong = publication.clone();
                wrong.response_json = "{}".into();
                assert_eq!(
                    store
                        .publish_receiver_source(&authority, &wrong)
                        .await
                        .expect("different response refuses"),
                    ReceiverSourceWrite::Refused
                );
                let request_census="SELECT json_array(state,response_json,claim_expires_at_ms,updated_at_ms) AS payload FROM media_session_requests";
                let request_before = store
                    .sharing_read(request_census, vec![])
                    .await
                    .expect("published request");
                let renewal = ReceiverSourceRenewal {
                    attachment: attachment.clone(),
                    lease_expires_at_ms: now_ms().expect("clock") + 30_000,
                };
                assert_eq!(
                    store
                        .renew_receiver_source_session(&authority, &renewal)
                        .await
                        .expect("published bound renewal"),
                    ReceiverSourceWrite::Applied
                );
                assert_eq!(
                    store
                        .sharing_read(request_census, vec![])
                        .await
                        .expect("resolved request retained"),
                    request_before
                );
            }
        }
    }
}
