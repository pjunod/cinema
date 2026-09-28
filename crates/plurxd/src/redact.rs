//! Redaction for text this process did not write.
//!
//! Operator-supplied cluster operation text and third-party panic payloads are
//! the same hazard wearing two hats: a string whose content nobody here chose,
//! on its way to a log ring an admin can read and to journald. One rule set
//! serves both, so a second caller cannot quietly grow a weaker one.

/// The limit the cluster-operations surface has always applied.
pub(crate) const OPERATOR_TEXT_LIMIT: usize = 512;

/// Replaces `value` wholesale if it looks like it carries a credential or a
/// filesystem path, and otherwise truncates it to `limit` characters.
///
/// The test is deliberately coarse and fails towards redaction. A path is
/// treated as sensitive because a path names a title: `/media/Movies/Night
/// Tide (2024)/Night Tide.mkv` in a log line is a content identifier, which is
/// the thing the review's §4.9 forbids in the metrics and which has no more
/// business in a log an admin screenshots.
pub(crate) fn redact_bounded(value: &str, limit: usize) -> String {
    let lower = value.to_ascii_lowercase();
    let sensitive = [
        "authorization",
        "bearer ",
        "token",
        "secret",
        "password",
        "signature",
        "api_key",
        "apikey",
        "username",
        "plxjoin:",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
        || value.contains('/')
        || value.contains('\\');
    if sensitive {
        return "[redacted potentially sensitive operator text]".to_owned();
    }
    value.chars().take(limit).collect()
}

/// The cluster-operations spelling: the same rule at the limit that surface has
/// always used.
pub(crate) fn redact_operator_text(value: &str) -> String {
    redact_bounded(value, OPERATOR_TEXT_LIMIT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_shaped_text_is_replaced_whole() {
        for value in [
            "Authorization: Bearer abc",
            "bearer abc",
            "my token is abc",
            "SECRET",
            "password=hunter2",
            "signature=deadbeef",
            "api_key=1",
            "apikey=1",
            "username=paul",
            "plxjoin:abcdef",
            "/mnt/media/Night Tide.mkv",
            "C:\\media\\Night Tide.mkv",
        ] {
            assert_eq!(
                redact_operator_text(value),
                "[redacted potentially sensitive operator text]",
                "{value}"
            );
        }
    }

    #[test]
    fn ordinary_text_is_kept_and_truncated_at_the_limit() {
        assert_eq!(redact_operator_text("drain node b"), "drain node b");
        let long = "a".repeat(1_000);
        assert_eq!(redact_operator_text(&long).len(), OPERATOR_TEXT_LIMIT);
        assert_eq!(redact_bounded(&long, 8).len(), 8);
    }

    #[test]
    fn the_limit_counts_characters_not_bytes() {
        // `chars().take(limit)` never splits a multi-byte character, which is
        // why this function cannot produce invalid UTF-8 from a truncation.
        let text = "é".repeat(10);
        assert_eq!(redact_bounded(&text, 4), "éééé");
    }
}
