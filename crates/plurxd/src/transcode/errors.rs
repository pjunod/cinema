use super::*;

/// A capacity refusal a retry of the same request can outlive: a hardware
/// slot, a CPU share, a queue position or a gate that frees on its own.
pub(super) fn capacity_error(message: impl AsRef<str>) -> String {
    format!("{RETRYABLE_CAPACITY_PREFIX}{}", message.as_ref())
}

/// A capacity refusal no retry of the same request can outlive. It stays in
/// the capacity class, so every server-side consumer of that class (yield,
/// placement, peer status) is unchanged; only the viewer's answer differs.
pub(super) fn terminal_capacity_error(code: &'static str, message: impl AsRef<str>) -> String {
    debug_assert!(
        TERMINAL_CAPACITY_CODES.contains(&code),
        "a terminal capacity code must be one the start contract names: {code}"
    );
    format!("{RETRYABLE_CAPACITY_PREFIX}[{code}] {}", message.as_ref())
}

/// [`terminal_capacity_error`] for the HTTP mapping's tests.
#[cfg(test)]
pub(crate) fn terminal_capacity_error_for_test(code: &'static str, message: &str) -> String {
    terminal_capacity_error(code, message)
}

pub(crate) fn replacement_deadline_error() -> String {
    terminal_capacity_error(
        REPLACEMENT_EXPIRED,
        "the replacement start expired before it could finish provisional work",
    )
}

/// The start asked for a height this worker cannot encode right now; the
/// request has to change (a smaller height), so re-posting it cannot help.
pub(crate) const TRANSCODE_HEIGHT_UNAVAILABLE: &str = "transcode_height_unavailable";
/// The start's own scratch grant is larger than the configured ceiling: no
/// amount of other sessions ending makes room for it.
pub(crate) const TRANSCODE_SCRATCH_EXHAUSTED: &str = "transcode_scratch_exhausted";
/// The replacement start spent its own deadline before provisional work
/// finished.
pub(crate) const REPLACEMENT_EXPIRED: &str = "replacement_expired";
const TERMINAL_CAPACITY_CODES: [&str; 3] = [
    TRANSCODE_HEIGHT_UNAVAILABLE,
    TRANSCODE_SCRATCH_EXHAUSTED,
    REPLACEMENT_EXPIRED,
];
/// What a waitable refusal tells a client to wait before re-posting the same
/// request. Admission keeps no estimate of when a slot frees; this is eight
/// admission polls, long enough for a just-ended session to hand its slot back.
pub(crate) const CAPACITY_RETRY_AFTER_MS: u64 = 2_000;

/// The terminal capacity code `code` names, if it names one. A worker's
/// refusal crosses to the ingress as this code alone.
pub(crate) fn terminal_capacity_code(code: &str) -> Option<&'static str> {
    TERMINAL_CAPACITY_CODES
        .iter()
        .copied()
        .find(|known| *known == code)
}

/// What a capacity refusal tells the viewer's client to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CapacityRefusal {
    /// Re-posting the same request after `retry_after_ms` can succeed.
    Waitable { retry_after_ms: u64 },
    /// No retry of the same request can succeed; the code says why.
    Terminal(&'static str),
}

/// Classify a refusal of the capacity class, with the sentence the viewer
/// should read. `None` for every other error.
pub(crate) fn capacity_refusal(error: &str) -> Option<(CapacityRefusal, &str)> {
    if is_replacement_wait_error(error) {
        return Some((
            CapacityRefusal::Waitable {
                retry_after_ms: REPLACEMENT_WAIT_RETRY_AFTER_SECS * 1_000,
            },
            error,
        ));
    }
    let rest = error.strip_prefix(RETRYABLE_CAPACITY_PREFIX)?;
    let terminal = rest
        .strip_prefix('[')
        .and_then(|tagged| tagged.split_once("] "))
        .and_then(|(code, message)| {
            terminal_capacity_code(code).map(|code| (CapacityRefusal::Terminal(code), message))
        });
    Some(terminal.unwrap_or((
        CapacityRefusal::Waitable {
            retry_after_ms: CAPACITY_RETRY_AFTER_MS,
        },
        error,
    )))
}

pub(crate) fn is_retryable_capacity_error(error: &str) -> bool {
    error.starts_with(RETRYABLE_CAPACITY_PREFIX)
}

pub(super) fn replacement_wait_error(message: impl AsRef<str>) -> String {
    format!("{REPLACEMENT_WAIT_PREFIX}{}", message.as_ref())
}

/// A subset of [`is_retryable_capacity_error`], so every existing server-side
/// consumer of the capacity class keeps seeing these unchanged.
pub(crate) fn is_replacement_wait_error(error: &str) -> bool {
    error.starts_with(REPLACEMENT_WAIT_PREFIX)
}

pub(crate) fn serving_fence_error(message: impl AsRef<str>) -> String {
    format!("{SERVING_FENCE_PREFIX}{}", message.as_ref())
}

pub(crate) fn is_serving_fence_error(error: &str) -> bool {
    error.starts_with(SERVING_FENCE_PREFIX)
}

pub(crate) fn start_infrastructure_error(message: impl AsRef<str>) -> String {
    format!("{START_INFRASTRUCTURE_PREFIX}{}", message.as_ref())
}

pub(crate) fn is_start_infrastructure_error(error: &str) -> bool {
    error.starts_with(START_INFRASTRUCTURE_PREFIX)
}

pub(crate) fn source_proof_pending_error(message: impl AsRef<str>) -> String {
    format!("{SOURCE_PROOF_PENDING_PREFIX}{}", message.as_ref())
}

/// A subset of [`is_start_infrastructure_error`], so every consumer of that
/// class keeps treating an unfinished proof as transient.
pub(crate) fn is_source_proof_pending_error(error: &str) -> bool {
    error.starts_with(SOURCE_PROOF_PENDING_PREFIX)
}

pub(super) fn unsupported_build_error(message: impl AsRef<str>) -> String {
    format!("{UNSUPPORTED_BUILD_PREFIX}{}", message.as_ref())
}

/// Strip the classification, leaving the sentence a viewer should read.
pub(crate) fn unsupported_build_reason(error: &str) -> Option<&str> {
    error.strip_prefix(UNSUPPORTED_BUILD_PREFIX)
}

pub(super) fn invalid_reopen_error(reason: &str) -> String {
    format!("{INVALID_REOPEN_PREFIX}{reason}")
}

pub(crate) fn invalid_reopen_reason(error: &str) -> Option<&str> {
    error.strip_prefix(INVALID_REOPEN_PREFIX)
}

pub(crate) fn vod_refusal_error(code: &'static str, message: impl AsRef<str>) -> String {
    format!("{VOD_REFUSAL_PREFIX}{code}: {}", message.as_ref())
}

pub(crate) fn vod_refusal(error: &str) -> Option<(&str, &str)> {
    let classified = error.strip_prefix(VOD_REFUSAL_PREFIX)?;
    let (code, message) = classified.split_once(": ")?;
    if code.is_empty() || message.is_empty() {
        return None;
    }
    Some((code, message))
}

/// Catalog evidence became unavailable before dispatch; the existing create
/// owner may retry discovery, while request identity and selection remain.
pub(crate) fn catalog_input_error(message: impl AsRef<str>) -> String {
    format!("catalog_input_unavailable: {}", message.as_ref())
}

pub(crate) fn is_catalog_input_error(error: &str) -> bool {
    error.starts_with("catalog_input_unavailable: ")
}
