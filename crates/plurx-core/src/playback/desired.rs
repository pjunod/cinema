//! What a viewer has asked for, as a policy rather than as a result.
//!
//! Handoff §1 asks for "a normalized complete selection" that preserves Auto,
//! Original and Manual as *different requested policies*. Nothing durable in
//! this system holds one today. What exists instead is a set of near misses,
//! each correct for its own job and wrong for this one:
//!
//! - the resolved output height, which cannot tell Original from a manual pick
//!   at the source's own height, and cannot tell either from what Auto happened
//!   to choose this minute;
//! - `SessionRequest`, which reduces the codec and dynamic-range policies to a
//!   `SessionKind` plus one `hdr10` flag, and the quality policy to a single
//!   `automatic: bool` with no room for Original at all;
//! - the durable intent fingerprint, which is deliberately *lossy* — it drops
//!   the Auto height so a retry recovers the first answer — and therefore
//!   cannot be used to decide whether a viewer has asked for something new.
//!
//! This type is the missing one. Two properties make it usable where those are
//! not.
//!
//! **It is total.** Every axis is answered explicitly, and the answers that are
//! illegal together cannot be built: a subtitle track lives inside the mode
//! that uses one, so "off, with a track" and "burn, with no track" — both of
//! which the wire type has to reject at runtime — are simply not values here.
//! That is what makes a digest over it meaningful: there is no unanswered axis
//! whose default could differ between two callers that agree on everything they
//! did say.
//!
//! **It is a policy, not a result.** `Auto` says the server chooses;
//! `Original` says never re-encode the video, which is a different promise from
//! any particular height and survives a source whose dimensions were never
//! probed; `Manual` names a rung. The delivered height is a fact about what
//! happened and belongs nowhere near this type.
//!
//! Nothing here decides anything. It is the vocabulary the durable
//! desired-ownership work needs in order to say what two viewers' asks are and
//! whether they differ.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The video quality policy a viewer asked for.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesiredQuality {
    /// The server picks the rung, and may change it.
    Auto,
    /// Preserve the source representation and never grant the server automatic
    /// rung authority.
    ///
    /// Distinct from `Manual` at the source's height, and the distinction is
    /// load-bearing rather than pedantic: a source whose dimensions were never
    /// probed still has an unambiguous Original, and a viewer who picked
    /// Original has refused re-encoding rather than chosen a number that
    /// happens to match.
    Original,
    /// A rung the viewer named.
    Manual { height: i64 },
}

/// The video codec policy a viewer asked for.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesiredCodec {
    Auto,
    H264,
    Hevc,
    Av1,
}

/// The dynamic-range policy a viewer asked for.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesiredDynamicRange {
    Auto,
    DolbyVision,
    Hdr10,
    Hlg,
    Sdr,
}

/// The subtitle policy a viewer asked for, with the track inside the mode that
/// uses one.
///
/// The wire type carries a mode and an `Option<track>` side by side and rejects
/// the two impossible pairings at validation time. Here they cannot be built,
/// which is the difference between a type that is checked and one that is
/// total — and totality is what a digest over this needs.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum DesiredSubtitles {
    Off,
    /// Rendered by the client from a text track the server serves.
    Native {
        track: i64,
    },
    /// Rendered by the client over the video.
    Overlay {
        track: i64,
    },
    /// Burned into the video by the server, which makes it a recipe input.
    Burn {
        track: i64,
    },
}

/// Everything a viewer has asked for about how a title is presented.
///
/// Deliberately free of transport: a playhead, a pause, a buffer sample and a
/// heartbeat all leave this unchanged, which is the property that lets "the
/// desired selection changed" mean something a scheduler can act on.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DesiredSelection {
    pub quality: DesiredQuality,
    pub codec: DesiredCodec,
    pub dynamic_range: DesiredDynamicRange,
    pub audio_track: Option<i64>,
    pub audio_offset_ms: i64,
    pub subtitles: DesiredSubtitles,
}

impl DesiredSelection {
    /// The canonical text this selection digests to.
    ///
    /// Written by hand rather than derived from the serializer, for two
    /// reasons. A digest that two binaries must agree on cannot depend on
    /// field ordering, tag naming or number formatting decided somewhere else
    /// — those are all reasonable things for a serializer to change, and any
    /// one of them silently turns every stored digest into a mismatch. And a
    /// reviewer can read this function and say what is and is not in the
    /// digest, which is not true of a derive.
    ///
    /// Every axis appears exactly once, in a fixed order, under a fixed key,
    /// and every variant has its own spelling — so `auto`, `original` and
    /// `manual:1080` are three different digests even where all three would
    /// deliver the same height today.
    pub fn canonical_form(&self) -> String {
        let quality = match self.quality {
            DesiredQuality::Auto => "auto".to_owned(),
            DesiredQuality::Original => "original".to_owned(),
            DesiredQuality::Manual { height } => format!("manual:{height}"),
        };
        let codec = match self.codec {
            DesiredCodec::Auto => "auto",
            DesiredCodec::H264 => "h264",
            DesiredCodec::Hevc => "hevc",
            DesiredCodec::Av1 => "av1",
        };
        let dynamic_range = match self.dynamic_range {
            DesiredDynamicRange::Auto => "auto",
            DesiredDynamicRange::DolbyVision => "dolby_vision",
            DesiredDynamicRange::Hdr10 => "hdr10",
            DesiredDynamicRange::Hlg => "hlg",
            DesiredDynamicRange::Sdr => "sdr",
        };
        let audio = match self.audio_track {
            Some(track) => format!("track:{track}"),
            None => "default".to_owned(),
        };
        let subtitles = match self.subtitles {
            DesiredSubtitles::Off => "off".to_owned(),
            DesiredSubtitles::Native { track } => format!("native:{track}"),
            DesiredSubtitles::Overlay { track } => format!("overlay:{track}"),
            DesiredSubtitles::Burn { track } => format!("burn:{track}"),
        };
        format!(
            "v1;quality={quality};codec={codec};dynamic_range={dynamic_range};\
             audio={audio};audio_offset_ms={};subtitles={subtitles}",
            self.audio_offset_ms
        )
    }

    /// A stable identity for this selection.
    ///
    /// Equal selections digest equally and different ones do not, which is the
    /// whole contract — this is compared, never parsed back.
    pub fn digest(&self) -> String {
        hex::encode(Sha256::digest(self.canonical_form().as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn baseline() -> DesiredSelection {
        DesiredSelection {
            quality: DesiredQuality::Auto,
            codec: DesiredCodec::Auto,
            dynamic_range: DesiredDynamicRange::Auto,
            audio_track: None,
            audio_offset_ms: 0,
            subtitles: DesiredSubtitles::Off,
        }
    }

    /// The three quality policies are three different asks, even where they
    /// would resolve to the same height.
    ///
    /// This is the distinction the rest of the system currently loses, and the
    /// reason it is worth a type: a viewer on a 1080p source who picks Original
    /// and a viewer who picks 1080p from the menu have asked for different
    /// things — one has refused re-encoding, the other has named a rung — and
    /// Auto is a third thing again, because it grants the server authority to
    /// change its mind later.
    #[test]
    fn auto_original_and_a_matching_manual_height_are_three_different_asks() {
        let source_height = 1080;
        let auto = DesiredSelection {
            quality: DesiredQuality::Auto,
            ..baseline()
        };
        let original = DesiredSelection {
            quality: DesiredQuality::Original,
            ..baseline()
        };
        let manual = DesiredSelection {
            quality: DesiredQuality::Manual {
                height: source_height,
            },
            ..baseline()
        };

        let digests = [auto.digest(), original.digest(), manual.digest()];
        assert_ne!(digests[0], digests[1], "Auto is not Original");
        assert_ne!(
            digests[1], digests[2],
            "Original is not a manual pick at the source's own height"
        );
        assert_ne!(digests[0], digests[2], "Auto is not a manual pick");
    }

    /// Every axis reaches the digest, and changing any one of them changes it.
    ///
    /// An axis that fell out of `canonical_form` would be invisible: two
    /// selections differing only in it would digest identically, and a viewer
    /// changing only that axis would be told nothing had changed. The list is
    /// exhaustive against the struct by construction — each entry mutates one
    /// field, and a field with no entry here is a field this test does not
    /// cover.
    #[test]
    fn every_axis_of_a_selection_reaches_its_digest() {
        let base = baseline();
        let variants = [
            (
                "quality",
                DesiredSelection {
                    quality: DesiredQuality::Manual { height: 720 },
                    ..base
                },
            ),
            (
                "codec",
                DesiredSelection {
                    codec: DesiredCodec::Hevc,
                    ..base
                },
            ),
            (
                "dynamic_range",
                DesiredSelection {
                    dynamic_range: DesiredDynamicRange::Hdr10,
                    ..base
                },
            ),
            (
                "audio_track",
                DesiredSelection {
                    audio_track: Some(0),
                    ..base
                },
            ),
            (
                "audio_offset_ms",
                DesiredSelection {
                    audio_offset_ms: -250,
                    ..base
                },
            ),
            (
                "subtitles",
                DesiredSelection {
                    subtitles: DesiredSubtitles::Burn { track: 0 },
                    ..base
                },
            ),
        ];

        let baseline_digest = base.digest();
        let mut seen = std::collections::BTreeSet::new();
        seen.insert(baseline_digest.clone());
        for (axis, variant) in variants {
            assert_ne!(
                variant.digest(),
                baseline_digest,
                "{axis} does not reach the digest"
            );
            assert!(
                seen.insert(variant.digest()),
                "{axis} collides with another axis's change"
            );
        }

        // `audio_track: Some(0)` and `None` are different asks — the default
        // track is what the server picks, track zero is what the viewer picked
        // — and a digest that formatted `None` as an empty string would let
        // them collide with each other rather than only with the baseline.
        let default_audio = DesiredSelection {
            audio_track: None,
            ..base
        };
        let first_track = DesiredSelection {
            audio_track: Some(0),
            ..base
        };
        assert_ne!(default_audio.digest(), first_track.digest());
    }

    /// A subtitle track belongs to the mode that uses one, and the digest keeps
    /// them apart.
    ///
    /// The wire type carries mode and track side by side, so the same track
    /// number under two modes is two asks that a naive encoding would blur.
    #[test]
    fn a_subtitle_track_is_read_together_with_the_mode_that_uses_it() {
        let track = 3;
        let modes = [
            DesiredSubtitles::Off,
            DesiredSubtitles::Native { track },
            DesiredSubtitles::Overlay { track },
            DesiredSubtitles::Burn { track },
        ];
        let digests = modes
            .iter()
            .map(|subtitles| {
                DesiredSelection {
                    subtitles: *subtitles,
                    ..baseline()
                }
                .digest()
            })
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(digests.len(), modes.len(), "one digest per subtitle policy");
    }

    /// The canonical form is written out, not derived, so the test writes it
    /// out too.
    ///
    /// A digest two binaries must agree on cannot be allowed to move quietly.
    /// Asserting the exact string is what turns a serializer change, a field
    /// reorder or a formatting tweak into a failure here rather than into
    /// every stored digest silently ceasing to match.
    #[test]
    fn the_canonical_form_is_pinned_exactly() {
        let selection = DesiredSelection {
            quality: DesiredQuality::Manual { height: 1080 },
            codec: DesiredCodec::Hevc,
            dynamic_range: DesiredDynamicRange::Hdr10,
            audio_track: Some(2),
            audio_offset_ms: -250,
            subtitles: DesiredSubtitles::Burn { track: 1 },
        };
        assert_eq!(
            selection.canonical_form(),
            "v1;quality=manual:1080;codec=hevc;dynamic_range=hdr10;\
             audio=track:2;audio_offset_ms=-250;subtitles=burn:1"
        );
        assert_eq!(
            DesiredSelection {
                quality: DesiredQuality::Auto,
                codec: DesiredCodec::Auto,
                dynamic_range: DesiredDynamicRange::Auto,
                audio_track: None,
                audio_offset_ms: 0,
                subtitles: DesiredSubtitles::Off,
            }
            .canonical_form(),
            "v1;quality=auto;codec=auto;dynamic_range=auto;\
             audio=default;audio_offset_ms=0;subtitles=off"
        );
    }
}
