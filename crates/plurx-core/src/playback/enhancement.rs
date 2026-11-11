//! What Dolby Vision enhancement the delivered picture actually carries, as
//! the player shows it: short badges for what is active (FEL, RPU, DYN) and
//! plain detail rows for everything, including why something is not in use.
//!
//! The server computes this once so every client renders the same thing and
//! none has to reconstruct the delivery route from wire fields.

use serde::{Deserialize, Serialize};

/// The enhancements a badge can name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnhancementBadge {
    /// The full enhancement layer's residual is in the delivered picture.
    #[serde(rename = "FEL")]
    Fel,
    /// The RPU's reshaping (polynomial/MMR) was applied by the server.
    #[serde(rename = "RPU")]
    Rpu,
    /// Dolby Vision dynamic display metadata (L1/L2/L8) reaches the display.
    #[serde(rename = "DYN")]
    Dyn,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnhancementDetail {
    pub badge: EnhancementBadge,
    pub active: bool,
    /// One plain sentence: what it is when active, why not when inactive.
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackEnhancement {
    /// Badges for the active enhancements only, in display order.
    pub active: Vec<EnhancementBadge>,
    /// Every enhancement that applies to this source, active or not.
    pub details: Vec<EnhancementDetail>,
    /// The FEL/RPU badges of a processed HDR10 delivery are shown only while
    /// the client holds a current `effective_processing` report for this
    /// generation (they clear on seek, fallback or replacement with it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub requires_effective_processing: bool,
}

/// How many of the served frames' display mappings applied the title's creative trims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CreativeTrims {
    #[default]
    None,
    Partial,
    All,
}

/// How the picture reaches the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnhancementRoute {
    /// Dolby Vision delivered as the source's own profile.
    NativeDolbyVision,
    /// Profile 7 rewritten to 8.1 from the base layer: the EL is dropped.
    ConvertedProfile81,
    /// The reconstructed picture re-encoded as 8.1 with the FEL baked in.
    ReconstructedProfile81 { fel_contributed: bool },
    /// Server-processed HDR10 (HDR10-E).
    ProcessedHdr10 {
        fel_contributed: bool,
        display_target: Option<u32>,
        /// How many served frames' display mappings applied the title's creative trims.
        creative_trims: CreativeTrims,
    },
    /// Server reconstructed SDR, awarded only from its current processing report.
    ProcessedSdr {
        fel_contributed: bool,
        display_target: Option<u32>,
        /// How many served frames' display mappings applied the title's creative trims.
        creative_trims: CreativeTrims,
    },
    /// The HDR10 base layer with the Dolby Vision metadata removed.
    Hdr10Base,
    /// Tone-mapped to SDR for an SDR display: nothing of Dolby Vision reaches
    /// the picture, and the rows say so rather than showing nothing.
    ToneMappedSdr,
    /// SDR or any other output: no Dolby Vision enhancement applies.
    Other,
}

#[derive(Debug, Clone)]
pub struct EnhancementFacts<'a> {
    /// Source Dolby Vision profile (5, 7, 8); `None` for a non-DV source.
    pub source_profile: Option<u8>,
    /// Whether the source declares an enhancement layer.
    pub source_el_present: Option<bool>,
    pub route: EnhancementRoute,
    /// Developer tab: "Process Dolby Vision for HDR output". `None` when the
    /// caller does not hold the settings snapshot (a prepared successor).
    pub hdr_processing_enabled: Option<bool>,
    /// Developer tab: FEL re-encode to Profile 8.1.
    pub fel_reencode_enabled: Option<bool>,
    /// Why processing was declined for this play, when it was.
    pub decline_reason: Option<&'a str>,
}

fn detail(badge: EnhancementBadge, active: bool, text: impl Into<String>) -> EnhancementDetail {
    EnhancementDetail {
        badge,
        active,
        text: text.into(),
    }
}

/// `None` for a source without Dolby Vision: the player shows nothing.
pub fn playback_enhancement(facts: &EnhancementFacts<'_>) -> Option<PlaybackEnhancement> {
    use EnhancementBadge::{Dyn, Fel, Rpu};
    let profile = facts.source_profile?;
    let has_el = profile == 7 && facts.source_el_present != Some(false);
    let not_used = |setting: Option<bool>, setting_name: &str| -> String {
        match (setting, facts.decline_reason) {
            (Some(false), _) => {
                format!("not used — {setting_name} is off in Settings → Developer")
            }
            (_, Some(reason)) => format!("not used for this play — {reason}"),
            (_, None) => "not used for this play".to_owned(),
        }
    };
    let mut details = Vec::new();
    let mut requires_effective_processing = false;
    match &facts.route {
        EnhancementRoute::NativeDolbyVision => {
            if has_el {
                details.push(detail(
                    Fel,
                    false,
                    "FEL: delivered for the display to decode",
                ));
            } else if profile == 7 {
                details.push(detail(Fel, false, "FEL: not in this source"));
            }
            details.push(detail(
                Rpu,
                false,
                "RPU: carried to the display, which applies it",
            ));
            details.push(detail(
                Dyn,
                true,
                "DYN: L1/L2/L8 dynamic metadata carried to the display",
            ));
        }
        EnhancementRoute::ConvertedProfile81 => {
            if has_el {
                details.push(detail(
                    Fel,
                    false,
                    format!(
                        "FEL: {}",
                        not_used(facts.fel_reencode_enabled, "FEL re-encode")
                    ),
                ));
            }
            details.push(detail(
                Rpu,
                false,
                "RPU: converted to Profile 8.1 — without the enhancement layer its \
                 reshaping is a pass-through; its dynamic metadata is DYN",
            ));
            details.push(detail(
                Dyn,
                true,
                "DYN: L1/L2/L8 dynamic metadata carried to the display",
            ));
        }
        EnhancementRoute::ReconstructedProfile81 { fel_contributed } => {
            details.push(detail(
                Fel,
                *fel_contributed,
                if *fel_contributed {
                    "FEL: full enhancement layer reconstructed into the picture"
                } else {
                    // Only the selected plan or its served-window receipt can
                    // say the base layer came from composition; neither did.
                    "FEL: not confirmed for this play — the processing receipt does not \
                     report the enhancement layer composed into the picture"
                },
            ));
            details.push(detail(
                Rpu,
                true,
                "RPU: reshaping applied by the server; a rewritten 8.1 RPU carries \
                 its dynamic metadata (DYN)",
            ));
            details.push(detail(
                Dyn,
                true,
                "DYN: L1/L2/L8 dynamic metadata carried to the display",
            ));
        }
        EnhancementRoute::ProcessedHdr10 {
            fel_contributed,
            display_target,
            creative_trims,
        } => {
            requires_effective_processing = true;
            if has_el {
                details.push(detail(
                    Fel,
                    *fel_contributed,
                    if *fel_contributed {
                        "FEL: full enhancement layer reconstructed into the picture"
                    } else {
                        "FEL: no enhancement-layer detail in this window"
                    },
                ));
            }
            details.push(detail(
                Rpu,
                true,
                "RPU: base reshaping (polynomial and MMR) applied by the server",
            ));
            details.push(detail(
                Dyn,
                display_target.is_some(),
                display_target.map_or_else(
                    || "DYN: not carried — HDR10 has no Dolby Vision dynamic metadata".into(),
                    |nits| match creative_trims {
                        CreativeTrims::All => format!("DYN: per-shot Dolby display mapping for a {nits}-nit display by the server, with the colourist's creative trims"),
                        CreativeTrims::Partial => format!("DYN: per-shot Dolby display mapping for a {nits}-nit display by the server, with the colourist's creative trims on some shots"),
                        CreativeTrims::None => format!("DYN: per-shot L1 metadata mapped for a {nits}-nit display by the server (L1 only; creative trims are not applied)"),
                    },
                ),
            ));
        }
        EnhancementRoute::ProcessedSdr {
            fel_contributed,
            display_target,
            creative_trims,
        } => {
            requires_effective_processing = true;
            if has_el {
                details.push(detail(
                    Fel,
                    *fel_contributed,
                    if *fel_contributed {
                        "FEL: full enhancement layer reconstructed into the SDR picture"
                    } else {
                        "FEL: no enhancement-layer detail in this window"
                    },
                ));
            }
            details.push(detail(
                Rpu,
                true,
                "RPU: base reshaping applied by the server before SDR conversion",
            ));
            details.push(detail(
                Dyn,
                *display_target == Some(100),
                match (*display_target == Some(100), creative_trims) {
                    (true, CreativeTrims::All) => "DYN: per-shot Dolby display mapping for 100-nit BT.709 SDR by the server, with the colourist's creative trims",
                    (true, CreativeTrims::Partial) => "DYN: per-shot Dolby display mapping for 100-nit BT.709 SDR by the server, with the colourist's creative trims on some shots",
                    (true, CreativeTrims::None) => "DYN: per-shot L1 mapped for 100-nit BT.709 SDR by the server (L1 only; creative trims are not applied)",
                    (false, _) => "DYN: no current per-shot L1 mapping receipt",
                },
            ));
        }
        EnhancementRoute::Hdr10Base => {
            let why = not_used(facts.hdr_processing_enabled, "Dolby Vision processing");
            if has_el {
                details.push(detail(Fel, false, format!("FEL: {why}")));
            }
            details.push(detail(Rpu, false, format!("RPU: {why}")));
            details.push(detail(
                Dyn,
                false,
                "DYN: not carried — HDR10 has no Dolby Vision dynamic metadata",
            ));
        }
        EnhancementRoute::ToneMappedSdr => {
            let why = "not used — this display gets SDR, tone-mapped from the HDR10 base";
            if has_el {
                details.push(detail(Fel, false, format!("FEL: {why}")));
            }
            details.push(detail(Rpu, false, format!("RPU: {why}")));
            details.push(detail(
                Dyn,
                false,
                "DYN: not carried — SDR has no Dolby Vision dynamic metadata",
            ));
        }
        EnhancementRoute::Other => return None,
    }
    let active = details
        .iter()
        .filter(|detail| detail.active)
        .map(|detail| detail.badge)
        .collect();
    Some(PlaybackEnhancement {
        active,
        details,
        requires_effective_processing,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use EnhancementBadge::{Dyn, Fel, Rpu};

    fn facts(route: EnhancementRoute) -> EnhancementFacts<'static> {
        EnhancementFacts {
            source_profile: Some(7),
            source_el_present: Some(true),
            route,
            hdr_processing_enabled: Some(true),
            fel_reencode_enabled: Some(false),
            decline_reason: None,
        }
    }

    #[test]
    fn processed_sdr_dyn_requires_the_actual_100_nit_mapping() {
        for target in [None, Some(100)] {
            let enhancement = playback_enhancement(&EnhancementFacts {
                source_profile: Some(7),
                source_el_present: Some(true),
                route: EnhancementRoute::ProcessedSdr {
                    fel_contributed: true,
                    display_target: target,
                    creative_trims: CreativeTrims::None,
                },
                hdr_processing_enabled: Some(true),
                fel_reencode_enabled: None,
                decline_reason: None,
            })
            .expect("DV source");
            assert!(enhancement.requires_effective_processing);
            assert_eq!(enhancement.active.contains(&Dyn), target == Some(100));
            assert!(enhancement.active.contains(&Fel));
            assert!(enhancement.active.contains(&Rpu));
            assert!(!enhancement
                .details
                .iter()
                .any(|detail| detail.text.contains("trims applied")));
        }
    }
    #[test]
    fn processed_hdr10_with_fel_shows_fel_and_rpu_only() {
        let shown = playback_enhancement(&facts(EnhancementRoute::ProcessedHdr10 {
            fel_contributed: true,
            display_target: None,
            creative_trims: CreativeTrims::None,
        }))
        .expect("DV source");
        assert_eq!(shown.active, [Fel, Rpu]);
        assert!(shown.requires_effective_processing);
        assert!(shown.details.iter().any(|d| d.badge == Dyn && !d.active));
    }

    #[test]
    fn targeted_hdr10_dyn_names_bucket_and_l1_limit() {
        let shown = playback_enhancement(&facts(EnhancementRoute::ProcessedHdr10 {
            fel_contributed: true,
            display_target: Some(1600),
            creative_trims: CreativeTrims::None,
        }))
        .expect("DV source");
        assert_eq!(shown.active, [Fel, Rpu, Dyn]);
        let dyn_detail = shown.details.iter().find(|d| d.badge == Dyn).expect("DYN");
        assert!(dyn_detail.text.contains("1600-nit"));
        assert!(dyn_detail.text.contains("L1 only"));
        assert!(shown.requires_effective_processing);
        let trimmed = playback_enhancement(&facts(EnhancementRoute::ProcessedHdr10 {
            fel_contributed: true,
            display_target: Some(1600),
            creative_trims: CreativeTrims::All,
        }))
        .expect("DV source");
        let dyn_detail = trimmed
            .details
            .iter()
            .find(|d| d.badge == Dyn)
            .expect("DYN");
        assert!(dyn_detail.text.contains("1600-nit") && dyn_detail.text.contains("creative trims"));
        assert!(!dyn_detail.text.contains("L1 only") && !dyn_detail.text.contains("some shots"));
        let partial = playback_enhancement(&facts(EnhancementRoute::ProcessedHdr10 {
            fel_contributed: true,
            display_target: Some(1600),
            creative_trims: CreativeTrims::Partial,
        }))
        .expect("DV source");
        let dyn_detail = partial
            .details
            .iter()
            .find(|d| d.badge == Dyn)
            .expect("DYN");
        assert!(dyn_detail.text.contains("creative trims on some shots"));
        assert!(!dyn_detail.text.contains("L1 only"));
    }

    #[test]
    fn converted_p81_shows_dyn_and_says_why_fel_is_unused() {
        let shown =
            playback_enhancement(&facts(EnhancementRoute::ConvertedProfile81)).expect("DV source");
        assert_eq!(shown.active, [Dyn]);
        let fel = shown
            .details
            .iter()
            .find(|d| d.badge == Fel)
            .expect("FEL row");
        assert!(!fel.active);
        assert!(fel.text.contains("FEL re-encode is off"), "{}", fel.text);
    }

    #[test]
    fn reconstructed_p81_carries_fel_and_dyn() {
        let shown = playback_enhancement(&facts(EnhancementRoute::ReconstructedProfile81 {
            fel_contributed: true,
        }))
        .expect("DV source");
        // The server applied the RPU's reshaping with the FEL; the display
        // applies the rewritten RPU's dynamic metadata.
        assert_eq!(shown.active, [Fel, Rpu, Dyn]);
        assert!(!shown.requires_effective_processing);
        // Without the plan or receipt confirming composition, FEL is not claimed.
        let unconfirmed = playback_enhancement(&facts(EnhancementRoute::ReconstructedProfile81 {
            fel_contributed: false,
        }))
        .expect("DV source");
        assert_eq!(unconfirmed.active, [Rpu, Dyn]);
        let fel = unconfirmed
            .details
            .iter()
            .find(|d| d.badge == Fel)
            .expect("FEL row");
        assert!(
            !fel.active && fel.text.contains("not confirmed"),
            "{}",
            fel.text
        );
    }

    #[test]
    fn hdr10_base_has_no_badges_and_a_neutral_reason() {
        let mut base = facts(EnhancementRoute::Hdr10Base);
        base.decline_reason = Some("the client does not report HLS fMP4 HEVC (hvc1)");
        let shown = playback_enhancement(&base).expect("DV source");
        assert!(shown.active.is_empty());
        assert!(shown.details[0]
            .text
            .contains("not used for this play — the client"));
        base.hdr_processing_enabled = Some(false);
        let shown = playback_enhancement(&base).expect("DV source");
        assert!(shown.details[0]
            .text
            .contains("is off in Settings → Developer"));
    }

    #[test]
    fn unknown_settings_give_a_neutral_reason() {
        let mut base = facts(EnhancementRoute::Hdr10Base);
        base.hdr_processing_enabled = None;
        let shown = playback_enhancement(&base).expect("DV source");
        assert!(shown
            .details
            .iter()
            .all(|detail| !detail.text.contains("Settings")));
    }

    #[test]
    fn native_profile8_shows_dyn_and_no_fel_row() {
        let mut native = facts(EnhancementRoute::NativeDolbyVision);
        native.source_profile = Some(8);
        native.source_el_present = Some(false);
        let shown = playback_enhancement(&native).expect("DV source");
        assert_eq!(shown.active, [Dyn]);
        assert!(shown.details.iter().all(|d| d.badge != Fel));
    }

    #[test]
    fn non_dv_sources_and_other_outputs_show_nothing() {
        let mut none = facts(EnhancementRoute::Hdr10Base);
        none.source_profile = None;
        assert!(playback_enhancement(&none).is_none());
        assert!(playback_enhancement(&facts(EnhancementRoute::Other)).is_none());
    }

    /// An SDR display still learns what the source carries and why none of
    /// it is in its picture: rows, no pills.
    #[test]
    fn a_tone_mapped_sdr_play_lists_every_enhancement_as_unused() {
        let shown =
            playback_enhancement(&facts(EnhancementRoute::ToneMappedSdr)).expect("DV source");
        assert!(shown.active.is_empty());
        assert_eq!(
            shown.details.iter().map(|d| d.badge).collect::<Vec<_>>(),
            [Fel, Rpu, Dyn]
        );
        assert!(shown.details.iter().all(|d| !d.active));
        assert!(
            shown.details[0].text.contains("SDR"),
            "{}",
            shown.details[0].text
        );
    }

    #[test]
    fn wire_names_are_the_badge_labels() {
        assert_eq!(
            serde_json::to_string(&[Fel, Rpu, Dyn]).expect("badges serialize"),
            r#"["FEL","RPU","DYN"]"#
        );
    }
}
