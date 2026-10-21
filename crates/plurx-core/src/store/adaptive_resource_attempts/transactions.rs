//! Shared deterministic transaction plans used by both actual session backends.
//! All optimistic snapshots are asserted again inside the write transaction.

use async_trait::async_trait;
use serde::Serialize;
use sha2::{Digest, Sha256};

use super::{
    schema::{self, quote, Layout},
    *,
};

mod adoption;
pub(crate) use adoption::{adopt, commit_adoption, AdoptionPlan};

macro_rules! impl_attempt_store {
    ($store:ty) => {
        #[async_trait::async_trait]
        impl crate::store::adaptive_resource_attempts::AdaptiveResourceAttemptStore for $store {
            async fn adaptive_attempt_schema(
                &self,
            ) -> Result<
                crate::store::adaptive_resource_attempts::AttemptSchemaReadiness,
                crate::error::StoreError,
            > {
                crate::store::adaptive_resource_attempts::transactions::readiness(self).await
            }
            async fn claim_adaptive_attempt(
                &self,
                input: &crate::store::adaptive_resource_attempts::AttemptClaim,
            ) -> Result<
                crate::store::adaptive_resource_attempts::AttemptMutation,
                crate::error::StoreError,
            > {
                crate::store::adaptive_resource_attempts::transactions::claim(self, input).await
            }
            async fn record_adaptive_prepared(
                &self,
                input: &crate::store::adaptive_resource_attempts::PreparedAttempt,
            ) -> Result<
                crate::store::adaptive_resource_attempts::AttemptMutation,
                crate::error::StoreError,
            > {
                crate::store::adaptive_resource_attempts::transactions::prepared(self, input).await
            }
            async fn select_adaptive_winner(
                &self,
                input: &crate::store::adaptive_resource_attempts::WinnerSelection,
            ) -> Result<
                crate::store::adaptive_resource_attempts::AttemptMutation,
                crate::error::StoreError,
            > {
                crate::store::adaptive_resource_attempts::transactions::winner(self, input).await
            }
            async fn adopt_adaptive_winner(
                &self,
                input: &crate::store::adaptive_resource_attempts::AdaptiveAdoption,
            ) -> Result<
                crate::store::adaptive_resource_attempts::AttemptAdoption,
                crate::error::StoreError,
            > {
                crate::store::adaptive_resource_attempts::transactions::adopt(self, input).await
            }
            async fn fence_adaptive_attempt(
                &self,
                input: &crate::store::adaptive_resource_attempts::AttemptFence,
            ) -> Result<
                crate::store::adaptive_resource_attempts::AttemptMutation,
                crate::error::StoreError,
            > {
                crate::store::adaptive_resource_attempts::transactions::fence(self, input).await
            }
            async fn adaptive_request_status(
                &self,
                key: &crate::store::adaptive_resource_attempts::RequestKey,
                principal: &crate::playback_principal::PlaybackPrincipal,
            ) -> Result<
                Option<crate::store::adaptive_resource_attempts::RequestStatus>,
                crate::error::StoreError,
            > {
                crate::store::adaptive_resource_attempts::transactions::status(self, key, principal)
                    .await
            }
            async fn unclosed_adaptive_attempts(
                &self,
                query: &crate::store::adaptive_resource_attempts::AttemptInventoryQuery,
            ) -> Result<
                crate::store::adaptive_resource_attempts::AttemptInventoryPage,
                crate::error::StoreError,
            > {
                crate::store::adaptive_resource_attempts::transactions::inventory(self, query).await
            }
        }
    };
}
pub(crate) use impl_attempt_store;

/// Implemented in the two leased session backend files, using with_conn and
/// SessionClient's certified layout dispatch respectively. Never a second DB.
#[async_trait]
pub(crate) trait Backend: Send + Sync {
    async fn attempt_layout(&self) -> Result<Layout, StoreError>;
    async fn attempt_read(&self, sql: String) -> Result<Vec<String>, StoreError>;
    async fn attempt_transaction(&self, statements: Vec<String>) -> Result<(), StoreError>;
    async fn attempt_activate(
        &self,
        plan: &AdoptionPlan,
    ) -> Result<Option<MediaSessionActivationOutcome>, StoreError>;
}

fn encode<T: Serialize>(value: &T) -> Result<String, StoreError> {
    serde_json::to_string(value).map_err(|_| invalid())
}

fn payload<T: Serialize>(action: &str, value: &T) -> Result<String, StoreError> {
    let mut digest = Sha256::new();
    digest.update(b"plurx/adaptive-resource-operation/v1\0");
    digest.update(action.as_bytes());
    digest.update([0]);
    digest.update(encode(value)?.as_bytes());
    Ok(hex::encode(digest.finalize()))
}

fn request_predicate(binding: &RequestBinding, alias: &str) -> String {
    format!(
        "{alias}.owner_key={} AND {alias}.request_id={} AND {alias}.request_incarnation_id={}",
        quote(&binding.principal.owner_key()),
        quote(&binding.key.request_id),
        quote(&binding.key.request_incarnation_id.to_string())
    )
}

fn record_projection(alias: &str) -> String {
    format!("json_object('key',json({alias}.key_json),'sequence',{alias}.sequence,'phase',{alias}.phase,'worker_instance_id',{alias}.worker_instance_id,'generation_hex',hex({alias}.serving_generation),'domains',json({alias}.domains_json),'worker',json({alias}.worker_json),'last_operation_id',{alias}.last_operation_id,'last_payload_digest',{alias}.last_payload_digest,'updated_at_ms',{alias}.updated_at_ms,'relational_key',json_object('request',json_object('request_id',{alias}.request_id,'request_incarnation_id',{alias}.request_incarnation_id),'attempt_id',{alias}.attempt_id,'route_incarnation_id',{alias}.route_incarnation_id,'session_id',{alias}.session_id,'worker_node_id',{alias}.worker_node_id,'owner_epoch',{alias}.owner_epoch,'ordinal',{alias}.ordinal))")
}

fn status_projection() -> String {
    format!("json_object('binding',json(r.binding_json),'relational_binding',json_array(r.owner_key,r.request_id,r.request_incarnation_id),'history_valid',EXISTS(SELECT 1 FROM media_resource_operations op WHERE op.operation_id=r.last_operation_id AND op.owner_key=r.owner_key AND op.request_id=r.request_id AND op.request_incarnation_id=r.request_incarnation_id AND op.payload_digest=r.last_payload_digest AND op.request_sequence=r.sequence),'sequence',r.sequence,'next_ordinal',r.next_ordinal,'winner_attempt_id',r.winner_attempt_id,'disposition',r.disposition,'last_operation_id',r.last_operation_id,'last_payload_digest',r.last_payload_digest,'attempts',json((SELECT json_group_array(json(record)) FROM (SELECT {} AS record FROM media_resource_attempts a WHERE a.owner_key=r.owner_key AND a.request_id=r.request_id AND a.request_incarnation_id=r.request_incarnation_id ORDER BY a.ordinal))))",record_projection("a"))
}

fn decode_record(value: &mut serde_json::Value) -> Result<(), StoreError> {
    let object = value.as_object_mut().ok_or_else(invalid)?;
    let key = object.remove("relational_key").ok_or_else(invalid)?;
    if object.get("key") != Some(&key) {
        return Err(invalid());
    }
    let generation = object.remove("generation_hex").ok_or_else(invalid)?;
    let bytes = hex::decode(generation.as_str().ok_or_else(invalid)?).map_err(|_| invalid())?;
    let bytes: [u8; 8] = bytes.try_into().map_err(|_| invalid())?;
    object.insert(
        "serving_generation".into(),
        serde_json::json!(u64::from_le_bytes(bytes)),
    );
    Ok(())
}

fn decode_status(encoded: &str) -> Result<RequestStatus, StoreError> {
    if encoded.len() > 128 * 1024 {
        return Err(invalid());
    }
    let mut value: serde_json::Value = serde_json::from_str(encoded).map_err(|_| invalid())?;
    let object = value.as_object_mut().ok_or_else(invalid)?;
    let relational = object.remove("relational_binding").ok_or_else(invalid)?;
    if object.remove("history_valid") != Some(serde_json::json!(1)) {
        return Err(invalid());
    }
    for attempt in value
        .get_mut("attempts")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(invalid)?
    {
        decode_record(attempt)?;
    }
    let status: RequestStatus = serde_json::from_value(value).map_err(|_| invalid())?;
    status.validate()?;
    if relational
        != serde_json::json!([
            status.binding.principal.owner_key(),
            status.binding.key.request_id,
            status.binding.key.request_incarnation_id
        ])
    {
        return Err(invalid());
    }
    Ok(status)
}

pub(crate) async fn readiness<B: Backend>(store: &B) -> Result<AttemptSchemaReadiness, StoreError> {
    let layout = store.attempt_layout().await?;
    let sql="SELECT json_array(name,sql) AS payload FROM sqlite_master WHERE sql IS NOT NULL AND (name GLOB 'media_resource_*' OR tbl_name GLOB 'media_resource_*') ORDER BY name".to_owned();
    let rows = store
        .attempt_read(sql)
        .await?
        .into_iter()
        .map(|row| serde_json::from_str::<(String, String)>(&row).map_err(|_| invalid()))
        .collect::<Result<Vec<_>, _>>()?;
    if !schema::verify_shape(&rows, layout) {
        return Ok(AttemptSchemaReadiness::Unavailable);
    }
    let rows = store
        .attempt_read(format!(
            "SELECT json_quote(CASE WHEN ({}) THEN 1 ELSE 0 END) AS payload",
            schema::ready_guard(layout)
        ))
        .await?;
    Ok(if rows.as_slice() == ["1"] {
        AttemptSchemaReadiness::Ready
    } else {
        AttemptSchemaReadiness::Unavailable
    })
}

/// New ordinary claim callers must not reacquire an expired adaptive root.
/// Old binaries are independently fenced by the actual write triggers.
#[cfg(feature = "hiqlite-store")]
pub(crate) async fn root_held<B: Backend>(
    store: &B,
    owner: &str,
    request: &str,
) -> Result<bool, StoreError> {
    let present = store
        .attempt_read(format!(
            "SELECT json_quote(NOT({})) AS payload",
            schema::absent_guard()
        ))
        .await?;
    if present.as_slice() == ["0"] {
        return Ok(false);
    }
    let layout = store.attempt_layout().await?;
    let rows=store.attempt_read(format!("SELECT CASE WHEN ({}) THEN json_quote(EXISTS(SELECT 1 FROM media_resource_requests WHERE owner_key={} AND request_id={})) ELSE 'unavailable' END AS payload",schema::ready_guard(layout),quote(owner),quote(request))).await?;
    match rows.as_slice() {
        [value] if value == "1" => Ok(true),
        [value] if value == "0" => Ok(false),
        _ => Err(StoreError::Migration(
            "adaptive root authority unavailable".into(),
        )),
    }
}

pub(crate) async fn status<B: Backend>(
    store: &B,
    key: &RequestKey,
    principal: &PlaybackPrincipal,
) -> Result<Option<RequestStatus>, StoreError> {
    key.validate()?;
    if !principal.local_user_id().is_some_and(|id| id > 0) {
        return Err(invalid());
    }
    if readiness(store).await? != AttemptSchemaReadiness::Ready {
        return Err(StoreError::Migration(
            "adaptive attempt authority unavailable".into(),
        ));
    }
    let layout = store.attempt_layout().await?;
    // Schema and complete data are evaluated in this ONE authority snapshot.
    // A false guard returns null, never an apparently empty request inventory.
    let rows=store.attempt_read(format!("SELECT CASE WHEN ({}) THEN coalesce((SELECT {} FROM media_resource_requests r WHERE r.owner_key={} AND r.request_id={} AND r.request_incarnation_id={}), 'absent') ELSE 'unavailable' END AS payload",schema::ready_guard(layout),status_projection(),quote(&principal.owner_key()),quote(&key.request_id),quote(&key.request_incarnation_id.to_string()))).await?;
    match rows.as_slice() {
        [row] if row == "absent" => Ok(None),
        [row] if row != "unavailable" => {
            let status = decode_status(row)?;
            if status.binding.key != *key || status.binding.principal != *principal {
                return Err(invalid());
            }
            Ok(Some(status))
        }
        _ => Err(StoreError::Migration(
            "adaptive attempt snapshot unavailable".into(),
        )),
    }
}

pub(crate) async fn inventory<B: Backend>(
    store: &B,
    query: &AttemptInventoryQuery,
) -> Result<AttemptInventoryPage, StoreError> {
    if !text(&query.worker_node_id, 256)
        || !(1..=128).contains(&query.limit)
        || query.after_attempt_id.is_some_and(|id| id.is_nil())
    {
        return Err(invalid());
    }
    if readiness(store).await? != AttemptSchemaReadiness::Ready {
        return Err(StoreError::Migration(
            "adaptive inventory unavailable".into(),
        ));
    }
    let layout = store.attempt_layout().await?;
    let cursor = query
        .after_attempt_id
        .map_or_else(String::new, |id| id.to_string());
    let rows=store.attempt_read(format!("SELECT CASE WHEN ({}) THEN json((SELECT json_group_array(json(entry)) FROM (SELECT json_object('binding',json(r.binding_json),'owner_key',a.owner_key,'attempt',json({})) AS entry FROM media_resource_attempts a LEFT JOIN media_resource_requests r USING(owner_key,request_id,request_incarnation_id) WHERE a.worker_node_id={} AND a.phase!='closed' AND a.attempt_id>{} ORDER BY a.attempt_id LIMIT {}))) ELSE 'unavailable' END AS payload",schema::ready_guard(layout),record_projection("a"),quote(&query.worker_node_id),quote(&cursor),u32::from(query.limit)+1)).await?;
    let [encoded] = rows.as_slice() else {
        return Err(invalid());
    };
    if encoded.len() > 4 * 1024 * 1024 {
        return Err(invalid());
    }
    let values: Vec<serde_json::Value> = serde_json::from_str(encoded).map_err(|_| invalid())?;
    let mut entries = Vec::new();
    let mut previous = cursor;
    for mut value in values {
        let binding: RequestBinding =
            serde_json::from_value(value.get_mut("binding").ok_or_else(invalid)?.take())
                .map_err(|_| invalid())?;
        let mut record = value.get_mut("attempt").ok_or_else(invalid)?.take();
        decode_record(&mut record)?;
        let attempt: AttemptRecord = serde_json::from_value(record).map_err(|_| invalid())?;
        attempt.key.validate(&binding)?;
        validate_domains(&attempt.domains)?;
        if !binding.principal.local_user_id().is_some_and(|id| id > 0)
            || value.get("owner_key").and_then(serde_json::Value::as_str)
                != Some(binding.principal.owner_key().as_str())
            || attempt.worker_instance_id.is_nil()
            || attempt.last_operation_id.is_nil()
            || attempt.updated_at_ms <= 0
            || attempt.key.worker_node_id != query.worker_node_id
            || attempt.phase == AttemptPhase::Closed
            || attempt.key.attempt_id.to_string() <= previous
            || attempt.sequence <= 0
            || !digest(&attempt.last_payload_digest)
        {
            return Err(invalid());
        }
        if let Some(worker) = &attempt.worker {
            worker.validate(&binding, &attempt.key)?;
            if worker.serving_generation != attempt.serving_generation
                || worker.worker_instance_id != attempt.worker_instance_id
            {
                return Err(invalid());
            }
        } else if matches!(attempt.phase, AttemptPhase::Prepared | AttemptPhase::Winner) {
            return Err(invalid());
        }
        previous = attempt.key.attempt_id.to_string();
        entries.push(AttemptInventoryEntry { binding, attempt });
    }
    if entries.len() > usize::from(query.limit) + 1 {
        return Err(invalid());
    }
    let next_cursor = if entries.len() > usize::from(query.limit) {
        entries.pop();
        entries.last().map(|entry| entry.attempt.key.attempt_id)
    } else {
        None
    };
    Ok(AttemptInventoryPage {
        entries,
        next_cursor,
    })
}

struct Operation<'a> {
    context: &'a MutationContext,
    binding: &'a RequestBinding,
    key: &'a AttemptKey,
    action: &'static str,
    payload: String,
}

impl Operation<'_> {
    fn validate(&self) -> Result<(), StoreError> {
        self.context.validate()?;
        self.key.validate(self.binding)
    }
    fn claim(&self) -> Result<String, StoreError> {
        encode(
            &serde_json::json!({"operation_id":self.context.operation_id,"action":self.action,"owner_key":self.binding.principal.owner_key(),"request_id":self.binding.key.request_id,"request_incarnation_id":self.binding.key.request_incarnation_id,"attempt_id":self.key.attempt_id,"route_incarnation_id":self.key.route_incarnation_id,"session_id":self.key.session_id,"owner_epoch":self.key.owner_epoch,"expected_request_sequence":self.context.expected_request_sequence,"expected_attempt_sequence":self.context.expected_attempt_sequence,"payload_digest":self.payload}),
        )
    }
    fn open(&self, layout: Layout) -> Result<Vec<String>, StoreError> {
        let claim = quote(&self.claim()?);
        Ok(vec![schema::assertion(&schema::ready_guard(layout)),format!("UPDATE media_resource_schema SET mutation_claim={claim} WHERE singleton=1 AND revision<9223372036854775807 AND mutation_claim IS NULL"),schema::assertion(&format!("EXISTS(SELECT 1 FROM media_resource_schema WHERE singleton=1 AND mutation_claim={claim})"))])
    }
    fn finish(&self, statements: &mut Vec<String>) -> Result<(), StoreError> {
        // SQL no-ops cannot mint history: both projections must advance.
        statements.push(schema::assertion(&format!("EXISTS(SELECT 1 FROM media_resource_requests r WHERE {} AND r.sequence={} AND r.last_operation_id={} AND r.last_payload_digest={}) AND EXISTS(SELECT 1 FROM media_resource_attempts a WHERE a.attempt_id={} AND a.key_json={} AND a.sequence={} AND a.last_operation_id={} AND a.last_payload_digest={})", request_predicate(self.binding,"r"),self.context.expected_request_sequence+1,quote(&self.context.operation_id.to_string()),quote(&self.payload),quote(&self.key.attempt_id.to_string()),quote(&encode(self.key)?),self.context.expected_attempt_sequence+1,quote(&self.context.operation_id.to_string()),quote(&self.payload))));
        statements.push(format!("INSERT INTO media_resource_operations(operation_id,owner_key,request_id,request_incarnation_id,attempt_id,action,payload_digest,request_sequence,attempt_sequence) VALUES({},{},{},{},{},{},{},{},{})",quote(&self.context.operation_id.to_string()),quote(&self.binding.principal.owner_key()),quote(&self.binding.key.request_id),quote(&self.binding.key.request_incarnation_id.to_string()),quote(&self.key.attempt_id.to_string()),quote(self.action),quote(&self.payload),self.context.expected_request_sequence+1,self.context.expected_attempt_sequence+1));
        let claim = quote(&self.claim()?);
        statements.push(format!("UPDATE media_resource_schema SET mutation_claim=NULL,revision=revision+1 WHERE singleton=1 AND mutation_claim={claim} AND revision<9223372036854775807"));
        statements.push(schema::assertion("EXISTS(SELECT 1 FROM media_resource_schema WHERE singleton=1 AND mutation_claim IS NULL)"));
        Ok(())
    }
}

async fn replay<B: Backend>(
    store: &B,
    operation: &Operation<'_>,
) -> Result<Option<AttemptMutation>, StoreError> {
    let rows=store.attempt_read(format!("SELECT json_array(owner_key,request_id,request_incarnation_id,attempt_id,action,payload_digest) AS payload FROM media_resource_operations WHERE operation_id={}",quote(&operation.context.operation_id.to_string()))).await?;
    let expected = encode(&(
        operation.binding.principal.owner_key(),
        &operation.binding.key.request_id,
        operation.binding.key.request_incarnation_id.to_string(),
        operation.key.attempt_id.to_string(),
        operation.action,
        &operation.payload,
    ))?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] if row == &expected => Ok(Some(AttemptMutation::Replay(
            status(store, &operation.binding.key, &operation.binding.principal)
                .await?
                .ok_or_else(invalid)?,
        ))),
        [_] => Ok(Some(AttemptMutation::Refused(AttemptRefusal::Conflict))),
        _ => Err(invalid()),
    }
}

async fn admit<B: Backend>(
    store: &B,
    operation: &Operation<'_>,
) -> Result<Option<AttemptMutation>, StoreError> {
    operation.validate()?;
    if !operation
        .binding
        .principal
        .local_user_id()
        .is_some_and(|id| id > 0)
    {
        return Ok(Some(AttemptMutation::Refused(
            AttemptRefusal::UnsupportedPrincipal,
        )));
    }
    if readiness(store).await? != AttemptSchemaReadiness::Ready {
        return Ok(Some(AttemptMutation::Refused(
            AttemptRefusal::UnsupportedSchema,
        )));
    }
    if let Some(result) = replay(store, operation).await? {
        return Ok(Some(result));
    }
    if !matches!(operation.action, "fence" | "winner")
        && operation.context.now_ms >= operation.binding.original_deadline_ms
    {
        return Ok(Some(AttemptMutation::Refused(AttemptRefusal::Expired)));
    }
    Ok(None)
}

fn exact_request(operation: &Operation<'_>) -> Result<String, StoreError> {
    Ok(format!("EXISTS(SELECT 1 FROM media_resource_requests r WHERE {} AND r.binding_json={} AND r.sequence={} AND r.sequence<9223372036854775807)",request_predicate(operation.binding,"r"),quote(&encode(operation.binding)?),operation.context.expected_request_sequence))
}

fn exact_attempt(operation: &Operation<'_>) -> Result<String, StoreError> {
    Ok(format!("EXISTS(SELECT 1 FROM media_resource_attempts a WHERE a.attempt_id={} AND a.key_json={} AND a.sequence={} AND a.sequence<9223372036854775807)",quote(&operation.key.attempt_id.to_string()),quote(&encode(operation.key)?),operation.context.expected_attempt_sequence))
}

fn advance_request(operation: &Operation<'_>, extra: &str) -> String {
    format!("UPDATE media_resource_requests AS r SET sequence=sequence+1,last_operation_id={},last_payload_digest={} {extra} WHERE {} AND sequence={}",quote(&operation.context.operation_id.to_string()),quote(&operation.payload),request_predicate(operation.binding,"r"),operation.context.expected_request_sequence)
}

fn advance_attempt(operation: &Operation<'_>, extra: &str) -> String {
    format!("UPDATE media_resource_attempts SET sequence=sequence+1,last_operation_id={},last_payload_digest={},updated_at_ms={} {extra} WHERE attempt_id={} AND sequence={}",quote(&operation.context.operation_id.to_string()),quote(&operation.payload),operation.context.now_ms,quote(&operation.key.attempt_id.to_string()),operation.context.expected_attempt_sequence)
}

/// Assert before acquisition as well as adoption. Preserve ordinary semantics:
/// an absent historical desired ask is tolerated, a different one is not.
fn playback_context(binding: &RequestBinding, layout: Layout) -> Vec<String> {
    let owner = quote(&binding.principal.owner_key());
    let playback = quote(&binding.playback_id);
    let predecessor = binding
        .expected_predecessor_incarnation_id
        .map_or_else(|| "NULL".into(), |id| quote(&id.to_string()));
    let pointer_owner = layout.owner("pointer");
    let mut statements = vec![schema::assertion(&format!("(SELECT current_incarnation_id FROM media_playback_pointers pointer WHERE {pointer_owner}={owner} AND pointer.playback_id={playback}) IS {predecessor}"))];
    if let Some(epoch) = binding.expected_predecessor_owner_epoch {
        let predecessor_owner = layout.owner("p");
        statements.push(schema::assertion(&format!("EXISTS(SELECT 1 FROM media_sessions p WHERE p.incarnation_id={predecessor} AND {predecessor_owner}={owner} AND p.user_id={} AND p.playback_id={playback} AND p.owner_epoch={epoch} AND p.state='active' AND p.publication_ready_at_ms=0)",binding.principal.local_user_id().unwrap_or(0))));
    }
    if let Some(revision) = binding.expected_desired_revision {
        let desired_owner = layout.owner("desired");
        statements.push(schema::assertion(&format!("NOT EXISTS(SELECT 1 FROM media_playback_desired desired WHERE {desired_owner}={owner} AND desired.playback_id={playback} AND desired.revision!={revision})")));
    }
    statements
}

async fn execute<B: Backend>(
    store: &B,
    operation: &Operation<'_>,
    mut statements: Vec<String>,
) -> Result<AttemptMutation, StoreError> {
    operation.finish(&mut statements)?;
    store.attempt_transaction(statements).await?;
    // A later authoritative snapshot may already contain another legitimate
    // operation. The immutable operation record confirms OUR exact commit.
    match replay(store, operation).await? {
        Some(AttemptMutation::Replay(status)) => Ok(AttemptMutation::Applied(status)),
        _ => Err(StoreError::Database(
            "adaptive operation acknowledgement did not settle".into(),
        )),
    }
}

pub(crate) async fn claim<B: Backend>(
    store: &B,
    input: &AttemptClaim,
) -> Result<AttemptMutation, StoreError> {
    input.validate()?;
    let operation = Operation {
        context: &input.context,
        binding: &input.binding,
        key: &input.key,
        action: "claim",
        payload: payload("claim", input)?,
    };
    if let Some(result) = admit(store, &operation).await? {
        return Ok(result);
    }
    let current = status(store, &input.binding.key, &input.binding.principal).await?;
    if let Some(current) = &current {
        if current.binding != input.binding {
            return Ok(AttemptMutation::Refused(AttemptRefusal::Conflict));
        }
        if current.disposition != RequestDisposition::Open || current.winner_attempt_id.is_some() {
            return Ok(AttemptMutation::Refused(AttemptRefusal::WinnerLost));
        }
        if current.sequence != input.context.expected_request_sequence
            || current.next_ordinal != input.key.ordinal
        {
            return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
        }
        let unresolved = current
            .attempts
            .iter()
            .filter(|a| a.phase != AttemptPhase::Closed)
            .collect::<Vec<_>>();
        if current.attempts.len() >= 3
            || unresolved.len() >= 2
            || current
                .attempts
                .iter()
                .any(|a| a.key.worker_node_id == input.key.worker_node_id)
            || unresolved.iter().any(|a| {
                a.domains
                    .iter()
                    .any(|domain| input.domains.contains(domain))
            })
        {
            return Ok(AttemptMutation::Refused(AttemptRefusal::Bound));
        }
    } else if input.context.expected_request_sequence != 0 || input.key.ordinal != 1 {
        return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
    }
    let layout = store.attempt_layout().await?;
    let mut statements = operation.open(layout)?;
    let binding = &input.binding;
    let key = &input.key;
    let user = binding.principal.local_user_id().ok_or_else(invalid)?;
    let owner = layout.owner("root");
    statements.extend(playback_context(binding, layout));
    statements.push(schema::assertion(&format!("(SELECT count(*) FROM media_sessions WHERE owner_node_id={} AND state IN('starting','active') AND lease_expires_at_ms>{})<4096",quote(&key.worker_node_id),input.context.now_ms)));
    statements.push(schema::assertion(&format!("EXISTS(SELECT 1 FROM media_session_requests root WHERE {owner}={} AND root.request_id={} AND root.incarnation_id={} AND root.request_fingerprint={} AND root.playback_id={} AND root.state='starting' AND root.claim_expires_at_ms={} AND root.claim_expires_at_ms>{} AND EXISTS(SELECT 1 FROM users WHERE id={user}))",quote(&binding.principal.owner_key()),quote(&binding.key.request_id),quote(&binding.key.request_incarnation_id.to_string()),quote(&binding.request_fingerprint),quote(&binding.playback_id),binding.original_deadline_ms,input.context.now_ms)));
    statements.push(schema::assertion(&format!("(SELECT count(*) FROM media_resource_attempts WHERE owner_key={} AND phase!='closed')<128 AND (SELECT count(*) FROM media_resource_attempts WHERE worker_node_id={} AND phase!='closed')<4096 AND (SELECT count(*) FROM media_sessions WHERE user_id={user})<4096 AND (SELECT count(*) FROM media_sessions WHERE user_id={user} AND state IN('starting','active') AND lease_expires_at_ms>{})<64",quote(&binding.principal.owner_key()),quote(&key.worker_node_id),input.context.now_ms)));
    if current.is_some() {
        statements.push(schema::assertion(&format!("({}) AND EXISTS(SELECT 1 FROM media_resource_requests r WHERE {} AND r.disposition='open' AND r.winner_attempt_id IS NULL AND r.next_ordinal={}) AND (SELECT count(*) FROM media_resource_attempts a WHERE {} AND a.phase!='closed')<2 AND NOT EXISTS(SELECT 1 FROM media_resource_attempts a,json_each(a.domains_json) d WHERE {} AND a.phase!='closed' AND d.value IN(SELECT value FROM json_each({})))",exact_request(&operation)?,request_predicate(binding,"r"),key.ordinal,request_predicate(binding,"a"),request_predicate(binding,"a"),quote(&encode(&input.domains)?))));
        statements.push(advance_request(&operation, ",next_ordinal=next_ordinal+1"));
    } else {
        statements.push(format!("INSERT INTO media_resource_requests(owner_key,request_id,request_incarnation_id,binding_json,next_ordinal,disposition,sequence,last_operation_id,last_payload_digest) VALUES({},{},{},{},2,'open',1,{},{})",quote(&binding.principal.owner_key()),quote(&binding.key.request_id),quote(&binding.key.request_incarnation_id.to_string()),quote(&encode(binding)?),quote(&input.context.operation_id.to_string()),quote(&operation.payload)));
    }
    statements.push(format!("INSERT INTO media_resource_attempts(attempt_id,owner_key,request_id,request_incarnation_id,ordinal,route_incarnation_id,session_id,worker_node_id,owner_epoch,key_json,worker_instance_id,serving_generation,domains_json,phase,sequence,last_operation_id,last_payload_digest,updated_at_ms) VALUES({},{},{},{},{},{},{},{},{},{},{},X'{}',{},'provisional',1,{},{},{})",quote(&key.attempt_id.to_string()),quote(&binding.principal.owner_key()),quote(&binding.key.request_id),quote(&binding.key.request_incarnation_id.to_string()),key.ordinal,quote(&key.route_incarnation_id.to_string()),quote(&key.session_id.to_string()),quote(&key.worker_node_id),key.owner_epoch,quote(&encode(key)?),quote(&input.worker_instance_id.to_string()),hex::encode(input.serving_generation.to_le_bytes()),quote(&encode(&input.domains)?),quote(&input.context.operation_id.to_string()),quote(&operation.payload),input.context.now_ms));
    let (columns, values) = if layout == Layout::Principal {
        (
            ",owner_key,principal_kind,share_grant_id,share_viewer_key",
            format!(
                ",{},'local',NULL,NULL",
                quote(&binding.principal.owner_key())
            ),
        )
    } else {
        ("", String::new())
    };
    statements.push(format!("INSERT INTO media_sessions(incarnation_id,session_id,user_id,playback_id,request_fingerprint,owner_node_id,owner_epoch,lease_expires_at_ms,state,recipe_json,response_json,publication_ready_at_ms,updated_at_ms,recovery_epoch{columns}) VALUES({},{},{user},{},{},{},{},{},'starting','{{}}','{{}}',{},{},{}{values})",quote(&key.route_incarnation_id.to_string()),quote(&key.session_id.to_string()),quote(&binding.playback_id),quote(&binding.request_fingerprint),quote(&key.worker_node_id),key.owner_epoch,binding.original_deadline_ms,crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,input.context.now_ms,quote(&binding.recovery_epoch.to_string())));
    statements.push(format!("INSERT INTO job_leases(resource,owner_node_id,fence,revision,expires_at_ms,updated_at_ms) VALUES({},{},{},1,{},{})",quote(&format!("session:{}",key.route_incarnation_id)),quote(&key.worker_node_id),key.owner_epoch,binding.original_deadline_ms,input.context.now_ms));
    statements.push(schema::assertion(&format!("EXISTS(SELECT 1 FROM media_sessions s JOIN job_leases l ON l.resource='session:'||s.incarnation_id WHERE s.incarnation_id={} AND s.session_id={} AND s.state='starting' AND s.owner_node_id={} AND s.owner_epoch={} AND s.publication_ready_at_ms={} AND s.lease_expires_at_ms={} AND l.owner_node_id=s.owner_node_id AND l.fence=s.owner_epoch AND l.expires_at_ms=s.lease_expires_at_ms)",quote(&key.route_incarnation_id.to_string()),quote(&key.session_id.to_string()),quote(&key.worker_node_id),key.owner_epoch,crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,binding.original_deadline_ms)));
    execute(store, &operation, statements).await
}

pub(crate) async fn prepared<B: Backend>(
    store: &B,
    input: &PreparedAttempt,
) -> Result<AttemptMutation, StoreError> {
    input.worker.validate(&input.binding, &input.key)?;
    let operation = Operation {
        context: &input.context,
        binding: &input.binding,
        key: &input.key,
        action: "prepared",
        payload: payload("prepared", input)?,
    };
    if let Some(result) = admit(store, &operation).await? {
        return Ok(result);
    }
    let Some(current) = status(store, &input.binding.key, &input.binding.principal).await? else {
        return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
    };
    if current.binding != input.binding {
        return Ok(AttemptMutation::Refused(AttemptRefusal::Conflict));
    }
    let Some(attempt) = current.attempts.iter().find(|a| a.key == input.key) else {
        return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
    };
    if current.disposition != RequestDisposition::Open
        || current.winner_attempt_id.is_some()
        || attempt.phase != AttemptPhase::Provisional
    {
        return Ok(AttemptMutation::Refused(AttemptRefusal::WinnerLost));
    }
    if current.sequence != input.context.expected_request_sequence
        || attempt.sequence != input.context.expected_attempt_sequence
        || attempt.worker_instance_id != input.worker.worker_instance_id
        || attempt.serving_generation != input.worker.serving_generation
    {
        return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
    }
    if input.context.now_ms >= input.worker.prepared_expires_at_ms {
        return Ok(AttemptMutation::Refused(AttemptRefusal::Expired));
    }
    let layout = store.attempt_layout().await?;
    let mut statements = operation.open(layout)?;
    statements.push(schema::assertion(&format!("({}) AND ({}) AND EXISTS(SELECT 1 FROM media_resource_requests r WHERE {} AND r.disposition='open' AND r.winner_attempt_id IS NULL) AND EXISTS(SELECT 1 FROM media_resource_attempts WHERE attempt_id={} AND phase='provisional' AND worker_json IS NULL AND worker_instance_id={} AND serving_generation=X'{}')",exact_request(&operation)?,exact_attempt(&operation)?,request_predicate(&input.binding,"r"),quote(&input.key.attempt_id.to_string()),quote(&input.worker.worker_instance_id.to_string()),hex::encode(input.worker.serving_generation.to_le_bytes()))));
    statements.push(advance_attempt(
        &operation,
        &format!(
            ",phase='prepared',worker_json={}",
            quote(&encode(&input.worker)?)
        ),
    ));
    statements.push(advance_request(&operation, ""));
    execute(store, &operation, statements).await
}

pub(crate) async fn winner<B: Backend>(
    store: &B,
    input: &WinnerSelection,
) -> Result<AttemptMutation, StoreError> {
    input.worker.validate(&input.binding, &input.key)?;
    let operation = Operation {
        context: &input.context,
        binding: &input.binding,
        key: &input.key,
        action: "winner",
        payload: payload("winner", input)?,
    };
    if let Some(result) = admit(store, &operation).await? {
        return Ok(result);
    }
    let Some(current) = status(store, &input.binding.key, &input.binding.principal).await? else {
        return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
    };
    if current.binding != input.binding {
        return Ok(AttemptMutation::Refused(AttemptRefusal::Conflict));
    }
    let Some(attempt) = current.attempts.iter().find(|a| a.key == input.key) else {
        return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
    };
    if current.winner_attempt_id == Some(input.key.attempt_id) {
        return Ok(if attempt.worker.as_ref() == Some(&input.worker) {
            AttemptMutation::Replay(current)
        } else {
            AttemptMutation::Refused(AttemptRefusal::Conflict)
        });
    }
    if current.winner_attempt_id.is_some() || current.disposition != RequestDisposition::Open {
        return Ok(AttemptMutation::Refused(AttemptRefusal::WinnerLost));
    }
    if current.sequence != input.context.expected_request_sequence
        || attempt.sequence != input.context.expected_attempt_sequence
        || attempt.phase != AttemptPhase::Prepared
        || attempt.worker.as_ref() != Some(&input.worker)
    {
        return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
    }
    if input.context.now_ms >= input.worker.prepared_expires_at_ms {
        return Ok(AttemptMutation::Refused(AttemptRefusal::Expired));
    }
    let layout = store.attempt_layout().await?;
    let mut statements = operation.open(layout)?;
    statements.push(schema::assertion(&format!("({}) AND ({}) AND EXISTS(SELECT 1 FROM media_resource_requests r WHERE {} AND r.winner_attempt_id IS NULL AND r.disposition='open') AND EXISTS(SELECT 1 FROM media_resource_attempts WHERE attempt_id={} AND phase='prepared' AND worker_json={}) AND NOT EXISTS(SELECT 1 FROM media_resource_attempts a WHERE {} AND a.sequence=9223372036854775807)",exact_request(&operation)?,exact_attempt(&operation)?,request_predicate(&input.binding,"r"),quote(&input.key.attempt_id.to_string()),quote(&encode(&input.worker)?),request_predicate(&input.binding,"a"))));
    statements.push(advance_request(
        &operation,
        &format!(
            ",winner_attempt_id={}",
            quote(&input.key.attempt_id.to_string())
        ),
    ));
    statements.push(advance_attempt(&operation, ",phase='winner'"));
    // Losing is a retained cleanup obligation, not a resource refund.
    statements.push(format!("UPDATE media_resource_attempts AS a SET phase='losing',sequence=sequence+1,last_operation_id={},last_payload_digest={},updated_at_ms={} WHERE {} AND a.attempt_id!={} AND a.phase!='closed'",quote(&input.context.operation_id.to_string()),quote(&operation.payload),input.context.now_ms,request_predicate(&input.binding,"a"),quote(&input.key.attempt_id.to_string())));
    statements.push(schema::assertion(&format!("NOT EXISTS(SELECT 1 FROM media_resource_attempts a WHERE {} AND a.attempt_id!={} AND a.phase NOT IN('losing','closed'))",request_predicate(&input.binding,"a"),quote(&input.key.attempt_id.to_string()))));
    execute(store, &operation, statements).await
}

pub(crate) async fn fence<B: Backend>(
    store: &B,
    input: &AttemptFence,
) -> Result<AttemptMutation, StoreError> {
    let operation = Operation {
        context: &input.context,
        binding: &input.binding,
        key: &input.key,
        action: "fence",
        payload: payload("fence", input)?,
    };
    if let Some(result) = admit(store, &operation).await? {
        return Ok(result);
    }
    let Some(current) = status(store, &input.binding.key, &input.binding.principal).await? else {
        return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
    };
    if current.binding != input.binding {
        return Ok(AttemptMutation::Refused(AttemptRefusal::Conflict));
    }
    if current.disposition == RequestDisposition::Adopted {
        return Ok(AttemptMutation::Refused(AttemptRefusal::WinnerLost));
    }
    if current.sequence != input.context.expected_request_sequence
        || !current.attempts.iter().any(|a| {
            a.key == input.key
                && a.sequence == input.context.expected_attempt_sequence
                && a.phase != AttemptPhase::Closed
        })
    {
        return Ok(AttemptMutation::Refused(AttemptRefusal::StaleBinding));
    }
    let layout = store.attempt_layout().await?;
    let mut statements = operation.open(layout)?;
    statements.push(schema::assertion(&format!("({}) AND ({}) AND EXISTS(SELECT 1 FROM media_resource_requests r WHERE {} AND r.disposition!='adopted')",exact_request(&operation)?,exact_attempt(&operation)?,request_predicate(&input.binding,"r"))));
    statements.push(advance_attempt(&operation, ",phase='cleanup'"));
    statements.push(advance_request(
        &operation,
        if input.cancel_request {
            ",disposition='cancelled'"
        } else {
            ""
        },
    ));
    execute(store, &operation, statements).await
}
