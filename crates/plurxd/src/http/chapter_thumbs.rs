//! Chapter thumbnails: one small frame per chapter, made on request and kept
//! under the node-local runtime cache.
//!
//! The watch view shows a picture for every chapter. Nothing on the server
//! held such a picture, so this route makes one the first time it is asked
//! for: a single `ffmpeg` seek into the source, one decoded frame scaled to
//! 320 px wide, written as JPEG beside the other node-local caches. The second
//! request for the same chapter is a file read. There is no background
//! producer and no library sweep — the only work this module ever does is the
//! chapter a viewer's page just asked for, and every extraction is counted so
//! the Developer tab can say how many ran, how many failed and how many are
//! running now. The `chapter_thumbnails` switch there stops new extractions;
//! nothing here outlives the request that started it by more than the
//! extraction timeout.

use std::path::{Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use tokio::sync::Semaphore;

use super::dto::{chapters_from_probe_json, ChapterDto};
use super::error::ApiError;
use super::extract::AuthUser;
use crate::state::AppState;

/// Width of every thumbnail. Height follows the source aspect.
const THUMB_WIDTH: u32 = 320;
/// Seek past the chapter mark so a fade-from-black chapter start still yields
/// a picture, but never past the chapter's midpoint.
const SEEK_OFFSET_MS: i64 = 2_000;
/// Two extractions at a time on a node. A watch page asks for every chapter of
/// the title at once; the rest queue for a permit rather than fanning out one
/// ffmpeg per chapter.
const EXTRACT_CONCURRENCY: usize = 2;
/// How long a request waits for a permit before it is told to come back.
const PERMIT_WAIT: Duration = Duration::from_secs(20);
/// One seek plus one decoded frame. A remux on a slow disk still finishes in a
/// few seconds; anything longer is a stuck child and is killed.
const EXTRACT_TIMEOUT: Duration = Duration::from_secs(15);
/// A 320-px JPEG is tens of kilobytes. Bound the pipe so a misbehaving encoder
/// cannot fill the cache.
const MAX_THUMB_BYTES: u64 = 2 * 1024 * 1024;
/// Under the runtime cache, never the served artwork root: that root is walked
/// by the artwork orphan sweep and is for library artwork only.
const CACHE_DIR: &str = "chapter-thumbs";
const CACHE_CONTROL: &str = "private, max-age=604800";

static EXTRACT_PERMITS: Semaphore = Semaphore::const_new(EXTRACT_CONCURRENCY);

/// Process-local counts for the Developer tab: requests answered from the
/// cache, extractions that produced a thumbnail, extractions that failed, and
/// extractions running right now.
static SERVED_CACHED: AtomicU64 = AtomicU64::new(0);
static GENERATED: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);
static IN_FLIGHT: AtomicU64 = AtomicU64::new(0);
static REFUSED_OFF: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub served_cached: u64,
    pub generated: u64,
    pub failed: u64,
    pub in_flight: u64,
    pub refused_off: u64,
}

pub(crate) fn snapshot() -> Snapshot {
    Snapshot {
        served_cached: SERVED_CACHED.load(Ordering::Relaxed),
        generated: GENERATED.load(Ordering::Relaxed),
        failed: FAILED.load(Ordering::Relaxed),
        in_flight: IN_FLIGHT.load(Ordering::Relaxed),
        refused_off: REFUSED_OFF.load(Ordering::Relaxed),
    }
}

pub(crate) fn cache_root(runtime_cache: &FsPath) -> PathBuf {
    runtime_cache.join(CACHE_DIR)
}

/// What the cache holds on disk right now: thumbnail count and bytes. Bounded
/// by the number of chapters ever viewed on this node, so a walk is cheap.
pub(crate) fn cache_footprint(runtime_cache: &FsPath) -> (u64, u64) {
    let mut files = 0u64;
    let mut bytes = 0u64;
    let Ok(titles) = std::fs::read_dir(cache_root(runtime_cache)) else {
        return (0, 0);
    };
    for title in titles.flatten() {
        let Ok(entries) = std::fs::read_dir(title.path()) else {
            continue;
        };
        for entry in entries.flatten() {
            if let Ok(metadata) = entry.metadata() {
                if metadata.is_file() {
                    files += 1;
                    bytes += metadata.len();
                }
            }
        }
    }
    (files, bytes)
}

pub(crate) async fn enabled(state: &AppState) -> bool {
    let value = state
        .store
        .get_setting(plurx_core::store::keys::CHAPTER_THUMBNAILS)
        .await
        .ok()
        .flatten();
    plurx_core::store::stored_switch(value.as_deref(), true)
}

/// The cache key names the file's identity, so a replaced file gets fresh
/// thumbnails and the old ones become an orphan directory the operator can
/// clear from the Developer tab.
fn title_dir(runtime_cache: &FsPath, file_id: i64, size: i64, mtime: i64) -> PathBuf {
    cache_root(runtime_cache).join(format!("{file_id}-{size}-{mtime}"))
}

fn seek_ms(chapter: &ChapterDto) -> i64 {
    let span = chapter.end_ms.saturating_sub(chapter.start_ms).max(0);
    chapter
        .start_ms
        .saturating_add(SEEK_OFFSET_MS.min(span / 2))
        .max(0)
}

/// GET /api/v1/files/:id/chapters/:index/thumb
pub async fn serve(
    _user: AuthUser,
    State(state): State<AppState>,
    Path((file_id, index)): Path<(i64, usize)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !enabled(&state).await {
        REFUSED_OFF.fetch_add(1, Ordering::Relaxed);
        return Err(ApiError::NotFound("chapter thumbnails are off"));
    }
    let file = state
        .store
        .get_file(file_id)
        .await?
        .ok_or(ApiError::NotFound("file"))?;
    let probe = state.catalogue.get_file_probe_json(file_id).await?;
    let chapters = chapters_from_probe_json(probe.as_deref());
    let chapter = chapters.get(index).ok_or(ApiError::NotFound("chapter"))?;

    let etag = format!("\"ch-{file_id}-{}-{}-{index}\"", file.size, file.mtime);
    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(',').any(|tag| tag.trim() == etag))
    {
        return Ok(not_modified(&etag));
    }

    let dir = title_dir(&state.runtime_cache_dir, file_id, file.size, file.mtime);
    let path = dir.join(format!("{index}.jpg"));
    if let Ok(bytes) = tokio::fs::read(&path).await {
        SERVED_CACHED.fetch_add(1, Ordering::Relaxed);
        return Ok(jpeg_response(bytes, &etag));
    }

    let permit = tokio::time::timeout(PERMIT_WAIT, EXTRACT_PERMITS.acquire())
        .await
        .map_err(|_| {
            ApiError::ServiceUnavailable(
                "chapter thumbnail extraction is busy; try again shortly".to_owned(),
            )
        })?
        .map_err(|_| ApiError::ServiceUnavailable("extraction is shut down".to_owned()))?;
    // A request that queued behind the one that made this thumbnail finds it
    // on disk now and never spawns a second extraction for the same chapter.
    if let Ok(bytes) = tokio::fs::read(&path).await {
        drop(permit);
        SERVED_CACHED.fetch_add(1, Ordering::Relaxed);
        return Ok(jpeg_response(bytes, &etag));
    }
    IN_FLIGHT.fetch_add(1, Ordering::Relaxed);
    let result = extract(
        &crate::ffmpeg::ffmpeg_bin(),
        &file.path,
        seek_ms(chapter),
        &dir,
        &path,
        &state.runtime_cache_dir,
    )
    .await;
    IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
    drop(permit);
    match result {
        Ok(bytes) => {
            GENERATED.fetch_add(1, Ordering::Relaxed);
            Ok(jpeg_response(bytes, &etag))
        }
        Err(reason) => {
            FAILED.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                file_id,
                chapter = index,
                path = %file.path.display(),
                reason,
                "chapter thumbnail extraction failed"
            );
            Err(ApiError::NotFound("chapter thumbnail could not be made"))
        }
    }
}

async fn extract(
    ffmpeg_bin: &str,
    source: &FsPath,
    seek_ms: i64,
    dir: &FsPath,
    target: &FsPath,
    runtime_cache: &FsPath,
) -> Result<Vec<u8>, String> {
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|error| format!("could not create {}: {error}", dir.display()))?;
    let temporary = dir.join(format!(
        ".{}.{}.tmp",
        target
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("thumb"),
        uuid::Uuid::new_v4().simple()
    ));
    let mut command = extract_command(ffmpeg_bin, source, seek_ms, runtime_cache);
    let child = crate::ffmpeg::BoundedDiagnosticChild::spawn_piped_output(&mut command)
        .map_err(|error| error.to_string())?;
    let generated = tokio::time::timeout(
        EXTRACT_TIMEOUT,
        child.output_to_bounded_file(&temporary, MAX_THUMB_BYTES),
    )
    .await;
    let outcome = match generated {
        Ok(Ok((status, _))) if status.success() => Ok(()),
        Ok(Ok((status, diagnostics))) => Err(format!("ffmpeg exited {status}: {diagnostics}")),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err(format!(
            "ffmpeg timed out after {} seconds",
            EXTRACT_TIMEOUT.as_secs()
        )),
    };
    if let Err(error) = outcome {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(error);
    }
    let bytes = tokio::fs::read(&temporary)
        .await
        .map_err(|error| error.to_string())?;
    // ffmpeg can exit 0 having written nothing when the seek lands past the
    // last frame; an empty file is a failure, not a thumbnail.
    if bytes.is_empty() {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err("ffmpeg produced no frame".to_owned());
    }
    tokio::fs::rename(&temporary, target)
        .await
        .map_err(|error| error.to_string())?;
    Ok(bytes)
}

/// The extraction child, built separately so a test can read its argv.
fn extract_command(
    ffmpeg_bin: &str,
    source: &FsPath,
    seek_ms: i64,
    runtime_cache: &FsPath,
) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(ffmpeg_bin);
    command.args(["-nostdin", "-hide_banner", "-loglevel", "error"]);
    // Input-side seek: the demuxer jumps to the nearest keyframe before the
    // mark and decodes forward from there, which is the cheap seek.
    command.args(["-ss", &format!("{}.{:03}", seek_ms / 1000, seek_ms % 1000)]);
    command.arg("-i");
    command.arg(source);
    command.args([
        "-map",
        "0:v:0",
        "-an",
        "-sn",
        "-frames:v",
        "1",
        "-vf",
        &format!("scale='min({THUMB_WIDTH},iw)':-2:flags=lanczos,format=yuvj420p"),
        "-q:v",
        "4",
        "-f",
        "image2pipe",
        "-vcodec",
        "mjpeg",
        "pipe:1",
    ]);
    crate::producer_spawn::configure_ffmpeg_runtime(&mut command, runtime_cache);
    command
}

fn jpeg_response(bytes: Vec<u8>, etag: &str) -> Response {
    let mut response = (StatusCode::OK, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    response
}

fn not_modified(etag: &str) -> Response {
    let mut response = StatusCode::NOT_MODIFIED.into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(CACHE_CONTROL),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chapter(start_ms: i64, end_ms: i64) -> ChapterDto {
        ChapterDto {
            index: 0,
            title: String::new(),
            start_ms,
            end_ms,
        }
    }

    #[test]
    fn seek_lands_two_seconds_in_but_never_past_the_midpoint() {
        assert_eq!(seek_ms(&chapter(0, 600_000)), 2_000);
        assert_eq!(seek_ms(&chapter(566_000, 866_000)), 568_000);
        // A three-second chapter seeks to its midpoint, not two seconds in.
        assert_eq!(seek_ms(&chapter(10_000, 13_000)), 11_500);
        assert_eq!(seek_ms(&chapter(10_000, 10_000)), 10_000);
    }

    #[test]
    fn extraction_argv_seeks_on_the_input_and_takes_one_scaled_frame() {
        let command = extract_command(
            "ffmpeg",
            FsPath::new("/media/Movies/Title (1993)/title.mkv"),
            568_250,
            FsPath::new("/cache"),
        );
        let argv: Vec<String> = command
            .as_std()
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let ss = argv.iter().position(|arg| arg == "-ss").expect("-ss");
        let input = argv.iter().position(|arg| arg == "-i").expect("-i");
        assert!(ss < input, "the seek must precede the input: {argv:?}");
        assert_eq!(argv[ss + 1], "568.250");
        assert!(argv.windows(2).any(|w| w == ["-frames:v", "1"]), "{argv:?}");
        assert!(argv.windows(2).any(|w| w == ["-map", "0:v:0"]), "{argv:?}");
        let vf = argv.iter().position(|arg| arg == "-vf").expect("-vf");
        assert!(argv[vf + 1].contains("min(320,iw)"), "{argv:?}");
        assert_eq!(argv.last().map(String::as_str), Some("pipe:1"));
        let env: Vec<(String, String)> = command
            .as_std()
            .get_envs()
            .filter_map(|(key, value)| {
                Some((
                    key.to_string_lossy().into_owned(),
                    value?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert!(env.contains(&("XDG_CACHE_HOME".to_owned(), "/cache".to_owned())));
    }

    #[test]
    fn cache_key_names_the_file_identity() {
        let dir = title_dir(FsPath::new("/cache"), 70, 7_100_000_000, 1_700_000_000);
        assert_eq!(
            dir,
            PathBuf::from("/cache/chapter-thumbs/70-7100000000-1700000000")
        );
    }

    #[test]
    fn footprint_counts_only_files_under_title_directories() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(cache_footprint(tmp.path()), (0, 0));
        let title = title_dir(tmp.path(), 1, 2, 3);
        std::fs::create_dir_all(&title).expect("dir");
        std::fs::write(title.join("0.jpg"), b"abcd").expect("write");
        std::fs::write(title.join("1.jpg"), b"ab").expect("write");
        std::fs::create_dir_all(title.join("not-a-thumb")).expect("dir");
        assert_eq!(cache_footprint(tmp.path()), (2, 6));
    }
}
