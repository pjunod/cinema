//! Bounded acquired captions replicated with their media source identity.

use crate::domain::DownloadedSubtitle;
use crate::error::StoreError;

pub(crate) const SCHEMA: &str =
    "ALTER TABLE files ADD COLUMN downloaded_subtitles TEXT NOT NULL DEFAULT '[]';";
pub const MAX_DOWNLOADED_SUBTITLE_BYTES: usize = 256 * 1024;
pub const MAX_DOWNLOADED_SUBTITLES: usize = 8;

pub fn valid_downloaded_vtt(vtt: &str) -> bool {
    if vtt.len() > MAX_DOWNLOADED_SUBTITLE_BYTES
        || !vtt.starts_with("WEBVTT\n")
        || vtt.contains('\0')
    {
        return false;
    }
    let mut cues = 0;
    for line in vtt.lines() {
        if let Some((start, rest)) = line.split_once(" --> ") {
            let end = rest.split_whitespace().next().unwrap_or("");
            match (timestamp(start), timestamp(end)) {
                (Some(start), Some(end)) if end > start => cues += 1,
                _ => return false,
            }
        }
    }
    cues > 0
}

fn timestamp(value: &str) -> Option<u64> {
    let (whole, millis) = value.split_once('.')?;
    if millis.len() != 3 || !millis.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let parts = whole.split(':').collect::<Vec<_>>();
    if !(2..=3).contains(&parts.len())
        || parts
            .iter()
            .any(|p| p.len() < 2 || !p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let seconds = parts[parts.len() - 1].parse::<u64>().ok()?;
    let minutes = parts[parts.len() - 2].parse::<u64>().ok()?;
    if seconds >= 60 || minutes >= 60 {
        return None;
    }
    let hours = if parts.len() == 3 {
        parts[0].parse::<u64>().ok()?
    } else {
        0
    };
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes * 60_000 + seconds * 1000 + millis.parse::<u64>().ok()?)
}

pub(crate) const CANDIDATES: &str = "SELECT id FROM files WHERE id > $1
    AND video_codec IS NOT NULL
    AND item_id IN (SELECT id FROM items WHERE kind IN ('movie','episode'))
    ORDER BY id LIMIT $2";

// The update is atomic on both backends: duplicate downloads and concurrent
// additions cannot lose another track. A new source revision starts a new list.
pub(crate) const ADD: &str = "WITH caption(file_id, source_size, source_mtime, payload, provider_id)
    AS (VALUES ($1, $2, $3, $4, $5))
    UPDATE files SET downloaded_subtitles = json_insert(
    CASE WHEN json_extract(downloaded_subtitles, '$[0].source_size') = (SELECT source_size FROM caption)
          AND json_extract(downloaded_subtitles, '$[0].source_mtime') = (SELECT source_mtime FROM caption)
         THEN downloaded_subtitles ELSE '[]' END, '$[#]', json((SELECT payload FROM caption)))
    WHERE id = (SELECT file_id FROM caption)
      AND size = (SELECT source_size FROM caption) AND mtime = (SELECT source_mtime FROM caption)
      AND (json_extract(downloaded_subtitles, '$[0].source_size') IS NOT (SELECT source_size FROM caption)
        OR json_extract(downloaded_subtitles, '$[0].source_mtime') IS NOT (SELECT source_mtime FROM caption)
        OR (json_array_length(downloaded_subtitles) < 8 AND NOT EXISTS (
            SELECT 1 FROM json_each(downloaded_subtitles)
            WHERE json_extract(value, '$.provider_file_id') = (SELECT provider_id FROM caption))))";

pub(crate) fn encode(track: &DownloadedSubtitle) -> Result<String, StoreError> {
    if track.provider_file_id <= 0
        || track.source_size < 0
        || track.language.is_empty()
        || track.language.len() > 12
        || !track
            .language
            .bytes()
            .all(|b| b.is_ascii_alphabetic() || b == b'-')
        || track.title.len() > 256
        || !valid_downloaded_vtt(&track.vtt)
    {
        return Err(StoreError::Database("invalid downloaded subtitle".into()));
    }
    serde_json::to_string(track).map_err(|e| StoreError::Database(e.to_string()))
}
