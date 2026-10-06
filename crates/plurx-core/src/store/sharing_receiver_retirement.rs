//! Exact retirement of B metadata after actor-owned physical settlement.
use super::{
    sharing::{ordered, Backend, Value},
    sharing_receiver_sessions::{source_assert, source_write_refused},
};
use crate::{
    error::StoreError,
    sharing::{invalid, is_hash},
    sharing_receiver_retirement::{
        ReceiverOrphan, ReceiverOrphanClaimOutcome, ReceiverPendingOwner,
        ReceiverPendingRetirementWitness, ReceiverRetirementDisposition, ReceiverRetirementOutcome,
        ReceiverRetirementReason, ReceiverRetirementWitness,
    },
};
use async_trait::async_trait;
use sha2::{Digest, Sha256};
#[async_trait]
pub trait SharingReceiverRetirementStore: Send + Sync {
    /// Cleanup-only exact claim with no created route/resources. Caller retains
    /// its private joined no-send evidence after a commit-unknown error.
    async fn retire_pending_receiver_request(
        &self,
        witness: &dyn ReceiverPendingRetirementWitness,
    ) -> Result<ReceiverRetirementOutcome, StoreError>;
    /// Cleanup-only: revoked login is permitted for this exact retained lineage.
    /// Commit-unknown remains an error; caller retains the physical receipt.
    async fn retire_receiver_session(
        &self,
        witness: &dyn ReceiverRetirementWitness,
    ) -> Result<ReceiverRetirementOutcome, StoreError>;
    /// Read-only keyset inventory of RemoteSource routes whose owner stopped
    /// renewing them. Inventory is never settlement evidence.
    async fn orphaned_receiver_sessions(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<super::sharing_receiver_orphans::ReceiverOrphanPage, StoreError>;
    /// Exclusive takeover of one exact observed orphan: owner, epoch and
    /// lease move to `next_owner_node_id` at epoch+1, fencing every old-epoch
    /// writer. Does not need sharing on, the original login or the import.
    async fn claim_orphaned_receiver_session(
        &self,
        orphan: &ReceiverOrphan,
        next_owner_node_id: &str,
    ) -> Result<ReceiverOrphanClaimOutcome, StoreError>;
}
#[async_trait]
impl<T: Backend> SharingReceiverRetirementStore for T {
    async fn orphaned_receiver_sessions(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> Result<super::sharing_receiver_orphans::ReceiverOrphanPage, StoreError> {
        super::sharing_receiver_orphans::inventory(self, after, limit).await
    }
    async fn claim_orphaned_receiver_session(
        &self,
        orphan: &ReceiverOrphan,
        next_owner_node_id: &str,
    ) -> Result<ReceiverOrphanClaimOutcome, StoreError> {
        super::sharing_receiver_orphans::claim(self, orphan, next_owner_node_id).await
    }
    async fn retire_pending_receiver_request(
        &self,
        w: &dyn ReceiverPendingRetirementWitness,
    ) -> Result<ReceiverRetirementOutcome, StoreError> {
        let i = w.intent();
        let r = &i.recipe;
        let bounded = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        let node = match w.owner() {
            ReceiverPendingOwner::Unassigned => None,
            ReceiverPendingOwner::Assigned(node) if bounded(node, 256) => Some(node.as_str()),
            ReceiverPendingOwner::Assigned(_) => return Err(invalid()),
        };
        if i.user_id <= 0
            || !is_hash(w.confirmation_id())
            || !is_hash(&i.login_hash)
            || r.parent_login_hash != i.login_hash
            || r.version != 1
            || r.source_request_id.is_nil()
            || r.reference.import_id != i.scope.import_id
            || r.reference.server_id != i.scope.source_server_id
            || r.reference.catalogue_epoch != i.scope.catalogue_epoch
            || r.lifecycle_generation != i.scope.lifecycle_generation
            || i.scope.libraries.as_slice() != [r.reference.library_id.clone()]
            || !(0..=9_007_199_254_740_991).contains(&i.source_position_ms)
            || !bounded(w.request_id(), 128)
            || !bounded(w.playback_id(), 128)
            || r.request_json.len() > 32768
            || serde_json::to_string(r).map_err(|_| invalid())?.len() > 65536
        {
            return Err(invalid());
        }
        let context = serde_json::json!({"user":i.user_id,"request":w.request_id(),"playback":w.playback_id(),"incarnation":r.source_request_id,"fingerprint":r.request_fingerprint()?,"node":node}).to_string();
        let values = vec![Value::Text(context)];
        let identity = "r.user_id=json_extract($1,'$.user') AND r.request_id=json_extract($1,'$.request') AND r.playback_id=json_extract($1,'$.playback') AND r.incarnation_id=json_extract($1,'$.incarnation') AND r.request_fingerprint=json_extract($1,'$.fingerprint') AND r.owner_node_id IS json_extract($1,'$.node') AND r.response_json IS NULL";
        let absent = "NOT EXISTS(SELECT 1 FROM media_sessions s WHERE s.incarnation_id=r.incarnation_id) AND NOT EXISTS(SELECT 1 FROM media_playback_pointers p WHERE p.current_incarnation_id=r.incarnation_id) AND NOT EXISTS(SELECT 1 FROM job_leases j WHERE j.resource='session:'||r.incarnation_id) AND NOT EXISTS(SELECT 1 FROM sharing_relay_upstream b WHERE b.incarnation_id=r.incarnation_id) AND NOT EXISTS(SELECT 1 FROM cache_consumer_pins p WHERE p.consumer_kind='media_session' AND p.consumer_id=r.incarnation_id) AND NOT EXISTS(SELECT 1 FROM sharing_delivery_grants g WHERE g.incarnation_id=r.incarnation_id) AND NOT EXISTS(SELECT 1 FROM media_session_terminal_acks a WHERE a.incarnation_id=r.incarnation_id)";
        let predicate = |state: &str| {
            format!("EXISTS(SELECT 1 FROM media_session_requests r WHERE {identity} AND r.state='{state}' AND ({absent}))")
        };
        let after = predicate("failed");
        if !self
            .sharing_read(
                &format!("SELECT 'replay' AS payload WHERE {after}"),
                values.clone(),
            )
            .await?
            .is_empty()
        {
            return match self
                .sharing_txn(vec![ordered(&source_assert(after, vec![]).0, values)?])
                .await
            {
                Ok(_) => Ok(ReceiverRetirementOutcome::Replay),
                Err(e) if source_write_refused(&e) => Ok(ReceiverRetirementOutcome::Refused),
                Err(e) => Err(e),
            };
        }
        let now = super::sharing::wall_clock_ms()?;
        let update = "UPDATE media_session_requests SET state='failed',claim_expires_at_ms=$2,updated_at_ms=$2 WHERE incarnation_id=json_extract($1,'$.incarnation') AND user_id=json_extract($1,'$.user') AND request_id=json_extract($1,'$.request') AND state='starting'";
        let written = format!("{after} AND EXISTS(SELECT 1 FROM media_session_requests r WHERE {identity} AND r.updated_at_ms=$2 AND r.claim_expires_at_ms=$2)");
        let statements = vec![
            ordered(
                &source_assert(predicate("starting"), vec![]).0,
                values.clone(),
            )?,
            ordered(update, vec![values[0].clone(), Value::Integer(now)])?,
            ordered(
                &source_assert(written, vec![]).0,
                vec![values[0].clone(), Value::Integer(now)],
            )?,
        ];
        match self.sharing_txn(statements).await {
            Ok(_) => Ok(ReceiverRetirementOutcome::Applied),
            Err(e) if source_write_refused(&e) => Ok(ReceiverRetirementOutcome::Refused),
            Err(e) => Err(e),
        }
    }
    async fn retire_receiver_session(
        &self,
        w: &dyn ReceiverRetirementWitness,
    ) -> Result<ReceiverRetirementOutcome, StoreError> {
        let i = w.intent();
        let o = w.owner();
        let r = &i.recipe;
        let bounded = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        if !is_hash(w.confirmation_id())
            || !is_hash(&i.login_hash)
            || r.parent_login_hash != i.login_hash
            || i.user_id <= 0
            || o.incarnation_id.is_nil()
            || o.session_id.is_nil()
            || o.incarnation_id != r.source_request_id
            || o.owner_epoch <= 0
            || o.lease_expires_at_ms < 0
            || !bounded(&o.owner_node_id, 256)
            || !bounded(&o.request_id, 128)
            || r.version != 1
            || r.reference.import_id != i.scope.import_id
            || r.reference.server_id != i.scope.source_server_id
            || r.reference.catalogue_epoch != i.scope.catalogue_epoch
            || r.lifecycle_generation != i.scope.lifecycle_generation
            || !(0..=9_007_199_254_740_991).contains(&i.source_position_ms)
        {
            return Err(invalid());
        }
        let recipe = serde_json::to_string(r).map_err(|_| invalid())?;
        if recipe.len() > 65536 || r.request_json.len() > 32768 {
            return Err(invalid());
        }
        let binding = match (w.disposition(), w.binding()) {
            (_, None) => serde_json::Value::Null,
            (ReceiverRetirementDisposition::SourceSettled, Some(b)) => {
                if b.reference != r.reference
                    || b.file_id != r.file_id
                    || b.file_revision != r.file_revision
                    || b.source_request_id != r.source_request_id
                    || b.source_session_id.is_nil()
                    || b.source_incarnation_id.is_nil()
                {
                    return Err(invalid());
                }
                let envelope = b.capability_envelope.to_persist().map_err(|_| invalid())?;
                if envelope.len() > 4096 || envelope.rsplit(':').next().is_none_or(|s| s.len() < 80)
                {
                    return Err(invalid());
                }
                serde_json::json!({"session":b.source_session_id,"incarnation":b.source_incarnation_id,"envelope":envelope})
            }
            _ => return Err(invalid()),
        };
        let mut context=serde_json::json!({"user":i.user_id,"incarnation":o.incarnation_id,"session":o.session_id,"node":o.owner_node_id,"epoch":o.owner_epoch,"request":o.request_id,"lease":o.lease_expires_at_ms,"recipe":recipe,"fingerprint":r.request_fingerprint()?,"position":i.source_position_ms,"import":i.scope.import_id,"lifecycle":i.scope.lifecycle_generation,"assignment":i.scope.assignment_generation,"endpoint":i.scope.endpoint_generation,"binding":binding}).to_string();
        let context_identity = context.clone();
        // The retained Start reply is current metadata, including any NULL
        // left by the real user-delete trigger. The transaction repeats this
        // preimage without restoring it or pretending it proves Source End.
        // The immutable receipt identity excludes this mutable trigger result.
        let reply_sql="SELECT CASE WHEN response_json IS NULL OR (typeof(response_json)='text' AND length(CAST(response_json AS BLOB))<=65536) THEN json_array(response_json) ELSE '{}' END AS payload FROM media_session_requests WHERE incarnation_id=json_extract($1,'$.incarnation') AND user_id=json_extract($1,'$.user') AND request_id=json_extract($1,'$.request') AND owner_node_id=json_extract($1,'$.node') AND request_fingerprint=json_extract($1,'$.fingerprint') LIMIT 2";
        let rows = self
            .sharing_read(reply_sql, vec![Value::Text(context.clone())])
            .await?;
        let [row] = rows.as_slice() else {
            return Ok(ReceiverRetirementOutcome::Refused);
        };
        let replies: Vec<Option<String>> = serde_json::from_str(row).map_err(|_| invalid())?;
        let [reply] = replies.as_slice() else {
            return Err(invalid());
        };
        let mut object: serde_json::Value =
            serde_json::from_str(&context).map_err(|_| invalid())?;
        object.as_object_mut().ok_or_else(invalid)?.insert(
            "request_response".into(),
            serde_json::to_value(reply).map_err(|_| invalid())?,
        );
        context = object.to_string();
        let reason = match w.reason() {
            ReceiverRetirementReason::Deleted => "deleted",
            ReceiverRetirementReason::Superseded => "superseded",
            ReceiverRetirementReason::AdminStop => "admin_stop",
            ReceiverRetirementReason::Revoked => "revoked",
            ReceiverRetirementReason::Replaced => "replaced",
        };
        let disposition = match w.disposition() {
            ReceiverRetirementDisposition::SourceSettled => "source_settled",
            ReceiverRetirementDisposition::NeverDispatched => "never_dispatched",
        };
        let mut digest = Sha256::new();
        digest.update(b"plurx.receiver.retirement-context.v1\0");
        digest.update(context_identity.as_bytes());
        let receipt=serde_json::json!({"kind":"receiver_retirement_v1","context":format!("{:x}",digest.finalize()),"confirmation":w.confirmation_id(),"disposition":disposition,"reason":reason}).to_string();
        let now = super::sharing::wall_clock_ms()?;
        let vals = vec![
            Value::Text(context),
            Value::Text(receipt),
            Value::Text(reason.into()),
            Value::Integer(now),
        ];
        let identity="s.incarnation_id=json_extract($1,'$.incarnation') AND s.session_id=json_extract($1,'$.session') AND s.user_id=json_extract($1,'$.user') AND s.owner_node_id=json_extract($1,'$.node') AND s.owner_epoch=json_extract($1,'$.epoch') AND s.recipe_json=json_extract($1,'$.recipe') AND s.request_fingerprint=json_extract($1,'$.fingerprint') AND s.media_origin_ms=json_extract($1,'$.position') AND s.lease_expires_at_ms=json_extract($1,'$.lease')";
        let request="r.incarnation_id=s.incarnation_id AND r.user_id=s.user_id AND r.request_id=json_extract($1,'$.request') AND r.owner_node_id=s.owner_node_id AND r.playback_id=s.playback_id AND r.request_fingerprint=s.request_fingerprint AND r.response_json IS json_extract($1,'$.request_response')";
        let upstream="b.incarnation_id=s.incarnation_id AND b.import_id=json_extract($1,'$.import') AND b.lifecycle_generation=json_extract($1,'$.lifecycle') AND b.assignment_generation=json_extract($1,'$.assignment') AND b.endpoint_revision=json_extract($1,'$.endpoint') AND b.remote_library_id=json_extract(s.recipe_json,'$.reference.library_id') AND b.remote_item_id=json_extract(s.recipe_json,'$.reference.item_id') AND b.remote_file_id=json_extract(s.recipe_json,'$.file_id') AND b.remote_revision=json_extract(s.recipe_json,'$.file_revision') AND b.source_request_id=json_extract(s.recipe_json,'$.source_request_id') AND b.source_position_ms=s.media_origin_ms AND ((json_extract($1,'$.binding') IS NULL AND b.source_session_id IS NULL AND b.source_incarnation_id IS NULL AND b.capability_envelope IS NULL AND r.state IN('starting','failed') AND r.response_json IS NULL) OR (json_extract($1,'$.binding') IS NOT NULL AND b.source_session_id=json_extract($1,'$.binding.session') AND b.source_incarnation_id=json_extract($1,'$.binding.incarnation') AND b.capability_envelope=json_extract($1,'$.binding.envelope') AND ((r.state='resolved' AND r.response_json=s.response_json AND (s.publication_ready_at_ms=0 OR s.state='ended')) OR (r.state IN('starting','failed') AND r.response_json IS NULL))))";
        let resources="NOT EXISTS(SELECT 1 FROM job_leases j WHERE j.resource='session:'||s.incarnation_id AND (j.owner_node_id<>s.owner_node_id OR j.fence<>s.owner_epoch OR (j.expires_at_ms<>s.lease_expires_at_ms AND (s.state<>'ended' OR j.expires_at_ms>s.lease_expires_at_ms)))) AND NOT EXISTS(SELECT 1 FROM cache_consumer_pins p WHERE p.consumer_kind='media_session' AND p.consumer_id=s.incarnation_id AND p.consumer_epoch<>s.owner_epoch) AND NOT EXISTS(SELECT 1 FROM media_playback_pointers p WHERE p.current_incarnation_id=s.incarnation_id AND (p.user_id<>s.user_id OR p.playback_id<>s.playback_id))";
        // Closure remains the private daemon factory's responsibility. This
        // same-write predicate refuses durable release with any open ingress
        // obligation, including an already-authorized late Register.
        let custody = "EXISTS(SELECT 1 FROM sharing_ingress_custody c WHERE c.principal_kind='receiver' AND c.incarnation_id=s.incarnation_id AND json_extract(c.custody_json,'$.version')=1 AND json_extract(c.custody_json,'$.sealed')=1 AND NOT EXISTS(SELECT 1 FROM json_each(c.custody_json,'$.slots') slot WHERE json_extract(slot.value,'$.closed_confirmation') IS NULL))";
        // A never-dispatched disposition additionally requires the durable
        // `none` dispatch marker: a recorded (or unknown) dispatch owes End.
        let before=format!("EXISTS(SELECT 1 FROM media_sessions s JOIN media_session_requests r ON {request} JOIN sharing_relay_upstream b ON {upstream} WHERE {identity} AND s.state IN('active','ended') AND (json_extract($2,'$.disposition')<>'never_dispatched' OR b.dispatch_envelope='none') AND ({resources}) AND ({custody}))");
        let after=format!("EXISTS(SELECT 1 FROM media_sessions s JOIN media_session_requests r ON {request} WHERE {identity} AND s.state='ended' AND s.response_json=$2 AND s.terminal_reason IS NOT NULL AND s.publication_ready_at_ms=0 AND r.state IN('resolved','failed') AND NOT EXISTS(SELECT 1 FROM sharing_delivery_grants g WHERE g.incarnation_id=s.incarnation_id AND g.state<>'revoked') AND NOT EXISTS(SELECT 1 FROM sharing_relay_upstream b WHERE b.incarnation_id=s.incarnation_id) AND NOT EXISTS(SELECT 1 FROM job_leases j WHERE j.resource='session:'||s.incarnation_id) AND NOT EXISTS(SELECT 1 FROM cache_consumer_pins p WHERE p.consumer_kind='media_session' AND p.consumer_id=s.incarnation_id) AND NOT EXISTS(SELECT 1 FROM media_playback_pointers p WHERE p.current_incarnation_id=s.incarnation_id) AND ({custody}))");
        let read = retirement_ordered(
            &format!("SELECT 'replay' AS payload WHERE ({after})"),
            vals.clone(),
        )?;
        let replay = self.sharing_read(&read.0, read.1).await?;
        if !replay.is_empty() {
            let assertion = retirement_ordered(&source_assert(after, vec![]).0, vals)?;
            return match self.sharing_txn(vec![assertion]).await {
                Ok(_) => Ok(ReceiverRetirementOutcome::Replay),
                Err(e) if source_write_refused(&e) => Ok(ReceiverRetirementOutcome::Refused),
                Err(e) => Err(e),
            };
        }
        let inc = "json_extract($1,'$.incarnation')";
        let sqls=vec![source_assert(before,vec![]).0,
      format!("UPDATE media_sessions SET state='ended',response_json=$2,terminal_reason=coalesce(terminal_reason,$3),publication_ready_at_ms=0,updated_at_ms=$4 WHERE incarnation_id={inc}"),
      format!("UPDATE media_session_requests SET state='failed',updated_at_ms=$4 WHERE incarnation_id={inc} AND user_id=json_extract($1,'$.user') AND request_id=json_extract($1,'$.request') AND state='starting'"),
      format!("DELETE FROM job_leases WHERE resource='session:'||{inc}"),
      format!("DELETE FROM cache_consumer_pins WHERE consumer_kind='media_session' AND consumer_id={inc} AND consumer_epoch=json_extract($1,'$.epoch')"),
      format!("DELETE FROM media_playback_pointers WHERE current_incarnation_id={inc} AND user_id=json_extract($1,'$.user')"),
      format!("UPDATE sharing_delivery_grants SET state='revoked' WHERE incarnation_id={inc} AND state='active'"),
      format!("DELETE FROM sharing_relay_upstream WHERE incarnation_id={inc}"),source_assert(after,vec![]).0];
        let statements = sqls
            .into_iter()
            .map(|sql| retirement_ordered(&sql, vals.clone()))
            .collect::<Result<Vec<_>, _>>()?;
        match self.sharing_txn(statements).await {
            Ok(_) => Ok(ReceiverRetirementOutcome::Applied),
            Err(e) if source_write_refused(&e) => Ok(ReceiverRetirementOutcome::Refused),
            Err(e) => Err(e),
        }
    }
}

/// Metadata-only test witness: this fixture does not assert physical settlement.
#[cfg(test)]
pub(crate) struct MetadataRetirementFixture {
    pub intent: crate::sharing_receiver_sessions::ReceiverSessionIntent,
    pub attachment: crate::sharing_receiver_sessions::ReceiverSourceAttachment,
    pub confirmation: String,
}
#[cfg(test)]
impl ReceiverRetirementWitness for MetadataRetirementFixture {
    fn intent(&self) -> &crate::sharing_receiver_sessions::ReceiverSessionIntent {
        &self.intent
    }
    fn owner(&self) -> &crate::sharing_receiver_sessions::ReceiverSourceOwner {
        &self.attachment.owner
    }
    fn binding(&self) -> Option<&crate::sharing_receiver_sessions::ReceiverSourceBinding> {
        Some(&self.attachment.binding)
    }
    fn disposition(&self) -> ReceiverRetirementDisposition {
        ReceiverRetirementDisposition::SourceSettled
    }
    fn reason(&self) -> ReceiverRetirementReason {
        ReceiverRetirementReason::AdminStop
    }
    fn confirmation_id(&self) -> &str {
        &self.confirmation
    }
}
/// Metadata-only fixture setup. It creates no producer/driver or closure
/// receipt; raw model witnesses exercise SQL fencing, not daemon ownership.
#[cfg(test)]
pub(crate) async fn install_model_ingress_fence<T: Backend + super::MediaSessionStore>(
    store: &T,
    incarnation: &str,
) {
    use super::SharingReceiverIngressStore;
    let Some(route) = store
        .media_session_route_by_incarnation(incarnation)
        .await
        .expect("model route")
    else {
        return;
    };
    if store
        .receiver_ingress_snapshot(&route)
        .await
        .expect("model custody")
        .is_none()
    {
        let identity = crate::auth::hash_token(&format!(
            "plurx.metadata-only.receiver.fence\0{incarnation}"
        ));
        store
            .seal_receiver_ingress_route(&route, &identity)
            .await
            .expect("empty model seal");
    }
}

#[cfg(test)]
pub(crate) async fn metadata_retirement_matrix<T: Backend + super::MediaSessionStore>(
    store: &T,
    mut witness: MetadataRetirementFixture,
    deleted: bool,
) {
    let inc = witness.attachment.owner.incarnation_id;
    install_model_ingress_fence(store, &inc.to_string()).await;
    // Metadata corruption fixture: even an expired retained grant must revoke
    // atomically; expiry by itself is never Source settlement evidence.
    store.sharing_txn(vec![("INSERT INTO sharing_delivery_grants(token_hash,incarnation_id,source_token_hash,state,deadline_ms) VALUES($1,$2,$3,'active',1)".into(),vec![inc.simple().to_string().repeat(2).into(),inc.into(),witness.intent.login_hash.clone().into()])]).await.expect("retained grant fixture");
    store.sharing_txn(vec![("CREATE TRIGGER retirement_ignore_grant BEFORE UPDATE ON sharing_delivery_grants BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("ignored grant revocation");
    assert_eq!(
        store
            .retire_receiver_session(&witness)
            .await
            .expect("grant revocation must settle"),
        ReceiverRetirementOutcome::Refused
    );
    store
        .sharing_txn(vec![(
            "DROP TRIGGER retirement_ignore_grant".into(),
            vec![],
        )])
        .await
        .expect("restore grant writer");
    let census="SELECT json_array((SELECT json_group_array(json_array(incarnation_id,state,response_json,lease_expires_at_ms,updated_at_ms)) FROM media_sessions),(SELECT json_group_array(json_array(incarnation_id,state,response_json,updated_at_ms)) FROM media_session_requests),(SELECT json_group_array(json_array(resource,owner_node_id,fence,expires_at_ms)) FROM job_leases),(SELECT json_group_array(json_array(user_id,playback_id,current_incarnation_id)) FROM media_playback_pointers),(SELECT count(*) FROM sharing_relay_upstream),(SELECT json_group_array(json_array(session_id,response_json,updated_at_ms)) FROM media_session_terminal_acks)) AS payload";
    let before = store
        .sharing_read(census, vec![])
        .await
        .expect("retirement preimage");
    let node = witness.attachment.owner.owner_node_id.clone();
    witness.attachment.owner.owner_node_id = "foreign-node".into();
    assert_eq!(
        store
            .retire_receiver_session(&witness)
            .await
            .expect("wrong owner refuses"),
        ReceiverRetirementOutcome::Refused
    );
    assert_eq!(
        store.sharing_read(census, vec![]).await.expect("unchanged"),
        before
    );
    witness.attachment.owner.owner_node_id = node;
    store.sharing_txn(vec![("CREATE TRIGGER retirement_ignore_route BEFORE UPDATE ON media_sessions BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("ignored write");
    assert_eq!(
        store
            .retire_receiver_session(&witness)
            .await
            .expect("ignored write refuses"),
        ReceiverRetirementOutcome::Refused
    );
    assert_eq!(
        store.sharing_read(census, vec![]).await.expect("rollback"),
        before
    );
    store
        .sharing_txn(vec![(
            "DROP TRIGGER retirement_ignore_route".into(),
            vec![],
        )])
        .await
        .expect("remove fixture");
    store.sharing_txn(vec![("INSERT INTO cache_consumer_pins(storage_id,recipe_hash,generation_id,consumer_kind,consumer_id,consumer_epoch,expires_at_ms) VALUES('fixture-storage','fixture-recipe','fixture-generation','media_session',$1,$2,0)".into(),vec![inc.into(),(witness.attachment.owner.owner_epoch+1).into()])]).await.expect("foreign pin epoch");
    assert_eq!(
        store
            .retire_receiver_session(&witness)
            .await
            .expect("foreign pin refuses"),
        ReceiverRetirementOutcome::Refused
    );
    store
        .sharing_txn(vec![(
            "UPDATE cache_consumer_pins SET consumer_epoch=$1 WHERE consumer_id=$2".into(),
            vec![witness.attachment.owner.owner_epoch.into(), inc.into()],
        )])
        .await
        .expect("actual owned pin");
    for table in [
        "job_leases",
        "cache_consumer_pins",
        "sharing_relay_upstream",
    ] {
        store.sharing_txn(vec![(format!("CREATE TRIGGER retirement_ignore_delete BEFORE DELETE ON {table} BEGIN SELECT RAISE(IGNORE); END"),vec![])]).await.expect("ignored resource deletion");
        let before = store
            .sharing_read(census, vec![])
            .await
            .expect("resource preimage");
        assert_eq!(
            store
                .retire_receiver_session(&witness)
                .await
                .expect("ignored cleanup refuses"),
            ReceiverRetirementOutcome::Refused
        );
        assert_eq!(
            store
                .sharing_read(census, vec![])
                .await
                .expect("resource rollback"),
            before
        );
        store
            .sharing_txn(vec![(
                "DROP TRIGGER retirement_ignore_delete".into(),
                vec![],
            )])
            .await
            .expect("remove fixture");
    }
    // A genuine guarded successor becomes current for the same playback.
    // Retirement of the old Source obligation must leave this owner untouched.
    use super::SharingReceiverSessionStore;
    let mut next_intent = witness.intent.clone();
    next_intent.recipe.source_request_id = uuid::Uuid::new_v4();
    let next_authority = store
        .prepare_receiver_session_authority(next_intent.clone())
        .await
        .expect("successor authority")
        .expect("current original login");
    let actual = crate::store::sharing::wall_clock_ms().expect("clock");
    let next = crate::domain::MediaSessionActivation {
        incarnation_id: next_intent.recipe.source_request_id.to_string(),
        session_id: uuid::Uuid::new_v4().to_string(),
        principal: crate::playback_principal::PlaybackPrincipal::LocalUser {
            user_id: witness.intent.user_id,
        },
        playback_id: "B-playback".into(),
        recovery_epoch: uuid::Uuid::new_v4().to_string(),
        expected_predecessor_incarnation_id: Some(inc.to_string()),
        fence_predecessor: true,
        request_id: Some("retirement-successor".into()),
        request_fingerprint: next_intent
            .recipe
            .request_fingerprint()
            .expect("fingerprint"),
        owner_node_id: "successor-node".into(),
        recipe_json: serde_json::to_string(&next_intent.recipe).expect("recipe"),
        response_json: "{}".into(),
        publication_ready_at_ms: crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,
        media_origin_ms: witness.intent.source_position_ms,
        now_ms: actual,
        lease_expires_at_ms: actual + 30000,
        expected_desired_revision: None,
    };
    assert!(matches!(
        store
            .claim_media_session_request(
                &next.principal,
                "retirement-successor",
                &next.request_fingerprint,
                &next.playback_id,
                &next.incarnation_id,
                actual,
                actual + 30000
            )
            .await
            .expect("successor request"),
        crate::domain::MediaSessionRequestClaim::Acquired { .. }
    ));
    assert!(store
        .assign_media_session_request_owner(
            &next.principal,
            "retirement-successor",
            &next.incarnation_id,
            &next.owner_node_id,
            actual
        )
        .await
        .expect("actual successor request owner"));
    assert!(store
        .activate_receiver_media_session(&next_authority, &next)
        .await
        .expect("actual guarded same-playback successor")
        .is_some());
    // Lost login/import authority is cleanup-only; the actual user-delete trigger
    // can already end/zero/fail/remove pointer, while retaining Source obligations.
    if deleted {
        store
            .sharing_txn(vec![(
                "DELETE FROM users WHERE id=$1".into(),
                vec![witness.intent.user_id.into()],
            )])
            .await
            .expect("actual user deletion");
        let terminal = store
            .media_session_route_by_incarnation(&inc.to_string())
            .await
            .expect("actual terminal metadata")
            .expect("retained old owner");
        assert_eq!(
            terminal.session_id,
            witness.attachment.owner.session_id.to_string()
        );
        assert_eq!(
            terminal.owner_node_id,
            witness.attachment.owner.owner_node_id
        );
        assert_eq!(terminal.owner_epoch, witness.attachment.owner.owner_epoch);
        witness.attachment.owner.lease_expires_at_ms = terminal.lease_expires_at_ms;
    } else {
        store
            .sharing_txn(vec![
                (
                    "DELETE FROM tokens WHERE token_hash=$1".into(),
                    vec![witness.intent.login_hash.clone().into()],
                ),
                ("UPDATE sharing_imports SET state='disabled'".into(), vec![]),
                (
                    "DELETE FROM media_playback_pointers WHERE current_incarnation_id=$1".into(),
                    vec![inc.into()],
                ),
            ])
            .await
            .expect("logout/import revocation and displaced old pointer");
        let actual = crate::store::sharing::wall_clock_ms().expect("clock");
        let ack = crate::domain::MediaSessionTerminalAck {
            incarnation_id: inc.to_string(),
            session_id: witness.attachment.owner.session_id.to_string(),
            owner_node_id: witness.attachment.owner.owner_node_id.clone(),
            owner_epoch: witness.attachment.owner.owner_epoch,
            client_instance_id: uuid::Uuid::new_v4().to_string(),
            sequence: 1,
            request_fingerprint: witness
                .intent
                .recipe
                .request_fingerprint()
                .expect("fingerprint"),
            response_json: "{}".into(),
            expires_at_ms: actual + 30000,
            updated_at_ms: actual,
        };
        assert!(store
            .record_media_session_terminal_ack(&ack)
            .await
            .expect("actual terminal ack, not Source proof"));
        witness.attachment.owner.lease_expires_at_ms = actual;
    }
    let successor_sql="SELECT json_array((SELECT json_group_array(json_array(incarnation_id,state,response_json,lease_expires_at_ms,updated_at_ms)) FROM media_sessions WHERE incarnation_id<>$1),(SELECT json_group_array(json_array(incarnation_id,state,response_json,updated_at_ms)) FROM media_session_requests WHERE incarnation_id<>$1),(SELECT json_group_array(json_array(resource,owner_node_id,fence,expires_at_ms)) FROM job_leases WHERE resource<>'session:'||$1),(SELECT json_group_array(json_array(user_id,playback_id,current_incarnation_id)) FROM media_playback_pointers WHERE current_incarnation_id<>$1)) AS payload";
    let successors = store
        .sharing_read(successor_sql, vec![inc.into()])
        .await
        .expect("actual second B owner");
    assert_eq!(
        store
            .retire_receiver_session(&witness)
            .await
            .expect("confirmed metadata retirement"),
        ReceiverRetirementOutcome::Applied
    );
    assert_eq!(
        store
            .sharing_read(successor_sql, vec![inc.into()])
            .await
            .expect("successor untouched"),
        successors
    );
    assert_eq!(store.sharing_read("SELECT CAST(count(*) AS TEXT) AS payload FROM sharing_delivery_grants WHERE incarnation_id=$1 AND state<>'revoked'",vec![inc.into()]).await.expect("all exact grants revoked"),vec!["0".to_string()]);
    let settled = store.sharing_read(census, vec![]).await.expect("receipt");
    assert_eq!(
        store
            .retire_receiver_session(&witness)
            .await
            .expect("lost response replay"),
        ReceiverRetirementOutcome::Replay
    );
    assert_eq!(
        store
            .sharing_read(census, vec![])
            .await
            .expect("read only replay"),
        settled
    );
    if !deleted {
        store
            .sharing_txn(vec![(
                "DELETE FROM users WHERE id=$1".into(),
                vec![witness.intent.user_id.into()],
            )])
            .await
            .expect("user deletion after terminal receipt");
        assert_eq!(
            store
                .retire_receiver_session(&witness)
                .await
                .expect("trigger-cleared reply never restored on retry"),
            ReceiverRetirementOutcome::Replay
        );
    }
    let settled = store
        .sharing_read(census, vec![])
        .await
        .expect("current generic terminal metadata");
    witness.confirmation = "b".repeat(64);
    assert_eq!(
        store
            .retire_receiver_session(&witness)
            .await
            .expect("different confirmation refuses"),
        ReceiverRetirementOutcome::Refused
    );
    assert_eq!(
        store.sharing_read(census, vec![]).await.expect("no rebind"),
        settled
    );
}

fn retirement_ordered(
    sql: &str,
    values: Vec<Value>,
) -> Result<super::sharing::Statement, StoreError> {
    // Both backends require every supplied parameter to occur. These validated
    // context/receipt/reason/clock operands are bound even on read-only assertions.
    ordered(
        &format!("{sql} AND $1 IS NOT NULL AND $2 IS NOT NULL AND $3 IS NOT NULL AND $4>0"),
        values,
    )
}

#[cfg(test)]
pub(crate) struct PendingMetadataFixture {
    pub intent: crate::sharing_receiver_sessions::ReceiverSessionIntent,
    pub owner: crate::sharing_receiver_sessions::ReceiverSourceOwner,
    pub disposition: ReceiverRetirementDisposition,
}
#[cfg(test)]
impl ReceiverRetirementWitness for PendingMetadataFixture {
    fn intent(&self) -> &crate::sharing_receiver_sessions::ReceiverSessionIntent {
        &self.intent
    }
    fn owner(&self) -> &crate::sharing_receiver_sessions::ReceiverSourceOwner {
        &self.owner
    }
    fn binding(&self) -> Option<&crate::sharing_receiver_sessions::ReceiverSourceBinding> {
        None
    }
    fn disposition(&self) -> ReceiverRetirementDisposition {
        self.disposition
    }
    fn reason(&self) -> ReceiverRetirementReason {
        ReceiverRetirementReason::AdminStop
    }
    fn confirmation_id(&self) -> &str {
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    }
}
#[cfg(test)]
pub(crate) async fn pending_metadata_retirement_matrix<T: Backend + super::MediaSessionStore>(
    store: &T,
    w: PendingMetadataFixture,
) {
    let inc = w.owner.incarnation_id;
    install_model_ingress_fence(store, &inc.to_string()).await;
    store.sharing_txn(vec![("UPDATE sharing_relay_upstream SET source_session_id='partial' WHERE incarnation_id=$1".into(),vec![inc.into()]),("CREATE TRIGGER retirement_ignore_assertion BEFORE INSERT ON sharing_relay_upstream BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("partial binding plus ignored assertion");
    assert_eq!(
        store
            .retire_receiver_session(&w)
            .await
            .expect("partial binding refuses before trigger"),
        ReceiverRetirementOutcome::Refused
    );
    store
        .sharing_txn(vec![
            ("DROP TRIGGER retirement_ignore_assertion".into(), vec![]),
            (
                "UPDATE sharing_relay_upstream SET source_session_id=NULL WHERE incarnation_id=$1"
                    .into(),
                vec![inc.into()],
            ),
        ])
        .await
        .expect("restore all NULL pending");
    assert_eq!(
        store
            .retire_receiver_session(&w)
            .await
            .expect("metadata-only owned no-send fixture"),
        ReceiverRetirementOutcome::Applied
    );
    let before=store.sharing_read("SELECT json_array(response_json,updated_at_ms) AS payload FROM media_sessions WHERE incarnation_id=$1",vec![inc.into()]).await.expect("terminal receipt");
    assert_eq!(
        store
            .retire_receiver_session(&w)
            .await
            .expect("exact pending retry"),
        ReceiverRetirementOutcome::Replay
    );
    assert_eq!(store.sharing_read("SELECT json_array(response_json,updated_at_ms) AS payload FROM media_sessions WHERE incarnation_id=$1",vec![inc.into()]).await.expect("read-only pending retry"),before);
}

#[cfg(test)]
mod pending_request_tests {
    use super::*;
    use crate::{
        domain::MediaSessionRequestClaim,
        playback_principal::PlaybackPrincipal,
        sharing::SourceId,
        sharing_catalogue::SharedReference,
        sharing_catalogue_details::FileRevision,
        sharing_receiver_sessions::{
            ReceiverProducerKind, ReceiverSessionIntent, RemoteSourceRecipe,
        },
        store::{
            sharing_catalogue::ReceiverCatalogueScope, MediaSessionStore, SqliteStore, UserStore,
        },
    };
    use uuid::Uuid;
    struct MetadataPending {
        intent: ReceiverSessionIntent,
        owner: ReceiverPendingOwner,
    }
    impl ReceiverPendingRetirementWitness for MetadataPending {
        fn intent(&self) -> &ReceiverSessionIntent {
            &self.intent
        }
        fn request_id(&self) -> &str {
            "pending-request"
        }
        fn playback_id(&self) -> &str {
            "pending-player"
        }
        fn owner(&self) -> &ReceiverPendingOwner {
            &self.owner
        }
        fn confirmation_id(&self) -> &str {
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        }
    }
    #[tokio::test]
    async fn sharing_receiver_pending_retirement_requires_exact_claim_and_absent_resources() {
        let _clock = crate::store::sharing::LogicalClock::install();
        let dir = tempfile::tempdir().expect("pool");
        for rebuilt in [false, true] {
            for pooled in [false, true] {
                for assigned in [false, true] {
                    let store = if pooled {
                        SqliteStore::open(
                            &dir.path()
                                .join(format!("{rebuilt}-{pooled}-{assigned}.sqlite")),
                        )
                        .expect("pool")
                    } else {
                        SqliteStore::open_in_memory().expect("memory")
                    };
                    if rebuilt {
                        store
                            .sharing_txn(
                                super::super::MEDIA_SESSION_PRINCIPAL_REBUILD_SCHEMA
                                    .split("-- next statement\n")
                                    .map(|s| (s.trim().trim_end_matches(';').into(), vec![]))
                                    .collect(),
                            )
                            .await
                            .expect("exact principal layout");
                    }
                    let user = store
                        .create_user("pending-owner", "hash", false)
                        .await
                        .expect("real user");
                    let hash = "b".repeat(64);
                    store
                        .create_token(&hash, user.id, None)
                        .await
                        .expect("actual original login");
                    let scope = ReceiverCatalogueScope {
                        import_id: Uuid::new_v4(),
                        source_server_id: Uuid::new_v4(),
                        catalogue_epoch: Uuid::new_v4(),
                        lifecycle_generation: 1,
                        assignment_generation: 1,
                        endpoint_generation: 1,
                        claim_id: Uuid::new_v4(),
                        remote_grant_id: Uuid::new_v4(),
                        libraries: vec![SourceId::parse("0").expect("zero")],
                    };
                    let recipe = RemoteSourceRecipe {
                        kind: ReceiverProducerKind::RemoteSource,
                        version: 1,
                        reference: SharedReference {
                            import_id: scope.import_id,
                            server_id: scope.source_server_id,
                            catalogue_epoch: scope.catalogue_epoch,
                            library_id: scope.libraries[0].clone(),
                            item_id: SourceId::parse("9223372036854775807").expect("item"),
                        },
                        lifecycle_generation: 1,
                        file_id: SourceId::parse("0").expect("file"),
                        file_revision: FileRevision::parse(&"c".repeat(64)).expect("revision"),
                        source_request_id: Uuid::new_v4(),
                        parent_login_hash: hash.clone(),
                        request_json: "{\"start\":17}".into(),
                    };
                    let mut w = MetadataPending {
                        intent: ReceiverSessionIntent {
                            scope,
                            user_id: user.id,
                            login_hash: hash,
                            recipe,
                            source_position_ms: 0,
                        },
                        owner: ReceiverPendingOwner::Unassigned,
                    };
                    store.sharing_txn(vec![("INSERT INTO sharing_viewers VALUES($1,$2)".into(),vec![user.id.into(),Uuid::new_v4().into()]),("INSERT INTO sharing_imports(id,source_server_id,catalogue_epoch,source_name,claim_id,remote_grant_id,credential_envelope,endpoints_json,assignment_generation,lifecycle_generation,endpoint_generation,state,created_at_ms,updated_at_ms) VALUES($1,$2,$3,'Source',$4,$5,'unopened metadata fixture','[]',1,1,1,'active',1,1)".into(),vec![w.intent.scope.import_id.into(),w.intent.scope.source_server_id.into(),w.intent.scope.catalogue_epoch.into(),w.intent.scope.claim_id.into(),w.intent.scope.remote_grant_id.into()]),("INSERT INTO sharing_assignments VALUES($1,'0',$2,1)".into(),vec![w.intent.scope.import_id.into(),user.id.into()])]).await.expect("actual B import/assignment metadata before revocation");
                    let principal = PlaybackPrincipal::LocalUser { user_id: user.id };
                    let inc = w.intent.recipe.source_request_id.to_string();
                    let now = crate::store::sharing::wall_clock_ms().expect("clock");
                    assert!(matches!(
                        store
                            .claim_media_session_request(
                                &principal,
                                w.request_id(),
                                &w.intent.recipe.request_fingerprint().expect("fp"),
                                w.playback_id(),
                                &inc,
                                now,
                                now + 30000
                            )
                            .await
                            .expect("genuine claim"),
                        MediaSessionRequestClaim::Acquired { .. }
                    ));
                    if assigned {
                        assert!(store
                            .assign_media_session_request_owner(
                                &principal,
                                w.request_id(),
                                &inc,
                                "B-owned",
                                now
                            )
                            .await
                            .expect("real assignment"));
                        w.owner = ReceiverPendingOwner::Assigned("B-owned".into());
                    }
                    let read="SELECT json_array(user_id,request_id,request_fingerprint,playback_id,incarnation_id,owner_node_id,state,response_json,claim_expires_at_ms,updated_at_ms) AS payload FROM media_session_requests WHERE incarnation_id=$1";
                    let retained_intent = w.intent.clone();
                    w.intent.login_hash = "e".repeat(64);
                    w.intent.recipe.parent_login_hash = w.intent.login_hash.clone();
                    assert_eq!(
                        store
                            .retire_pending_receiver_request(&w)
                            .await
                            .expect("wrong original login lineage"),
                        ReceiverRetirementOutcome::Refused
                    );
                    w.intent = retained_intent.clone();
                    w.intent.recipe.request_json = "{\"start\":18}".into();
                    assert_eq!(
                        store
                            .retire_pending_receiver_request(&w)
                            .await
                            .expect("changed original recipe"),
                        ReceiverRetirementOutcome::Refused
                    );
                    w.intent = retained_intent;
                    let original = store
                        .sharing_read(read, vec![inc.clone().into()])
                        .await
                        .expect("snapshot");
                    store.sharing_txn(vec![("CREATE TRIGGER pending_ignore_assertion BEFORE INSERT ON sharing_relay_upstream BEGIN SELECT RAISE(IGNORE); END".into(),vec![]),("UPDATE media_session_requests SET owner_node_id='foreign' WHERE incarnation_id=$1".into(),vec![inc.clone().into()])]).await.expect("foreign takeover plus ignored assertion");
                    let foreign = store
                        .sharing_read(read, vec![inc.clone().into()])
                        .await
                        .expect("foreign snapshot");
                    assert_eq!(
                        store
                            .retire_pending_receiver_request(&w)
                            .await
                            .expect("foreign refuses"),
                        ReceiverRetirementOutcome::Refused
                    );
                    assert_eq!(
                        store
                            .sharing_read(read, vec![inc.clone().into()])
                            .await
                            .expect("unchanged"),
                        foreign
                    );
                    store.sharing_txn(vec![("UPDATE media_session_requests SET owner_node_id=json_extract($1,'$') WHERE incarnation_id=$2".into(),vec![if assigned {Value::Text("\"B-owned\"".into())}else{Value::Text("null".into())},inc.clone().into()]),("CREATE TRIGGER pending_ignore_update BEFORE UPDATE ON media_session_requests BEGIN SELECT RAISE(IGNORE); END".into(),vec![])]).await.expect("restore real captured owner plus ignored update");
                    assert_eq!(
                        store
                            .retire_pending_receiver_request(&w)
                            .await
                            .expect("suppressed UPDATE refuses"),
                        ReceiverRetirementOutcome::Refused
                    );
                    assert_eq!(
                        store
                            .sharing_read(read, vec![inc.clone().into()])
                            .await
                            .expect("whole rollback"),
                        original
                    );
                    store
                        .sharing_txn(vec![
                            ("DROP TRIGGER pending_ignore_update".into(), vec![]),
                            (
                                "INSERT INTO job_leases VALUES($1,'B-owned',1,1,1,1)".into(),
                                vec![format!("session:{inc}").into()],
                            ),
                        ])
                        .await
                        .expect("even expired resource is retained");
                    assert_eq!(
                        store
                            .retire_pending_receiver_request(&w)
                            .await
                            .expect("expired lease is not no-send proof"),
                        ReceiverRetirementOutcome::Refused
                    );
                    store
                        .sharing_txn(vec![
                            (
                                "DELETE FROM job_leases WHERE resource=$1".into(),
                                vec![format!("session:{inc}").into()],
                            ),
                            (
                                "DELETE FROM sharing_imports WHERE id=$1".into(),
                                vec![w.intent.scope.import_id.into()],
                            ),
                            if assigned {
                                ("DELETE FROM users WHERE id=$1".into(), vec![user.id.into()])
                            } else {
                                (
                                    "DELETE FROM tokens WHERE token_hash=$1".into(),
                                    vec![w.intent.login_hash.clone().into()],
                                )
                            },
                        ])
                        .await
                        .expect("cleanup after actual user deletion");
                    // This is an actual metadata failure writer, not physical
                    // evidence. The test witness remains metadata-only.
                    if assigned {
                        let _changed = store
                            .fail_media_session_request(&principal, w.request_id(), &inc, now + 1)
                            .await
                            .expect("existing generic failure state");
                        assert_eq!(store.sharing_read("SELECT state AS payload FROM media_session_requests WHERE incarnation_id=$1",vec![inc.clone().into()]).await.expect("actual failed metadata"),vec!["failed".to_string()]);
                    }
                    assert_eq!(
                        store
                            .retire_pending_receiver_request(&w)
                            .await
                            .expect("revoked cleanup"),
                        if assigned {
                            ReceiverRetirementOutcome::Replay
                        } else {
                            ReceiverRetirementOutcome::Applied
                        }
                    );
                    assert_eq!(store.sharing_read("SELECT state AS payload FROM media_session_requests WHERE incarnation_id=$1",vec![inc.clone().into()]).await.expect("failed"),vec!["failed".to_string()]);
                    let ended = store
                        .sharing_read(read, vec![inc.clone().into()])
                        .await
                        .expect("failed");
                    assert_eq!(
                        store
                            .retire_pending_receiver_request(&w)
                            .await
                            .expect("exact retry"),
                        ReceiverRetirementOutcome::Replay
                    );
                    assert_eq!(
                        store
                            .sharing_read(read, vec![inc.into()])
                            .await
                            .expect("read-only"),
                        ended
                    );
                }
            }
        }
    }
}
