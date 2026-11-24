//! The two Playback info rows that say where a Dolby Vision picture's
//! metadata went and what display the stream was mapped for (wire shape in
//! `docs/API.md`, labels and order in `tests/playback/playback-info-fields.json`):
//!
//! ```text
//!  Dolby Vision metadata   In the file           L1 · L2 · L5 · L6 · L8 · L9 · L11 (CM v4.0)
//!                          Applied by Cinema     L1 · L8
//!                          Left for the display  None; delivered as HDR10
//!  Display target          Reported peak         1000 nits · your setting
//!                          Mapped for            1000 nits
//!                          Title peak            1412 nits
//! ```
//!
//! Reporters only. Every input is a fact some other owner already decided —
//! the delivery route, the current processing report, the session's display
//! target policy, the title's recorded levels — and nothing here feeds back
//! into a decision. The server writes each sub-row's text so every client
//! shows the same words; the structured fields beside it are for tests and
//! diagnostics, not for clients to re-derive a list from.

use serde::{Deserialize, Serialize};

use super::caps::DisplayPeakSource;
use super::enhancement::{CreativeTrims, EnhancementRoute};
use crate::scan::dv_levels::DvTitleLevels;

/// The longest sub-row text the server writes, and the most a receiver
/// relays.
pub const MAX_ROW_TEXT: usize = 512;
/// Display-metadata level numbers are a byte; a title carries a handful.
const MAX_LEVELS: usize = 32;

/// `In the file`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InFileRow {
    /// `None` until the title's levels are recorded: unknown, never empty.
    #[serde(default)]
    pub levels: Option<Vec<u8>>,
    /// `"2.9"` or `"4.0"` when the levels are known.
    #[serde(default)]
    pub cm_version: Option<String>,
    pub text: String,
}

/// One level the served picture consumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedLevel {
    /// `None` names the title's creative trims when its levels are not
    /// recorded, so which trim level was used cannot be said.
    #[serde(default)]
    pub level: Option<u8>,
    /// Only some of the window's frames consumed it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub partial: bool,
}

/// `Applied by Cinema`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppliedRow {
    pub levels: Vec<AppliedLevel>,
    pub text: String,
}

/// `Left for the display`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeftRow {
    /// The levels still in the delivered RPU; `None` when the delivery
    /// carries none (`delivered` says why) or the title's levels are unknown.
    #[serde(default)]
    pub levels: Option<Vec<u8>>,
    /// `"hdr10"` or `"sdr"` when the delivery carries no Dolby Vision
    /// metadata at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered: Option<String>,
    pub text: String,
}

/// The `Dolby Vision metadata` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DvMetadataRows {
    pub in_file: InFileRow,
    pub applied: AppliedRow,
    pub left_for_display: LeftRow,
}

fn level_list(levels: impl IntoIterator<Item = u8>) -> String {
    levels
        .into_iter()
        .map(|level| format!("L{level}"))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Which levels the server's display mapping consumes, defined once.
///
/// The mapping follows the branch `dv_display_management` takes for every
/// frame (`transcode/dv_processing/display_target.rs`): a display-target
/// mapping always reads L1; its creative trims come from L8 on a CM v4.0
/// grade (a frame with L254 or L8 takes that branch) and from L2 on a CM v2.9
/// one. The base reshaping is the RPU's mapping curves, not a display level,
/// so it adds none; L6's mastering floor and L9's primaries describe the
/// output's static metadata and are not claimed here.
fn applied_levels(
    display_target: Option<u32>,
    creative_trims: CreativeTrims,
    levels: Option<&DvTitleLevels>,
) -> Vec<AppliedLevel> {
    let mut applied = Vec::new();
    if display_target.is_none() {
        return applied;
    }
    applied.push(AppliedLevel {
        level: Some(1),
        partial: false,
    });
    let partial = match creative_trims {
        CreativeTrims::None => return applied,
        CreativeTrims::Partial => true,
        CreativeTrims::All => false,
    };
    applied.push(AppliedLevel {
        level: levels.map(|levels| if levels.cm_v40 { 8 } else { 2 }),
        partial,
    });
    applied
}

fn applied_text(applied: &[AppliedLevel]) -> String {
    if applied.is_empty() {
        return "None".into();
    }
    applied
        .iter()
        .map(|entry| {
            let name = entry
                .level
                .map_or_else(|| "Creative trims".to_owned(), |level| format!("L{level}"));
            if entry.partial {
                format!("{name} (some shots)")
            } else {
                name
            }
        })
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The `Dolby Vision metadata` row for a delivery; `None` for a source
/// without Dolby Vision (the route says `Other`).
///
/// For a processed delivery the route must carry the current processing
/// report's facts: the row describes that report and travels with it.
pub fn dv_metadata_rows(
    route: &EnhancementRoute,
    levels: Option<&DvTitleLevels>,
) -> Option<DvMetadataRows> {
    let in_file = match levels {
        Some(levels) => {
            let version = if levels.cm_v40 { "4.0" } else { "2.9" };
            let shown: Vec<u8> = levels
                .levels
                .iter()
                .copied()
                .filter(|level| *level != 254)
                .collect();
            InFileRow {
                levels: Some(levels.levels.iter().copied().collect()),
                cm_version: Some(version.to_owned()),
                text: format!("{} (CM v{version})", level_list(shown)),
            }
        }
        None => InFileRow {
            levels: None,
            cm_version: None,
            text: "Unavailable".into(),
        },
    };
    let carried = || LeftRow {
        levels: levels.map(|levels| levels.levels.iter().copied().collect()),
        delivered: None,
        text: levels.map_or_else(
            || "Unavailable".to_owned(),
            |levels| level_list(levels.levels.iter().copied().filter(|level| *level != 254)),
        ),
    };
    let none_delivered = |range: &str, name: &str| LeftRow {
        levels: None,
        delivered: Some(range.to_owned()),
        text: format!("None; delivered as {name}"),
    };
    // Every Dolby Vision delivery keeps the source RPU's display metadata:
    // the 8.1 conversion and the reconstructed 8.1 both rewrite the mapping
    // and leave the extension blocks (`author_p81.c`: mode-2 conversion, then
    // mapping removal), so the display applies all of it.
    let (applied, left) = match route {
        EnhancementRoute::NativeDolbyVision
        | EnhancementRoute::ConvertedProfile81
        | EnhancementRoute::ReconstructedProfile81 { .. } => (Vec::new(), carried()),
        EnhancementRoute::ProcessedHdr10 {
            display_target,
            creative_trims,
            ..
        } => (
            applied_levels(*display_target, *creative_trims, levels),
            none_delivered("hdr10", "HDR10"),
        ),
        EnhancementRoute::ProcessedSdr {
            display_target,
            creative_trims,
            ..
        } => (
            applied_levels(*display_target, *creative_trims, levels),
            none_delivered("sdr", "SDR"),
        ),
        EnhancementRoute::Hdr10Base => (Vec::new(), none_delivered("hdr10", "HDR10")),
        EnhancementRoute::ToneMappedSdr => (Vec::new(), none_delivered("sdr", "SDR")),
        EnhancementRoute::Other => return None,
    };
    Some(DvMetadataRows {
        in_file,
        applied: AppliedRow {
            text: applied_text(&applied),
            levels: applied,
        },
        left_for_display: left,
    })
}

/// What the session's display-target policy concluded, without the client
/// reconstructing it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DisplayTargetState {
    /// The current report proves a mapping for this peak.
    Targeted { mapped_nits: u32 },
    /// The title's own peak already fits the display's bucket.
    AlreadyFits { bucket_nits: u32 },
    /// The master grade is sent, for the server's reason.
    NotTargeted { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportedPeak {
    pub nits: u32,
    #[serde(default)]
    pub source: Option<DisplayPeakSource>,
}

/// The `Display target` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplayTargetRows {
    #[serde(default)]
    pub reported_peak: Option<ReportedPeak>,
    pub reported_peak_text: String,
    pub state: DisplayTargetState,
    pub mapped_for_text: String,
    #[serde(default)]
    pub title_peak_nits: Option<u32>,
    pub title_peak_text: String,
}

/// Facts the display-target row reports.
#[derive(Debug, Clone, Default)]
pub struct DisplayTargetFacts<'a> {
    /// What the client reported (`DisplayCaps::max_nits`).
    pub reported_peak_nits: Option<i64>,
    pub peak_source: Option<DisplayPeakSource>,
    /// The current processing report's display-target mapping.
    pub mapped_nits: Option<u32>,
    /// The delivery is SDR (mapped for 100 nits).
    pub sdr: bool,
    /// The session's policy declined because the title already fits this
    /// bucket.
    pub title_fits_nits: Option<u32>,
    /// The session's policy note, when it declined for another reason.
    pub decline_note: Option<&'a str>,
    /// The scanned L1 maximum of an eligible title.
    pub title_peak_nits: Option<u32>,
}

const TARGETING_OFF: &str = "display targeting is off for this session; the master grade is sent";

fn bounded(mut text: String) -> String {
    text.retain(|c| !c.is_control());
    if text.len() > MAX_ROW_TEXT {
        let mut end = MAX_ROW_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

/// The `Display target` row of a processed delivery.
pub fn display_target_rows(facts: &DisplayTargetFacts<'_>) -> DisplayTargetRows {
    let reported_peak = facts
        .reported_peak_nits
        .and_then(|nits| u32::try_from(nits).ok())
        .filter(|nits| *nits > 0)
        .map(|nits| ReportedPeak {
            nits,
            source: facts.peak_source,
        });
    let reported_peak_text = match &reported_peak {
        None => "Not reported".to_owned(),
        Some(ReportedPeak { nits, source }) => match source {
            Some(DisplayPeakSource::Viewer) => format!("{nits} nits · your setting"),
            Some(DisplayPeakSource::Platform) => format!("{nits} nits · reported by the device"),
            None => format!("{nits} nits"),
        },
    };
    let state = match (facts.mapped_nits, facts.title_fits_nits) {
        (Some(mapped_nits), _) => DisplayTargetState::Targeted { mapped_nits },
        (None, Some(bucket_nits)) => DisplayTargetState::AlreadyFits { bucket_nits },
        (None, None) => DisplayTargetState::NotTargeted {
            reason: bounded(
                facts
                    .decline_note
                    .filter(|note| !note.trim().is_empty())
                    .unwrap_or(TARGETING_OFF)
                    .to_owned(),
            ),
        },
    };
    let mapped_for_text = bounded(match &state {
        DisplayTargetState::Targeted { mapped_nits } if facts.sdr => {
            format!("{mapped_nits} nits (SDR)")
        }
        DisplayTargetState::Targeted { mapped_nits } => format!("{mapped_nits} nits"),
        DisplayTargetState::AlreadyFits { bucket_nits } => {
            format!("Not needed — the title already fits a {bucket_nits}-nit display")
        }
        DisplayTargetState::NotTargeted { reason } => format!("Not targeted — {reason}"),
    });
    DisplayTargetRows {
        reported_peak,
        reported_peak_text,
        state,
        mapped_for_text,
        title_peak_nits: facts.title_peak_nits,
        title_peak_text: facts
            .title_peak_nits
            .map_or_else(|| "Unavailable".to_owned(), |nits| format!("{nits} nits")),
    }
}

/// Both rows of a processed delivery, built by the session owner beside the
/// processing report they describe and cleared with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DvProcessingRows {
    pub dv_metadata: DvMetadataRows,
    pub display_target: DisplayTargetRows,
}

impl DvMetadataRows {
    /// The shape [`dv_metadata_rows`] builds: bounded texts and level lists.
    /// A receiver relays rows only when they hold.
    pub fn is_well_formed(&self) -> bool {
        let text = |text: &str| {
            !text.trim().is_empty()
                && text.len() <= MAX_ROW_TEXT
                && !text.chars().any(char::is_control)
        };
        let list = |levels: &Option<Vec<u8>>| levels.as_ref().is_none_or(|l| l.len() <= MAX_LEVELS);
        text(&self.in_file.text)
            && text(&self.applied.text)
            && text(&self.left_for_display.text)
            && list(&self.in_file.levels)
            && list(&self.left_for_display.levels)
            && self.applied.levels.len() <= 2
            && self
                .in_file
                .cm_version
                .as_deref()
                .is_none_or(|v| matches!(v, "2.9" | "4.0"))
            && self
                .left_for_display
                .delivered
                .as_deref()
                .is_none_or(|v| matches!(v, "hdr10" | "sdr"))
    }
}

impl DvProcessingRows {
    pub fn is_well_formed(&self) -> bool {
        let text = |text: &str| {
            !text.trim().is_empty()
                && text.len() <= MAX_ROW_TEXT
                && !text.chars().any(char::is_control)
        };
        self.dv_metadata.is_well_formed()
            && text(&self.display_target.reported_peak_text)
            && text(&self.display_target.mapped_for_text)
            && text(&self.display_target.title_peak_text)
            && match &self.display_target.state {
                DisplayTargetState::NotTargeted { reason } => reason.len() <= MAX_ROW_TEXT,
                _ => true,
            }
    }

    /// A bounded relay of rows the owner built: parsed, checked and
    /// rejected whole when anything is out of shape.
    pub fn from_owner_wire(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > 4096 {
            return None;
        }
        let rows: Self = serde_json::from_slice(bytes).ok()?;
        rows.is_well_formed().then_some(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn cm40() -> DvTitleLevels {
        DvTitleLevels {
            levels: BTreeSet::from([1, 2, 5, 6, 8, 9, 11, 254]),
            cm_v40: true,
        }
    }

    fn cm29() -> DvTitleLevels {
        DvTitleLevels {
            levels: BTreeSet::from([1, 2, 5, 6]),
            cm_v40: false,
        }
    }

    fn processed(display_target: Option<u32>, creative_trims: CreativeTrims) -> EnhancementRoute {
        EnhancementRoute::ProcessedHdr10 {
            fel_contributed: true,
            display_target,
            creative_trims,
        }
    }

    #[test]
    fn direct_play_applies_nothing_and_leaves_every_level_for_the_display() {
        let rows = dv_metadata_rows(&EnhancementRoute::NativeDolbyVision, Some(&cm40()))
            .expect("DV source");
        assert_eq!(
            rows.in_file.text,
            "L1 · L2 · L5 · L6 · L8 · L9 · L11 (CM v4.0)"
        );
        assert_eq!(rows.in_file.levels, Some(vec![1, 2, 5, 6, 8, 9, 11, 254]));
        assert_eq!(rows.applied.levels, vec![]);
        assert_eq!(rows.applied.text, "None");
        assert_eq!(
            rows.left_for_display.levels,
            Some(vec![1, 2, 5, 6, 8, 9, 11, 254])
        );
        assert_eq!(rows.left_for_display.delivered, None);
        assert_eq!(
            rows.left_for_display.text,
            "L1 · L2 · L5 · L6 · L8 · L9 · L11"
        );
        assert!(rows.is_well_formed());
    }

    #[test]
    fn conversion_to_8_1_applies_nothing_and_keeps_the_display_metadata() {
        for route in [
            EnhancementRoute::ConvertedProfile81,
            EnhancementRoute::ReconstructedProfile81 {
                fel_contributed: true,
            },
        ] {
            let rows = dv_metadata_rows(&route, Some(&cm29())).expect("DV source");
            assert_eq!(rows.in_file.text, "L1 · L2 · L5 · L6 (CM v2.9)");
            assert!(rows.applied.levels.is_empty(), "{route:?}");
            assert_eq!(rows.left_for_display.text, "L1 · L2 · L5 · L6");
        }
    }

    #[test]
    fn enhanced_hdr10_with_a_peak_applies_l1_and_the_grades_own_trim_level() {
        let rows = dv_metadata_rows(&processed(Some(1000), CreativeTrims::All), Some(&cm40()))
            .expect("DV source");
        assert_eq!(rows.applied.text, "L1 · L8");
        assert_eq!(
            rows.left_for_display,
            LeftRow {
                levels: None,
                delivered: Some("hdr10".into()),
                text: "None; delivered as HDR10".into()
            }
        );
        let rows = dv_metadata_rows(
            &processed(Some(1000), CreativeTrims::Partial),
            Some(&cm29()),
        )
        .expect("DV source");
        assert_eq!(rows.applied.text, "L1 · L2 (some shots)");
        let rows = dv_metadata_rows(&processed(Some(600), CreativeTrims::None), Some(&cm40()))
            .expect("DV source");
        assert_eq!(rows.applied.text, "L1");
        // Unknown levels never name a trim level they cannot prove.
        let rows =
            dv_metadata_rows(&processed(Some(1000), CreativeTrims::All), None).expect("DV source");
        assert_eq!(rows.applied.text, "L1 · Creative trims");
        assert_eq!(rows.in_file.text, "Unavailable");
        let sdr = EnhancementRoute::ProcessedSdr {
            fel_contributed: false,
            display_target: Some(100),
            creative_trims: CreativeTrims::All,
        };
        let rows = dv_metadata_rows(&sdr, Some(&cm40())).expect("DV source");
        assert_eq!(rows.applied.text, "L1 · L8");
        assert_eq!(rows.left_for_display.text, "None; delivered as SDR");
    }

    #[test]
    fn enhanced_hdr10_without_a_peak_applies_no_display_level() {
        let rows = dv_metadata_rows(&processed(None, CreativeTrims::None), Some(&cm40()))
            .expect("DV source");
        assert_eq!(rows.applied.text, "None");
        let target = display_target_rows(&DisplayTargetFacts {
            decline_note: Some(
                "your display did not report a supported HDR peak (400–4999 nits), so the master grade is sent",
            ),
            title_peak_nits: Some(1412),
            ..Default::default()
        });
        assert_eq!(target.reported_peak, None);
        assert_eq!(target.reported_peak_text, "Not reported");
        assert!(matches!(
            target.state,
            DisplayTargetState::NotTargeted { .. }
        ));
        assert_eq!(
            target.mapped_for_text,
            "Not targeted — your display did not report a supported HDR peak (400–4999 nits), so the master grade is sent"
        );
        assert_eq!(target.title_peak_text, "1412 nits");
        let off = display_target_rows(&DisplayTargetFacts::default());
        assert_eq!(
            off.mapped_for_text,
            format!("Not targeted — {TARGETING_OFF}")
        );
        assert_eq!(off.title_peak_text, "Unavailable");
    }

    #[test]
    fn a_targeted_stream_reports_the_peak_its_source_and_the_title_peak() {
        let target = display_target_rows(&DisplayTargetFacts {
            reported_peak_nits: Some(1000),
            peak_source: Some(DisplayPeakSource::Viewer),
            mapped_nits: Some(1000),
            title_peak_nits: Some(1412),
            ..Default::default()
        });
        assert_eq!(target.reported_peak_text, "1000 nits · your setting");
        assert_eq!(
            target.state,
            DisplayTargetState::Targeted { mapped_nits: 1000 }
        );
        assert_eq!(target.mapped_for_text, "1000 nits");
        let platform = display_target_rows(&DisplayTargetFacts {
            reported_peak_nits: Some(1499),
            peak_source: Some(DisplayPeakSource::Platform),
            mapped_nits: Some(1000),
            ..Default::default()
        });
        assert_eq!(
            platform.reported_peak_text,
            "1499 nits · reported by the device"
        );
        let sdr = display_target_rows(&DisplayTargetFacts {
            mapped_nits: Some(100),
            sdr: true,
            ..Default::default()
        });
        assert_eq!(sdr.mapped_for_text, "100 nits (SDR)");
    }

    #[test]
    fn a_title_that_already_fits_says_so_rather_than_not_targeted() {
        let target = display_target_rows(&DisplayTargetFacts {
            reported_peak_nits: Some(1000),
            peak_source: Some(DisplayPeakSource::Platform),
            title_fits_nits: Some(1000),
            decline_note: Some("this title already fits a 1000-nit display; master grade sent"),
            title_peak_nits: Some(812),
            ..Default::default()
        });
        assert_eq!(
            target.state,
            DisplayTargetState::AlreadyFits { bucket_nits: 1000 }
        );
        assert_eq!(
            target.mapped_for_text,
            "Not needed — the title already fits a 1000-nit display"
        );
        assert_eq!(target.title_peak_text, "812 nits");
    }

    #[test]
    fn a_missing_title_scan_is_unavailable_never_an_empty_list() {
        for route in [
            EnhancementRoute::NativeDolbyVision,
            EnhancementRoute::Hdr10Base,
            processed(Some(1000), CreativeTrims::None),
        ] {
            let rows = dv_metadata_rows(&route, None).expect("DV source");
            assert_eq!(rows.in_file.levels, None);
            assert_eq!(rows.in_file.text, "Unavailable");
        }
        let rows = dv_metadata_rows(&EnhancementRoute::NativeDolbyVision, None).expect("DV");
        assert_eq!(rows.left_for_display.levels, None);
        assert_eq!(rows.left_for_display.text, "Unavailable");
        assert_eq!(dv_metadata_rows(&EnhancementRoute::Other, None), None);
    }

    #[test]
    fn the_owner_wire_round_trips_and_refuses_out_of_shape_rows() {
        let rows = DvProcessingRows {
            dv_metadata: dv_metadata_rows(
                &processed(Some(1000), CreativeTrims::All),
                Some(&cm40()),
            )
            .expect("DV"),
            display_target: display_target_rows(&DisplayTargetFacts {
                reported_peak_nits: Some(1000),
                mapped_nits: Some(1000),
                ..Default::default()
            }),
        };
        let bytes = serde_json::to_vec(&rows).expect("serialize");
        assert_eq!(
            DvProcessingRows::from_owner_wire(&bytes),
            Some(rows.clone())
        );
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        value["dv_metadata"]["applied"]["text"] = serde_json::json!("");
        assert_eq!(
            DvProcessingRows::from_owner_wire(&serde_json::to_vec(&value).expect("json")),
            None
        );
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        value["extra"] = serde_json::json!(1);
        assert_eq!(
            DvProcessingRows::from_owner_wire(&serde_json::to_vec(&value).expect("json")),
            None
        );
        assert_eq!(DvProcessingRows::from_owner_wire(&[b' '; 5000]), None);
    }
}
