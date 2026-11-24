//! What each signed-in playback device decodes, and how a version will play
//! on it (Curator ADR 0026 §5 and §7).
//!
//! Clients already send their capabilities with every playback decision
//! ([`DeviceCaps`]); nothing kept them. This module is the record of the last
//! document each device sent, the HDR summary Curator reads to derive a
//! quality-profile order, and the one-line outcome the title page shows per
//! device. The outcome is a readout of [`super::decide`] — the same verdict
//! Play would reach — never a second opinion.

use serde::{Deserialize, Serialize};

use super::caps::{DeviceCaps, Transfer};
use super::{Decision, PlaybackMethod};
use crate::domain::MediaFile;
use crate::scan::dv_title::TitleElKind;

/// Settings-key prefix of a device's last capability document. One key per
/// login token, so two devices of one account never race each other.
pub const DEVICE_CAPS_KEY_PREFIX: &str = "playback.device_caps.";

/// The settings key for a login token's capability record: the prefix and
/// the first sixteen hex digits of the token's digest. The digest itself
/// stays in the token table.
pub fn device_caps_key(token_hash: &str) -> String {
    let prefix: String = token_hash.chars().take(16).collect();
    format!("{DEVICE_CAPS_KEY_PREFIX}{prefix}")
}

/// The record stored under [`device_caps_key`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceCapsRecord {
    pub user_id: i64,
    /// First eight hex digits of the token digest: what the device list
    /// (`TokenSummary::token_hash_prefix`) names the device by.
    pub token_prefix: String,
    pub caps: DeviceCaps,
    pub recorded_at: i64,
}

impl DeviceCapsRecord {
    /// The caps as recorded: the window-size and peak-provenance readings,
    /// which change with every resize and say nothing about what the device
    /// decodes, are left out so a record is rewritten only when the device's
    /// abilities change.
    pub fn new(user_id: i64, token_hash: &str, mut caps: DeviceCaps, recorded_at: i64) -> Self {
        if let Some(display) = caps.display.as_mut() {
            display.presentation_target = None;
            display.peak_source = None;
        }
        DeviceCapsRecord {
            user_id,
            token_prefix: token_hash.chars().take(8).collect(),
            caps,
            recorded_at,
        }
    }

    /// Whether `other` describes the same abilities (ignoring when).
    pub fn same_abilities(&self, other: &DeviceCapsRecord) -> bool {
        serde_json::to_value(&self.caps).ok() == serde_json::to_value(&other.caps).ok()
            && self.user_id == other.user_id
    }
}

/// What a device does with each HDR presentation, for Curator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceHdr {
    /// It renders Dolby Vision at all.
    pub dolby_vision: bool,
    /// The Dolby Vision profiles it claims to decode.
    pub dv_profiles: Vec<u8>,
    /// It decodes a profile 7 enhancement layer (claims profile 7), so a FEL
    /// stream reaches it whole. Without it the EL is dropped.
    pub fel: bool,
    /// It plays profile 5, which has no HDR10 base to fall back to.
    pub profile5: bool,
    pub hdr10: bool,
    pub hlg: bool,
    /// HDR10+ dynamic metadata. Capability documents do not say, so this is
    /// `None`: a device is assumed to render the static HDR10 base.
    pub hdr10plus: Option<bool>,
}

/// Summarise a capability document's HDR abilities.
pub fn device_hdr(caps: &DeviceCaps) -> DeviceHdr {
    let display = caps.display.clone().unwrap_or_default();
    let mut dv_profiles: Vec<u8> = caps
        .video
        .iter()
        .flat_map(|v| v.dv_profiles.iter().copied())
        .collect();
    dv_profiles.sort_unstable();
    dv_profiles.dedup();
    let presents = |t: Transfer| caps.video.iter().any(|v| v.present.contains(&t));
    let dolby_vision = display.dolby_vision && !dv_profiles.is_empty();
    DeviceHdr {
        dolby_vision,
        fel: dolby_vision && dv_profiles.contains(&7),
        profile5: dolby_vision && dv_profiles.contains(&5),
        hdr10: display.hdr && presents(Transfer::Pq),
        hlg: presents(Transfer::Hlg),
        hdr10plus: None,
        dv_profiles,
    }
}

/// How one version plays on one device, in the title page's words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlaysOn {
    /// "Full FEL", "DV P8", "EL dropped · plays as 8.1", "HDR10 base layer".
    pub label: String,
    /// `direct_play`, `remux` or `transcode`.
    pub method: &'static str,
    /// The source has an enhancement layer this delivery does not carry.
    pub el_dropped: bool,
    /// The delivered picture is the version's own grade; false when it is a
    /// fallback (a base layer, a tone-map), which the page shows as a caution.
    pub full_quality: bool,
}

/// Describe a decision for a version. `el_kind` is what the complete-title
/// scan found (FEL or MEL), when it has run.
pub fn plays_on(file: &MediaFile, el_kind: Option<TitleElKind>, d: &Decision) -> PlaysOn {
    let method = match d.method {
        PlaybackMethod::DirectPlay => "direct_play",
        PlaybackMethod::Remux => "remux",
        PlaybackMethod::Transcode => "transcode",
    };
    let source_dv = file.dolby_vision.profile;
    let source_el = file.dolby_vision.el_present == Some(true);
    let (label, el_dropped, full) = match d.delivered_dynamic_range {
        "dolby_vision" => match (d.delivered_dolby_vision_profile, source_el) {
            (Some(7), true) => match el_kind {
                Some(TitleElKind::Fel) => ("Full FEL".to_owned(), false, true),
                Some(TitleElKind::Mel) => ("DV P7 MEL".to_owned(), false, true),
                None => ("DV P7, both layers".to_owned(), false, true),
            },
            (_, true) => (
                "EL dropped · plays as 8.1".to_owned(),
                true,
                el_kind != Some(TitleElKind::Fel),
            ),
            (Some(p), false) => (format!("DV P{p}"), false, true),
            (None, false) => ("Dolby Vision".to_owned(), false, true),
        },
        "hdr10" if source_dv.is_some() => ("HDR10 base layer".to_owned(), source_el, false),
        "hdr10" => ("HDR10".to_owned(), false, true),
        "hlg" if source_dv.is_some() => ("HLG base layer".to_owned(), source_el, false),
        "hlg" => ("HLG".to_owned(), false, true),
        _ if file.hdr.as_deref().is_some_and(|h| h != "sdr") => {
            ("SDR, tone-mapped".to_owned(), source_el, false)
        }
        _ => ("SDR".to_owned(), false, true),
    };
    PlaysOn {
        label,
        method,
        el_dropped,
        full_quality: full,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playback::caps::{DisplayCaps, VideoCaps};

    fn caps(profiles: &[u8], dv: bool, hdr: bool, present: &[Transfer]) -> DeviceCaps {
        DeviceCaps {
            v: DeviceCaps::VERSION,
            video: vec![VideoCaps {
                codec: "hevc".into(),
                dv_profiles: profiles.to_vec(),
                present: present.to_vec(),
                ..Default::default()
            }],
            display: Some(DisplayCaps {
                hdr,
                dolby_vision: dv,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn device_hdr_reads_the_capability_document() {
        let linux = device_hdr(&caps(
            &[8, 7, 5],
            true,
            true,
            &[Transfer::Pq, Transfer::Hlg],
        ));
        assert_eq!(linux.dv_profiles, vec![5, 7, 8]);
        assert!(linux.dolby_vision && linux.fel && linux.profile5 && linux.hdr10 && linux.hlg);
        assert_eq!(linux.hdr10plus, None);

        let apple = device_hdr(&caps(&[5, 8], true, true, &[Transfer::Pq]));
        assert!(apple.dolby_vision && !apple.fel && apple.profile5 && !apple.hlg);

        // Profiles claimed by the decoder mean nothing on a panel without DV.
        let sdr_panel = device_hdr(&caps(&[5, 8], false, false, &[Transfer::Sdr]));
        assert!(!sdr_panel.dolby_vision && !sdr_panel.fel && !sdr_panel.hdr10);
    }

    #[test]
    fn a_record_ignores_window_geometry() {
        let mut a = caps(&[8], true, true, &[Transfer::Pq]);
        let b = a.clone();
        a.display.as_mut().expect("display").max_nits = Some(1000);
        let ra = DeviceCapsRecord::new(1, "abcdef0123456789ffff", a, 10);
        let rb = DeviceCapsRecord::new(1, "abcdef0123456789ffff", b, 20);
        assert_eq!(ra.token_prefix, "abcdef01");
        assert!(
            !ra.same_abilities(&rb),
            "a different peak is a different device reading"
        );
        assert_eq!(
            device_caps_key("abcdef0123456789ffff"),
            "playback.device_caps.abcdef0123456789"
        );
    }
}
