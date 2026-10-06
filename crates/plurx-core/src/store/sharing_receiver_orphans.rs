//! Crash recovery inventory and exclusive takeover claim for B RemoteSource
//! routes whose owner stopped renewing them. Lease expiry and row shape only
//! make a route *claimable*; they are never Source settlement evidence. The
//! claim fences every old-epoch writer, and the ordinary exact retirement
//! still requires the daemon's confirmed Source End or the durable no-send
//! marker.
use super::{
    sharing::{ordered, Backend, Statement, Value},
    sharing_receiver_sessions::{source_assert, source_write_refused},
};
use crate::{
    error::StoreError,
    sharing::{invalid, SourceId},
    sharing_catalogue_details::FileRevision,
    sharing_receiver_retirement::{
        ClaimedReceiverOrphan, ReceiverOrphan, ReceiverOrphanClaimOutcome, ReceiverOrphanDispatch,
        ReceiverOrphanStrandedReason,
    },
    sharing_receiver_sessions::{
        ReceiverSessionIntent, ReceiverSourceBinding, ReceiverSourceOwner, RemoteSourceRecipe,
    },
    store::sharing_catalogue::ReceiverCatalogueScope,
};
use serde::Deserialize;
use uuid::Uuid;

/// The same grace the generic session maintenance waits past an unrenewed
/// lease before it treats the owner as gone.
pub const RECEIVER_ORPHAN_GRACE_MS: i64 = 60_000;
/// The claimed route's lease: long enough for the bounded retirement owner
/// (Source End plus Store retries) without renewal. When it lapses, another
/// node may claim again at the next epoch; the old claim is then fenced.
pub const RECEIVER_ORPHAN_CLAIM_LEASE_MS: i64 = 180_000;
/// Largest inventory page.
pub const RECEIVER_ORPHAN_PAGE_MAX: usize = 16;

/// One keyset page. `unreadable` names rows that matched the orphan shape but
/// whose metadata cannot be decoded; they are reported, never claimed.
pub struct ReceiverOrphanPage {
    pub orphans: Vec<ReceiverOrphan>,
    pub unreadable: Vec<String>,
    pub next_cursor: Option<String>,
}

fn now_ms() -> Result<i64, StoreError> {
    super::sharing::wall_clock_ms()
}

fn removed_prefix() -> String {
    crate::cluster::coordination::removed_job_owner_key("")
}

fn text(column: &str, max: usize) -> String {
    format!("CASE WHEN typeof({column})='text' AND length(CAST({column} AS BLOB))<={max} THEN {column} ELSE NULL END")
}

const JOINS: &str = "FROM sharing_relay_upstream b JOIN media_sessions s ON s.incarnation_id=b.incarnation_id JOIN job_leases j ON j.resource='session:'||s.incarnation_id JOIN media_session_requests r ON r.incarnation_id=s.incarnation_id AND r.user_id=s.user_id";

/// Orphan shape over aliases s/b/j/r. `now`, `grace` and `prefix` are SQL
/// expressions. Pending (all Source columns NULL, unresolved request),
/// attached (all set) and partial bindings match; claimability is narrower.
fn orphan_shape(now: &str, grace: &str, prefix: &str) -> String {
    format!("json_valid(s.recipe_json) AND json_extract(s.recipe_json,'$.kind')='remote_source' AND s.state IN('active','ended') AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms<=s.lease_expires_at_ms AND r.owner_node_id=s.owner_node_id AND r.playback_id=s.playback_id AND r.request_fingerprint=s.request_fingerprint AND (s.lease_expires_at_ms<={now}-{grace} OR (s.lease_expires_at_ms<={now} AND EXISTS(SELECT 1 FROM settings WHERE key={prefix}||s.owner_node_id))) AND NOT (b.source_session_id IS NULL AND b.source_incarnation_id IS NULL AND b.capability_envelope IS NULL AND NOT (r.state IN('starting','failed') AND r.response_json IS NULL))")
}

pub(crate) async fn inventory<T: Backend>(
    store: &T,
    after: Option<&str>,
    limit: usize,
) -> Result<ReceiverOrphanPage, StoreError> {
    if limit == 0 || limit > RECEIVER_ORPHAN_PAGE_MAX {
        return Err(invalid());
    }
    let cursor = after.unwrap_or("");
    if cursor.len() > 36 {
        return Err(invalid());
    }
    let now = now_ms()?;
    let payload = format!(
        "json_object('inc',{},'session',{},'user',s.user_id,'playback',{},'node',{},'epoch',s.owner_epoch,'lease',s.lease_expires_at_ms,'recipe',{},'origin',s.media_origin_ms,'request',{},'import',{},'lifecycle',b.lifecycle_generation,'assignment',b.assignment_generation,'endpoint',b.endpoint_revision,'library',{},'item',{},'file',{},'revision',{},'source_request',{},'position',b.source_position_ms,'source_session',{},'source_incarnation',{},'envelope',{},'dispatch',{},'claim',{},'grant',{},'any_binding',(b.source_session_id IS NOT NULL OR b.source_incarnation_id IS NOT NULL OR b.capability_envelope IS NOT NULL))",
        text("s.incarnation_id", 36),
        text("s.session_id", 36),
        text("s.playback_id", 128),
        text("s.owner_node_id", 128),
        text("s.recipe_json", 65536),
        text("r.request_id", 128),
        text("b.import_id", 36),
        text("b.remote_library_id", 32),
        text("b.remote_item_id", 32),
        text("b.remote_file_id", 32),
        text("b.remote_revision", 64),
        text("b.source_request_id", 36),
        text("b.source_session_id", 36),
        text("b.source_incarnation_id", 36),
        text("b.capability_envelope", 4096),
        text("b.dispatch_envelope", 4096),
        text("i.claim_id", 36),
        text("i.remote_grant_id", 36),
    );
    let sql = format!(
        "SELECT {payload} AS payload {JOINS} LEFT JOIN sharing_imports i ON i.id=b.import_id WHERE b.incarnation_id>$4 AND {} ORDER BY b.incarnation_id LIMIT $5",
        orphan_shape("$1", "$2", "$3")
    );
    let rows = store
        .sharing_read(
            &sql,
            vec![
                now.into(),
                RECEIVER_ORPHAN_GRACE_MS.into(),
                removed_prefix().into(),
                cursor.to_owned().into(),
                i64::try_from(limit).map_err(|_| invalid())?.into(),
            ],
        )
        .await?;
    let full = rows.len() == limit;
    let mut page = ReceiverOrphanPage {
        orphans: Vec::new(),
        unreadable: Vec::new(),
        next_cursor: None,
    };
    let mut last = None;
    for row in rows {
        let value: serde_json::Value = serde_json::from_str(&row).map_err(|_| invalid())?;
        let incarnation = value
            .get("inc")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(invalid)?
            .to_owned();
        match decode(value, now) {
            Some(orphan) => page.orphans.push(orphan),
            None => page.unreadable.push(incarnation.clone()),
        }
        last = Some(incarnation);
    }
    if full {
        page.next_cursor = last;
    }
    Ok(page)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    inc: String,
    session: Option<String>,
    user: i64,
    playback: Option<String>,
    node: Option<String>,
    epoch: i64,
    lease: i64,
    recipe: Option<String>,
    origin: i64,
    request: Option<String>,
    import: Option<String>,
    lifecycle: i64,
    assignment: i64,
    endpoint: i64,
    library: Option<String>,
    item: Option<String>,
    file: Option<String>,
    revision: Option<String>,
    source_request: Option<String>,
    position: i64,
    source_session: Option<String>,
    source_incarnation: Option<String>,
    envelope: Option<String>,
    dispatch: Option<String>,
    claim: Option<String>,
    grant: Option<String>,
    any_binding: i64,
}

fn canonical(value: Option<&str>) -> Option<Uuid> {
    let value = value?;
    let parsed = Uuid::parse_str(value).ok()?;
    (!parsed.is_nil() && parsed.to_string() == value).then_some(parsed)
}

fn sealed(value: &str) -> Option<crate::secrets::SealedSecret> {
    let sealed = crate::secrets::SealedSecret::from_stored(value.to_owned());
    let persisted = sealed.to_persist().ok()?;
    // v1 XChaCha nonce (24 bytes) plus authentication tag (16 bytes).
    (persisted.len() <= 4096 && persisted.rsplit(':').next().is_some_and(|b| b.len() >= 80))
        .then_some(sealed)
}

/// Every durable column must agree with the canonical recipe. Anything that
/// does not decode is reported as unreadable rather than guessed.
fn decode(value: serde_json::Value, now: i64) -> Option<ReceiverOrphan> {
    let row: Row = serde_json::from_value(value).ok()?;
    let recipe_json = row.recipe?;
    let recipe: RemoteSourceRecipe = serde_json::from_str(&recipe_json).ok()?;
    let incarnation = canonical(Some(&row.inc))?;
    let session = canonical(row.session.as_deref())?;
    let request = row.request?;
    let playback = row.playback?;
    let node = row.node?;
    if serde_json::to_string(&recipe).ok()? != recipe_json
        || recipe.version != 1
        || recipe.source_request_id != incarnation
        || row.user <= 0
        || row.epoch <= 0
        || row.origin != row.position
        || row.position < 0
        || canonical(row.import.as_deref())? != recipe.reference.import_id
        || row.lifecycle != recipe.lifecycle_generation
        || row.assignment <= 0
        || row.endpoint <= 0
        || SourceId::parse(row.library.as_deref()?).ok()? != recipe.reference.library_id
        || SourceId::parse(row.item.as_deref()?).ok()? != recipe.reference.item_id
        || SourceId::parse(row.file.as_deref()?).ok()? != recipe.file_id
        || FileRevision::parse(row.revision.as_deref()?).ok()? != recipe.file_revision
        || canonical(row.source_request.as_deref())? != recipe.source_request_id
        || request.is_empty()
        || playback.is_empty()
        || node.is_empty()
        || [&request, &playback, &node]
            .iter()
            .any(|s| s.chars().any(char::is_control))
    {
        return None;
    }
    let complete = match (
        row.source_session.as_deref(),
        row.source_incarnation.as_deref(),
        row.envelope.as_deref(),
    ) {
        (Some(s), Some(i), Some(e)) => Some((canonical(Some(s))?, canonical(Some(i))?, sealed(e)?)),
        _ => None,
    };
    let partial = complete.is_none() && row.any_binding != 0;
    let binding = complete.map(
        |(session_id, incarnation_id, envelope)| ReceiverSourceBinding {
            reference: recipe.reference.clone(),
            file_id: recipe.file_id.clone(),
            file_revision: recipe.file_revision.clone(),
            source_request_id: recipe.source_request_id,
            source_session_id: session_id,
            source_incarnation_id: incarnation_id,
            capability_envelope: envelope,
        },
    );
    let dispatch = match row.dispatch.as_deref() {
        None => ReceiverOrphanDispatch::Unknown,
        Some("none") => ReceiverOrphanDispatch::NotDispatched,
        Some(value) => ReceiverOrphanDispatch::Sealed(sealed(value)?),
    };
    let stranded = if partial {
        Some(ReceiverOrphanStrandedReason::PartialBinding)
    } else if binding.is_none() && matches!(dispatch, ReceiverOrphanDispatch::Unknown) {
        Some(ReceiverOrphanStrandedReason::DispatchUnknown)
    } else {
        None
    };
    // Claim and grant identity are not part of retirement lineage. The import
    // row may already be gone (revocation is cleanup-only), so nil stands for
    // "no longer present" and is never used to authorize anything.
    let scope = ReceiverCatalogueScope {
        import_id: recipe.reference.import_id,
        source_server_id: recipe.reference.server_id,
        catalogue_epoch: recipe.reference.catalogue_epoch,
        lifecycle_generation: row.lifecycle,
        assignment_generation: row.assignment,
        endpoint_generation: row.endpoint,
        claim_id: canonical(row.claim.as_deref()).unwrap_or(Uuid::nil()),
        remote_grant_id: canonical(row.grant.as_deref()).unwrap_or(Uuid::nil()),
        libraries: vec![recipe.reference.library_id.clone()],
    };
    Some(ReceiverOrphan {
        intent: ReceiverSessionIntent {
            scope,
            user_id: row.user,
            login_hash: recipe.parent_login_hash.clone(),
            source_position_ms: row.position,
            recipe,
        },
        owner: ReceiverSourceOwner {
            incarnation_id: incarnation,
            session_id: session,
            owner_node_id: node,
            owner_epoch: row.epoch,
            request_id: request,
            lease_expires_at_ms: row.lease,
            now_ms: now,
        },
        playback_id: playback,
        binding,
        dispatch,
        stranded,
    })
}

fn claim_ordered(sql: &str, values: &[Value]) -> Result<Statement, StoreError> {
    // Both backends require every supplied parameter to occur.
    ordered(&format!("{sql} AND $2>0 AND $3>0"), values.to_vec())
}

pub(crate) async fn claim<T: Backend>(
    store: &T,
    orphan: &ReceiverOrphan,
    next: &str,
) -> Result<ReceiverOrphanClaimOutcome, StoreError> {
    let o = &orphan.owner;
    if orphan.stranded.is_some()
        || next.is_empty()
        || next.len() > 128
        || next.chars().any(char::is_control)
        || o.owner_epoch <= 0
        || o.owner_epoch == i64::MAX
    {
        return Ok(ReceiverOrphanClaimOutcome::Refused);
    }
    let now = now_ms()?;
    let lease = now
        .checked_add(RECEIVER_ORPHAN_CLAIM_LEASE_MS)
        .ok_or_else(invalid)?;
    let binding = orphan.binding.as_ref();
    let envelope = binding
        .map(|b| b.capability_envelope.to_persist().map(str::to_owned))
        .transpose()
        .map_err(|_| invalid())?;
    let dispatch = match &orphan.dispatch {
        ReceiverOrphanDispatch::NotDispatched => Some("none".to_owned()),
        ReceiverOrphanDispatch::Sealed(sealed) => {
            Some(sealed.to_persist().map_err(|_| invalid())?.to_owned())
        }
        ReceiverOrphanDispatch::Unknown => None,
    };
    let context = serde_json::json!({
        "inc": o.incarnation_id, "session": o.session_id, "user": orphan.intent.user_id,
        "playback": orphan.playback_id, "node": o.owner_node_id, "epoch": o.owner_epoch,
        "lease": o.lease_expires_at_ms,
        "recipe": serde_json::to_string(&orphan.intent.recipe).map_err(|_| invalid())?,
        "request": o.request_id,
        "source_session": binding.map(|b| b.source_session_id),
        "source_incarnation": binding.map(|b| b.source_incarnation_id),
        "envelope": envelope, "dispatch": dispatch, "next": next,
        "grace": RECEIVER_ORPHAN_GRACE_MS, "prefix": removed_prefix(),
        "removed_next": crate::cluster::coordination::removed_job_owner_key(next),
    })
    .to_string();
    let values = [
        Value::Text(context),
        Value::Integer(now),
        Value::Integer(lease),
    ];
    let lineage = "s.incarnation_id=json_extract($1,'$.inc') AND s.session_id=json_extract($1,'$.session') AND s.user_id=json_extract($1,'$.user') AND s.playback_id=json_extract($1,'$.playback') AND s.recipe_json=json_extract($1,'$.recipe') AND r.request_id=json_extract($1,'$.request') AND b.source_session_id IS json_extract($1,'$.source_session') AND b.source_incarnation_id IS json_extract($1,'$.source_incarnation') AND b.capability_envelope IS json_extract($1,'$.envelope') AND b.dispatch_envelope IS json_extract($1,'$.dispatch')";
    let claimable = "((b.source_session_id IS NOT NULL AND b.source_incarnation_id IS NOT NULL AND b.capability_envelope IS NOT NULL) OR (b.source_session_id IS NULL AND b.source_incarnation_id IS NULL AND b.capability_envelope IS NULL AND b.dispatch_envelope IS NOT NULL))";
    let before = format!(
        "EXISTS(SELECT 1 {JOINS} WHERE {lineage} AND s.owner_node_id=json_extract($1,'$.node') AND s.owner_epoch=json_extract($1,'$.epoch') AND s.lease_expires_at_ms=json_extract($1,'$.lease') AND {claimable} AND {}) AND NOT EXISTS(SELECT 1 FROM settings WHERE key=json_extract($1,'$.removed_next'))",
        orphan_shape("$2", "json_extract($1,'$.grace')", "json_extract($1,'$.prefix')")
    );
    let after = format!(
        "EXISTS(SELECT 1 {JOINS} WHERE {lineage} AND s.owner_node_id=json_extract($1,'$.next') AND s.owner_epoch=json_extract($1,'$.epoch')+1 AND s.lease_expires_at_ms=$3 AND s.state IN('active','ended') AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms=$3 AND r.owner_node_id=s.owner_node_id) AND NOT EXISTS(SELECT 1 FROM sharing_delivery_grants g WHERE g.incarnation_id=json_extract($1,'$.inc') AND g.state='active') AND NOT EXISTS(SELECT 1 FROM cache_consumer_pins p WHERE p.consumer_kind='media_session' AND p.consumer_id=json_extract($1,'$.inc') AND p.consumer_epoch=json_extract($1,'$.epoch'))"
    );
    let inc = "json_extract($1,'$.inc')";
    let statements = vec![
        claim_ordered(&source_assert(before, vec![]).0, &values)?,
        claim_ordered(&format!("UPDATE job_leases SET owner_node_id=json_extract($1,'$.next'),fence=json_extract($1,'$.epoch')+1,revision=revision+1,expires_at_ms=$3,updated_at_ms=$2 WHERE resource='session:'||{inc} AND owner_node_id=json_extract($1,'$.node') AND fence=json_extract($1,'$.epoch') AND revision<9223372036854775807"), &values)?,
        claim_ordered(&format!("UPDATE media_sessions SET owner_node_id=json_extract($1,'$.next'),owner_epoch=json_extract($1,'$.epoch')+1,lease_expires_at_ms=$3,updated_at_ms=$2 WHERE incarnation_id={inc} AND owner_node_id=json_extract($1,'$.node') AND owner_epoch=json_extract($1,'$.epoch')"), &values)?,
        claim_ordered(&format!("UPDATE media_session_requests SET owner_node_id=json_extract($1,'$.next'),updated_at_ms=$2 WHERE incarnation_id={inc} AND request_id=json_extract($1,'$.request') AND owner_node_id=json_extract($1,'$.node')"), &values)?,
        claim_ordered(&format!("UPDATE sharing_delivery_grants SET state='revoked' WHERE incarnation_id={inc} AND state='active'"), &values)?,
        claim_ordered(&format!("DELETE FROM cache_consumer_pins WHERE consumer_kind='media_session' AND consumer_id={inc} AND consumer_epoch=json_extract($1,'$.epoch')"), &values)?,
        claim_ordered(&source_assert(after, vec![]).0, &values)?,
    ];
    let claimed = |lease: i64| {
        let mut orphan = orphan.clone();
        orphan.owner.owner_node_id = next.to_owned();
        orphan.owner.owner_epoch = o.owner_epoch + 1;
        orphan.owner.lease_expires_at_ms = lease;
        orphan.owner.now_ms = now;
        ReceiverOrphanClaimOutcome::Claimed(Box::new(ClaimedReceiverOrphan(orphan)))
    };
    let failure = match store.sharing_txn(statements).await {
        Ok(_) => return Ok(claimed(lease)),
        Err(error) => error,
    };
    // Exact re-read decides a refusal or an uncertain commit: this node's
    // next epoch is ours; the unchanged source is Refused (or still pending
    // after commit-unknown); anything else lost.
    let rows = store
        .sharing_read(
            "SELECT json_object('session',session_id,'node',owner_node_id,'epoch',owner_epoch,'lease',lease_expires_at_ms) AS payload FROM media_sessions WHERE incarnation_id=$1",
            vec![o.incarnation_id.to_string().into()],
        )
        .await?;
    #[derive(Deserialize)]
    struct Current {
        session: Option<String>,
        node: Option<String>,
        epoch: i64,
        lease: i64,
    }
    let current: Option<Current> = match rows.as_slice() {
        [] => None,
        [row] => Some(serde_json::from_str(row).map_err(|_| invalid())?),
        _ => return Err(invalid()),
    };
    let Some(current) = current.filter(|c| c.session.as_deref() == Some(&o.session_id.to_string()))
    else {
        return Ok(ReceiverOrphanClaimOutcome::Lost);
    };
    let actual = now_ms()?;
    if current.node.as_deref() == Some(next)
        && current.epoch == o.owner_epoch + 1
        && current.lease > actual
    {
        return Ok(claimed(current.lease));
    }
    let unchanged = current.node.as_deref() == Some(o.owner_node_id.as_str())
        && current.epoch == o.owner_epoch
        && current.lease == o.lease_expires_at_ms;
    match (unchanged, source_write_refused(&failure)) {
        (true, true) => Ok(ReceiverOrphanClaimOutcome::Refused),
        (true, false) => Err(failure),
        (false, _) => Ok(ReceiverOrphanClaimOutcome::Lost),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        domain::{
            MediaSessionActivation, MediaSessionRequestClaim, MEDIA_SESSION_PUBLICATION_BLOCKED,
        },
        playback_principal::PlaybackPrincipal,
        secrets::{CredentialKey, SharingSecretPurpose},
        sharing_catalogue::SharedReference,
        sharing_receiver_retirement::{ReceiverRetirementDisposition, ReceiverRetirementOutcome},
        sharing_receiver_sessions::{
            ReceiverDispatchRecord, ReceiverPendingRenewal, ReceiverProducerKind,
            ReceiverSourceAttachment, ReceiverSourceWrite,
        },
        store::{
            sharing_receiver_retirement::{MetadataRetirementFixture, PendingMetadataFixture},
            MediaSessionStore, SharingReceiverRetirementStore, SharingReceiverSessionStore,
            SharingStore, SqliteStore, UserStore,
        },
    };

    struct Env {
        scope: ReceiverCatalogueScope,
        user: i64,
        hash: String,
    }
    struct Route {
        intent: ReceiverSessionIntent,
        owner: ReceiverSourceOwner,
    }

    fn clock() -> i64 {
        now_ms().expect("clock")
    }

    async fn stores(dir: &std::path::Path) -> Vec<SqliteStore> {
        let mut out = Vec::new();
        for rebuilt in [false, true] {
            for pooled in [false, true] {
                let store = if pooled {
                    SqliteStore::open(&dir.join(format!(
                        "orphan-{rebuilt}-{pooled}-{}.sqlite",
                        Uuid::new_v4()
                    )))
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
                        .expect("principal layout");
                }
                out.push(store);
            }
        }
        out
    }

    async fn env(store: &SqliteStore) -> Env {
        let user = store
            .create_user("orphan-viewer", "fixture-hash", false)
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
            libraries: vec![SourceId::parse("0").expect("library")],
        };
        store.sharing_txn(vec![
            ("INSERT INTO settings(key,value) VALUES('sharing_enabled','true')".into(), vec![]),
            ("INSERT INTO sharing_viewers(user_id,viewer_id) VALUES($1,$2)".into(), vec![user.id.into(), Uuid::new_v4().into()]),
            ("INSERT INTO sharing_imports(id,source_server_id,catalogue_epoch,source_name,claim_id,remote_grant_id,credential_envelope,endpoints_json,assignment_generation,lifecycle_generation,endpoint_generation,state,created_at_ms,updated_at_ms) VALUES($1,$2,$3,'Source',$4,$5,'fixture never opened','[]',1,1,1,'active',1000,1000)".into(), vec![scope.import_id.into(), scope.source_server_id.into(), scope.catalogue_epoch.into(), scope.claim_id.into(), scope.remote_grant_id.into()]),
            ("INSERT INTO sharing_assignments VALUES($1,'0',$2,1)".into(), vec![scope.import_id.into(), user.id.into()]),
        ]).await.expect("B import fixture");
        Env {
            scope,
            user: user.id,
            hash,
        }
    }

    async fn route(store: &SqliteStore, env: &Env, label: &str) -> Route {
        let recipe = RemoteSourceRecipe {
            kind: ReceiverProducerKind::RemoteSource,
            version: 1,
            reference: SharedReference {
                import_id: env.scope.import_id,
                server_id: env.scope.source_server_id,
                catalogue_epoch: env.scope.catalogue_epoch,
                library_id: env.scope.libraries[0].clone(),
                item_id: SourceId::parse("9223372036854775807").expect("item"),
            },
            lifecycle_generation: 1,
            file_id: SourceId::parse("0").expect("file"),
            file_revision: FileRevision::parse(&"a".repeat(64)).expect("revision"),
            source_request_id: Uuid::new_v4(),
            parent_login_hash: env.hash.clone(),
            request_json: format!("{{\"label\":\"{label}\"}}"),
        };
        let intent = ReceiverSessionIntent {
            scope: env.scope.clone(),
            user_id: env.user,
            login_hash: env.hash.clone(),
            recipe: recipe.clone(),
            source_position_ms: 0,
        };
        let authority = store
            .prepare_receiver_session_authority(intent.clone())
            .await
            .expect("authority")
            .expect("authorized");
        let now = clock();
        let request = format!("request-{label}");
        let activation = MediaSessionActivation {
            incarnation_id: recipe.source_request_id.to_string(),
            session_id: Uuid::new_v4().to_string(),
            principal: PlaybackPrincipal::LocalUser { user_id: env.user },
            playback_id: format!("playback-{label}"),
            recovery_epoch: Uuid::new_v4().to_string(),
            expected_predecessor_incarnation_id: None,
            fence_predecessor: true,
            request_id: Some(request.clone()),
            request_fingerprint: recipe.request_fingerprint().expect("fingerprint"),
            owner_node_id: "B-dead".into(),
            recipe_json: serde_json::to_string(&recipe).expect("recipe"),
            response_json: "{}".into(),
            publication_ready_at_ms: MEDIA_SESSION_PUBLICATION_BLOCKED,
            media_origin_ms: 0,
            now_ms: now,
            lease_expires_at_ms: now + 30_000,
            expected_desired_revision: None,
        };
        assert!(matches!(
            store
                .claim_media_session_request(
                    &activation.principal,
                    &request,
                    &activation.request_fingerprint,
                    &activation.playback_id,
                    &activation.incarnation_id,
                    now,
                    now + 30_000
                )
                .await
                .expect("request claim"),
            MediaSessionRequestClaim::Acquired { .. }
        ));
        assert!(store
            .assign_media_session_request_owner(
                &activation.principal,
                &request,
                &activation.incarnation_id,
                "B-dead",
                now
            )
            .await
            .expect("assign"));
        let outcome = store
            .activate_receiver_media_session(&authority, &activation)
            .await
            .expect("activation")
            .expect("blocked B route");
        Route {
            owner: ReceiverSourceOwner {
                incarnation_id: recipe.source_request_id,
                session_id: Uuid::parse_str(&activation.session_id).expect("session"),
                owner_node_id: "B-dead".into(),
                owner_epoch: outcome.route.owner_epoch,
                request_id: request,
                lease_expires_at_ms: activation.lease_expires_at_ms,
                now_ms: now,
            },
            intent,
        }
    }

    /// Simulate the dead owner's last renewal `ago_ms` in the past.
    async fn age(store: &SqliteStore, route: &Route, ago_ms: i64) -> i64 {
        let lease = clock() - ago_ms;
        let inc = route.owner.incarnation_id.to_string();
        store
            .sharing_txn(vec![
                ("UPDATE media_sessions SET lease_expires_at_ms=$1 WHERE incarnation_id=$2".into(), vec![lease.into(), inc.clone().into()]),
                ("UPDATE job_leases SET expires_at_ms=$1 WHERE resource='session:'||$2".into(), vec![lease.into(), inc.clone().into()]),
                ("UPDATE media_session_requests SET claim_expires_at_ms=$1 WHERE incarnation_id=$2".into(), vec![lease.into(), inc.into()]),
            ])
            .await
            .expect("aged lease fixture");
        lease
    }

    async fn page(store: &SqliteStore) -> Vec<ReceiverOrphan> {
        store
            .orphaned_receiver_sessions(None, RECEIVER_ORPHAN_PAGE_MAX)
            .await
            .expect("inventory")
            .orphans
    }

    async fn claimed(store: &SqliteStore, orphan: &ReceiverOrphan, next: &str) -> ReceiverOrphan {
        match store
            .claim_orphaned_receiver_session(orphan, next)
            .await
            .expect("claim")
        {
            ReceiverOrphanClaimOutcome::Claimed(claimed) => {
                // SQL-only model fixture: empty metadata fence is NOT a
                // daemon driver receipt or evidence of a physical producer.
                super::super::sharing_receiver_retirement::install_model_ingress_fence(
                    store,
                    &claimed.orphan().owner().incarnation_id.to_string(),
                )
                .await;
                claimed.orphan().clone()
            }
            _ => panic!("exact orphan claim must win"),
        }
    }

    fn sealed(import: Uuid, text: &str) -> crate::secrets::SealedSecret {
        CredentialKey::from_bytes([7; 32])
            .seal_sharing(SharingSecretPurpose::Upstream, Uuid::new_v4(), import, text)
            .expect("sealed fixture")
    }

    fn census_sql() -> &'static str {
        "SELECT json_array((SELECT json_group_array(json_array(incarnation_id,state,owner_node_id,owner_epoch,lease_expires_at_ms,response_json,updated_at_ms)) FROM media_sessions),(SELECT json_group_array(json_array(incarnation_id,owner_node_id,state,response_json,updated_at_ms)) FROM media_session_requests),(SELECT json_group_array(json_array(resource,owner_node_id,fence,revision,expires_at_ms)) FROM job_leases),(SELECT json_group_array(json_array(incarnation_id,source_session_id,dispatch_envelope)) FROM sharing_relay_upstream)) AS payload"
    }

    #[tokio::test]
    async fn sharing_receiver_orphan_claim_requires_grace_exact_fence_and_remote_recipe() {
        let _clock = crate::store::sharing::LogicalClock::install();
        let dir = tempfile::tempdir().expect("dir");
        for store in stores(dir.path()).await {
            let env = env(&store).await;
            let route = route(&store, &env, "grace").await;
            let inc = route.owner.incarnation_id.to_string();
            assert!(
                page(&store).await.is_empty(),
                "a live lease is never an orphan"
            );
            age(&store, &route, 30_000).await;
            assert!(
                page(&store).await.is_empty(),
                "expiry inside the takeover grace"
            );
            store
                .sharing_txn(vec![(
                    "INSERT INTO settings(key,value) VALUES($1,'1')".into(),
                    vec![crate::cluster::coordination::removed_job_owner_key("B-dead").into()],
                )])
                .await
                .expect("removed owner");
            assert_eq!(
                page(&store).await.len(),
                1,
                "a removed owner needs no grace"
            );
            store
                .sharing_txn(vec![(
                    "DELETE FROM settings WHERE key=$1".into(),
                    vec![crate::cluster::coordination::removed_job_owner_key("B-dead").into()],
                )])
                .await
                .expect("restore owner");
            let lease = age(&store, &route, RECEIVER_ORPHAN_GRACE_MS + 1_000).await;
            let found = page(&store).await;
            let [orphan] = found.as_slice() else {
                panic!("one orphan")
            };
            assert_eq!(orphan.owner().lease_expires_at_ms, lease);
            assert!(matches!(
                orphan.dispatch(),
                ReceiverOrphanDispatch::NotDispatched
            ));
            assert!(orphan.binding().is_none() && orphan.stranded().is_none());
            assert_eq!(orphan.intent().recipe, route.intent.recipe);
            // A job lease held by another fence is not this route's dead owner.
            store
                .sharing_txn(vec![(
                    "UPDATE job_leases SET fence=fence+1 WHERE resource='session:'||$1".into(),
                    vec![inc.clone().into()],
                )])
                .await
                .expect("foreign fence");
            assert!(page(&store).await.is_empty());
            assert_eq!(
                store
                    .claim_orphaned_receiver_session(orphan, "B-next")
                    .await
                    .expect("claim"),
                "refused"
            );
            store
                .sharing_txn(vec![(
                    "UPDATE job_leases SET fence=fence-1 WHERE resource='session:'||$1".into(),
                    vec![inc.clone().into()],
                )])
                .await
                .expect("restore fence");
            // Only RemoteSource recipes are receiver routes.
            store.sharing_txn(vec![("UPDATE media_sessions SET recipe_json=json_set(recipe_json,'$.kind','local') WHERE incarnation_id=$1".into(), vec![inc.clone().into()])]).await.expect("foreign recipe");
            assert!(page(&store).await.is_empty());
            store
                .sharing_txn(vec![(
                    "UPDATE media_sessions SET recipe_json=$1 WHERE incarnation_id=$2".into(),
                    vec![
                        serde_json::to_string(&route.intent.recipe)
                            .expect("recipe")
                            .into(),
                        inc.clone().into(),
                    ],
                )])
                .await
                .expect("restore recipe");
            // A removed next owner refuses; a stale observation loses.
            store
                .sharing_txn(vec![(
                    "INSERT INTO settings(key,value) VALUES($1,'1')".into(),
                    vec![crate::cluster::coordination::removed_job_owner_key("B-next").into()],
                )])
                .await
                .expect("removed next");
            let before = store
                .sharing_read(census_sql(), vec![])
                .await
                .expect("census");
            assert_eq!(
                store
                    .claim_orphaned_receiver_session(orphan, "B-next")
                    .await
                    .expect("claim")
                    .kind(),
                "refused"
            );
            assert_eq!(
                store
                    .sharing_read(census_sql(), vec![])
                    .await
                    .expect("unchanged"),
                before
            );
            let mut stale = orphan.clone();
            stale.owner.lease_expires_at_ms += 1;
            assert_eq!(
                store
                    .claim_orphaned_receiver_session(&stale, "B-other")
                    .await
                    .expect("claim")
                    .kind(),
                "lost"
            );
            let won = claimed(&store, orphan, "B-other").await;
            assert_eq!(won.owner().owner_node_id, "B-other");
            assert_eq!(won.owner().owner_epoch, route.owner.owner_epoch + 1);
            assert!(
                won.owner().lease_expires_at_ms > clock() + RECEIVER_ORPHAN_CLAIM_LEASE_MS - 10_000
            );
            let lease_row = store.sharing_read("SELECT json_array(owner_node_id,fence,expires_at_ms) AS payload FROM job_leases WHERE resource='session:'||$1", vec![inc.clone().into()]).await.expect("lease");
            assert_eq!(
                lease_row,
                vec![serde_json::json!([
                    "B-other",
                    won.owner().owner_epoch,
                    won.owner().lease_expires_at_ms
                ])
                .to_string()]
            );
            let request_owner = store.sharing_read("SELECT owner_node_id AS payload FROM media_session_requests WHERE incarnation_id=$1", vec![inc.into()]).await.expect("request");
            assert_eq!(request_owner, vec!["B-other".to_string()]);
            assert!(page(&store).await.is_empty(), "the claimed lease is live");
        }
    }

    impl ReceiverOrphanClaimOutcome {
        fn kind(&self) -> &'static str {
            match self {
                Self::Claimed(_) => "claimed",
                Self::Lost => "lost",
                Self::Refused => "refused",
            }
        }
    }
    impl PartialEq<&'static str> for ReceiverOrphanClaimOutcome {
        fn eq(&self, other: &&'static str) -> bool {
            self.kind() == *other
        }
    }
    impl std::fmt::Debug for ReceiverOrphanClaimOutcome {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.kind())
        }
    }

    #[tokio::test]
    async fn sharing_receiver_orphan_claim_is_exclusive_and_refuses_old_epoch_writers() {
        let _clock = crate::store::sharing::LogicalClock::install();
        let dir = tempfile::tempdir().expect("dir");
        for store in stores(dir.path()).await {
            let env = env(&store).await;
            let route = route(&store, &env, "exclusive").await;
            // The old owner keeps a fresh authority; only the fence stops it.
            let authority = store
                .prepare_receiver_session_authority(route.intent.clone())
                .await
                .expect("authority")
                .expect("authorized");
            age(&store, &route, RECEIVER_ORPHAN_GRACE_MS + 1_000).await;
            let orphan = page(&store).await.pop().expect("orphan");
            let won = claimed(&store, &orphan, "B-first").await;
            assert_eq!(
                store
                    .claim_orphaned_receiver_session(&orphan, "B-second")
                    .await
                    .expect("second"),
                "lost"
            );
            let now = clock();
            let stale = ReceiverPendingRenewal {
                incarnation_id: route.owner.incarnation_id.to_string(),
                owner_node_id: "B-dead".into(),
                owner_epoch: route.owner.owner_epoch,
                request_id: route.owner.request_id.clone(),
                now_ms: now,
                lease_expires_at_ms: now + 30_000,
            };
            let authority = store
                .prepare_receiver_session_authority(authority.intent.clone())
                .await
                .expect("authority")
                .expect("authorized");
            assert!(!store
                .renew_pending_receiver_session(&authority, &stale)
                .await
                .expect("renew"));
            assert_eq!(
                store
                    .record_receiver_dispatch(
                        &authority,
                        &ReceiverDispatchRecord {
                            owner: stale,
                            envelope: sealed(env.scope.import_id, "dispatch")
                        }
                    )
                    .await
                    .expect("record"),
                ReceiverSourceWrite::Refused
            );
            let mut old = route.owner.clone();
            old.lease_expires_at_ms = orphan.owner().lease_expires_at_ms;
            let witness = PendingMetadataFixture {
                intent: orphan.intent().clone(),
                owner: old,
                disposition: ReceiverRetirementDisposition::NeverDispatched,
            };
            assert_eq!(
                store
                    .retire_receiver_session(&witness)
                    .await
                    .expect("old retire"),
                ReceiverRetirementOutcome::Refused
            );
            // The winner's never-dispatched retirement (claim CAS is the
            // durable no-send proof) applies and replays read-only.
            let witness = PendingMetadataFixture {
                intent: won.intent().clone(),
                owner: won.owner().clone(),
                disposition: ReceiverRetirementDisposition::NeverDispatched,
            };
            assert_eq!(
                store
                    .retire_receiver_session(&witness)
                    .await
                    .expect("retire"),
                ReceiverRetirementOutcome::Applied
            );
            let settled = store
                .sharing_read(census_sql(), vec![])
                .await
                .expect("census");
            assert_eq!(
                store
                    .retire_receiver_session(&witness)
                    .await
                    .expect("replay"),
                ReceiverRetirementOutcome::Replay
            );
            assert_eq!(
                store
                    .sharing_read(census_sql(), vec![])
                    .await
                    .expect("read only"),
                settled
            );
            assert!(page(&store).await.is_empty());
        }
    }

    #[tokio::test]
    async fn sharing_receiver_orphan_claim_after_maintenance_ended_row() {
        let _clock = crate::store::sharing::LogicalClock::install();
        let dir = tempfile::tempdir().expect("dir");
        for store in stores(dir.path()).await {
            let env = env(&store).await;
            let route = route(&store, &env, "maintenance").await;
            let inc = route.owner.incarnation_id.to_string();
            age(&store, &route, RECEIVER_ORPHAN_GRACE_MS + 1_000).await;
            store
                .maintain_media_sessions(clock())
                .await
                .expect("maintenance");
            let state = store.sharing_read("SELECT json_array(state,terminal_reason) AS payload FROM media_sessions WHERE incarnation_id=$1", vec![inc.clone().into()]).await.expect("ended");
            assert_eq!(state, vec![r#"["ended","replaced"]"#.to_string()]);
            // Maintenance keeps the bound rows; their lease is the end time.
            assert!(
                page(&store).await.is_empty(),
                "grace restarts at the maintenance end"
            );
            store.sharing_txn(vec![
                ("UPDATE media_sessions SET lease_expires_at_ms=lease_expires_at_ms-$1 WHERE incarnation_id=$2".into(), vec![(RECEIVER_ORPHAN_GRACE_MS + 1_000).into(), inc.clone().into()]),
                ("UPDATE job_leases SET expires_at_ms=min(expires_at_ms,(SELECT lease_expires_at_ms FROM media_sessions WHERE incarnation_id=$1)) WHERE resource='session:'||$1".into(), vec![inc.clone().into()]),
            ]).await.expect("time passes");
            let orphan = page(&store).await.pop().expect("ended orphan");
            let won = claimed(&store, &orphan, "B-next").await;
            let state = store
                .sharing_read(
                    "SELECT state AS payload FROM media_sessions WHERE incarnation_id=$1",
                    vec![inc.clone().into()],
                )
                .await
                .expect("state");
            assert_eq!(
                state,
                vec!["ended".to_string()],
                "the claim never revives a route"
            );
            let witness = PendingMetadataFixture {
                intent: won.intent().clone(),
                owner: won.owner().clone(),
                disposition: ReceiverRetirementDisposition::NeverDispatched,
            };
            assert_eq!(
                store
                    .retire_receiver_session(&witness)
                    .await
                    .expect("retire"),
                ReceiverRetirementOutcome::Applied
            );
            assert_eq!(
                store
                    .sharing_read(
                        "SELECT CAST(count(*) AS TEXT) AS payload FROM sharing_relay_upstream",
                        vec![]
                    )
                    .await
                    .expect("binding"),
                vec!["0".to_string()]
            );
        }
    }

    #[tokio::test]
    async fn sharing_receiver_dispatch_record_is_fenced_by_claim() {
        let _clock = crate::store::sharing::LogicalClock::install();
        let dir = tempfile::tempdir().expect("dir");
        for store in stores(dir.path()).await {
            let env = env(&store).await;
            let route = route(&store, &env, "dispatch").await;
            let inc = route.owner.incarnation_id.to_string();
            let authority = store
                .prepare_receiver_session_authority(route.intent.clone())
                .await
                .expect("authority")
                .expect("authorized");
            let now = clock();
            let owner = ReceiverPendingRenewal {
                incarnation_id: inc.clone(),
                owner_node_id: "B-dead".into(),
                owner_epoch: route.owner.owner_epoch,
                request_id: route.owner.request_id.clone(),
                now_ms: now,
                lease_expires_at_ms: route.owner.lease_expires_at_ms,
            };
            // The `none` marker is not a sealed credential row.
            let sealed_rows = store
                .sharing_sealed_census()
                .await
                .expect("census")
                .sealed_rows();
            let record = ReceiverDispatchRecord {
                owner: owner.clone(),
                envelope: sealed(env.scope.import_id, "dispatch-one"),
            };
            assert_eq!(
                store
                    .record_receiver_dispatch(&authority, &record)
                    .await
                    .expect("record"),
                ReceiverSourceWrite::Applied
            );
            assert_eq!(
                store
                    .sharing_sealed_census()
                    .await
                    .expect("census")
                    .sealed_rows(),
                sealed_rows + 1,
                "the dispatch capsule needs the key that sealed it"
            );
            assert_eq!(
                store
                    .record_receiver_dispatch(&authority, &record)
                    .await
                    .expect("replay"),
                ReceiverSourceWrite::Replay
            );
            let other = ReceiverDispatchRecord {
                owner: owner.clone(),
                envelope: sealed(env.scope.import_id, "dispatch-two"),
            };
            assert_eq!(
                store
                    .record_receiver_dispatch(&authority, &other)
                    .await
                    .expect("other"),
                ReceiverSourceWrite::Refused
            );
            let stored = store.sharing_read("SELECT dispatch_envelope AS payload FROM sharing_relay_upstream WHERE incarnation_id=$1", vec![inc.clone().into()]).await.expect("stored");
            assert_eq!(
                stored,
                vec![record.envelope.to_persist().expect("persist").to_owned()]
            );
            // A recorded dispatch owes End: never-dispatched retirement refuses.
            age(&store, &route, RECEIVER_ORPHAN_GRACE_MS + 1_000).await;
            let orphan = page(&store).await.pop().expect("dispatched orphan");
            assert!(
                matches!(orphan.dispatch(), ReceiverOrphanDispatch::Sealed(s) if s == &record.envelope)
            );
            let won = claimed(&store, &orphan, "B-next").await;
            let never = PendingMetadataFixture {
                intent: won.intent().clone(),
                owner: won.owner().clone(),
                disposition: ReceiverRetirementDisposition::NeverDispatched,
            };
            assert_eq!(
                store.retire_receiver_session(&never).await.expect("never"),
                ReceiverRetirementOutcome::Refused
            );
            let settled = PendingMetadataFixture {
                disposition: ReceiverRetirementDisposition::SourceSettled,
                ..never
            };
            assert_eq!(
                store
                    .retire_receiver_session(&settled)
                    .await
                    .expect("lost-Start End"),
                ReceiverRetirementOutcome::Applied
            );
            // A fresh route claimed before its owner recorded: nothing may be sent.
            let late = route_with(&store, &env, "late").await;
            age(&store, &late, RECEIVER_ORPHAN_GRACE_MS + 1_000).await;
            let orphan = page(&store).await.pop().expect("pending orphan");
            claimed(&store, &orphan, "B-next").await;
            let authority = store
                .prepare_receiver_session_authority(late.intent.clone())
                .await
                .expect("authority")
                .expect("authorized");
            let now = clock();
            let fenced = ReceiverDispatchRecord {
                owner: ReceiverPendingRenewal {
                    incarnation_id: late.owner.incarnation_id.to_string(),
                    owner_node_id: "B-dead".into(),
                    owner_epoch: late.owner.owner_epoch,
                    request_id: late.owner.request_id.clone(),
                    now_ms: now,
                    lease_expires_at_ms: now + 30_000,
                },
                envelope: sealed(env.scope.import_id, "too-late"),
            };
            assert_eq!(
                store
                    .record_receiver_dispatch(&authority, &fenced)
                    .await
                    .expect("fenced"),
                ReceiverSourceWrite::Refused
            );
        }
    }

    async fn route_with(store: &SqliteStore, env: &Env, label: &str) -> Route {
        route(store, env, label).await
    }

    #[tokio::test]
    async fn sharing_receiver_orphan_retirement_preserves_other_routes_and_replays_read_only() {
        let _clock = crate::store::sharing::LogicalClock::install();
        let dir = tempfile::tempdir().expect("dir");
        for store in stores(dir.path()).await {
            let env = env(&store).await;
            let route = route(&store, &env, "attached").await;
            let live = route_with(&store, &env, "live").await;
            let authority = store
                .prepare_receiver_session_authority(route.intent.clone())
                .await
                .expect("authority")
                .expect("authorized");
            let mut owner = route.owner.clone();
            owner.now_ms = clock();
            let attachment = ReceiverSourceAttachment {
                owner,
                binding: ReceiverSourceBinding {
                    reference: route.intent.recipe.reference.clone(),
                    file_id: route.intent.recipe.file_id.clone(),
                    file_revision: route.intent.recipe.file_revision.clone(),
                    source_request_id: route.intent.recipe.source_request_id,
                    source_session_id: Uuid::new_v4(),
                    source_incarnation_id: Uuid::new_v4(),
                    capability_envelope: sealed(env.scope.import_id, "capsule"),
                },
            };
            assert_eq!(
                store
                    .attach_receiver_source(&authority, &attachment)
                    .await
                    .expect("attach"),
                ReceiverSourceWrite::Applied
            );
            age(&store, &route, RECEIVER_ORPHAN_GRACE_MS + 1_000).await;
            let orphan = page(&store).await.pop().expect("attached orphan");
            let binding = orphan.binding().expect("attached binding");
            assert_eq!(
                binding.source_session_id,
                attachment.binding.source_session_id
            );
            assert!(binding.capability_envelope == attachment.binding.capability_envelope);
            let others = "SELECT json_array((SELECT json_group_array(json_array(incarnation_id,state,owner_node_id,owner_epoch,lease_expires_at_ms,updated_at_ms)) FROM media_sessions WHERE incarnation_id<>$1),(SELECT json_group_array(json_array(resource,owner_node_id,fence,revision,expires_at_ms)) FROM job_leases WHERE resource<>'session:'||$1),(SELECT json_group_array(json_array(incarnation_id,dispatch_envelope)) FROM sharing_relay_upstream WHERE incarnation_id<>$1)) AS payload";
            let inc = route.owner.incarnation_id.to_string();
            let before = store
                .sharing_read(others, vec![inc.clone().into()])
                .await
                .expect("others");
            let won = claimed(&store, &orphan, "B-next").await;
            let witness = MetadataRetirementFixture {
                intent: won.intent().clone(),
                attachment: ReceiverSourceAttachment {
                    owner: won.owner().clone(),
                    binding: won.binding().expect("binding").clone(),
                },
                confirmation: "e".repeat(64),
            };
            assert_eq!(
                store
                    .retire_receiver_session(&witness)
                    .await
                    .expect("retire"),
                ReceiverRetirementOutcome::Applied
            );
            assert_eq!(
                store
                    .sharing_read(others, vec![inc.clone().into()])
                    .await
                    .expect("others"),
                before
            );
            let settled = store
                .sharing_read(census_sql(), vec![])
                .await
                .expect("census");
            assert_eq!(
                store
                    .retire_receiver_session(&witness)
                    .await
                    .expect("replay"),
                ReceiverRetirementOutcome::Replay
            );
            assert_eq!(
                store
                    .sharing_read(census_sql(), vec![])
                    .await
                    .expect("read only"),
                settled
            );
            assert!(page(&store).await.is_empty());
            let _ = live;
        }
    }

    #[tokio::test]
    async fn sharing_receiver_orphan_inventory_keyset_progress() {
        let _clock = crate::store::sharing::LogicalClock::install();
        let dir = tempfile::tempdir().expect("dir");
        for store in stores(dir.path()).await {
            let env = env(&store).await;
            let mut routes = Vec::new();
            for label in ["a", "b", "c"] {
                routes.push(route(&store, &env, label).await);
            }
            // Age only after every activation: activation may end a user's
            // expired routes, which restarts their grace.
            let mut incarnations = Vec::new();
            for route in &routes {
                age(&store, route, RECEIVER_ORPHAN_GRACE_MS + 1_000).await;
                incarnations.push(route.owner.incarnation_id.to_string());
            }
            incarnations.sort();
            let first = store
                .orphaned_receiver_sessions(None, 2)
                .await
                .expect("first page");
            let seen: Vec<String> = first
                .orphans
                .iter()
                .map(|o| o.owner().incarnation_id.to_string())
                .collect();
            assert_eq!(seen, incarnations[..2]);
            assert_eq!(first.next_cursor.as_deref(), Some(incarnations[1].as_str()));
            let second = store
                .orphaned_receiver_sessions(first.next_cursor.as_deref(), 2)
                .await
                .expect("second page");
            assert_eq!(second.orphans.len(), 1);
            assert_eq!(
                second.orphans[0].owner().incarnation_id.to_string(),
                incarnations[2]
            );
            assert!(second.next_cursor.is_none());
            assert!(store.orphaned_receiver_sessions(None, 0).await.is_err());
            assert!(store
                .orphaned_receiver_sessions(None, RECEIVER_ORPHAN_PAGE_MAX + 1)
                .await
                .is_err());
            // Unknown dispatch and a partial binding are reported, never claimed.
            store.sharing_txn(vec![
                ("UPDATE sharing_relay_upstream SET dispatch_envelope=NULL WHERE incarnation_id=$1".into(), vec![incarnations[0].clone().into()]),
                ("UPDATE sharing_relay_upstream SET source_session_id=$1 WHERE incarnation_id=$2".into(), vec![Uuid::new_v4().to_string().into(), incarnations[1].clone().into()]),
                ("UPDATE sharing_relay_upstream SET remote_file_id='not-a-number' WHERE incarnation_id=$1".into(), vec![incarnations[2].clone().into()]),
            ]).await.expect("degraded rows");
            let page = store
                .orphaned_receiver_sessions(None, RECEIVER_ORPHAN_PAGE_MAX)
                .await
                .expect("page");
            assert_eq!(page.unreadable, vec![incarnations[2].clone()]);
            let reasons: Vec<_> = page.orphans.iter().map(|o| o.stranded()).collect();
            assert_eq!(
                reasons,
                vec![
                    Some(ReceiverOrphanStrandedReason::DispatchUnknown),
                    Some(ReceiverOrphanStrandedReason::PartialBinding)
                ]
            );
            for orphan in &page.orphans {
                let before = store
                    .sharing_read(census_sql(), vec![])
                    .await
                    .expect("census");
                assert_eq!(
                    store
                        .claim_orphaned_receiver_session(orphan, "B-next")
                        .await
                        .expect("claim"),
                    "refused"
                );
                assert_eq!(
                    store
                        .sharing_read(census_sql(), vec![])
                        .await
                        .expect("unchanged"),
                    before
                );
            }
        }
    }
}
