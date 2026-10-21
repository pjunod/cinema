//! Adaptive activation branch: no second ordinary activation after commit.
use super::*;

/// Private validated plan, never a deserializable activation capability.
pub(crate) struct AdoptionPlan {
    pub(crate) input: AdaptiveAdoption,
    statements: Vec<String>,
}

fn route_json(alias: &str) -> String {
    let fields = [
        "incarnation_id",
        "session_id",
        "playback_id",
        "recovery_epoch",
        "request_fingerprint",
        "owner_node_id",
        "owner_epoch",
        "lease_expires_at_ms",
        "state",
        "terminal_reason",
        "publication_ready_at_ms",
        "recipe_json",
        "response_json",
        "produced_playable_through_ms",
        "fetched_through_ms",
        "media_origin_ms",
        "media_sequence",
        "discontinuity_sequence",
        "updated_at_ms",
        "drain_deadline_ms",
    ];
    let mut entries = fields
        .iter()
        .map(|field| format!("'{field}',{alias}.{field}"))
        .collect::<Vec<_>>();
    entries.push(format!(
        "'principal',json_object('kind','local','user_id',{alias}.user_id)"
    ));
    format!("json_object({})", entries.join(","))
}

async fn outcome<B: Backend>(
    store: &B,
    input: &AdaptiveAdoption,
) -> Result<MediaSessionActivationOutcome, StoreError> {
    let layout = store.attempt_layout().await?;
    let predecessor = input
        .binding
        .expected_predecessor_incarnation_id
        .map_or_else(|| "NULL".into(), |id| quote(&id.to_string()));
    let rows=store.attempt_read(format!("SELECT CASE WHEN ({}) THEN json_object('route',json({}),'predecessor',json((SELECT {} FROM media_sessions p WHERE p.incarnation_id={predecessor} AND p.user_id={} AND p.playback_id={}))) ELSE 'unavailable' END AS payload FROM media_sessions s WHERE s.incarnation_id={}",schema::ready_guard(layout),route_json("s"),route_json("p"),input.binding.principal.local_user_id().ok_or_else(invalid)?,quote(&input.binding.playback_id),quote(&input.key.route_incarnation_id.to_string()))).await?;
    let [row] = rows.as_slice() else {
        return Err(invalid());
    };
    let value: serde_json::Value = serde_json::from_str(row).map_err(|_| invalid())?;
    let route: crate::domain::MediaSessionRoute =
        serde_json::from_value(value.get("route").cloned().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
    let predecessor: Option<crate::domain::MediaSessionRoute> =
        serde_json::from_value(value.get("predecessor").cloned().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
    let activation = &input.activation;
    if route.incarnation_id != input.key.route_incarnation_id.to_string()
        || route.session_id != input.key.session_id.to_string()
        || route.owner_epoch != input.key.owner_epoch
        || route.owner_node_id != input.key.worker_node_id
        || route.principal != input.binding.principal
        || route.playback_id != input.binding.playback_id
        || route.request_fingerprint != input.binding.request_fingerprint
        || route.recovery_epoch != input.binding.recovery_epoch.to_string()
        || route.recipe_json != activation.recipe_json
        || route.response_json != activation.response_json
        || route.media_origin_ms != activation.media_origin_ms
    {
        return Err(invalid());
    }
    Ok(MediaSessionActivationOutcome { route, predecessor })
}

/// Called from activate_with_authority's private typed adaptive branch.
pub(crate) async fn commit_adoption<B: Backend>(
    store: &B,
    plan: &AdoptionPlan,
) -> Result<Option<MediaSessionActivationOutcome>, StoreError> {
    store.attempt_transaction(plan.statements.clone()).await?;
    Ok(Some(outcome(store, &plan.input).await?))
}

pub(crate) async fn adopt<B: Backend>(
    store: &B,
    input: &AdaptiveAdoption,
) -> Result<AttemptAdoption, StoreError> {
    input.worker.validate(&input.binding, &input.key)?;
    let operation = Operation {
        context: &input.context,
        binding: &input.binding,
        key: &input.key,
        action: "adopt",
        payload: payload("adopt", input)?,
    };
    if let Some(result) = admit(store, &operation).await? {
        return match result {
            AttemptMutation::Replay(_) => Ok(AttemptAdoption::Replay(outcome(store, input).await?)),
            AttemptMutation::Refused(reason) => Ok(AttemptAdoption::Refused(reason)),
            AttemptMutation::Applied(_) => Err(invalid()),
        };
    }
    let binding = &input.binding;
    let key = &input.key;
    let activation = &input.activation;
    if activation.incarnation_id != key.route_incarnation_id.to_string()
        || activation.session_id != key.session_id.to_string()
        || activation.principal != binding.principal
        || activation.playback_id != binding.playback_id
        || activation.recovery_epoch != binding.recovery_epoch.to_string()
        || activation.request_id.as_deref() != Some(binding.key.request_id.as_str())
        || activation.request_fingerprint != binding.request_fingerprint
        || activation.owner_node_id != key.worker_node_id
        || !activation.fence_predecessor
        || activation.expected_predecessor_incarnation_id
            != binding
                .expected_predecessor_incarnation_id
                .map(|id| id.to_string())
        || activation.expected_desired_revision != binding.expected_desired_revision
        || activation.publication_ready_at_ms != crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
        || activation.now_ms != input.context.now_ms
        || activation.lease_expires_at_ms <= activation.now_ms
        || activation.recipe_json.len() > 32 * 1024
        || selected_recipe_digest(&activation.recipe_json)? != input.worker.selected_recipe_digest
        || activation.response_json.len() > 64 * 1024
        || serde_json::from_str::<serde_json::Value>(&activation.recipe_json).is_err()
        || serde_json::from_str::<serde_json::Value>(&activation.response_json).is_err()
    {
        return Ok(AttemptAdoption::Refused(AttemptRefusal::Conflict));
    }
    let Some(current) = status(store, &binding.key, &binding.principal).await? else {
        return Ok(AttemptAdoption::Refused(AttemptRefusal::StaleBinding));
    };
    if current.binding != *binding {
        return Ok(AttemptAdoption::Refused(AttemptRefusal::Conflict));
    }
    if current.winner_attempt_id != Some(key.attempt_id)
        || current.disposition != RequestDisposition::Open
    {
        return Ok(AttemptAdoption::Refused(AttemptRefusal::WinnerLost));
    }
    if current.sequence != input.context.expected_request_sequence
        || !current.attempts.iter().any(|a| {
            a.key == *key
                && a.sequence == input.context.expected_attempt_sequence
                && a.phase == AttemptPhase::Winner
                && a.worker.as_ref() == Some(&input.worker)
        })
    {
        return Ok(AttemptAdoption::Refused(AttemptRefusal::StaleBinding));
    }
    if input.context.now_ms >= input.worker.prepared_expires_at_ms {
        return Ok(AttemptAdoption::Refused(AttemptRefusal::Expired));
    }
    let layout = store.attempt_layout().await?;
    let user = binding.principal.local_user_id().ok_or_else(invalid)?;
    let mut statements = operation.open(layout)?;
    statements.push(schema::assertion(&format!("({}) AND ({}) AND EXISTS(SELECT 1 FROM media_resource_requests r WHERE {} AND r.disposition='open' AND r.winner_attempt_id={}) AND EXISTS(SELECT 1 FROM media_resource_attempts WHERE attempt_id={} AND phase='winner' AND worker_json={})",exact_request(&operation)?,exact_attempt(&operation)?,request_predicate(binding,"r"),quote(&key.attempt_id.to_string()),quote(&key.attempt_id.to_string()),quote(&encode(&input.worker)?))));
    let pointer_owner = layout.owner("pointer");
    let predecessor = binding
        .expected_predecessor_incarnation_id
        .map_or_else(|| "NULL".into(), |id| quote(&id.to_string()));
    let pointer=format!("(SELECT current_incarnation_id FROM media_playback_pointers pointer WHERE {pointer_owner}={} AND pointer.playback_id={})",quote(&binding.principal.owner_key()),quote(&binding.playback_id));
    statements.push(schema::assertion(&format!("{pointer} IS {predecessor}")));
    if let Some(epoch) = binding.expected_predecessor_owner_epoch {
        statements.push(schema::assertion(&format!("EXISTS(SELECT 1 FROM media_sessions WHERE incarnation_id={predecessor} AND user_id={user} AND playback_id={} AND owner_epoch={epoch} AND state='active' AND publication_ready_at_ms=0)",quote(&binding.playback_id))));
    }
    if let Some(revision) = binding.expected_desired_revision {
        let desired_owner = layout.owner("desired");
        statements.push(schema::assertion(&format!("NOT EXISTS(SELECT 1 FROM media_playback_desired desired WHERE {desired_owner}={} AND desired.playback_id={} AND desired.revision!={revision})",quote(&binding.principal.owner_key()),quote(&binding.playback_id))));
    }
    let root_owner = layout.owner("root");
    statements.push(schema::assertion(&format!("EXISTS(SELECT 1 FROM media_session_requests root WHERE {root_owner}={} AND root.request_id={} AND root.incarnation_id={} AND root.request_fingerprint={} AND root.playback_id={} AND root.state='starting' AND root.claim_expires_at_ms={} AND root.claim_expires_at_ms>{} AND EXISTS(SELECT 1 FROM users WHERE id={user}))",quote(&binding.principal.owner_key()),quote(&binding.key.request_id),quote(&binding.key.request_incarnation_id.to_string()),quote(&binding.request_fingerprint),quote(&binding.playback_id),binding.original_deadline_ms,input.context.now_ms)));
    statements.push(schema::assertion(&format!("EXISTS(SELECT 1 FROM media_sessions s JOIN job_leases l ON l.resource='session:'||s.incarnation_id WHERE s.incarnation_id={} AND s.session_id={} AND s.user_id={user} AND s.playback_id={} AND s.request_fingerprint={} AND s.recovery_epoch={} AND s.owner_node_id={} AND s.owner_epoch={} AND s.state='starting' AND s.publication_ready_at_ms={} AND l.owner_node_id=s.owner_node_id AND l.fence=s.owner_epoch AND l.expires_at_ms>{} AND l.revision<9223372036854775807)",quote(&key.route_incarnation_id.to_string()),quote(&key.session_id.to_string()),quote(&binding.playback_id),quote(&binding.request_fingerprint),quote(&binding.recovery_epoch.to_string()),quote(&key.worker_node_id),key.owner_epoch,crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,input.context.now_ms)));
    statements.push(format!("UPDATE job_leases SET expires_at_ms={},revision=revision+1,updated_at_ms={} WHERE resource={} AND owner_node_id={} AND fence={}",activation.lease_expires_at_ms,input.context.now_ms,quote(&format!("session:{}",key.route_incarnation_id)),quote(&key.worker_node_id),key.owner_epoch));
    statements.push(format!("UPDATE media_sessions SET state='active',recipe_json={},response_json={},media_origin_ms={},lease_expires_at_ms={},updated_at_ms={} WHERE incarnation_id={} AND state='starting'",quote(&activation.recipe_json),quote(&activation.response_json),activation.media_origin_ms,activation.lease_expires_at_ms,input.context.now_ms,quote(&key.route_incarnation_id.to_string())));
    statements.push(format!("UPDATE media_sessions SET state='ended',terminal_reason='superseded',lease_expires_at_ms={},publication_ready_at_ms={},updated_at_ms={} WHERE incarnation_id={predecessor} AND user_id={user} AND playback_id={} AND state!='ended'",input.context.now_ms,crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED,input.context.now_ms,quote(&binding.playback_id)));
    statements.push(format!("DELETE FROM cache_consumer_pins WHERE consumer_kind='media_session' AND consumer_id={predecessor}"));
    statements.push(format!("UPDATE job_leases SET expires_at_ms=min(expires_at_ms,{}),revision=revision+1,updated_at_ms={} WHERE resource='session:'||{predecessor} AND revision<9223372036854775807 AND EXISTS(SELECT 1 FROM media_sessions s WHERE s.incarnation_id={predecessor} AND s.owner_node_id=job_leases.owner_node_id AND s.owner_epoch=job_leases.fence)",input.context.now_ms,input.context.now_ms));
    let (column, columns, values) = if layout == Layout::Principal {
        (
            "owner_key",
            ",owner_key,principal_kind,share_grant_id,share_viewer_key",
            format!(
                ",{},'local',NULL,NULL",
                quote(&binding.principal.owner_key())
            ),
        )
    } else {
        ("user_id", "", String::new())
    };
    let desired_owner = layout.owner("desired");
    statements.push(format!("INSERT INTO media_playback_pointers(user_id,playback_id,current_incarnation_id,updated_at_ms,desired_revision{columns}) VALUES({user},{},{},{},(SELECT revision FROM media_playback_desired desired WHERE {desired_owner}={} AND playback_id={}){values}) ON CONFLICT({column},playback_id) DO UPDATE SET current_incarnation_id=excluded.current_incarnation_id,updated_at_ms=excluded.updated_at_ms,desired_revision=excluded.desired_revision",quote(&binding.playback_id),quote(&key.route_incarnation_id.to_string()),input.context.now_ms,quote(&binding.principal.owner_key()),quote(&binding.playback_id)));
    statements.push(format!("UPDATE media_session_requests AS root SET incarnation_id={},owner_node_id={},state='resolved',response_json={},updated_at_ms={} WHERE {root_owner}={} AND request_id={}",quote(&key.route_incarnation_id.to_string()),quote(&key.worker_node_id),quote(&activation.response_json),input.context.now_ms,quote(&binding.principal.owner_key()),quote(&binding.key.request_id)));
    statements.push(advance_request(&operation, ",disposition='adopted'"));
    statements.push(advance_attempt(&operation, ""));
    statements.push(schema::assertion(&format!("{pointer}={} AND EXISTS(SELECT 1 FROM media_sessions WHERE incarnation_id={} AND state='active' AND owner_epoch={} AND publication_ready_at_ms={})",quote(&key.route_incarnation_id.to_string()),quote(&key.route_incarnation_id.to_string()),key.owner_epoch,crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED)));
    operation.finish(&mut statements)?;
    let plan = AdoptionPlan {
        input: input.clone(),
        statements,
    };
    let outcome = store.attempt_activate(&plan).await?.ok_or_else(invalid)?;
    Ok(AttemptAdoption::Applied(outcome))
}
