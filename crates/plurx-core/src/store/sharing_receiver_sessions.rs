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
    sharing_receiver_sessions::{ReceiverSessionIntent, ReceiverSessionWriteAuthority},
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
}
#[async_trait]
impl<T: Backend> SharingReceiverSessionStore for T {
    async fn prepare_receiver_session_authority(
        &self,
        intent: ReceiverSessionIntent,
    ) -> Result<Option<ReceiverSessionWriteAuthority>, StoreError> {
        let now = now_ms()?;
        let reference = &intent.recipe.reference;
        if intent.user_id <= 0
            || !is_hash(&intent.login_hash)
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
    let sql = format!("INSERT INTO sharing_relay_upstream(incarnation_id,import_id,lifecycle_generation,assignment_generation,remote_library_id,remote_item_id,remote_file_id,remote_revision,source_request_id,endpoint_revision,source_position_ms) SELECT NULL,'',1,1,'0','0','0','', '',1,0 WHERE NOT ({} AND {lineage})", authority_predicate());
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
        (format!("INSERT INTO sharing_relay_upstream(incarnation_id,import_id,lifecycle_generation,assignment_generation,remote_library_id,remote_item_id,remote_file_id,remote_revision,source_request_id,endpoint_revision,source_position_ms) SELECT NULL,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11 WHERE NOT EXISTS(SELECT 1 FROM sharing_relay_upstream b WHERE {exact} AND {route})"), values),
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
                        request_json: "{\"caps_v2\":{}}".into(),
                    };
                    let intent = ReceiverSessionIntent {
                        scope: scope.clone(),
                        user_id: user.id,
                        login_hash: hash.clone(),
                        recipe: recipe.clone(),
                        source_position_ms: 1234,
                    };
                    let authority = store
                        .prepare_receiver_session_authority(intent)
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
                    let outcome = store
                        .activate_receiver_media_session(&authority, &activation)
                        .await
                        .expect("guarded activation");
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
}
