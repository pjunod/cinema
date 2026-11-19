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

/// Viewer commits also run against legacy databases without the adjunct.
/// Only use the scoped SQL when all its referenced adaptive tables exist.
pub(crate) const VIEWER_COMMIT_SCHEMA: &str = "SELECT CAST(COUNT(*)=5 AS TEXT) AS payload
    FROM sqlite_master WHERE type='table' AND name IN
    ('media_resource_schema','media_resource_requests','media_resource_attempts',
     'media_resource_operations','media_resource_assertions')";

pub(crate) struct ViewerCommitClaim {
    pub(crate) open: String,
    pub(crate) pointer_guard: String,
    pub(crate) close: String,
}

/// Give the existing viewer CAS the one pointer mutation the adaptive guard
/// requires. The claim has no session or operation fields and is removed
/// before the transaction finishes. It cannot adopt a second winner.
pub(crate) fn viewer_commit_claim(
    layout: Layout,
    outage: schema::OutageFence,
    staged: &crate::domain::MediaSessionStagedGeneration,
    request: &crate::domain::MediaSessionPreparationCommitRequest,
) -> ViewerCommitClaim {
    let owner = quote(&staged.principal.owner_key());
    let playback = quote(&staged.playback_id);
    let next = quote(&staged.staged_incarnation_id);
    let prior = quote(&staged.expected_predecessor_incarnation_id);
    let prior_node = quote(&request.expected_predecessor_owner_node_id);
    let prior_epoch = request.expected_predecessor_owner_epoch;
    let now = request.now_ms;
    let blocked = MEDIA_SESSION_PUBLICATION_BLOCKED;
    let ask = request
        .expected_desired_revision
        .map_or_else(|| "NULL".into(), |revision| revision.to_string());
    let (receipt, exact_predecessor) = request.control_receipt.as_ref().map_or_else(
        || ("NULL".to_owned(), "0".to_owned()),
        |receipt| {
            (
                quote(&receipt.request_fingerprint),
                format!(
                    "old.incarnation_id={} AND old.session_id={}",
                    quote(&receipt.incarnation_id),
                    quote(&receipt.session_id)
                ),
            )
        },
    );
    let claim = format!(
        "json_object('action','adopt','route_incarnation_id',{next},'viewer_commit',{receipt})"
    );
    let active_claim = format!(
        "EXISTS(SELECT 1 FROM media_resource_schema WHERE singleton=1 AND mutation_claim={claim})"
    );
    let after_handoff = layout.owner("s");
    let before_handoff = layout.owner("old");
    let prep_owner = layout.owner("p");
    let desired_owner = layout.owner("desired");
    let pointer_owner = layout.owner("pointer");
    let root_owner = layout.owner("root");
    let ready = schema::ready_guard_for(layout, outage);
    let removed = quote(&crate::cluster::coordination::removed_job_owner_key(""));
    let worker_unfenced = match outage {
        schema::OutageFence::Standalone => "1",
        schema::OutageFence::Replicated => {
            "NOT EXISTS(SELECT 1 FROM cluster_operation_leases WHERE node_id=s.owner_node_id)
            AND NOT EXISTS(SELECT 1 FROM cluster_node_maintenance WHERE node_id=s.owner_node_id)"
        }
    };
    ViewerCommitClaim {
        open: format!(
            "UPDATE media_resource_schema SET mutation_claim={claim}
             WHERE singleton=1 AND revision<9223372036854775807 AND ({ready})
             AND EXISTS(SELECT 1 FROM media_resource_attempts a
             JOIN media_resource_requests r USING(owner_key,request_id,request_incarnation_id)
             JOIN media_resource_operations op ON op.operation_id=r.last_operation_id
             JOIN media_sessions s ON s.incarnation_id=a.route_incarnation_id
             JOIN media_session_requests root ON root.request_id=r.request_id AND {root_owner}={owner}
             JOIN media_session_preparations p ON p.staged_incarnation_id=s.incarnation_id
             JOIN media_sessions old ON old.incarnation_id=p.expected_predecessor_incarnation_id
             JOIN media_playback_pointers pointer ON pointer.current_incarnation_id=old.incarnation_id
             JOIN quality_preparation_owners q ON q.staged_incarnation_id=s.incarnation_id
             JOIN job_leases lease ON lease.resource='session:'||s.incarnation_id
             JOIN job_leases old_lease ON old_lease.resource='session:'||old.incarnation_id
             WHERE a.route_incarnation_id={next} AND r.owner_key={owner} AND a.phase='winner'
             AND a.ordinal=1 AND r.winner_attempt_id=a.attempt_id AND r.disposition='adopted'
             AND op.action='adopt' AND op.attempt_id=a.attempt_id
             AND op.owner_key=r.owner_key AND op.request_id=r.request_id
             AND op.request_incarnation_id=r.request_incarnation_id
             AND op.payload_digest=r.last_payload_digest AND op.payload_digest=a.last_payload_digest
             AND op.request_sequence=r.sequence AND op.attempt_sequence=a.sequence
             AND json_extract(r.binding_json,'$.expected_predecessor_incarnation_id')={prior}
             AND json_extract(r.binding_json,'$.expected_predecessor_owner_epoch')={prior_epoch}
             AND json_extract(r.binding_json,'$.expected_desired_revision') IS {ask}
             AND (SELECT revision FROM media_playback_desired desired
             WHERE {desired_owner}={owner} AND desired.playback_id={playback}) IS {ask}
             AND {after_handoff}={owner} AND {before_handoff}={owner}
             AND {prep_owner}={owner} AND {pointer_owner}={owner}
             AND s.playback_id={playback} AND old.playback_id={playback}
             AND p.playback_id={playback} AND pointer.playback_id={playback}
             AND s.state='active' AND s.publication_ready_at_ms={blocked}
             AND s.request_fingerprint=json_extract(r.binding_json,'$.request_fingerprint')
             AND root.state='resolved' AND root.incarnation_id=s.incarnation_id
             AND root.request_fingerprint=s.request_fingerprint AND root.playback_id={playback}
             AND root.owner_node_id=s.owner_node_id
             AND s.owner_node_id=a.worker_node_id AND s.owner_epoch=a.owner_epoch AND a.owner_epoch=1
             AND json_extract(a.worker_json,'$.node_id')=s.owner_node_id
             AND json_extract(a.worker_json,'$.worker_instance_id')=a.worker_instance_id
             AND json_extract(a.worker_json,'$.planning_generation')=json_extract(r.binding_json,'$.planning_generation')
             AND json_extract(a.worker_json,'$.planning_source_digest')=json_extract(r.binding_json,'$.planning_source_digest')
             AND s.session_id=a.session_id AND s.recovery_epoch=old.recovery_epoch
             AND s.recovery_epoch=json_extract(r.binding_json,'$.recovery_epoch')
             AND s.lease_expires_at_ms=p.deadline_ms AND p.deadline_ms>{now}
             AND lease.owner_node_id=s.owner_node_id AND lease.fence=s.owner_epoch
             AND lease.expires_at_ms=s.lease_expires_at_ms AND lease.revision<9223372036854775807
             AND old.incarnation_id={prior} AND old.owner_node_id={prior_node} AND old.owner_epoch={prior_epoch}
             AND old.state='active' AND old.publication_ready_at_ms=0 AND old.lease_expires_at_ms>{now}
             AND old_lease.owner_node_id=old.owner_node_id AND old_lease.fence=old.owner_epoch
             AND old_lease.expires_at_ms=old.lease_expires_at_ms AND ({exact_predecessor})
             AND NOT EXISTS(SELECT 1 FROM quality_cancellation_receipts c WHERE c.receipt_key=q.cancellation_key)
             AND NOT EXISTS(SELECT 1 FROM settings WHERE key={removed}||s.owner_node_id)
             AND ({worker_unfenced}))"
        ),
        pointer_guard: format!(
            "AND (NOT EXISTS(SELECT 1 FROM media_resource_attempts WHERE route_incarnation_id={next})
             OR {active_claim})"
        ),
        close: format!(
            "UPDATE media_resource_schema SET mutation_claim=NULL,revision=revision+1
             WHERE singleton=1 AND mutation_claim={claim}"
        ),
    }
}
