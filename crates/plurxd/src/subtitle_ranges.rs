//! Playback text ranges are disposable windows, never whole-track artifacts.
//! One session owner drives a bounded fan-out; each completed window can serve
//! immediately, while the existing durable subtitle job continues separately.

use std::path::Path;
use std::time::Duration;

use futures_util::stream::{FuturesUnordered, StreamExt};
use plurx_core::domain::MediaFile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::http::peer_transport::{PeerAuthMode, PeerTransport};
use crate::subtitle_source::StoreAccess;

pub(crate) const PATH: &str = "/internal/media/subtitle-range";
pub(crate) const MAX_RESPONSE: usize = 16 * 1024 * 1024;
const MAX_VTT: u64 = 8 * 1024 * 1024;
const PEER_BUDGET: Duration = Duration::from_secs(30);
pub(crate) const WORK_BUDGET: Duration = Duration::from_secs(25);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub file_id: i64,
    pub size: i64,
    pub mtime: i64,
    pub ordinal: i64,
    pub anchor: i64,
    pub span: i64,
    pub attestation: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Answer {
    pub request: Request,
    pub vtt: String,
    pub sha256: String,
}

/// Only the indexed container/text combination exercised by the seek proof.
/// Other containers, downloaded tracks and bitmap streams keep their producer.
pub(crate) fn indexed_text(file: &MediaFile, ordinal: i64) -> bool {
    matches!(file.container.as_deref(), Some("mkv" | "matroska"))
        && file.subtitle_streams.iter().any(|track| {
            track.index == ordinal
                && matches!(
                    track.codec.as_str(),
                    "subrip" | "srt" | "ass" | "ssa" | "webvtt" | "text"
                )
        })
}

impl Request {
    pub(crate) fn matches(&self, file: &MediaFile) -> bool {
        self.file_id == file.id
            && self.size == file.size
            && self.mtime == file.mtime
            && self.anchor >= 0
            && self.span == crate::subtitles::bounded_window_seconds(self.span)
            && self.anchor % self.span == 0
            && file
                .duration_ms
                .is_some_and(|duration| self.anchor < duration / 1000)
            && self.attestation.len() == 64
            && self.attestation.bytes().all(|b| b.is_ascii_hexdigit())
            && indexed_text(file, self.ordinal)
    }

    fn same(&self, other: &Self) -> bool {
        self.file_id == other.file_id
            && self.size == other.size
            && self.mtime == other.mtime
            && self.ordinal == other.ordinal
            && self.anchor == other.anchor
            && self.span == other.span
            && self.attestation == other.attestation
    }
}

async fn publish(
    dir: &Path,
    file: &MediaFile,
    ordinal: i64,
    anchor: i64,
    span: i64,
    bytes: &[u8],
) -> Result<(), String> {
    if !std::str::from_utf8(bytes).is_ok_and(|text| valid_vtt(text, anchor, span)) {
        return Err("invalid subtitle range".to_owned());
    }
    let cached = crate::subtitles::vtt_window_path(dir, file, ordinal, anchor, span);
    let name = cached
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("invalid range cache name")?;
    let directory = plurx_core::fs_secure::SecureDirectory::open(dir)
        .await
        .map_err(|e| e.to_string())?;
    directory
        .atomic_write_child(name, bytes)
        .await
        .map_err(|e| e.to_string())
}

/// Current range first; at most two peers work on the next grid ranges.
/// Futures remain under the caller's session owner, so a seek drops the whole
/// fan-out and no obsolete response can publish on the requester afterward.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn prepare(
    access: &StoreAccess,
    dir: &Path,
    tmp: &Path,
    file: &MediaFile,
    ordinal: i64,
    anchor: i64,
    span: i64,
) -> Result<(), String> {
    if !indexed_text(file, ordinal) {
        return crate::subtitles::extract_vtt_window(tmp, file, ordinal, anchor, span).await;
    }
    let current = async {
        crate::subtitles::extract_vtt_window(tmp, file, ordinal, anchor, span).await?;
        let bytes = plurx_core::fs_secure::read_bounded_regular(tmp, MAX_VTT)
            .await
            .map_err(|e| e.to_string())?;
        publish(dir, file, ordinal, anchor, span, &bytes).await
    };
    let ahead = tokio::time::timeout(
        PEER_BUDGET,
        prefetch(access, dir, file, ordinal, anchor, span),
    );
    let (current, _) = tokio::join!(current, ahead);
    current
}

async fn prefetch(
    access: &StoreAccess,
    dir: &Path,
    file: &MediaFile,
    ordinal: i64,
    anchor: i64,
    span: i64,
) -> Result<(), String> {
    let Some(membership) = access.membership() else {
        return Ok(());
    };
    let node_id = access.node_id().unwrap_or_default();
    let peers = tokio::time::timeout(Duration::from_secs(2), membership.media_peers()).await;
    let peers: Vec<_> = peers
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default()
        .into_iter()
        .filter(|peer| peer.reachable && peer.node_id != node_id && peer.http_base.is_some())
        .take(2)
        .collect();
    if peers.is_empty() {
        return Ok(());
    }
    let attested =
        crate::fragment_index_cluster::attest_source(node_id, file, None, &|_| {}).await?;
    let digest = attested.observation.source_sha256.clone();
    let transport = PeerTransport::new(membership.clone());
    let mut ahead = FuturesUnordered::new();
    for (offset, peer) in peers.into_iter().enumerate() {
        let next = anchor.saturating_add(span.saturating_mul(offset as i64 + 1));
        if file
            .duration_ms
            .is_none_or(|duration| next >= duration / 1000)
        {
            continue;
        }
        if crate::subtitles::read_cached_window(dir, file, ordinal, next, span)
            .await
            .ok()
            .flatten()
            .is_some()
        {
            continue;
        }
        let request = Request {
            file_id: file.id,
            size: file.size,
            mtime: file.mtime,
            ordinal,
            anchor: next,
            span,
            attestation: digest.clone(),
        };
        let transport = transport.clone();
        ahead.push(async move {
            let body = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
            let response = transport
                .request(
                    &peer.node_id,
                    peer.http_base.as_deref().unwrap_or_default(),
                    reqwest::Method::POST,
                    PATH,
                    body,
                    tokio::time::Instant::now() + PEER_BUDGET,
                    MAX_RESPONSE,
                    PeerAuthMode::ExactRequestAndMemberResponse,
                )
                .await
                .map_err(|_| "subtitle peer unavailable".to_owned())?;
            if !response.status.is_success() {
                return Err("subtitle peer refused range".to_owned());
            }
            let answer: Answer =
                serde_json::from_slice(&response.body).map_err(|e| e.to_string())?;
            if !request.same(&answer.request)
                || answer.sha256 != hex::encode(Sha256::digest(answer.vtt.as_bytes()))
                || !valid_vtt(&answer.vtt, request.anchor, request.span)
            {
                return Err("subtitle peer returned a different range".to_owned());
            }
            Ok((request.anchor, answer.vtt))
        });
    }
    while let Some(result) = ahead.next().await {
        if let Ok((next, vtt)) = result {
            verify_source(file, &attested).await?;
            publish(dir, file, ordinal, next, span, vtt.as_bytes()).await?;
            tracing::info!(
                file_id = file.id,
                ordinal,
                anchor = next,
                "peer subtitle playback range cached"
            );
        }
    }
    Ok(())
}

async fn verify_source(
    file: &MediaFile,
    attested: &crate::fragment_index_cluster::AttestedSource,
) -> Result<(), String> {
    if !crate::fragment_index_cluster::source_still_matches(&attested.handle, &attested.observation)
        .unwrap_or(false)
        || crate::fragment_index_cluster::inspect_source(file)
            .await?
            .as_str()
            != attested.observation.object_version
    {
        return Err("subtitle range source superseded".to_owned());
    }
    Ok(())
}

/// Peer execution opens only the library's stored path, never a caller path.
/// The returned bytes are bound to the sampled digest and exact range.
pub(crate) async fn execute(
    state: &crate::state::AppState,
    request: Request,
) -> Result<Answer, String> {
    let _claim = RangeClaim::acquire(&request)?;
    let file = state
        .store
        .get_file(request.file_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or("subtitle file missing")?;
    if !request.matches(&file) {
        return Err("invalid subtitle range identity".to_owned());
    }
    let attested =
        crate::fragment_index_cluster::attest_source(&state.node_id, &file, None, &|_| {}).await?;
    if request.attestation != attested.observation.source_sha256 {
        return Err("subtitle source digest mismatch".to_owned());
    }
    tokio::fs::create_dir_all(&state.subs_dir)
        .await
        .map_err(|e| e.to_string())?;
    let tmp = state
        .subs_dir
        .join(format!(".range-{}.vtt", uuid::Uuid::new_v4()));
    let cleanup = Temp(tmp.clone());
    crate::subtitles::extract_vtt_window(
        &tmp,
        &file,
        request.ordinal,
        request.anchor,
        request.span,
    )
    .await?;
    verify_source(&file, &attested).await?;
    let bytes = plurx_core::fs_secure::read_bounded_regular(&tmp, MAX_VTT)
        .await
        .map_err(|e| e.to_string())?;
    let vtt = String::from_utf8(bytes).map_err(|e| e.to_string())?;
    if !valid_vtt(&vtt, request.anchor, request.span) {
        return Err("invalid subtitle range output".to_owned());
    }
    drop(cleanup);
    let sha256 = hex::encode(Sha256::digest(vtt.as_bytes()));
    Ok(Answer {
        request,
        vtt,
        sha256,
    })
}

struct Temp(std::path::PathBuf);
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Reject malformed cue timestamps and responses outside the requested range.
/// Cues may extend past its end; ownership follows their start timestamp, as
/// in the existing playback-window producer. Empty windows are valid.
fn valid_vtt(text: &str, anchor: i64, span: i64) -> bool {
    if text.len() as u64 > MAX_VTT || !text.starts_with("WEBVTT\n") {
        return false;
    }
    let end = anchor
        .saturating_add(span)
        .saturating_add(crate::subtitles::WINDOW_SLACK_SECONDS) as f64;
    for block in text
        .split("\n\n")
        .skip(1)
        .filter(|block| !block.trim().is_empty())
    {
        let mut lines = block.lines();
        let Some(first) = lines.next() else {
            return false;
        };
        let timing = if first.contains("-->") {
            first
        } else {
            let Some(line) = lines.next() else {
                return false;
            };
            line
        };
        let Some((start, stop)) = timing.split_once(" --> ") else {
            return false;
        };
        let Some(start) = crate::subtitles::parse_window_timestamp(start) else {
            return false;
        };
        let Some(stop) = stop
            .split_whitespace()
            .next()
            .and_then(crate::subtitles::parse_window_timestamp)
        else {
            return false;
        };
        if !start.is_finite()
            || !stop.is_finite()
            || start < anchor as f64
            || start > end
            || stop < start
        {
            return false;
        }
        if lines.next().is_none() {
            return false;
        }
    }
    true
}

static CLAIMS: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);
struct RangeClaim(String);
impl RangeClaim {
    fn acquire(request: &Request) -> Result<Self, String> {
        let key = serde_json::to_string(request).map_err(|e| e.to_string())?;
        let mut claims = CLAIMS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !claims.insert(key.clone()) {
            return Err("subtitle range already running".to_owned());
        }
        Ok(Self(key))
    }
}
impl Drop for RangeClaim {
    fn drop(&mut self) {
        CLAIMS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plurx_core::domain::SubtitleStream;

    fn file(path: std::path::PathBuf) -> MediaFile {
        MediaFile {
            downloaded_subtitles: Vec::new(),
            id: 77,
            item_id: 1,
            path,
            size: 12345,
            mtime: 678,
            duration_ms: Some(600_000),
            container: Some("mkv".into()),
            video_codec: Some("h264".into()),
            video_codec_tag: None,
            field_order: None,
            video_profile: None,
            width: Some(64),
            height: Some(64),
            bit_depth: Some(8),
            hdr: None,
            hdr_format: None,
            max_cll: None,
            max_fall: None,
            mastering_max_luminance: None,
            luminance_source: None,
            bitrate: None,
            audio_streams: vec![],
            subtitle_streams: vec![SubtitleStream {
                index: 0,
                codec: "subrip".into(),
                ..Default::default()
            }],
            scanned_at: 0,
            audio_offset_ms: 0,
            probed: true,
            dolby_vision: Default::default(),
        }
    }

    fn request() -> Request {
        Request {
            file_id: 77,
            size: 12345,
            mtime: 678,
            ordinal: 0,
            anchor: 400,
            span: 200,
            attestation: "a".repeat(64),
        }
    }

    #[test]
    fn range_identity_rejects_stale_stamp_bad_grid_and_bitmap() {
        let mut file = file("unused".into());
        let mut request = request();
        assert!(request.matches(&file));
        request.size += 1;
        assert!(!request.matches(&file));
        request.size -= 1;
        request.anchor += 1;
        assert!(!request.matches(&file));
        request.anchor -= 1;
        file.subtitle_streams[0].codec = "hdmv_pgs_subtitle".into();
        assert!(!request.matches(&file));
    }

    #[test]
    fn peer_range_rejects_malformed_nonfinite_and_wrong_timeline() {
        assert!(valid_vtt(
            "WEBVTT\n\n06:40.000 --> 06:44.000\nhello\n",
            400,
            200
        ));
        assert!(valid_vtt("WEBVTT\n\n", 400, 200));
        for text in [
            "WEBVTT\n\n00:00.000 --> 00:04.000\nwrong origin\n",
            "WEBVTT\n\n06:NaN --> 06:44.000\nnonfinite\n",
            "WEBVTT\n\n06:44.000 --> 06:40.000\nnegative duration\n",
            "WEBVTT\n\nnot a cue\n",
        ] {
            assert!(!valid_vtt(text, 400, 200), "{text}");
        }
    }

    #[test]
    fn same_range_deduplicates_and_cancel_releases_claim() {
        let mut request = request();
        request.file_id = 787818;
        let first = RangeClaim::acquire(&request).expect("first owner");
        assert!(RangeClaim::acquire(&request).is_err());
        let mut next = request.clone();
        next.anchor += next.span;
        let _next = RangeClaim::acquire(&next).expect("distinct range");
        drop(first);
        assert!(RangeClaim::acquire(&request).is_ok());
    }

    #[tokio::test]
    async fn indexed_late_window_matches_full_scan_with_nonzero_source_start() {
        let dir = crate::test_tempdir().expect("fixture");
        let input = dir.path().join("source.mkv");
        let srt = dir.path().join("source.srt");
        std::fs::write(&srt, "1\n00:00:10,000 --> 00:00:14,000\nearly\n\n2\n00:06:50,000 --> 00:06:54,000\nlate\n\n3\n00:08:30,000 --> 00:08:34,000\nnext\n").expect("fixture operation");
        let result = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
            .args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=s=64x64:r=1:d=600",
                "-i",
            ])
            .arg(&srt)
            .args([
                "-c:v",
                "libx264",
                "-preset",
                "ultrafast",
                "-g",
                "10",
                "-c:s",
                "srt",
                "-output_ts_offset",
                "7.5",
            ])
            .arg(&input)
            .output()
            .await
            .expect("ffmpeg available");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let mut file = file(input.clone());
        let metadata = std::fs::metadata(&input).expect("fixture operation");
        let stamp = crate::fragment_index_cluster::source_stamp(&metadata);
        file.size = metadata.len() as i64;
        file.mtime = stamp.mtime;
        let ranged = dir.path().join("ranged.vtt");
        crate::subtitles::extract_vtt_window(&ranged, &file, 0, 400, 200)
            .await
            .expect("bounded extraction");
        let scan = dir.path().join("scan.vtt");
        let result = tokio::process::Command::new(crate::ffmpeg::ffmpeg_bin())
            .args(["-v", "error", "-i"])
            .arg(&input)
            .args(["-ss", "400", "-to", "660", "-map", "0:s:0", "-f", "webvtt"])
            .arg(&scan)
            .output()
            .await
            .expect("fixture operation");
        assert!(result.status.success());
        let expected = crate::subtitles::normalize_window_cues(
            &std::fs::read(scan).expect("fixture operation"),
            400,
        );
        let actual = std::fs::read(ranged).expect("fixture operation");
        assert_eq!(actual, expected, "indexed seek must retain exact cue times");
        assert!(String::from_utf8(actual)
            .expect("fixture operation")
            .contains("late"));
    }

    #[tokio::test]
    async fn indexed_text_starts_window_after_midpoint_while_whole_track_is_healthy() {
        let dir = crate::test_tempdir().expect("fixture operation");
        let file = file(dir.path().join("unused.mkv"));
        let session = format!("indexed-late-{}", uuid::Uuid::new_v4());
        let started = crate::subtitles::warm_vtt_window_with(
            &session,
            Some(1),
            dir.path(),
            &file,
            0,
            400,
            200,
            |_tmp, _file, _index, _anchor, _span| async {
                std::future::pending::<Result<(), String>>().await
            },
        )
        .await;
        assert!(
            started,
            "indexed seeks must not retain the prefix-scan midpoint limit"
        );
        crate::subtitles::release_session_window(&session).await;
    }
}
