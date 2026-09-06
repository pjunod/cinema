//! One envelope both requests carry, and the law for ordering two of them.
//!
//! Handoff §1 asks for "an explicit shared envelope to ordinary create **and**
//! control, including when advisory control is off". The reason that phrasing
//! is so specific is that today the two requests describe the same viewer in
//! two incompatible vocabularies, and neither of them can be *ordered*.
//!
//! Create sends flat legacy scalars — a height beside a `quality_auto` flag, a
//! `copy`/`aac` pair, a `preserve_dolby_vision`/`hdr10` pair. Control sends a
//! structured [`DesiredSelection`]. So the same ask has two spellings, and when
//! the control protocol setting is off a session has no control channel at all
//! and the create body is the *only* thing the server ever learns. An ordering
//! rule that lives on the control path is, in that configuration, an ordering
//! rule that never runs.
//!
//! # Why three revisions and not one
//!
//! The handoff rules out four counters already in the tree, and each is ruled
//! out for the same underlying reason: it moves for something that is not a
//! change of media intent.
//!
//! - A capture's `sourceRevision` advances on the reporter's sampling cadence —
//!   every playhead sample, several times a second.
//! - An attachment generation identifies which reporter produced a capture.
//! - The generic intent generation counts *seek commands* only, so it never
//!   moves for a quality, codec, audio-track, offset or subtitle change.
//! - The control sequence counts exchanges, one per heartbeat, and resets when
//!   an owner takes over.
//!
//! A single counter cannot fix this, because the three things a viewer can
//! change are genuinely independent and have genuinely different consequences.
//! Asking for a different *recipe* invalidates work in flight. Asking for a
//! different *destination* does not — the recipe is still the right one, only
//! aimed somewhere else. Asking to pause changes neither: "pause changes
//! transport without discarding a valid recipe" is a requirement, and it is
//! only expressible if transport has a counter of its own to move.
//!
//! So: three counters, each monotone within a lifetime, each moving for exactly
//! one kind of change. A scheduler can then ask the question it actually has —
//! "is the recipe I am building still the one that is wanted" — without a
//! heartbeat or a pause answering it.
//!
//! # Why a revision and not only a digest
//!
//! [`DesiredSelection::digest`] already says whether two asks are *the same*.
//! It cannot say which is *later*, and that is not a gap that can be closed by
//! hashing more carefully: a content hash is ABA-blind by construction. A
//! viewer who picks 1080p, then 720p, then 1080p again has made three asks, and
//! the first and third digest identically. Under a digest alone, a stale
//! packet carrying the first is indistinguishable from a fresh packet carrying
//! the third, so a late-arriving obsolete ask can win.
//!
//! The revision is monotone, so it separates them. The digest stays, because
//! the revision alone cannot say whether an ask that is newer is also
//! *different* — and re-doing work for an ask that changed nothing is the other
//! failure. Together they answer both questions; neither answers both alone.
//!
//! # Why a lifetime, and why envelopes from different lifetimes never compare
//!
//! The revisions are counted by the client, which means they are only
//! meaningful relative to the counter that produced them. A player that
//! restarts, or a second controller that takes the title over, starts from its
//! own zero. Comparing across those is not a stale-ask problem, it is a
//! meaningless comparison — and one that would silently resolve in favour of
//! whichever side had been running longer.
//!
//! [`MediaIntentEnvelope::supersedes`] therefore refuses across lifetimes
//! rather than guessing, and the caller has to settle the lifetime change by
//! the rule that governs lifetime changes (fencing and cleanup transfer), which
//! is a different question with a different answer.

use serde::{Deserialize, Serialize};

use super::desired::DesiredSelection;

/// The longest a lifetime identifier may be.
///
/// Long enough for a UUID with room to spare, short enough that the column it
/// is compared against has a bound a database can enforce.
pub const MAX_LIFETIME_ID: usize = 128;

/// Why an envelope cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IntentEnvelopeError {
    /// A lifetime that is empty or over [`MAX_LIFETIME_ID`] bytes.
    LifetimeId,
    /// A revision of zero.
    ///
    /// Zero is reserved for "no ask has been made", so an envelope that claims
    /// it is claiming to be an ask and not one at the same time. Rejecting it
    /// keeps `0` usable as the durable "nothing recorded" value rather than
    /// making it ambiguous with a first ask.
    Revision(&'static str),
}

impl std::fmt::Display for IntentEnvelopeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LifetimeId => write!(f, "intent lifetime id must be 1..={MAX_LIFETIME_ID} bytes"),
            Self::Revision(which) => {
                write!(f, "intent {which} revision must be greater than zero")
            }
        }
    }
}

impl std::error::Error for IntentEnvelopeError {}

/// What one ask is, and where it sits relative to the asks around it.
///
/// The same value travels on ordinary create and on control, so a session
/// created with the control protocol switched off still carries an orderable,
/// complete statement of what its viewer wants.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MediaIntentEnvelope {
    /// Stable while one controller drives one title.
    ///
    /// Not the media-session incarnation, which changes on every restart, and
    /// not the owner epoch, which changes on takeover. A recovery restart that
    /// re-creates the session keeps this, because the viewer did not ask for
    /// anything and their ask should not appear to have been withdrawn and
    /// re-made.
    pub lifetime_id: String,
    /// Advances when, and only when, [`Self::selection`] changes.
    pub recipe_revision: u64,
    /// Advances when the viewer asks for a different film position.
    ///
    /// A playhead that moves because the film is playing is not an ask and does
    /// not move this. The wire cannot distinguish two coalesced same-target
    /// seeks from one, which is exactly why this is counted by the client that
    /// issued them rather than inferred from clock movement.
    pub destination_revision: u64,
    /// Advances on play, pause and rate changes.
    ///
    /// Separate so that a pause can be ordered against other transport without
    /// being mistakable for a recipe change — pausing must not discard a valid
    /// recipe.
    pub transport_revision: u64,
    /// The complete normalized ask.
    pub selection: DesiredSelection,
}

/// How one envelope stands relative to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentOrder {
    /// Same lifetime, and this envelope's revision is strictly greater.
    Newer,
    /// Same lifetime and the same revision.
    Same,
    /// Same lifetime, and this envelope's revision is strictly smaller.
    Older,
    /// Different lifetimes. These revisions were counted by different
    /// counters and mean nothing to each other.
    Incomparable,
}

/// Which of the three counters a question is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentAxis {
    /// "Is the recipe I am building still the one that is wanted?"
    Recipe,
    /// "Is the position I am aiming at still the one that is wanted?"
    Destination,
    /// "Is this the latest thing the viewer said about playing or pausing?"
    Transport,
}

impl MediaIntentEnvelope {
    /// Reject an envelope that cannot mean anything, before it is stored or
    /// compared.
    pub fn validate(&self) -> Result<(), IntentEnvelopeError> {
        if self.lifetime_id.is_empty() || self.lifetime_id.len() > MAX_LIFETIME_ID {
            return Err(IntentEnvelopeError::LifetimeId);
        }
        if self.recipe_revision == 0 {
            return Err(IntentEnvelopeError::Revision("recipe"));
        }
        if self.destination_revision == 0 {
            return Err(IntentEnvelopeError::Revision("destination"));
        }
        if self.transport_revision == 0 {
            return Err(IntentEnvelopeError::Revision("transport"));
        }
        Ok(())
    }

    /// The digest of the ask this envelope carries.
    pub fn digest(&self) -> String {
        self.selection.digest()
    }

    /// The revision this envelope carries on one axis.
    pub fn revision(&self, axis: IntentAxis) -> u64 {
        match axis {
            IntentAxis::Recipe => self.recipe_revision,
            IntentAxis::Destination => self.destination_revision,
            IntentAxis::Transport => self.transport_revision,
        }
    }

    /// Where this envelope sits relative to `other` on one axis.
    ///
    /// The axis is a parameter rather than three methods because the caller's
    /// question is always about one axis, and making them name it stops the
    /// mistake this type exists to prevent: answering "is the recipe stale"
    /// with a counter that a heartbeat moved.
    pub fn order(&self, other: &Self, axis: IntentAxis) -> IntentOrder {
        if self.lifetime_id != other.lifetime_id {
            return IntentOrder::Incomparable;
        }
        match self.revision(axis).cmp(&other.revision(axis)) {
            std::cmp::Ordering::Greater => IntentOrder::Newer,
            std::cmp::Ordering::Equal => IntentOrder::Same,
            std::cmp::Ordering::Less => IntentOrder::Older,
        }
    }

    /// Whether this envelope replaces `other` on one axis.
    ///
    /// Strictly newer, and only within a lifetime. Equal revisions do not
    /// supersede: a repeated packet is the same ask arriving twice, and
    /// treating it as a new one is how a retry becomes a second decision.
    pub fn supersedes(&self, other: &Self, axis: IntentAxis) -> bool {
        matches!(self.order(other, axis), IntentOrder::Newer)
    }

    /// Whether this envelope asks for something the `other` did not.
    ///
    /// Deliberately not "is it newer": a revision that advanced while the
    /// selection stayed identical is a viewer who changed their mind back, or a
    /// client counting more eagerly than it needs to. Either way there is no
    /// new work to do, and doing it anyway restarts a session for nothing.
    ///
    /// Across lifetimes this compares the asks themselves, because that is the
    /// only thing the two have in common — the revisions are not comparable but
    /// the selections are.
    pub fn asks_for_something_new(&self, other: &Self) -> bool {
        self.selection != other.selection
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::desired::{
        DesiredCodec, DesiredDynamicRange, DesiredQuality, DesiredSubtitles,
    };

    fn selection(quality: DesiredQuality) -> DesiredSelection {
        DesiredSelection {
            quality,
            codec: DesiredCodec::Auto,
            dynamic_range: DesiredDynamicRange::Auto,
            audio_track: None,
            audio_offset_ms: 0,
            subtitles: DesiredSubtitles::Off,
        }
    }

    fn envelope(recipe: u64, quality: DesiredQuality) -> MediaIntentEnvelope {
        MediaIntentEnvelope {
            lifetime_id: "lifetime-a".to_owned(),
            recipe_revision: recipe,
            destination_revision: 1,
            transport_revision: 1,
            selection: selection(quality),
        }
    }

    /// The property a digest cannot have, stated directly.
    ///
    /// A viewer who returns to an earlier selection has made a third ask. Its
    /// digest is the first ask's digest — that is what a content hash is for
    /// and there is nothing wrong with it — so the digest cannot be what
    /// decides which of the two is current. The revision can, and this asserts
    /// both halves at once so neither can be removed without the other being
    /// noticed.
    #[test]
    fn returning_to_an_earlier_selection_is_a_later_ask_with_the_same_digest() {
        let first = envelope(1, DesiredQuality::Manual { height: 1080 });
        let second = envelope(2, DesiredQuality::Manual { height: 720 });
        let third = envelope(3, DesiredQuality::Manual { height: 1080 });

        assert_eq!(
            first.digest(),
            third.digest(),
            "the same ask digests the same however many times it is made"
        );
        assert!(
            third.supersedes(&first, IntentAxis::Recipe),
            "and the revision is what puts the third ask after the first"
        );
        assert!(
            !first.supersedes(&third, IntentAxis::Recipe),
            "a stale packet carrying the first ask must not win over the third"
        );
        assert!(
            !third.asks_for_something_new(&first),
            "the third ask is later, and it is not new work"
        );
        assert!(
            second.asks_for_something_new(&first),
            "the second ask is both later and different"
        );
    }

    /// A heartbeat and a pause must not answer a question about the recipe.
    ///
    /// This is the whole reason there are three counters rather than one. The
    /// envelope below moves both of the other axes as far as they can go while
    /// leaving the recipe alone; if any of that leaked into the recipe axis,
    /// a scheduler would tear down work the viewer never asked it to.
    #[test]
    fn transport_and_destination_movement_leave_the_recipe_axis_alone() {
        let building = envelope(4, DesiredQuality::Auto);
        let mut moved = building.clone();
        moved.destination_revision = 900;
        moved.transport_revision = 900;

        assert!(
            !moved.supersedes(&building, IntentAxis::Recipe),
            "seeking and pausing do not make the recipe under construction stale"
        );
        assert_eq!(
            moved.order(&building, IntentAxis::Recipe),
            IntentOrder::Same,
            "the recipe axis reports what it is asked about and nothing else"
        );
        assert!(
            moved.supersedes(&building, IntentAxis::Destination),
            "the destination did move, and the destination axis says so"
        );
        assert!(
            moved.supersedes(&building, IntentAxis::Transport),
            "so did transport"
        );
    }

    /// Revisions from two different counters are not evidence about each other.
    #[test]
    fn envelopes_from_different_lifetimes_never_supersede_each_other() {
        let mine = envelope(2, DesiredQuality::Auto);
        let mut theirs = envelope(900, DesiredQuality::Original);
        theirs.lifetime_id = "lifetime-b".to_owned();

        for axis in [
            IntentAxis::Recipe,
            IntentAxis::Destination,
            IntentAxis::Transport,
        ] {
            assert_eq!(
                theirs.order(&mine, axis),
                IntentOrder::Incomparable,
                "a larger number from a different counter is not a later ask"
            );
            assert!(!theirs.supersedes(&mine, axis));
            assert!(!mine.supersedes(&theirs, axis));
        }
        assert!(
            theirs.asks_for_something_new(&mine),
            "the selections are still comparable, and these two differ"
        );
    }

    /// A retry is one ask arriving twice, not two asks.
    #[test]
    fn an_identical_repeat_does_not_supersede() {
        let ask = envelope(7, DesiredQuality::Auto);
        let repeat = ask.clone();
        assert_eq!(repeat.order(&ask, IntentAxis::Recipe), IntentOrder::Same);
        assert!(!repeat.supersedes(&ask, IntentAxis::Recipe));
        assert!(!repeat.asks_for_something_new(&ask));
    }

    /// Zero is the durable "nothing recorded" value, so no envelope may claim
    /// it — otherwise a first ask and no ask at all become the same row.
    #[test]
    fn a_zero_revision_on_any_axis_is_refused() {
        let good = envelope(1, DesiredQuality::Auto);
        good.validate().expect("a complete envelope is valid");

        for (axis, mutate) in [
            (
                "recipe",
                Box::new(|e: &mut MediaIntentEnvelope| e.recipe_revision = 0)
                    as Box<dyn Fn(&mut MediaIntentEnvelope)>,
            ),
            (
                "destination",
                Box::new(|e: &mut MediaIntentEnvelope| e.destination_revision = 0),
            ),
            (
                "transport",
                Box::new(|e: &mut MediaIntentEnvelope| e.transport_revision = 0),
            ),
        ] {
            let mut broken = good.clone();
            mutate(&mut broken);
            assert_eq!(
                broken.validate(),
                Err(IntentEnvelopeError::Revision(axis)),
                "a zero {axis} revision must be refused, and named"
            );
        }
    }

    /// A lifetime that cannot identify anything, or that no column can hold.
    #[test]
    fn a_lifetime_id_outside_its_bounds_is_refused() {
        let mut empty = envelope(1, DesiredQuality::Auto);
        empty.lifetime_id = String::new();
        assert_eq!(empty.validate(), Err(IntentEnvelopeError::LifetimeId));

        let mut longest = envelope(1, DesiredQuality::Auto);
        longest.lifetime_id = "x".repeat(MAX_LIFETIME_ID);
        longest
            .validate()
            .expect("the bound itself is a legal length");

        let mut over = envelope(1, DesiredQuality::Auto);
        over.lifetime_id = "x".repeat(MAX_LIFETIME_ID + 1);
        assert_eq!(over.validate(), Err(IntentEnvelopeError::LifetimeId));
    }

    /// The wire shape is part of the contract: two binaries have to agree on
    /// it, and `deny_unknown_fields` means a field added on one side and not
    /// the other is a refusal rather than a silent default.
    #[test]
    fn the_envelope_round_trips_through_its_wire_form() {
        let ask = envelope(3, DesiredQuality::Original);
        let json = serde_json::to_string(&ask).expect("serializing");
        let back: MediaIntentEnvelope = serde_json::from_str(&json).expect("deserializing");
        assert_eq!(back, ask);

        let with_extra = json.replace('{', "{\"unexpected\":1,");
        serde_json::from_str::<MediaIntentEnvelope>(&with_extra)
            .expect_err("an unknown field is refused rather than dropped");
    }
}
