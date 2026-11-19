//! Transfer a winning worker into the existing pending playback preparation.
use super::adoption::{check_open_winner, open_winner_assertion, route_json};
use super::*;
use crate::domain::{MediaSessionRoute, MEDIA_SESSION_PUBLICATION_BLOCKED};
use crate::store::{
    MEDIA_SESSION_MAX_CURRENT_PER_USER, MEDIA_SESSION_MAX_MEDIA_MILLIS, MEDIA_SESSION_MAX_OWNED,
    MEDIA_SESSION_MAX_SESSION_ROWS_PER_USER,
};

/// The first staged path retains a private verified worker product until
/// readiness. Its recipe has no mutable first-attachment cache receipt.
fn exact_preparation(input: &AdaptiveStagedAdoption) -> Result<bool, StoreError> {
    let binding = &input.binding;
    let key = &input.key;
    let p = &input.preparation;
    if key.ordinal != 1
        || key.owner_epoch != 1
        || p.incarnation_id != key.route_incarnation_id.to_string()
        || p.session_id != key.session_id.to_string()
        || p.principal != binding.principal
        || p.playback_id != binding.playback_id
        || p.request_fingerprint != binding.request_fingerprint
        || p.owner_node_id != key.worker_node_id
        || p.expected_predecessor_incarnation_id == p.incarnation_id
        || binding
            .expected_predecessor_incarnation_id
            .is_none_or(|id| p.expected_predecessor_incarnation_id != id.to_string())
        || Some(p.expected_predecessor_owner_epoch) != binding.expected_predecessor_owner_epoch
        || !text(&p.expected_predecessor_owner_node_id, 256)
        || p.expected_desired_revision != binding.expected_desired_revision
        || p.quality_cancellation_key
            .as_deref()
            .is_none_or(|value| !digest(value))
        || p.now_ms != input.context.now_ms
        || p.deadline_ms <= p.now_ms
        || !(-MEDIA_SESSION_MAX_MEDIA_MILLIS..=MEDIA_SESSION_MAX_MEDIA_MILLIS)
            .contains(&p.media_origin_ms)
        || p.response_json.len() > 64 * 1024
        || binding.key.request_id.starts_with("jellyfin:")
        || selected_recipe_digest(&p.recipe_json)? != input.worker.selected_recipe_digest
    {
        return Ok(false);
    }
    let recipe: serde_json::Value = serde_json::from_str(&p.recipe_json).map_err(|_| invalid())?;
    let response: serde_json::Value =
        serde_json::from_str(&p.response_json).map_err(|_| invalid())?;
    Ok(recipe.is_object()
        && recipe["library_channel"].is_null()
        && recipe["retained_output"].is_null()
        && response.is_object()
        && response.get("prepared_output_capture_pending").is_none()
        && response.get("prepared_output_capture_complete").is_none())
}

async fn staged_outcome<B: Backend>(
    store: &B,
    input: &AdaptiveStagedAdoption,
) -> Result<MediaSessionActivationOutcome, StoreError> {
    #[derive(serde::Deserialize)]
    struct Snapshot {
        route: MediaSessionRoute,
        predecessor: MediaSessionRoute,
    }
    let p = &input.preparation;
    let layout = store.attempt_layout().await?;
    let owner = quote(&p.principal.owner_key());
    let user = p.principal.local_user_id().ok_or_else(invalid)?;
    let rows = store
        .attempt_read(format!(
            "SELECT CASE WHEN ({}) THEN json_object('route',json({}),
             'predecessor',json({})) ELSE 'unavailable' END AS payload
             FROM media_sessions s JOIN media_sessions predecessor
             ON predecessor.incarnation_id={}
             WHERE s.incarnation_id={} AND s.user_id={user} AND predecessor.user_id={user}
             AND {}={owner} AND {}={owner} AND s.playback_id={} AND predecessor.playback_id=s.playback_id",
            schema::ready_guard_for(layout, store.attempt_outage_fence()),
            route_json("s"),
            route_json("predecessor"),
            quote(&p.expected_predecessor_incarnation_id),
            quote(&p.incarnation_id),
            layout.owner("s"),
            layout.owner("predecessor"),
            quote(&p.playback_id),
        ))
        .await?;
    let [value] = rows.as_slice() else {
        return Err(invalid());
    };
    let Snapshot { route, predecessor } = serde_json::from_str(value).map_err(|_| invalid())?;
    if route.incarnation_id != p.incarnation_id
        || route.session_id != p.session_id
        || route.principal != p.principal
        || route.playback_id != p.playback_id
        || route.request_fingerprint != p.request_fingerprint
        || route.recovery_epoch != input.binding.recovery_epoch.to_string()
        || route.owner_node_id != p.owner_node_id
        || route.owner_epoch != input.key.owner_epoch
        || route.recipe_json != p.recipe_json
        || route.response_json != p.response_json
        || route.media_origin_ms != p.media_origin_ms
        || predecessor.incarnation_id != p.expected_predecessor_incarnation_id
    {
        return Err(invalid());
    }
    Ok(MediaSessionActivationOutcome {
        route,
        predecessor: Some(predecessor),
    })
}

pub(crate) async fn stage<B: Backend>(
    store: &B,
    input: &AdaptiveStagedAdoption,
) -> Result<AttemptAdoption, StoreError> {
    input.worker.validate(&input.binding, &input.key)?;
    // The replicated transaction and guard use the existing Adopt opcode.
    // The digest separates the staged receipt from an ordinary activation.
    let operation = Operation {
        context: &input.context,
        binding: &input.binding,
        key: &input.key,
        action: "adopt",
        payload: payload("staged-adopt", input)?,
    };
    if let Some(result) = admit(store, &operation).await? {
        return match result {
            AttemptMutation::Replay(_) => {
                Ok(AttemptAdoption::Replay(staged_outcome(store, input).await?))
            }
            AttemptMutation::Refused(reason) => Ok(AttemptAdoption::Refused(reason)),
            AttemptMutation::Applied(_) => Err(invalid()),
        };
    }
    if !exact_preparation(input)? {
        return Ok(AttemptAdoption::Refused(AttemptRefusal::Conflict));
    }
    if let Some(reason) = check_open_winner(store, &operation, &input.worker).await? {
        return Ok(AttemptAdoption::Refused(reason));
    }

    let binding = &input.binding;
    let p = &input.preparation;
    let layout = store.attempt_layout().await?;
    let outage = store.attempt_outage_fence();
    let user = binding.principal.local_user_id().ok_or_else(invalid)?;
    let now = p.now_ms;
    let deadline = p.deadline_ms;
    let blocked = MEDIA_SESSION_PUBLICATION_BLOCKED;
    let owner = quote(&binding.principal.owner_key());
    let playback = quote(&p.playback_id);
    let predecessor = quote(&p.expected_predecessor_incarnation_id);
    let predecessor_node = quote(&p.expected_predecessor_owner_node_id);
    let predecessor_epoch = p.expected_predecessor_owner_epoch;
    let successor = quote(&p.incarnation_id);
    let session = quote(&p.session_id);
    let worker = quote(&p.owner_node_id);
    let lease = quote(&format!("session:{}", p.incarnation_id));
    let recovery = quote(&binding.recovery_epoch.to_string());
    let fingerprint = quote(&p.request_fingerprint);
    let recipe = quote(&p.recipe_json);
    let response = quote(&p.response_json);
    let root_owner = layout.owner("root");
    let prepared_owner = layout.owner("prep");
    let route_owner = layout.owner("s");
    let desired_owner = layout.owner("desired");
    let desired_revision = p
        .expected_desired_revision
        .map_or_else(|| "NULL".into(), |n| n.to_string());
    let cancellation = quote(p.quality_cancellation_key.as_deref().ok_or_else(invalid)?);
    let root_id = quote(&binding.key.request_id);

    let mut statements = operation.open(layout, outage)?;
    statements.push(open_winner_assertion(&operation, &input.worker)?);
    if outage == schema::OutageFence::Replicated {
        statements.push(schema::assertion(&schema::worker_unfenced_guard(
            &p.owner_node_id,
        )));
    }
    statements.extend(playback_context(binding, layout));
    // Fence a replacement owner and a changed ask inside the same write.
    statements.push(schema::assertion(&format!(
        "EXISTS(SELECT 1 FROM media_sessions s JOIN job_leases l
         ON l.resource='session:'||s.incarnation_id
         WHERE {route_owner}={owner} AND s.user_id={user} AND s.incarnation_id={predecessor}
         AND s.playback_id={playback} AND s.owner_node_id={predecessor_node}
         AND s.owner_epoch={predecessor_epoch} AND s.state='active' AND s.publication_ready_at_ms=0
         AND s.recovery_epoch={recovery} AND s.lease_expires_at_ms>{now}
         AND l.owner_node_id=s.owner_node_id AND l.fence=s.owner_epoch AND l.expires_at_ms=s.lease_expires_at_ms)
         AND (SELECT revision FROM media_playback_desired desired WHERE {desired_owner}={owner}
         AND desired.playback_id={playback}) IS {desired_revision}"
    )));
    statements.push(schema::assertion(&format!(
        "EXISTS(SELECT 1 FROM media_session_requests root WHERE {root_owner}={owner}
         AND root.request_id={root_id} AND root.incarnation_id={} AND root.request_fingerprint={fingerprint}
         AND root.playback_id={playback} AND root.state='starting' AND root.claim_expires_at_ms={}
         AND root.claim_expires_at_ms>{now} AND EXISTS(SELECT 1 FROM users WHERE id={user}))",
        quote(&binding.key.request_incarnation_id.to_string()),
        binding.original_deadline_ms
    )));
    // Apply the same session, worker and cancellation limits as an ordinary preparation.
    statements.push(schema::assertion(&format!(
        "NOT EXISTS(SELECT 1 FROM media_session_preparations prep WHERE
         ({prepared_owner}={owner} AND prep.playback_id={playback}) OR prep.staged_incarnation_id={successor})
         AND NOT EXISTS(SELECT 1 FROM quality_cancellation_receipts WHERE receipt_key={cancellation})
         AND NOT EXISTS(SELECT 1 FROM quality_preparation_owners
         WHERE staged_incarnation_id={successor} OR cancellation_key={cancellation})
         AND NOT EXISTS(SELECT 1 FROM settings WHERE key={})
         AND (SELECT count(*) FROM media_sessions s WHERE {route_owner}={owner}
         AND s.state IN('starting','active') AND s.lease_expires_at_ms>{now}
         AND s.incarnation_id!={successor})<{MEDIA_SESSION_MAX_CURRENT_PER_USER}
         AND (SELECT count(*) FROM media_sessions s WHERE {route_owner}={owner}
         AND s.incarnation_id!={successor})<{MEDIA_SESSION_MAX_SESSION_ROWS_PER_USER}
         AND (SELECT count(*) FROM media_sessions s WHERE s.owner_node_id={worker}
         AND s.state='active' AND s.lease_expires_at_ms>{now}
         AND s.incarnation_id!={successor})<{MEDIA_SESSION_MAX_OWNED}",
        quote(&crate::cluster::coordination::removed_job_owner_key(&p.owner_node_id))
    )));
    // Convert the one existing route and lease in place under its original epoch.
    statements.push(schema::assertion(&format!(
        "EXISTS(SELECT 1 FROM media_sessions s JOIN job_leases l ON l.resource={lease}
         WHERE s.incarnation_id={successor} AND s.session_id={session} AND s.user_id={user}
         AND {route_owner}={owner} AND s.playback_id={playback} AND s.request_fingerprint={fingerprint}
         AND s.recovery_epoch={recovery} AND s.owner_node_id={worker} AND s.owner_epoch=1
         AND s.state='starting' AND s.publication_ready_at_ms={blocked}
         AND s.lease_expires_at_ms>{now} AND l.expires_at_ms=s.lease_expires_at_ms
         AND l.owner_node_id=s.owner_node_id AND l.fence=s.owner_epoch AND l.revision<9223372036854775807)"
    )));
    statements.push(format!(
        "UPDATE job_leases SET expires_at_ms={deadline},revision=revision+1,updated_at_ms={now}
         WHERE resource={lease} AND owner_node_id={worker} AND fence=1"
    ));
    statements.push(format!(
        "UPDATE media_sessions SET state='active',recipe_json={recipe},response_json={response},
         media_origin_ms={},lease_expires_at_ms={deadline},updated_at_ms={now}
         WHERE incarnation_id={successor} AND state='starting'",
        p.media_origin_ms
    ));
    let (columns, values) = if layout == Layout::Principal {
        (
            ",owner_key,principal_kind,share_grant_id,share_viewer_key",
            format!(",{owner},'local',NULL,NULL"),
        )
    } else {
        ("", String::new())
    };
    statements.push(format!(
        "INSERT INTO media_session_preparations(user_id,playback_id,staged_incarnation_id,
         expected_predecessor_incarnation_id,deadline_ms,created_at_ms,updated_at_ms{columns})
         VALUES({user},{playback},{successor},{predecessor},{deadline},{now},{now}{values})"
    ));
    statements.push(format!(
        "INSERT INTO quality_preparation_owners(staged_incarnation_id,cancellation_key)
         VALUES({successor},{cancellation})"
    ));
    statements.push(format!(
        "UPDATE media_session_requests AS root SET incarnation_id={successor},owner_node_id={worker},
         state='resolved',response_json={response},updated_at_ms={now}
         WHERE {root_owner}={owner} AND root.request_id={root_id}"
    ));
    statements.push(advance_request(&operation, ",disposition='adopted'"));
    statements.push(advance_attempt(&operation, ""));
    statements.extend(playback_context(binding, layout));
    statements.push(schema::assertion(&format!(
        "EXISTS(SELECT 1 FROM media_sessions s JOIN job_leases l ON l.resource={lease}
         JOIN media_session_preparations prep ON prep.staged_incarnation_id=s.incarnation_id
         JOIN quality_preparation_owners q ON q.staged_incarnation_id=s.incarnation_id
         WHERE s.incarnation_id={successor} AND s.session_id={session} AND s.owner_node_id={worker}
         AND s.owner_epoch=1 AND s.state='active' AND s.publication_ready_at_ms={blocked}
         AND s.recipe_json={recipe} AND s.response_json={response} AND s.lease_expires_at_ms={deadline}
         AND l.owner_node_id=s.owner_node_id AND l.fence=s.owner_epoch AND l.expires_at_ms={deadline}
         AND prep.expected_predecessor_incarnation_id={predecessor} AND prep.deadline_ms={deadline}
         AND q.cancellation_key={cancellation})
         AND EXISTS(SELECT 1 FROM media_session_requests root WHERE {root_owner}={owner}
         AND root.request_id={root_id} AND root.incarnation_id={successor} AND root.owner_node_id={worker}
         AND root.state='resolved' AND root.response_json={response})"
    )));
    operation.finish(&mut statements)?;
    store.attempt_transaction(statements).await?;
    Ok(AttemptAdoption::Applied(
        staged_outcome(store, input).await?,
    ))
}
