//! Seal the actual private output choice of a newly reserved, unpublished successor.
use serde_json::Value;

use crate::error::StoreError;
use crate::transcode::VodCopyLayout;

#[derive(Clone, Debug)]
pub struct PreparedOutputSeal {
    pub incarnation_id: String,
    pub session_id: String,
    pub user_id: i64,
    pub playback_id: String,
    pub owner_node_id: String,
    pub owner_epoch: i64,
    pub expected_recipe_json: String,
    pub expected_response_json: String,
    pub predecessor_incarnation_id: String,
    pub predecessor_owner_node_id: String,
    pub predecessor_owner_epoch: i64,
    pub deadline_ms: i64,
    pub now_ms: i64,
    /// A sealed replay validates current ownership without changing either JSON value.
    pub already_complete: bool,
    /// Exact facts from the attachment's opaque response owner; None is a sealed choice too.
    pub retained_output: Option<Value>,
    /// Actual layout captured behind the same unpublished attachment fence.
    pub vod_copy_layout: Option<VodCopyLayout>,
}
fn valid_proof(proof: Option<&serde_json::Value>) -> bool {
    proof.is_none_or(|proof| {
        proof.as_object().is_some_and(|fields| fields.len() == 4)
            && proof["artifact_id"]
                .as_str()
                .is_some_and(|id| uuid::Uuid::parse_str(id).is_ok())
            && proof["output_identity"].as_str().is_some_and(|id| {
                id.len() == 64
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
            && proof["average_bps"].as_u64().is_some_and(|rate| rate > 0)
            && proof["peak_bps"].as_u64().is_some_and(|rate| rate > 0)
    })
}

impl PreparedOutputSeal {
    /// A caller which reserved pending capture accepts only its exact sealed
    /// successor. Receipt presence alone cannot prove any other recipe/owner.
    pub fn matches_reservation(
        preparation: &crate::domain::MediaSessionPreparation,
        route: &crate::domain::MediaSessionRoute,
    ) -> bool {
        route.incarnation_id == preparation.incarnation_id
            && route.session_id == preparation.session_id
            && route.principal == preparation.principal
            && route.playback_id == preparation.playback_id
            && route.request_fingerprint == preparation.request_fingerprint
            && route.owner_node_id == preparation.owner_node_id
            && route.owner_epoch == 1
            && route.state == "active"
            && route.media_origin_ms == preparation.media_origin_ms
            && route.lease_expires_at_ms == preparation.deadline_ms
            && route.publication_ready_at_ms == crate::domain::MEDIA_SESSION_PUBLICATION_BLOCKED
            && sealed_reservation_matches(
                &preparation.recipe_json,
                &preparation.response_json,
                &route.recipe_json,
                &route.response_json,
            )
    }

    pub(crate) fn sealed_json(&self) -> Result<(String, String), StoreError> {
        let invalid = || StoreError::Task("invalid first-preparation output seal".into());
        let response: Value =
            serde_json::from_str(&self.expected_response_json).map_err(|_| invalid())?;
        let recipe: Value =
            serde_json::from_str(&self.expected_recipe_json).map_err(|_| invalid())?;
        if self.user_id <= 0
            || self.owner_epoch <= 0
            || self.predecessor_owner_epoch <= 0
            || self.now_ms < 0
            || self.deadline_ms <= self.now_ms
            || self.expected_recipe_json.len() > 32768
            || self.expected_response_json.len() > 65536
            || recipe["retained_output_receiver"] != 1
            || !valid_proof(self.retained_output.as_ref())
        {
            return Err(invalid());
        }
        if self.vod_copy_layout.is_some() && response["session_id"] != self.session_id {
            return Err(invalid());
        }
        if self.already_complete {
            if response["prepared_output_capture_complete"] != true
                || response.get("prepared_output_capture_pending").is_some()
                || recipe
                    .get("retained_output")
                    .filter(|value| !value.is_null())
                    != self.retained_output.as_ref()
                || layout_from_recipe(&recipe) != Some(self.vod_copy_layout.clone())
                || !completed_layout_is_valid(&recipe, &response, self.vod_copy_layout.as_ref())
            {
                return Err(invalid());
            }
            return Ok((
                self.expected_recipe_json.clone(),
                self.expected_response_json.clone(),
            ));
        }
        let (recipe, response) = seal_values(
            recipe,
            response,
            self.retained_output.clone(),
            self.vod_copy_layout.as_ref(),
        )
        .ok_or_else(invalid)?;
        let sealed_recipe = recipe.to_string();
        let sealed_response = response.to_string();
        if sealed_recipe.len() > 32768 || sealed_response.len() > 65536 {
            return Err(invalid());
        }
        Ok((sealed_recipe, sealed_response))
    }
}

fn layout_from_recipe(recipe: &Value) -> Option<Option<VodCopyLayout>> {
    match recipe.pointer("/request/vod_copy_layout") {
        Some(value) => serde_json::from_value(value.clone()).ok(),
        None => Some(None),
    }
}

fn playlist_paths(response: &Value) -> Option<(String, String)> {
    let id = response["session_id"].as_str()?;
    let uuid = uuid::Uuid::parse_str(id).ok()?;
    if uuid.to_string() != id {
        return None;
    }
    Some((
        format!("/api/v1/hls/{id}/index.m3u8"),
        format!("/api/v1/hls/{id}/master.m3u8"),
    ))
}

fn completed_layout_is_valid(
    recipe: &Value,
    response: &Value,
    layout: Option<&VodCopyLayout>,
) -> bool {
    let Some(layout) = layout else {
        return true;
    };
    let Some((index, master)) = playlist_paths(response) else {
        return false;
    };
    let playlist = response["playlist_url"].as_str().unwrap_or_default();
    if layout.pending() || !layout.valid() {
        return false;
    }
    if layout.split() {
        recipe["request"]["split_audio"] == true
            && playlist == master
            && response.get("prepared_playlist_shape").is_none()
    } else {
        recipe["request"]["split_audio"] != true
            && playlist.split('?').next() == Some(index.as_str())
    }
}

fn seal_values(
    mut recipe: Value,
    mut response: Value,
    proof: Option<Value>,
    layout: Option<&VodCopyLayout>,
) -> Option<(Value, Value)> {
    if recipe["retained_output_receiver"] != 1
        || !recipe["retained_output"].is_null()
        || response["prepared_output_capture_pending"] != true
        || response.get("prepared_output_capture_complete").is_some()
        || !valid_proof(proof.as_ref())
    {
        return None;
    }
    match (layout_from_recipe(&recipe)?, layout) {
        (None, None) => {}
        (Some(pending), Some(layout)) if pending.accepts(layout) => {
            let (index, master) = playlist_paths(&response)?;
            if recipe["request"]["split_audio"] != true
                || response["playlist_url"].as_str()?.split('?').next() != Some(index.as_str())
            {
                return None;
            }
            let request = recipe.get_mut("request")?.as_object_mut()?;
            request.insert("vod_copy_layout".into(), serde_json::to_value(layout).ok()?);
            if layout.split() {
                let response_fields = response.as_object_mut()?;
                response_fields.insert("playlist_url".into(), Value::String(master));
                response_fields.remove("prepared_playlist_shape");
            } else {
                request.remove("split_audio");
            }
        }
        _ => return None,
    }
    recipe
        .as_object_mut()?
        .insert("retained_output".into(), proof.unwrap_or(Value::Null));
    let fields = response.as_object_mut()?;
    fields.remove("prepared_output_capture_pending");
    fields.insert("prepared_output_capture_complete".into(), Value::Bool(true));
    Some((recipe, response))
}

/// An original fresh reservation can replay its own sealed row without
/// changing the captured choice. Every other recipe/response value stays exact.
pub(crate) fn sealed_reservation_matches(
    expected_recipe: &str,
    expected_response: &str,
    actual_recipe: &str,
    actual_response: &str,
) -> bool {
    let parse = |text: &str| serde_json::from_str::<Value>(text).ok();
    let (
        Some(expected_recipe),
        Some(expected_response),
        Some(mut actual_recipe),
        Some(actual_response),
    ) = (
        parse(expected_recipe),
        parse(expected_response),
        parse(actual_recipe),
        parse(actual_response),
    )
    else {
        return false;
    };
    let Some(actual_layout) = layout_from_recipe(&actual_recipe) else {
        return false;
    };
    let Some((mut expected_recipe, expected_response)) = seal_values(
        expected_recipe,
        expected_response,
        actual_recipe
            .get("retained_output")
            .filter(|value| !value.is_null())
            .cloned(),
        actual_layout.as_ref(),
    ) else {
        return false;
    };
    if let Some(fields) = actual_recipe.as_object_mut() {
        fields.remove("retained_output");
    }
    expected_recipe
        .as_object_mut()
        .expect("sealed recipe object")
        .remove("retained_output");
    expected_recipe == actual_recipe && expected_response == actual_response
}

// The ownership predicate one seal must satisfy. Shared by the conditional
// writer and its consistent read, so the two can never drift apart.
macro_rules! seal_predicate {
    () => {
        "s.incarnation_id=$4 AND s.session_id=$5 AND s.user_id=$6 AND s.playback_id=$7
 AND s.owner_node_id=$8 AND s.owner_epoch=$9 AND s.state='active'
 AND length(CAST($15 AS BLOB))<=32768
 AND length(CAST($16 AS BLOB))<=65536
 AND s.publication_ready_at_ms=9223372036854775807 AND s.lease_expires_at_ms=$10 AND s.lease_expires_at_ms>$11
 AND ((s.recipe_json=$2 AND s.response_json=$3)
 OR (NOT $1 AND s.recipe_json=$15
 AND s.response_json=$16))
 AND NOT EXISTS(SELECT 1 FROM quality_preparation_owners q JOIN quality_cancellation_receipts c ON c.receipt_key=q.cancellation_key WHERE q.staged_incarnation_id=s.incarnation_id)
 AND EXISTS(SELECT 1 FROM job_leases j WHERE j.resource='session:'||s.incarnation_id
 AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms=s.lease_expires_at_ms)
 AND EXISTS(SELECT 1 FROM media_session_preparations p JOIN media_playback_pointers ptr
 ON ptr.user_id=p.user_id AND ptr.playback_id=p.playback_id
 JOIN media_sessions predecessor ON predecessor.incarnation_id=p.expected_predecessor_incarnation_id
 WHERE p.user_id=$6 AND p.playback_id=$7 AND p.staged_incarnation_id=$4
 AND p.expected_predecessor_incarnation_id=$12 AND p.deadline_ms=$10 AND p.deadline_ms>$11
 AND ptr.current_incarnation_id=$12 AND predecessor.user_id=$6 AND predecessor.playback_id=$7
 AND predecessor.owner_node_id=$13 AND predecessor.owner_epoch=$14 AND predecessor.state='active'
 AND predecessor.lease_expires_at_ms>$11)"
    };
}

// Both backends bind the same ownership fence and completed JSON values.
macro_rules! seal_bindings {
    () => {
        "WITH seal_bindings AS (SELECT $1, $2, $3, $4, $5, $6, $7, $8, $9,
 $10, $11, $12, $13, $14, $15, $16) "
    };
}
pub(crate) const SEAL: &str = concat!(
    seal_bindings!(),
    "UPDATE media_sessions AS s SET
 recipe_json=CASE WHEN $1 THEN s.recipe_json ELSE $15 END,
 response_json=CASE WHEN $1 THEN s.response_json ELSE $16 END
 WHERE ",
    seal_predicate!()
);

// A read of the same predicate avoids a replicated UPDATE on sealed replays.
pub(crate) const SEALED: &str = concat!(
    seal_bindings!(),
    "SELECT EXISTS(SELECT 1 FROM media_sessions AS s WHERE ",
    seal_predicate!(),
    ") AS sealed"
);
