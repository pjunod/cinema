//! B-only ordered history from an exact currently published receiver owner.
use crate::sharing_receiver_sessions::ReceiverSourceAttachment;

/// Server-only actor input. Identity and original login come from the retained
/// attachment and opaque current authority; updated_at is the Store clock.
#[derive(Clone)]
pub struct ReceiverProgress {
    pub attachment: ReceiverSourceAttachment,
    pub sequence: i64,
    pub position_ms: i64,
    pub duration_ms: Option<i64>,
    pub watched: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverProgressOutcome {
    Applied,
    Replay,
    Stale,
    Conflict,
    Refused,
}
