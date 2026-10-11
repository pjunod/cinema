//! Strict Cinema remote wire types and receiver-local admission.
//!
//! Call `apply` synchronously on the UI owner after semantic authorization,
//! with a synchronous effect callback. This module grants no user authority
//! and owns neither network leases nor player state. Times are receiver-local
//! monotonic elapsed milliseconds; never send them over the wire.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use uuid::Uuid;

pub const PROTOCOL_VERSION: &str = "cinema.remote.v1";
pub const MAX_BODY_BYTES: usize = 16 * 1024;
pub const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
pub const MAX_OWNER_BYTES: usize = 128;
pub const MAX_OPTION_BYTES: usize = 128;
pub const MAX_TEXT_BYTES: usize = 512;
pub const MAX_CREDITS: usize = 16;
pub const MAX_RESULTS: usize = 64;
pub const RESULT_TTL_MS: u64 = 10_000;
pub const INTERACTION_TTL_MS: u64 = 1_000;
pub const PLAYBACK_TTL_MS: u64 = 3_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Version {
    #[serde(rename = "cinema.remote.v1")]
    V1,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub owner_node_id: String,
    pub session_id: Uuid,
    pub receiver_epoch: Uuid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackKind {
    Audio,
    Subtitles,
    Quality,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Navigate { direction: Direction },
    Select {},
    Back {},
    Home {},
    SetPlaying { playing: bool },
    SeekRelative { seconds: i32 },
    SeekAbsolute { position_ms: u64 },
    Stop {},
    OpenTracks { kind: TrackKind },
    ChooseTrack { kind: TrackKind, option_id: String },
    TextReplace { text_nonce: Uuid, text: String },
    PlayItem { item_id: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CreditKind {
    Interaction,
    Playback,
}

impl CreditKind {
    pub const fn ttl_ms(self) -> u64 {
        match self {
            Self::Interaction => INTERACTION_TTL_MS,
            Self::Playback => PLAYBACK_TTL_MS,
        }
    }
}

impl Action {
    pub const fn credit_kind(&self) -> CreditKind {
        match self {
            Self::Navigate { .. }
            | Self::Select {}
            | Self::Back {}
            | Self::Home {}
            | Self::TextReplace { .. }
            | Self::OpenTracks { .. } => CreditKind::Interaction,
            _ => CreditKind::Playback,
        }
    }

    pub fn validate(&self) -> Result<(), Outcome> {
        let valid = match self {
            Self::SeekRelative { seconds } => matches!(seconds, -30 | -10 | 10 | 30),
            Self::SeekAbsolute { position_ms } => *position_ms <= MAX_SAFE_INTEGER,
            Self::PlayItem { item_id } => positive(*item_id),
            Self::ChooseTrack { option_id, .. } => {
                !option_id.is_empty() && option_id.len() <= MAX_OPTION_BYTES
            }
            Self::TextReplace { text, .. } => text.len() <= MAX_TEXT_BYTES,
            _ => true,
        };
        if valid {
            Ok(())
        } else {
            Err(Outcome::Invalid)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub version: Version,
    pub target: Target,
    pub grant_id: Uuid,
    pub control_epoch: Uuid,
    pub sequence: u64,
    pub credit: Uuid,
    pub context_revision: u64,
    pub focus_revision: u64,
    pub action: Action,
}

fn positive(value: u64) -> bool {
    value > 0 && value <= MAX_SAFE_INTEGER
}

impl Command {
    pub fn validate(&self) -> Result<(), Outcome> {
        if self.target.owner_node_id.is_empty()
            || self.target.owner_node_id.len() > MAX_OWNER_BYTES
            || self.target.owner_node_id.chars().any(char::is_control)
            || !positive(self.sequence)
            || !positive(self.context_revision)
            || !positive(self.focus_revision)
        {
            return Err(Outcome::Invalid);
        }
        self.action.validate()
    }

    /// Enforces the transport byte cap before allocating parsed strings.
    pub fn decode(bytes: &[u8]) -> Result<Self, Outcome> {
        if bytes.len() > MAX_BODY_BYTES {
            return Err(Outcome::Invalid);
        }
        let command: Self = serde_json::from_slice(bytes).map_err(|_| Outcome::Invalid)?;
        command.validate()?;
        Ok(command)
    }

    pub fn encode(&self) -> Result<Vec<u8>, Outcome> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| Outcome::Invalid)?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(Outcome::Invalid);
        }
        Ok(bytes)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Applied,
    DuplicateOrOld,
    Expired,
    StaleTarget,
    StaleControl,
    StaleContext,
    StaleFocus,
    RestrictedSurface,
    Unauthorized,
    Unsupported,
    Busy,
    Unavailable,
    Invalid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credit {
    pub nonce: Uuid,
    pub kind: CreditKind,
}

/// Caller-owned current UI fences. Changing target/control/context clears
/// credits; a physical input must additionally call `invalidate` before acting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiverContext {
    pub grant_id: Uuid,
    pub target: Target,
    pub control_epoch: Uuid,
    pub context_revision: u64,
    pub focus_revision: u64,
    pub text_nonce: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acknowledgement {
    pub control_epoch: Uuid,
    pub sequence: u64,
    pub outcome: Outcome,
}

struct LocalCredit {
    wire: Credit,
    deadline_ms: u64,
}
struct RecentResult {
    ack: Acknowledgement,
    expires_ms: u64,
}

/// No interior mutability: `&mut self` serializes admission before the effect.
#[derive(Default)]
pub struct ReceiverGuard {
    context: Option<ReceiverContext>,
    active: bool,
    credits: VecDeque<LocalCredit>,
    results: VecDeque<RecentResult>,
    last_sequence: u64,
    last_now_ms: Option<u64>,
}

impl ReceiverGuard {
    /// Clear credits before physical input or a UI context change.
    /// Logout, background, lease loss and revocation must use `deactivate`.
    /// Keep the replay high-water mark: invalidation must not resurrect Select.
    pub fn invalidate(&mut self) {
        self.credits.clear();
    }

    /// Suspend network admission. Reactivation requires a new target/control
    /// epoch; retaining replay identity prevents resurrection after recovery.
    pub fn deactivate(&mut self) {
        self.active = false;
        self.invalidate();
        self.results.clear();
    }

    pub fn set_context(&mut self, context: ReceiverContext) -> Result<(), Outcome> {
        if !positive(context.context_revision)
            || !positive(context.focus_revision)
            || context.target.owner_node_id.is_empty()
            || context.target.owner_node_id.len() > MAX_OWNER_BYTES
        {
            self.deactivate();
            return Err(Outcome::Invalid);
        }
        let changed_epoch = self.context.as_ref().is_none_or(|old| {
            old.target != context.target || old.control_epoch != context.control_epoch
        });
        if !changed_epoch
            && (!self.active
                || self
                    .context
                    .as_ref()
                    .is_some_and(|old| old.grant_id != context.grant_id))
        {
            self.deactivate();
            return Err(Outcome::Invalid);
        }
        if changed_epoch {
            self.last_sequence = 0;
            self.results.clear();
        }
        if self.context.as_ref().is_none_or(|old| {
            changed_epoch
                || old.context_revision != context.context_revision
                || old.text_nonce != context.text_nonce
        }) {
            self.invalidate();
        }
        self.context = Some(context);
        self.active = true;
        Ok(())
    }

    fn observe_clock(&mut self, now_ms: u64) -> Result<(), Outcome> {
        if self.last_now_ms.is_some_and(|old| now_ms < old) {
            self.invalidate();
            return Err(Outcome::Invalid);
        }
        self.last_now_ms = Some(now_ms);
        self.credits.retain(|entry| now_ms < entry.deadline_ms);
        self.results.retain(|entry| now_ms < entry.expires_ms);
        Ok(())
    }

    /// Mint a fresh receiver-owned nonce; callers cannot reuse an expired
    /// externally supplied nonce to revive a delayed command.
    pub fn mint_credit(&mut self, kind: CreditKind, now_ms: u64) -> Result<Credit, Outcome> {
        let credit = Credit {
            nonce: Uuid::new_v4(),
            kind,
        };
        self.insert_credit(credit, now_ms)?;
        Ok(credit)
    }

    // Deterministic nonce injection is private and used only by fixture tests.
    fn insert_credit(&mut self, credit: Credit, now_ms: u64) -> Result<(), Outcome> {
        self.observe_clock(now_ms)?;
        if !self.active {
            return Err(Outcome::Unavailable);
        }
        if self
            .credits
            .iter()
            .any(|entry| entry.wire.nonce == credit.nonce)
        {
            return Err(Outcome::Invalid);
        }
        let deadline_ms = now_ms
            .checked_add(credit.kind.ttl_ms())
            .ok_or(Outcome::Invalid)?;
        if self.credits.len() == MAX_CREDITS {
            self.credits.pop_front();
        }
        self.credits.push_back(LocalCredit {
            wire: credit,
            deadline_ms,
        });
        Ok(())
    }

    /// Call synchronously on the UI owner. `semantic` is the current grant,
    /// capability and UI-parameter decision, computed without yielding.
    /// Sequence is consumed before `effect` executes; its actual returned
    /// outcome is recorded afterwards. No acknowledgement is observable
    /// before the effect returns. This is not asynchronous media completion.
    pub fn apply(
        &mut self,
        command: &Command,
        now_ms: u64,
        semantic: Result<(), Outcome>,
        effect: impl FnOnce() -> Outcome,
    ) -> Outcome {
        if let Err(reason) = self.admit_inner(command, now_ms, semantic) {
            return reason;
        }
        let outcome = effect();
        let expires_ms = now_ms + RESULT_TTL_MS; // checked before sequence consumption
        if self.results.len() == MAX_RESULTS {
            self.results.pop_front();
        }
        self.results.push_back(RecentResult {
            ack: Acknowledgement {
                control_epoch: command.control_epoch,
                sequence: command.sequence,
                outcome,
            },
            expires_ms,
        });
        outcome
    }

    fn admit_inner(
        &mut self,
        command: &Command,
        now_ms: u64,
        semantic: Result<(), Outcome>,
    ) -> Result<(), Outcome> {
        self.observe_clock(now_ms)?;
        command.validate()?;
        if !self.active {
            return Err(Outcome::Unavailable);
        }
        let current = self.context.as_ref().ok_or(Outcome::Unavailable)?;
        if command.target != current.target {
            return Err(Outcome::StaleTarget);
        }
        if command.grant_id != current.grant_id {
            return Err(Outcome::Unauthorized);
        }
        if command.control_epoch != current.control_epoch {
            return Err(Outcome::StaleControl);
        }
        if command.sequence <= self.last_sequence {
            return Err(Outcome::DuplicateOrOld);
        }
        let credit = self
            .credits
            .iter()
            .find(|entry| entry.wire.nonce == command.credit)
            .ok_or(Outcome::Expired)?;
        if credit.wire.kind != command.action.credit_kind() {
            return Err(Outcome::Invalid);
        }
        if command.context_revision != current.context_revision {
            return Err(Outcome::StaleContext);
        }
        if matches!(command.action, Action::Select {})
            && command.focus_revision != current.focus_revision
        {
            return Err(Outcome::StaleFocus);
        }
        if let Action::TextReplace { text_nonce, .. } = command.action {
            if Some(text_nonce) != current.text_nonce {
                return Err(Outcome::StaleContext);
            }
        }
        // Error(Applied) is not permission and must never consume a sequence.
        if let Err(reason) = semantic {
            return Err(if reason == Outcome::Applied {
                Outcome::Invalid
            } else {
                reason
            });
        }
        now_ms.checked_add(RESULT_TTL_MS).ok_or(Outcome::Invalid)?;
        self.last_sequence = command.sequence;
        Ok(())
    }

    /// Lookup does not admit or apply a command; old entries remain replay
    /// rejected by the high-water mark even after their acknowledgements expire.
    pub fn result(
        &mut self,
        control_epoch: Uuid,
        sequence: u64,
        now_ms: u64,
    ) -> Option<Acknowledgement> {
        self.observe_clock(now_ms).ok()?;
        self.results
            .iter()
            .find(|entry| {
                entry.ack.control_epoch == control_epoch && entry.ack.sequence == sequence
            })
            .map(|entry| entry.ack)
    }
}

#[cfg(test)]
#[path = "remote_control/tests.rs"]
mod tests;
