use super::*;

/// Stable non-secret correlation for bearer session capabilities. Raw UUIDs
/// authorize playback and therefore never belong in logs, traces, metrics, or
/// diagnostics even though they look like ordinary identifiers.
pub(crate) fn session_log_id(session_id: &str) -> String {
    let digest = Sha256::digest(session_id.as_bytes());
    format!("s-{}", hex::encode(digest))
}

/// Remove the bearer capability anywhere a child-process diagnostic echoed
/// it. Structured fields already use [`session_log_id`], but ffmpeg repeats
/// paths and complete arguments in stderr; sanitizing the message body keeps
/// those unstructured surfaces under the same contract.
pub(super) fn session_log_text(text: &str, session_id: &str) -> String {
    if session_id.is_empty() {
        return text.to_owned();
    }
    text.replace(session_id, &session_log_id(session_id))
}

pub(super) fn ffmpeg_args_log_message(label: &str, args: &[String], session_id: &str) -> String {
    format!(
        "{label}: {}",
        session_log_text(&crate::scratch_put::redact(&args.join(" ")), session_id)
    )
}

pub(super) fn log_ffmpeg_stderr(session_id: &str, encoder: &str, line: &str) {
    let line = session_log_text(&crate::scratch_put::redact(line), session_id);
    tracing::warn!(
        target: "plurxd::transcode",
        session = %session_log_id(session_id),
        encoder,
        "transcode ffmpeg: {line}"
    );
}
