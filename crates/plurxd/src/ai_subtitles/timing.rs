use axum::{
    extract::{Path, Query, State},
    Json,
};
use plurx_core::domain::MediaFile;
use serde::{Deserialize, Serialize};

use crate::{
    http::{error::ApiError, extract::AuthUser},
    state::AppState,
};

const LIMIT_MS: i64 = 15_000;

#[derive(Deserialize)]
pub(crate) struct Track {
    track_id: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Timing {
    offset_ms: i64,
}

fn track_id(file: &MediaFile, index: i64) -> Option<String> {
    file.downloaded_subtitle(index)
        .filter(|caption| caption.transcription.is_some() || caption.text_translation.is_some())
        .map(|caption| caption.stable_id())
}

fn key(file: i64, track: &str) -> String {
    format!("subtitles.timing.{file}.{track}")
}

async fn validate(state: &AppState, id: i64, index: i64, track: Track) -> Result<String, ApiError> {
    let file = state
        .store
        .get_file(id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let current = track_id(&file, index).ok_or(ApiError::NotFound("AI subtitle track"))?;
    if current != track.track_id {
        return Err(ApiError::Conflict("The subtitle list changed".into()));
    }
    Ok(key(id, &current))
}

async fn read(state: &AppState, key: &str) -> Result<i64, ApiError> {
    Ok(state
        .store
        .get_setting(key)
        .await?
        .and_then(|raw| raw.parse::<i64>().ok())
        .filter(|offset| (-LIMIT_MS..=LIMIT_MS).contains(offset))
        .unwrap_or(0))
}

pub(crate) async fn get(
    _user: AuthUser,
    State(state): State<AppState>,
    Path((id, index)): Path<(i64, i64)>,
    Query(track): Query<Track>,
) -> Result<Json<Timing>, ApiError> {
    let key = validate(&state, id, index, track).await?;
    Ok(Json(Timing {
        offset_ms: read(&state, &key).await?,
    }))
}

pub(crate) async fn put(
    AuthUser(user): AuthUser,
    State(state): State<AppState>,
    Path((id, index)): Path<(i64, i64)>,
    Query(track): Query<Track>,
    Json(input): Json<Timing>,
) -> Result<Json<Timing>, ApiError> {
    super::may_start(&state, &user).await?;
    if !(-LIMIT_MS..=LIMIT_MS).contains(&input.offset_ms) {
        return Err(ApiError::BadRequest(
            "Choose a subtitle shift within 15 seconds".into(),
        ));
    }
    let key = validate(&state, id, index, track).await?;
    state
        .store
        .put_setting(&key, &input.offset_ms.to_string())
        .await?;
    Ok(Json(input))
}

pub(crate) async fn shifted(
    state: &AppState,
    file: &MediaFile,
    index: i64,
    bytes: Vec<u8>,
) -> Result<Vec<u8>, ApiError> {
    let Some(track) = track_id(file, index) else {
        return Ok(bytes);
    };
    let offset = read(state, &key(file.id, &track)).await?;
    if offset == 0 {
        return Ok(bytes);
    }
    let vtt = String::from_utf8(bytes)
        .map_err(|_| ApiError::Internal("Stored subtitle is not UTF-8".into()))?;
    Ok(shift_vtt(&vtt, offset).into_bytes())
}

fn timestamp(raw: &str) -> Option<i64> {
    let (whole, fraction) = raw.rsplit_once('.')?;
    let mut values = whole.rsplit(':');
    let sec = values.next()?.parse::<i64>().ok()?;
    let min = values.next()?.parse::<i64>().ok()?;
    let hours = values
        .next()
        .map(str::parse::<i64>)
        .transpose()
        .ok()?
        .unwrap_or(0);
    if values.next().is_some()
        || !(0..60).contains(&sec)
        || !(0..60).contains(&min)
        || !(0..=99).contains(&hours)
        || fraction.len() != 3
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((hours * 3600 + min * 60 + sec) * 1000 + fraction.parse::<i64>().ok()?)
}

fn format_timestamp(value: i64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        value / 3_600_000,
        value / 60_000 % 60,
        value / 1000 % 60,
        value % 1000
    )
}

fn shift_vtt(vtt: &str, offset: i64) -> String {
    let vtt = vtt.replace("\r\n", "\n");
    let mut output = String::with_capacity(vtt.len());
    for block in vtt.split("\n\n") {
        let mut lines = block.split_inclusive('\n');
        let Some(first) = lines.next() else { continue };
        let (identifier, timing) = if first.contains(" --> ") {
            ("", first)
        } else if let Some(second) = lines.next() {
            (first, second)
        } else {
            ("", "")
        };
        let pair = timing
            .trim_end()
            .split_once(" --> ")
            .and_then(|(left, right)| {
                let (right, settings) = right
                    .split_once(' ')
                    .map_or((right, ""), |(time, rest)| (time, rest));
                Some((timestamp(left)?, timestamp(right)?, settings))
            });
        if let Some((start, end, settings)) = pair {
            let start = (start + offset).max(0);
            let end = end + offset;
            if end <= start {
                continue;
            }
            output.push_str(identifier);
            output.push_str(&format_timestamp(start));
            output.push_str(" --> ");
            output.push_str(&format_timestamp(end));
            if !settings.is_empty() {
                output.push(' ');
                output.push_str(settings);
            }
            output.push('\n');
            for line in lines {
                output.push_str(line);
            }
        } else {
            output.push_str(block);
        }
        output.push_str("\n\n");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::shift_vtt;

    #[test]
    fn stored_ai_timing_preserves_text_and_removes_cues_before_zero() {
        let original = "WEBVTT\n\nA\n00:00:00.100 --> 00:00:00.200\nearly\n\nB\n00:00:00.250 --> 00:00:00.900 align:center\nOne --> two\n";
        let shifted = shift_vtt(original, -400);
        assert!(shifted.starts_with("WEBVTT\n\n"));
        assert!(!shifted.contains("early"));
        assert!(shifted.contains("B\n00:00:00.000 --> 00:00:00.500 align:center\nOne --> two"));
    }
}
