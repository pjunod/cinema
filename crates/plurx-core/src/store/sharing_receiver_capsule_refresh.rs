//! Current ciphertext hints for an already retained receiver owner. A snapshot
//! authenticates no actor, grant, Source or closure; exact retirement CAS still
//! checks the returned ciphertext and the daemon checks actual retained facts.
use super::{
    sharing::Backend, sharing_receiver_ingress::route_guard,
    sharing_receiver_sessions::source_write_refused,
};
use crate::{
    domain::MediaSessionRoute,
    error::StoreError,
    secrets::SealedSecret,
    sharing::invalid,
    sharing_receiver_sessions::{
        ReceiverSessionIntent, ReceiverSourceBinding, ReceiverSourceOwner, RemoteSourceRecipe,
    },
};
use serde::Deserialize;

pub struct ReceiverCleanupCapsules {
    pub binding: Option<ReceiverSourceBinding>,
    /// `None` means the explicit durable `none` marker, never SQL absence.
    pub dispatch: Option<SealedSecret>,
}
fn sealed(value: String) -> Result<SealedSecret, StoreError> {
    if value.len() > 4096 {
        return Err(invalid());
    }
    let sealed = SealedSecret::from_stored(value);
    let persisted = sealed.to_persist().map_err(|_| invalid())?;
    if persisted
        .rsplit(':')
        .next()
        .is_none_or(|body| body.len() < 80)
    {
        return Err(invalid());
    }
    Ok(sealed)
}
pub(super) async fn cleanup_capsules<T: Backend>(
    store: &T,
    route: &MediaSessionRoute,
    owner: &ReceiverSourceOwner,
    binding: Option<&ReceiverSourceBinding>,
    intent: &ReceiverSessionIntent,
) -> Result<Option<ReceiverCleanupCapsules>, StoreError> {
    if owner.incarnation_id.to_string() != route.incarnation_id
        || owner.session_id.to_string() != route.session_id
        || owner.owner_node_id != route.owner_node_id
        || owner.owner_epoch != route.owner_epoch
        || route.owner_epoch <= 0
    {
        return Ok(None);
    }
    let Some(user) = route.principal.local_user_id() else {
        return Ok(None);
    };
    let recipe: RemoteSourceRecipe =
        serde_json::from_str(&route.recipe_json).map_err(|_| invalid())?;
    if serde_json::to_string(&intent.recipe).map_err(|_| invalid())? != route.recipe_json
        || intent.scope.import_id != recipe.reference.import_id
        || intent.scope.source_server_id != recipe.reference.server_id
        || intent.scope.catalogue_epoch != recipe.reference.catalogue_epoch
        || intent.scope.lifecycle_generation != recipe.lifecycle_generation
        || !intent
            .scope
            .libraries
            .contains(&recipe.reference.library_id)
        || recipe != intent.recipe
        || user != intent.user_id
        || intent.login_hash != recipe.parent_login_hash
        || route.media_origin_ms != intent.source_position_ms
        || binding.is_some_and(|binding| {
            binding.reference != recipe.reference
                || binding.file_id != recipe.file_id
                || binding.file_revision != recipe.file_revision
                || binding.source_request_id != recipe.source_request_id
        })
    {
        return Ok(None);
    }
    // The same-query predicate below repeats the complete current fence. The
    // schema guard transaction alone is not a snapshot across subsequent reads.
    match store.sharing_txn(vec![route_guard(route)?]).await {
        Ok(_) => {}
        Err(error) if source_write_refused(&error) => return Ok(None),
        Err(error) => return Err(error),
    }
    let identity=serde_json::to_string(&serde_json::json!({"inc":route.incarnation_id,"session":route.session_id,"user":user,"node":route.owner_node_id,"epoch":route.owner_epoch,"recipe":route.recipe_json,"fingerprint":route.request_fingerprint,"playback":route.playback_id,"position":route.media_origin_ms,"request":owner.request_id,"import":recipe.reference.import_id,"library":recipe.reference.library_id,"item":recipe.reference.item_id,"file":recipe.file_id,"revision":recipe.file_revision,"source_request":recipe.source_request_id,"source_session":binding.map(|b|b.source_session_id),"source_incarnation":binding.map(|b|b.source_incarnation_id),"bound":binding.is_some(),"lifecycle":recipe.lifecycle_generation,"assignment":intent.scope.assignment_generation,"endpoint":intent.scope.endpoint_generation})).map_err(|_|invalid())?;
    let rows=store.sharing_read("SELECT json_object('capability',CASE WHEN length(CAST(b.capability_envelope AS BLOB))<=4096 THEN b.capability_envelope ELSE NULL END,'dispatch',CASE WHEN length(CAST(b.dispatch_envelope AS BLOB))<=4096 THEN b.dispatch_envelope ELSE NULL END) AS payload FROM media_sessions s JOIN media_session_requests r ON r.incarnation_id=s.incarnation_id AND r.user_id=s.user_id AND r.owner_node_id=s.owner_node_id AND r.request_fingerprint=s.request_fingerprint AND r.playback_id=s.playback_id JOIN sharing_relay_upstream b ON b.incarnation_id=s.incarnation_id WHERE s.incarnation_id=json_extract($1,'$.inc') AND s.session_id=json_extract($1,'$.session') AND s.user_id=json_extract($1,'$.user') AND s.owner_node_id=json_extract($1,'$.node') AND s.owner_epoch=json_extract($1,'$.epoch') AND s.recipe_json=json_extract($1,'$.recipe') AND s.request_fingerprint=json_extract($1,'$.fingerprint') AND s.playback_id=json_extract($1,'$.playback') AND s.media_origin_ms=json_extract($1,'$.position') AND s.state IN('active','ended') AND r.request_id=json_extract($1,'$.request') AND b.import_id=json_extract($1,'$.import') AND b.lifecycle_generation=json_extract($1,'$.lifecycle') AND b.assignment_generation=json_extract($1,'$.assignment') AND b.endpoint_revision=json_extract($1,'$.endpoint') AND b.source_position_ms=json_extract($1,'$.position') AND b.remote_library_id=json_extract($1,'$.library') AND b.remote_item_id=json_extract($1,'$.item') AND b.remote_file_id=json_extract($1,'$.file') AND b.remote_revision=json_extract($1,'$.revision') AND b.source_request_id=json_extract($1,'$.source_request') AND ((json_extract($1,'$.bound')=1 AND b.source_session_id=json_extract($1,'$.source_session') AND b.source_incarnation_id=json_extract($1,'$.source_incarnation') AND b.capability_envelope IS NOT NULL) OR (json_extract($1,'$.bound')=0 AND b.source_session_id IS NULL AND b.source_incarnation_id IS NULL AND b.capability_envelope IS NULL)) LIMIT 2",vec![identity.into()]).await?;
    let [row] = rows.as_slice() else {
        return if rows.is_empty() {
            Ok(None)
        } else {
            Err(invalid())
        };
    };
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Row {
        capability: Option<String>,
        dispatch: Option<String>,
    }
    let row: Row = serde_json::from_str(row).map_err(|_| invalid())?;
    let dispatch = match row.dispatch {
        Some(value) if value == "none" => None,
        Some(value) => Some(sealed(value)?),
        None => return Ok(None),
    };
    let binding = match (binding, row.capability) {
        (Some(retained), Some(capability)) => {
            let mut current = retained.clone();
            current.capability_envelope = sealed(capability)?;
            Some(current)
        }
        (None, None) => None,
        _ => return Ok(None),
    };
    Ok(Some(ReceiverCleanupCapsules { binding, dispatch }))
}
