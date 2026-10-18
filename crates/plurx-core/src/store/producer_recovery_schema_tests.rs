use super::MEDIA_SESSION_PRODUCER_RECOVERY_SCHEMA;
use crate::domain::MAX_DECODE_RESTRICTION_BYTES;

/// The column's `CHECK` and the encoder's bound are one number.
///
/// They are written in two places — a SQL literal and a Rust constant —
/// and nothing but this stops them drifting. If they drift upward on the
/// Rust side, every restriction between the two values encodes cleanly,
/// is silently rejected by the constraint, and reports back to the caller
/// as "the budget is already spent": a playback permanently loses its one
/// automatic recovery with no error recorded anywhere.
#[test]
fn the_stored_restriction_cap_is_the_constant_the_encoder_enforces() {
    assert!(
        MEDIA_SESSION_PRODUCER_RECOVERY_SCHEMA
            .contains(&format!("<= {MAX_DECODE_RESTRICTION_BYTES}")),
        "the ledger's CHECK must name MAX_DECODE_RESTRICTION_BYTES exactly"
    );
}
