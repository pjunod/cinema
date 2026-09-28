use super::*;

pub(super) fn capacity_error(message: impl AsRef<str>) -> String {
    format!("{RETRYABLE_CAPACITY_PREFIX}{}", message.as_ref())
}

pub(super) fn replacement_deadline_error() -> String {
    capacity_error("the replacement start expired before it could finish provisional work")
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
