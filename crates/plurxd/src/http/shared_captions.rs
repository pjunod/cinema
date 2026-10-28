//! Committed caption reads only. Source grant authority is checked by the
//! ordinary file-asset handler before and after this bounded projection.
use super::{error::ApiError, hls::SourcePlaybackTarget};
use crate::state::AppState;
use axum::{
    body::Bytes,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use plurx_core::{
    sharing_resources::{SharingFileResource, SharingFileResourceKind as Kind},
    store::shared_caption_read::{GrantCaptionPrincipal, GrantCaptionQuery, GrantCaptionRead},
};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::BTreeSet;
use uuid::Uuid;

pub(crate) const PAGE_BYTES: usize = 65_536;
pub(crate) const SEGMENT_BYTES: usize = 16_384;
fn invalid() -> ApiError {
    ApiError::typed(
        StatusCode::BAD_GATEWAY,
        "sharing_caption_invalid",
        "The Source returned invalid caption data",
    )
}
fn expired() -> ApiError {
    ApiError::typed(
        StatusCode::GONE,
        "sharing_caption_snapshot_expired",
        "Caption coverage changed; refresh this window",
    )
}
fn missing() -> ApiError {
    ApiError::typed(
        StatusCode::NOT_FOUND,
        "sharing_caption_unavailable",
        "These captions are not available for this shared file",
    )
}
fn digest(value: &str) -> bool {
    plurx_core::store::subtitle_translation::valid_digest(value)
}
fn string<'a>(row: &'a Value, key: &str) -> Result<&'a str, ApiError> {
    row[key].as_str().ok_or_else(invalid)
}
fn integer(row: &Value, key: &str, min: i64, max: i64) -> Result<i64, ApiError> {
    row[key]
        .as_i64()
        .filter(|n| (*n >= min) && (*n <= max))
        .ok_or_else(invalid)
}
fn counter(row: &Value, key: &str) -> Result<String, ApiError> {
    let value = string(row, key)?;
    if !value
        .parse::<i64>()
        .is_ok_and(|n| n >= 0 && n.to_string() == value)
    {
        return Err(invalid());
    }
    Ok(value.into())
}
fn hash(row: &Value, key: &str) -> Result<String, ApiError> {
    let value = string(row, key)?;
    if !digest(value) {
        return Err(invalid());
    }
    Ok(value.into())
}
fn keys(row: &Value, expected: &[&str]) -> Result<(), ApiError> {
    let object = row.as_object().ok_or_else(invalid)?;
    if object.len() != expected.len() || expected.iter().any(|key| !object.contains_key(*key)) {
        return Err(invalid());
    }
    Ok(())
}
fn scalar_asset(row: &Value) -> Result<Value, ApiError> {
    let task = string(row, "task")?;
    let spoken = string(row, "spoken_language")?;
    let output = string(row, "output_language")?;
    if !matches!(task, "transcribe" | "translate")
        || spoken.is_empty()
        || spoken.len() > 16
        || !spoken.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
        || output.len() > 16
        || output != (if task == "translate" { "en" } else { spoken })
    {
        return Err(invalid());
    }
    let model = string(row, "model_id")?;
    if model.is_empty()
        || model.len() > 64
        || !model
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(invalid());
    }
    Ok(
        json!({"asset_id":hash(row,"asset_id")?,"profile_id":hash(row,"profile_id")?,"model_id":model,"model_sha256":hash(row,"model_sha256")?,"content_revision":hash(row,"content_revision")?,"audio_index":integer(row,"audio_index",0,255)?,"spoken_language":spoken,"output_language":output,"task":task,"duration_ms":integer(row,"duration_ms",1,14_400_000)?}),
    )
}
async fn read(
    state: &AppState,
    principal: &GrantCaptionPrincipal,
    query: GrantCaptionQuery,
) -> Result<Vec<Value>, ApiError> {
    Ok(state
        .store
        .read_shared_captions(GrantCaptionRead {
            principal: principal.clone(),
            now_ms: crate::state::clock_ms(),
            query,
        })
        .await?)
}
async fn asset(
    state: &AppState,
    principal: &GrantCaptionPrincipal,
    id: &str,
) -> Result<Value, ApiError> {
    let row = read(
        state,
        principal,
        GrantCaptionQuery::Asset {
            asset_id: id.into(),
        },
    )
    .await?
    .into_iter()
    .next()
    .ok_or_else(missing)?;
    crate::subtitle_translation::authorize_asset_source(state, id).await?;
    Ok(row)
}
fn body(value: Value) -> Result<Response, ApiError> {
    let bytes = serde_json::to_vec(&value).map_err(|_| invalid())?;
    if bytes.len() > PAGE_BYTES {
        return Err(invalid());
    }
    Ok((
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        Bytes::from(bytes),
    )
        .into_response())
}

pub(crate) async fn source(
    state: &AppState,
    target: &SourcePlaybackTarget,
    grant: Uuid,
    resource: &SharingFileResource,
) -> Result<Response, ApiError> {
    let principal = GrantCaptionPrincipal {
        grant_id: grant.to_string(),
        binding_sha256: plurx_core::store::subtitle_translation::digest(
            &json!({"grant":grant,"target":target}),
        )?,
        file_id: target.file_id.as_str().parse().map_err(|_| missing())?,
    };
    let (path, query) = resource
        .as_str()
        .split_once('?')
        .map_or((resource.as_str(), ""), |(a, b)| (a, b));
    let fields = path.split('/').collect::<Vec<_>>();
    match resource.kind() {
        Kind::Captions => {
            let after = query.strip_prefix("after=").map(str::to_owned);
            let rows = read(state, &principal, GrantCaptionQuery::Assets { after }).await?;
            if rows.len() > 17 {
                return Err(invalid());
            }
            let more = rows.len() > 16;
            let mut entries = Vec::new();
            let mut last = None;
            for row in rows.into_iter().take(16) {
                let id = hash(&row, "asset_id")?;
                last = Some(id.clone());
                match crate::subtitle_translation::authorize_asset_source(state, &id).await {
                    Ok(()) => {}
                    Err(error)
                        if matches!(
                            &error,
                            ApiError::Typed {
                                code: "source_verifying",
                                ..
                            } | ApiError::TypedRetry {
                                code: "source_verifying",
                                ..
                            } | ApiError::TypedDetail {
                                code: "source_verifying",
                                ..
                            }
                        ) =>
                    {
                        return Err(error)
                    }
                    Err(_) => continue,
                }
                entries.push(scalar_asset(&row)?);
                crate::subtitle_translation::authorize_asset_source(state, &id).await?;
            }
            body(json!({"version":1,"assets":entries,"next_asset":if more{last}else{None}}))
        }
        Kind::CaptionManifest => {
            let id = *fields.get(1).ok_or_else(invalid)?;
            let row = asset(state, &principal, id).await?;
            let position = query
                .strip_prefix("position_ms=")
                .and_then(|v| v.parse::<i64>().ok())
                .ok_or_else(invalid)?;
            let lease = read(
                state,
                &principal,
                GrantCaptionQuery::AcquireLease {
                    asset_id: id.into(),
                },
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                ApiError::typed(
                    StatusCode::TOO_MANY_REQUESTS,
                    "sharing_caption_capacity",
                    "Caption read capacity is busy",
                )
            })?;
            let lease_id = string(&lease, "lease_id")?.to_owned();
            let revision = counter(&lease, "revision")?;
            let refs = read(
                state,
                &principal,
                GrantCaptionQuery::Window {
                    asset_id: id.into(),
                    lease_id: lease_id.clone(),
                    position_ms: position,
                },
            )
            .await?;
            if refs.len() > 8 {
                return Err(invalid());
            }
            let mut coverage: Vec<(i64, i64)> = Vec::new();
            for reference in &refs {
                let start = integer(reference, "start_ms", 0, 14_400_000)?;
                let end = integer(reference, "end_ms", start + 1, 14_400_000)?;
                if let Some(last) = coverage.last_mut().filter(|last| start <= last.1) {
                    last.1 = last.1.max(end);
                } else {
                    coverage.push((start, end));
                }
            }
            let final_lease = read(
                state,
                &principal,
                GrantCaptionQuery::Lease {
                    asset_id: id.into(),
                    lease_id: lease_id.clone(),
                },
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(expired)?;
            let expires =
                integer(&final_lease, "expires_at_ms", 0, i64::MAX)? - crate::state::clock_ms();
            if expires <= 0 {
                return Err(expired());
            }
            let mut projection = scalar_asset(&row)?;
            let object = projection.as_object_mut().ok_or_else(invalid)?;
            object.remove("model_id");
            object.remove("model_sha256");
            object.insert("version".into(), 1.into());
            object.insert("manifest_revision".into(), revision.clone().into());
            let state_name = if row["stopped"] == 1 {
                "stopped"
            } else {
                row["state"]
                    .as_str()
                    .filter(|s| {
                        matches!(
                            *s,
                            "partial" | "complete" | "stopped" | "failed" | "suspended"
                        )
                    })
                    .unwrap_or("partial")
            };
            object.insert("state".into(), state_name.into());
            object.insert("reason".into(), safe_reason(row["reason"].as_str()).into());
            object.insert(
                "snapshot".into(),
                json!({"lease_id":lease_id,"revision":revision,"expires_in_ms":expires}),
            );
            object.insert(
                "coverage".into(),
                json!(coverage
                    .into_iter()
                    .map(|(start, end)| json!({"start_ms":start,"end_ms":end}))
                    .collect::<Vec<_>>()),
            );
            object.insert("segments".into(), json!(refs));
            crate::subtitle_translation::authorize_asset_source(state, id).await?;
            body(projection)
        }
        Kind::CaptionSegment => {
            let id = *fields.get(1).ok_or_else(invalid)?;
            let segment = *fields.get(3).ok_or_else(invalid)?;
            let row = asset(state, &principal, id).await?;
            let lease = query.strip_prefix("lease=").ok_or_else(invalid)?;
            let held = read(
                state,
                &principal,
                GrantCaptionQuery::Segment {
                    asset_id: id.into(),
                    lease_id: lease.into(),
                    segment_id: segment.into(),
                },
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(expired)?;
            let bytes = string(&held, "body")?.as_bytes();
            if bytes.len() > SEGMENT_BYTES
                || hex::encode(sha2::Sha256::digest(bytes)) != string(&held, "digest")?
            {
                return Err(invalid());
            }
            let parsed: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
            validate_segment(&parsed, id, segment)?;
            if parsed["provenance"]["source_sha256"] != row["content_revision"]
                || parsed["provenance"]["profile_id"] != row["profile_id"]
                || parsed["provenance"]["model_sha256"] != row["model_sha256"]
            {
                return Err(invalid());
            }
            read(
                state,
                &principal,
                GrantCaptionQuery::Lease {
                    asset_id: id.into(),
                    lease_id: lease.into(),
                },
            )
            .await?
            .into_iter()
            .next()
            .ok_or_else(expired)?;
            crate::subtitle_translation::authorize_asset_source(state, id).await?;
            Ok((
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                Bytes::copy_from_slice(bytes),
            )
                .into_response())
        }
        _ => Err(invalid()),
    }
}
fn safe_reason(reason: Option<&str>) -> Option<&'static str> {
    match reason {
        Some("waiting_for_playback_capacity") => Some("waiting_for_playback_capacity"),
        Some("operation_stopped") => Some("operation_stopped"),
        Some("worker_interrupted") => Some("worker_interrupted"),
        Some("source_changed") => Some("source_changed"),
        Some("inference_failed") => Some("inference_failed"),
        _ => None,
    }
}
fn validate_segment(row: &Value, asset: &str, segment: &str) -> Result<(), ApiError> {
    keys(
        row,
        &[
            "version",
            "asset_id",
            "segment_id",
            "core_index",
            "start_ms",
            "end_ms",
            "cues",
            "provenance",
        ],
    )?;
    if row["version"] != 1
        || string(row, "asset_id")? != asset
        || string(row, "segment_id")? != segment
    {
        return Err(invalid());
    }
    integer(row, "core_index", 0, 2047)?;
    let start = integer(row, "start_ms", 0, 14_400_000)?;
    let end = integer(row, "end_ms", start + 1, 14_400_000)?;
    let p = &row["provenance"];
    keys(
        p,
        &[
            "engine_sha256",
            "model_sha256",
            "operation_epoch",
            "platform",
            "profile_id",
            "source_sha256",
            "worker_fence",
        ],
    )?;
    for key in [
        "engine_sha256",
        "model_sha256",
        "profile_id",
        "source_sha256",
    ] {
        hash(p, key)?;
    }
    for key in ["operation_epoch", "worker_fence"] {
        counter(p, key)?;
    }
    let platform = string(p, "platform")?;
    if platform.is_empty()
        || platform.len() > 64
        || !platform
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(invalid());
    }
    let cues = row["cues"]
        .as_array()
        .filter(|rows| rows.len() <= 128)
        .ok_or_else(invalid)?;
    let mut seen = BTreeSet::new();
    for cue in cues {
        keys(cue, &["cue_id", "start_ms", "end_ms", "text"])?;
        let id = hash(cue, "cue_id")?;
        if !seen.insert(id) {
            return Err(invalid());
        }
        let low = integer(cue, "start_ms", start, end - 1)?;
        integer(cue, "end_ms", low + 1, end)?;
        let text = string(cue, "text")?;
        if text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\t'))
        {
            return Err(invalid());
        }
    }
    Ok(())
}
/// B accepts only this closed public projection; Source private paths and work
/// routing identities never become browser/native fields.
pub(crate) fn project(resource: &SharingFileResource, bytes: &[u8]) -> Result<Bytes, ApiError> {
    let row: Value = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let (path, _) = resource
        .as_str()
        .split_once('?')
        .unwrap_or((resource.as_str(), ""));
    let fields = path.split('/').collect::<Vec<_>>();
    match resource.kind() {
        Kind::Captions => {
            keys(&row, &["version", "assets", "next_asset"])?;
            if row["version"] != 1 {
                return Err(invalid());
            }
            let assets = row["assets"]
                .as_array()
                .filter(|rows| rows.len() <= 16)
                .ok_or_else(invalid)?;
            for asset in assets {
                keys(
                    asset,
                    &[
                        "asset_id",
                        "profile_id",
                        "model_id",
                        "model_sha256",
                        "content_revision",
                        "audio_index",
                        "spoken_language",
                        "output_language",
                        "task",
                        "duration_ms",
                    ],
                )?;
                scalar_asset(asset)?;
            }
            if !row["next_asset"].is_null() {
                hash(&row, "next_asset")?;
            }
        }
        Kind::CaptionManifest => {
            keys(
                &row,
                &[
                    "version",
                    "asset_id",
                    "content_revision",
                    "profile_id",
                    "audio_index",
                    "spoken_language",
                    "output_language",
                    "task",
                    "duration_ms",
                    "manifest_revision",
                    "state",
                    "reason",
                    "snapshot",
                    "coverage",
                    "segments",
                ],
            )?;
            if row["version"] != 1
                || string(&row, "asset_id")? != *fields.get(1).ok_or_else(invalid)?
            {
                return Err(invalid());
            }
            hash(&row, "asset_id")?;
            hash(&row, "profile_id")?;
            hash(&row, "content_revision")?;
            counter(&row, "manifest_revision")?;
            let snapshot = &row["snapshot"];
            keys(snapshot, &["lease_id", "revision", "expires_in_ms"])?;
            let lease = string(snapshot, "lease_id")?;
            if !Uuid::parse_str(lease).is_ok_and(|id| !id.is_nil() && id.to_string() == lease)
                || snapshot["revision"] != row["manifest_revision"]
            {
                return Err(invalid());
            }
            integer(snapshot, "expires_in_ms", 1, 30_000)?;
            let duration = integer(&row, "duration_ms", 1, 14_400_000)?;
            integer(&row, "audio_index", 0, 255)?;
            let task = string(&row, "task")?;
            let spoken = string(&row, "spoken_language")?;
            if !matches!(task, "transcribe" | "translate")
                || spoken.is_empty()
                || spoken.len() > 16
                || !spoken.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
                || string(&row, "output_language")?
                    != if task == "translate" { "en" } else { spoken }
            {
                return Err(invalid());
            }
            if !matches!(
                string(&row, "state")?,
                "partial" | "complete" | "stopped" | "failed" | "suspended"
            ) {
                return Err(invalid());
            }
            if !row["reason"].is_null() && safe_reason(row["reason"].as_str()).is_none() {
                return Err(invalid());
            }
            for name in ["coverage", "segments"] {
                let entries = row[name]
                    .as_array()
                    .filter(|rows| rows.len() <= 8)
                    .ok_or_else(invalid)?;
                for entry in entries {
                    keys(
                        entry,
                        if name == "segments" {
                            &["segment_id", "digest", "start_ms", "end_ms"]
                        } else {
                            &["start_ms", "end_ms"]
                        },
                    )?;
                    let start = integer(entry, "start_ms", 0, duration - 1)?;
                    integer(entry, "end_ms", start + 1, duration)?;
                    if name == "segments" {
                        hash(entry, "segment_id")?;
                        hash(entry, "digest")?;
                    }
                }
            }
        }
        Kind::CaptionSegment => validate_segment(
            &row,
            fields.get(1).ok_or_else(invalid)?,
            fields.get(3).ok_or_else(invalid)?,
        )?,
        _ => return Err(invalid()),
    }
    if bytes.len()
        > if matches!(resource.kind(), Kind::CaptionSegment) {
            SEGMENT_BYTES
        } else {
            PAGE_BYTES
        }
    {
        return Err(invalid());
    }
    Ok(Bytes::copy_from_slice(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metadata() -> Value {
        json!({"asset_id":"a".repeat(64),"profile_id":"b".repeat(64),"model_id":"small","model_sha256":"c".repeat(64),"content_revision":"d".repeat(64),"audio_index":2,"spoken_language":"de","output_language":"en","task":"translate","duration_ms":120000})
    }
    #[test]
    fn public_catalog_projection_refuses_private_authority_and_unknown_task_fields() {
        let resource = SharingFileResource::parse("captions").expect("closed path");
        let mut body = json!({"version":1,"assets":[metadata()],"next_asset":null});
        let bytes = serde_json::to_vec(&body).expect("body");
        assert_eq!(
            project(&resource, &bytes).expect("public projection"),
            Bytes::from(bytes)
        );
        body["assets"][0]["file_id"] = "7".into();
        assert!(project(&resource, &serde_json::to_vec(&body).expect("body")).is_err());
        body["assets"][0] = metadata();
        body["assets"][0]["worker_node"] = "private-worker".into();
        assert!(project(&resource, &serde_json::to_vec(&body).expect("body")).is_err());
        body["assets"][0] = metadata();
        body["assets"][0]["output_language"] = "fr".into();
        assert!(project(&resource, &serde_json::to_vec(&body).expect("body")).is_err());
        body["assets"][0] = metadata();
        body["assets"][0]["spoken_language"] = "/private/source".into();
        assert!(project(&resource, &serde_json::to_vec(&body).expect("body")).is_err());
    }
}
