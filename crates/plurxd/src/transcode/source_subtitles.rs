//! Source-owned native text extraction. No generic subtitle flight or cache
//! lookup can manufacture this operation's physical settlement receipt.
use super::{source_actor::SourceProducerAuthority, source_preparation::SourceProbeHookOwner};
use plurx_core::{
    domain::MediaFile, sharing_source_sessions::SourceDispatchAssignment, store::Store,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Instant,
};
use tokio::sync::Notify;

const TRACK_BYTES: usize = 2 * 1024 * 1024;
const TOTAL_BYTES: usize = 8 * 1024 * 1024;
const TRACKS: usize = 32;
const CUES: usize = 50_000;

pub(super) struct SourceNativeTracks {
    assignment: SourceDispatchAssignment,
    object_version: String,
    probe: String,
    tracks: BTreeMap<u16, Vec<u8>>,
    selected: Option<i64>,
}
impl SourceNativeTracks {
    pub(super) fn matches(&self, assignment: &SourceDispatchAssignment, object: &str) -> bool {
        self.assignment.same_identity(assignment) && self.object_version == object
    }
    pub(super) fn indexes(&self) -> BTreeSet<u16> {
        self.tracks.keys().copied().collect()
    }
    pub(super) fn selected(&self) -> Option<i64> {
        self.selected
    }
    pub(super) fn probe(&self) -> &str {
        &self.probe
    }
    pub(super) fn track(&self, index: u16) -> Option<&[u8]> {
        self.tracks.get(&index).map(Vec::as_slice)
    }
}
pub(super) struct SourceNativeSettlement {
    assignment: SourceDispatchAssignment,
}
impl SourceNativeSettlement {
    pub(super) fn matches(&self, assignment: &SourceDispatchAssignment) -> bool {
        self.assignment.same_identity(assignment)
    }
}
struct NativeState {
    permit: Mutex<Option<crate::vodencode::EncodePermit>>,
    result: Mutex<Option<Result<SourceNativeTracks, String>>>,
    settled: AtomicBool,
    changed: Notify,
}
pub(super) struct SourceNativeOperation {
    assignment: SourceDispatchAssignment,
    state: Arc<NativeState>,
    cancel: tokio_util::sync::CancellationToken,
}
impl SourceNativeOperation {
    pub(super) fn cancel(&self) {
        self.cancel.cancel();
    }
    pub(super) async fn settle(&self) -> SourceNativeSettlement {
        loop {
            let changed = self.state.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.state.settled.load(Ordering::Acquire) {
                return SourceNativeSettlement {
                    assignment: self.assignment.clone(),
                };
            }
            changed.await;
        }
    }
    pub(super) async fn outcome(&self) -> Result<SourceNativeTracks, String> {
        let _settled = self.settle().await;
        self.state
            .result
            .lock()
            .map_err(|_| "Source native result poisoned".to_owned())?
            .take()
            .ok_or_else(|| "Source native result consumed".to_owned())?
    }
}
/// Only embedded plain text tracks are included in this finite lane.
pub(super) fn supported_tracks(
    file: &MediaFile,
    selected: Option<i64>,
) -> Result<Vec<u16>, String> {
    if file.subtitle_streams.len() > 4096 {
        return Err("Source subtitle metadata exceeds bound".into());
    }
    let tracks: Vec<u16> = file
        .subtitle_streams
        .iter()
        .enumerate()
        .filter(|(i, s)| {
            file.downloaded_subtitle(*i as i64).is_none()
                && plurx_core::tracks::is_native_text_subtitle(&s.codec)
        })
        .map(|(i, _)| i as u16)
        .collect();
    if tracks.is_empty()
        || tracks.len() > TRACKS
        || selected.is_some_and(|i| !(0..=4095).contains(&i) || !tracks.contains(&(i as u16)))
    {
        return Err("Source native track is unsupported".into());
    }
    Ok(tracks)
}
#[allow(clippy::too_many_arguments)]
pub(super) fn start_source_native(
    file: MediaFile,
    source: crate::fragment_index_cluster::SourceFence,
    assignment: SourceDispatchAssignment,
    gate: Arc<SourceProducerAuthority>,
    selected: Option<i64>,
    permit: crate::vodencode::EncodePermit,
    store: Arc<dyn Store>,
    deadline: Instant,
    hooks: Arc<SourceProbeHookOwner>,
) -> SourceNativeOperation {
    let state = Arc::new(NativeState {
        permit: Mutex::new(Some(permit)),
        result: Mutex::new(None),
        settled: AtomicBool::new(false),
        changed: Notify::new(),
    });
    let cancel = tokio_util::sync::CancellationToken::new();
    let owned = Arc::clone(&state);
    let owned_cancel = cancel.clone();
    let key = assignment.clone();
    tokio::spawn(async move {
        let result = extract_native(
            &file,
            &source,
            &key,
            &gate,
            selected,
            store.as_ref(),
            deadline,
            &owned_cancel,
            &hooks,
        )
        .await;
        // extract_native returns only after each real child and both pipe
        // tasks have settled. A task panic retains this state and permit.
        drop(
            owned
                .permit
                .lock()
                .expect("Source native permit owner")
                .take(),
        );
        *owned.result.lock().expect("Source native result owner") = Some(result);
        owned.settled.store(true, Ordering::Release);
        owned.changed.notify_waiters();
    });
    SourceNativeOperation {
        assignment,
        state,
        cancel,
    }
}
#[allow(clippy::too_many_arguments)]
async fn extract_native(
    file: &MediaFile,
    source: &crate::fragment_index_cluster::SourceFence,
    assignment: &SourceDispatchAssignment,
    gate: &SourceProducerAuthority,
    selected: Option<i64>,
    store: &dyn Store,
    deadline: Instant,
    cancel: &tokio_util::sync::CancellationToken,
    hooks: &SourceProbeHookOwner,
) -> Result<SourceNativeTracks, String> {
    let indexes = supported_tracks(file, selected)?;
    let initial = gate
        .current_preparation(assignment)
        .await
        .map_err(|_| "Source native current authority unavailable".to_owned())?;
    let probe = store
        .source_index_probe_evidence(&initial)
        .await
        .map_err(|_| "Source native evidence read failed".to_owned())?
        .ok_or_else(|| "Source native stored probe unavailable".to_owned())?;
    let mut tracks = BTreeMap::new();
    let mut total = 0usize;
    for ordinal in indexes {
        // A new actual membership read supplies each child's own original
        // five-second observation window. The absolute start budget is fixed.
        let proof = gate
            .current_preparation(assignment)
            .await
            .map_err(|_| "Source native member authority unavailable".to_owned())?;
        let command = crate::ffmpeg::source_native_text_command(&source.handle, ordinal)?;
        let output = super::source_preparation::run_child(
            file,
            source,
            &proof,
            store,
            deadline,
            cancel,
            command,
            TRACK_BYTES,
            hooks,
        )
        .await?;
        validate_text(&output.stdout)?;
        total = total
            .checked_add(output.stdout.len())
            .filter(|n| *n <= TOTAL_BYTES)
            .ok_or_else(|| "Source native aggregate exceeds bound".to_owned())?;
        tracks.insert(ordinal, output.stdout);
    }
    if cancel.is_cancelled() || Instant::now() >= deadline || !source.unchanged() {
        return Err("Source native extraction expired or file changed".into());
    }
    Ok(SourceNativeTracks {
        assignment: assignment.clone(),
        object_version: source.object_version().into(),
        probe,
        tracks,
        selected,
    })
}
fn validate_text(bytes: &[u8]) -> Result<(), String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "Source native text is not UTF-8".to_owned())?;
    if !text.starts_with("WEBVTT") || bytes.len() > TRACK_BYTES || text.contains('\0') {
        return Err("Source native document is unsupported".into());
    }
    let mut count = 0usize;
    for line in text.lines().filter(|line| line.contains("-->")) {
        count += 1;
        if count > CUES || line.len() > 4096 {
            return Err("Source native cue bound exceeded".into());
        }
        let (left, right) = line
            .split_once("-->")
            .ok_or_else(|| "Source native cue malformed".to_owned())?;
        let end = right
            .split_whitespace()
            .next()
            .ok_or_else(|| "Source native cue end missing".to_owned())?;
        let start = timestamp(left)?;
        let end = timestamp(end)?;
        if end < start {
            return Err("Source native cue runs backwards".into());
        }
    }
    Ok(())
}
fn timestamp(raw: &str) -> Result<f64, String> {
    let parts: Vec<_> = raw.trim().split(':').collect();
    let parsed = match parts.as_slice() {
        [h, m, s] => h
            .parse::<u64>()
            .ok()
            .zip(m.parse::<u64>().ok())
            .zip(s.parse::<f64>().ok())
            .filter(|((_, m), s)| *m < 60 && s.is_finite() && *s >= 0.0 && *s < 60.0)
            .map(|((h, m), s)| h as f64 * 3600.0 + m as f64 * 60.0 + s),
        [m, s] => m
            .parse::<u64>()
            .ok()
            .zip(s.parse::<f64>().ok())
            .filter(|(_, s)| s.is_finite() && *s >= 0.0 && *s < 60.0)
            .map(|(m, s)| m as f64 * 60.0 + s),
        _ => None,
    };
    parsed
        .filter(|n| n.is_finite() && *n <= 1_728_000.0)
        .ok_or_else(|| "Source native cue timestamp exceeds bound".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_native_text_refuses_malformed_and_excessive_cues() {
        assert!(validate_text(b"WEBVTT\n\n00:00:00.200 --> 00:00:01.800\nactual\n").is_ok());
        for malformed in ["00:00:NaN", "00:00:60", "00:60:00", "-1:00:00", "481:00:00"] {
            assert!(validate_text(
                format!("WEBVTT\n\n{malformed} --> 00:00:02.000\ntext\n").as_bytes()
            )
            .is_err());
        }
        assert!(validate_text(b"WEBVTT\n\n00:00:02.000 --> 00:00:01.000\ntext\n").is_err());
        assert!(validate_text(b"WEBVTT\0").is_err());
        assert!(validate_text(&vec![b'x'; TRACK_BYTES + 1]).is_err());
        let excessive = format!(
            "WEBVTT\n{}",
            "00:00:00.000 --> 00:00:01.000\n".repeat(CUES + 1)
        );
        assert!(validate_text(excessive.as_bytes()).is_err());
    }
}
