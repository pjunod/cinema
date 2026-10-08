//! Seal the actual private output choice of a newly reserved, unpublished successor.
use crate::error::StoreError;

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
    pub retained_output: Option<serde_json::Value>,
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

    pub(crate) fn proof_json(&self) -> Result<String, StoreError> {
        let response: serde_json::Value = serde_json::from_str(&self.expected_response_json)
            .map_err(|_| StoreError::Task("invalid preparation response".into()))?;
        let recipe: serde_json::Value = serde_json::from_str(&self.expected_recipe_json)
            .map_err(|_| StoreError::Task("invalid preparation recipe".into()))?;
        let proof_valid = valid_proof(self.retained_output.as_ref());
        if self.user_id <= 0
            || self.owner_epoch <= 0
            || self.predecessor_owner_epoch <= 0
            || self.now_ms < 0
            || self.deadline_ms <= self.now_ms
            || self.expected_recipe_json.len() > 32768
            || self.expected_response_json.len() > 65536
            || !recipe.is_object()
            || recipe["retained_output_receiver"] != 1
            || if self.already_complete {
                response["prepared_output_capture_complete"] != true
                    || response.get("prepared_output_capture_pending").is_some()
                    || recipe["retained_output"]
                        != self
                            .retained_output
                            .clone()
                            .unwrap_or(serde_json::Value::Null)
            } else {
                !recipe["retained_output"].is_null()
                    || response["prepared_output_capture_pending"] != true
                    || response.get("prepared_output_capture_complete").is_some()
            }
            || !proof_valid
        {
            return Err(StoreError::Task(
                "invalid first-preparation output seal".into(),
            ));
        }
        serde_json::to_string(&self.retained_output)
            .map_err(|error| StoreError::Task(error.to_string()))
    }
}
/// An original fresh reservation can replay its own sealed row without
/// changing the captured choice. Every other recipe/response value stays exact.
pub(crate) fn sealed_reservation_matches(
    expected_recipe: &str,
    expected_response: &str,
    actual_recipe: &str,
    actual_response: &str,
) -> bool {
    let parse = |text: &str| serde_json::from_str::<serde_json::Value>(text).ok();
    let (
        Some(mut expected_recipe),
        Some(mut expected_response),
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
    if !expected_recipe.is_object()
        || !actual_recipe.is_object()
        || !expected_response.is_object()
        || expected_recipe["retained_output_receiver"] != 1
        || !expected_recipe["retained_output"].is_null()
        || expected_response["prepared_output_capture_pending"] != true
        || expected_response
            .get("prepared_output_capture_complete")
            .is_some()
        || actual_response["prepared_output_capture_complete"] != true
        || actual_response
            .get("prepared_output_capture_pending")
            .is_some()
        || !valid_proof(
            actual_recipe
                .get("retained_output")
                .filter(|value| !value.is_null()),
        )
    {
        return false;
    }
    expected_recipe
        .as_object_mut()
        .expect("object")
        .remove("retained_output");
    actual_recipe
        .as_object_mut()
        .expect("object")
        .remove("retained_output");
    let fields = expected_response.as_object_mut().expect("object");
    fields.remove("prepared_output_capture_pending");
    fields.insert(
        "prepared_output_capture_complete".into(),
        serde_json::Value::Bool(true),
    );
    expected_recipe == actual_recipe && expected_response == actual_response
}

// Both backends use the same atomic ownership predicate. Only the private artifact
// field and capture marker change; no playback pointer, request, lease or intent moves.
pub(crate) const SEAL: &str = "UPDATE media_sessions AS s SET
 recipe_json=CASE WHEN $15 THEN s.recipe_json ELSE json_set($8, '$.retained_output', json($1)) END,
 response_json=CASE WHEN $15 THEN s.response_json ELSE json_set(json_remove($9, '$.prepared_output_capture_pending'), '$.prepared_output_capture_complete', json('true')) END
 WHERE s.incarnation_id=$2 AND s.session_id=$3 AND s.user_id=$4 AND s.playback_id=$5
 AND s.owner_node_id=$6 AND s.owner_epoch=$7 AND s.state='active'
 AND length(CAST(json_set($8, '$.retained_output', json($1)) AS BLOB))<=32768
 AND length(CAST(json_set(json_remove($9, '$.prepared_output_capture_pending'), '$.prepared_output_capture_complete', json('true')) AS BLOB))<=65536
 AND s.publication_ready_at_ms=9223372036854775807 AND s.lease_expires_at_ms=$14 AND s.lease_expires_at_ms>$13
 AND ((s.recipe_json=$8 AND s.response_json=$9)
 OR (NOT $15 AND s.recipe_json=json_set($8, '$.retained_output', json($1))
 AND s.response_json=json_set(json_remove($9, '$.prepared_output_capture_pending'), '$.prepared_output_capture_complete', json('true'))))
 AND NOT EXISTS(SELECT 1 FROM quality_preparation_owners q JOIN quality_cancellation_receipts c ON c.receipt_key=q.cancellation_key WHERE q.staged_incarnation_id=s.incarnation_id)
 AND EXISTS(SELECT 1 FROM job_leases j WHERE j.resource='session:'||s.incarnation_id
 AND j.owner_node_id=s.owner_node_id AND j.fence=s.owner_epoch AND j.expires_at_ms=s.lease_expires_at_ms)
 AND EXISTS(SELECT 1 FROM media_session_preparations p JOIN media_playback_pointers ptr
 ON ptr.user_id=p.user_id AND ptr.playback_id=p.playback_id
 JOIN media_sessions predecessor ON predecessor.incarnation_id=p.expected_predecessor_incarnation_id
 WHERE p.user_id=$4 AND p.playback_id=$5 AND p.staged_incarnation_id=$2
 AND p.expected_predecessor_incarnation_id=$10 AND p.deadline_ms=$14 AND p.deadline_ms>$13
 AND ptr.current_incarnation_id=$10 AND predecessor.user_id=$4 AND predecessor.playback_id=$5
 AND predecessor.owner_node_id=$11 AND predecessor.owner_epoch=$12 AND predecessor.state='active'
 AND predecessor.lease_expires_at_ms>$13)";
