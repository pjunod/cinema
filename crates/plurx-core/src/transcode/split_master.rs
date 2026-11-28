//! The one writer of every split HLS master (P62 S2).
//!
//! A split presentation is one or more video variants plus at most one
//! alternate audio group of separately published renditions. Every layout —
//! the P49 split copy family, the continuous-quality family and the native
//! Safari split — describes itself as a [`SplitPresentation`] and renders
//! through [`SplitPresentation::master_text`], so one set of rules holds for
//! all of them. The rules are in
//! `docs/streaming/SPLIT-HLS-RENDITION-CONTRACT.md` §2; each one is enforced
//! here, and a presentation that breaks one is refused rather than written.
//!
//! Only proved facts become tags. An attribute the caller cannot prove is
//! either absent by definition (no `AVERAGE-BANDWIDTH` without an average) or
//! a refusal (an HDR source with no proved range), never a guess.

use std::collections::BTreeSet;
use std::fmt::{self, Write as _};

/// The `VIDEO-RANGE` of a variant. Absence means SDR in HLS, so a split
/// master always writes the range it proved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoRange {
    Sdr,
    Pq,
    Hlg,
}

impl VideoRange {
    /// The range an ISO/IEC 23091-2 transfer characteristic proves, if any.
    /// Unspecified (2) and reserved values prove nothing.
    pub fn from_transfer(transfer: u16) -> Option<Self> {
        match transfer {
            1 | 4 | 5 | 6 | 7 | 8 | 13 | 14 | 15 => Some(Self::Sdr),
            16 => Some(Self::Pq),
            18 => Some(Self::Hlg),
            _ => None,
        }
    }

    /// The range of an AVC rendition. An unspecified AVC transfer is SDR: no
    /// consumer HDR delivery uses AVC, and every AVC split admitted today is
    /// an 8-bit copy or an 8-bit encode.
    pub fn of_avc(transfer: u16) -> Self {
        Self::from_transfer(transfer).unwrap_or(Self::Sdr)
    }

    fn attribute(self) -> &'static str {
        match self {
            Self::Sdr => "SDR",
            Self::Pq => "PQ",
            Self::Hlg => "HLG",
        }
    }
}

/// A delivery budget in bits per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplitBudget {
    pub peak_bps: u64,
    pub average_bps: Option<u64>,
}

impl SplitBudget {
    fn valid(self) -> bool {
        self.peak_bps > 0
            && self
                .average_bps
                .is_none_or(|average| average > 0 && average <= self.peak_bps)
    }
}

/// How the presentation proves its bandwidth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SplitBandwidth {
    /// Every video variant and every audio rendition carries its own budget.
    PerRole,
    /// One budget for the whole presentation. The native split has only the
    /// muxed copy's budget while it projects out of that copy (S5 removes it).
    /// Only a single-variant presentation can use it.
    Combined(SplitBudget),
}

/// One video variant.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitVideo {
    /// 64 lowercase hex; the child URI is `video/{rendition_id}/index.m3u8`.
    pub rendition_id: String,
    /// RFC 6381 codec of the verified init, exactly as the client must read it.
    pub codec: String,
    pub supplemental_codecs: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub frame_rate: Option<f64>,
    pub video_range: VideoRange,
    /// Required under [`SplitBandwidth::PerRole`], absent under `Combined`.
    pub budget: Option<SplitBudget>,
}

/// One alternate audio rendition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitAudio {
    /// 64 lowercase hex; the child URI is `audio/{rendition_id}/index.m3u8`.
    pub rendition_id: String,
    /// The source audio stream index. It becomes `STABLE-RENDITION-ID="a{n}"`,
    /// which the web player switches by in place.
    pub source_index: i64,
    /// The stream title, if any. NAME falls back to the language, then
    /// `Track N`.
    pub title: Option<String>,
    /// The source language tag, normalised to BCP 47 on write.
    pub language: Option<String>,
    pub default: bool,
    /// Proved by the init; written as `CHANNELS`.
    pub channels: Option<u16>,
    /// RFC 6381 codec of the verified init.
    pub codec: String,
    /// Required under [`SplitBandwidth::PerRole`], absent under `Combined`.
    pub budget: Option<SplitBudget>,
}

/// What the source says about one audio stream, for a layout whose
/// rendition facts do not carry it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitAudioSource {
    pub source_index: i64,
    pub title: Option<String>,
    pub language: Option<String>,
}

/// Everything a split master says.
#[derive(Debug, Clone, PartialEq)]
pub struct SplitPresentation {
    pub variants: Vec<SplitVideo>,
    pub audio: Vec<SplitAudio>,
    pub bandwidth: SplitBandwidth,
    /// Rendered subtitle `EXT-X-MEDIA` lines of group `subs`, from the one
    /// subtitle writer. Empty when the presentation has none.
    pub subtitles: String,
    /// Only a layout that proves every child starts each segment on a sync
    /// sample may set this. No layout proves it today: copy video can cut on
    /// its ceiling, and init verification cannot attest every fragment join.
    pub independent_segments: bool,
}

/// Why a presentation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitMasterError(pub &'static str);

impl fmt::Display for SplitMasterError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "split master refused: {}", self.0)
    }
}

impl std::error::Error for SplitMasterError {}

const SUBTITLE_PREFIX: &str = "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",";

/// HLS quoted strings cannot hold quotes or line breaks.
fn quoted(value: &str) -> String {
    value
        .chars()
        .map(|c| match c {
            '"' => '\'',
            '\r' | '\n' => ' ',
            c => c,
        })
        .collect()
}

fn hex_id(id: &str) -> bool {
    id.len() == 64 && id.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn codec_token(codec: &str) -> bool {
    !codec.is_empty()
        && codec
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}

/// The BCP 47 tag written as `LANGUAGE`, or none when the source names none.
fn language(raw: Option<&str>) -> Option<String> {
    let tag: String = crate::tracks::bcp47_tag(raw)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect();
    (!tag.is_empty() && tag != "und").then_some(tag)
}

impl SplitPresentation {
    /// NAMEs in source order, unique within the group: the title, else the
    /// language, else `Track N`; a repeat gains ` (N)`.
    fn audio_names(audio: &[&SplitAudio]) -> Vec<String> {
        let mut used = BTreeSet::new();
        audio
            .iter()
            .enumerate()
            .map(|(ordinal, track)| {
                let base = track
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                    .map(quoted)
                    .or_else(|| {
                        track
                            .language
                            .as_deref()
                            .map(str::trim)
                            .filter(|lang| !lang.is_empty())
                            .map(quoted)
                    })
                    .unwrap_or_else(|| format!("Track {}", ordinal + 1));
                let mut name = base.clone();
                let mut copy = 2;
                while !used.insert(name.clone()) {
                    name = format!("{base} ({copy})");
                    copy += 1;
                }
                name
            })
            .collect()
    }

    fn validate(&self) -> Result<(), SplitMasterError> {
        if self.variants.is_empty() {
            return Err(SplitMasterError("no video variant"));
        }
        let mut ids = BTreeSet::new();
        for id in self
            .variants
            .iter()
            .map(|video| video.rendition_id.as_str())
            .chain(self.audio.iter().map(|audio| audio.rendition_id.as_str()))
        {
            if !hex_id(id) || !ids.insert(id) {
                return Err(SplitMasterError(
                    "rendition ids must be distinct 64-digit lowercase hex",
                ));
            }
        }
        let mut stable = BTreeSet::new();
        if self
            .audio
            .iter()
            .any(|audio| audio.source_index < 0 || !stable.insert(audio.source_index))
        {
            return Err(SplitMasterError("audio source indexes must be distinct"));
        }
        if !self.audio.is_empty() && self.audio.iter().filter(|audio| audio.default).count() != 1 {
            return Err(SplitMasterError("an audio group needs exactly one default"));
        }
        if self.variants.iter().any(|video| {
            !codec_token(&video.codec)
                || video
                    .supplemental_codecs
                    .as_deref()
                    // A supplemental entry may carry a compatibility brand
                    // after a slash (`dvhe.08.06/db1p`).
                    .is_some_and(|codecs| {
                        codecs.split(',').any(|entry| {
                            let mut parts = entry.splitn(2, '/');
                            !parts.next().is_some_and(codec_token)
                                || parts.next().is_some_and(|brand| !codec_token(brand))
                        })
                    })
                || video
                    .frame_rate
                    .is_some_and(|rate| !rate.is_finite() || rate <= 0.0)
                || video.width.is_some() != video.height.is_some()
                || video.width.is_some_and(|w| w == 0)
                || video.height.is_some_and(|h| h == 0)
        }) || self.audio.iter().any(|audio| {
            !codec_token(&audio.codec) || audio.channels.is_some_and(|channels| channels == 0)
        }) {
            return Err(SplitMasterError(
                "a codec, raster or rate is not a proved value",
            ));
        }
        let budgets_present = self
            .variants
            .iter()
            .map(|video| video.budget)
            .chain(self.audio.iter().map(|audio| audio.budget));
        match &self.bandwidth {
            SplitBandwidth::PerRole => {
                for budget in budgets_present {
                    if !budget.is_some_and(SplitBudget::valid) {
                        return Err(SplitMasterError("a rendition has no valid budget"));
                    }
                }
            }
            SplitBandwidth::Combined(budget) => {
                if self.variants.len() != 1 || !budget.valid() {
                    return Err(SplitMasterError(
                        "a combined budget describes exactly one variant",
                    ));
                }
                for each in budgets_present {
                    if each.is_some() {
                        return Err(SplitMasterError("a combined budget excludes role budgets"));
                    }
                }
            }
        }
        if !self.subtitles.is_empty()
            && (!self.subtitles.ends_with('\n')
                || self
                    .subtitles
                    .lines()
                    .any(|line| !line.starts_with(SUBTITLE_PREFIX)))
        {
            return Err(SplitMasterError("subtitle lines are not one subs group"));
        }
        Ok(())
    }

    fn variant_budget(&self, video: &SplitVideo) -> Result<SplitBudget, SplitMasterError> {
        let overflow = || SplitMasterError("bandwidth overflows");
        match &self.bandwidth {
            SplitBandwidth::Combined(budget) => Ok(*budget),
            SplitBandwidth::PerRole => {
                let video = video
                    .budget
                    .ok_or(SplitMasterError("video budget missing"))?;
                let audio: Vec<SplitBudget> =
                    self.audio.iter().filter_map(|audio| audio.budget).collect();
                let peak = video
                    .peak_bps
                    .checked_add(audio.iter().map(|b| b.peak_bps).max().unwrap_or(0))
                    .ok_or_else(overflow)?;
                let average = match video.average_bps {
                    Some(video_average) if audio.iter().all(|b| b.average_bps.is_some()) => {
                        let audio_average = audio
                            .iter()
                            .filter_map(|b| b.average_bps)
                            .max()
                            .unwrap_or(0);
                        Some(
                            video_average
                                .checked_add(audio_average)
                                .ok_or_else(overflow)?,
                        )
                    }
                    _ => None,
                };
                Ok(SplitBudget {
                    peak_bps: peak,
                    average_bps: average,
                })
            }
        }
    }

    /// Render the master, or refuse a presentation that breaks a rule.
    pub fn master_text(&self) -> Result<String, SplitMasterError> {
        self.validate()?;
        let mut audio: Vec<&SplitAudio> = self.audio.iter().collect();
        audio.sort_by_key(|track| track.source_index);
        let names = Self::audio_names(&audio);
        let mut variants: Vec<(&SplitVideo, SplitBudget)> = self
            .variants
            .iter()
            .map(|video| self.variant_budget(video).map(|budget| (video, budget)))
            .collect::<Result<_, _>>()?;
        variants.sort_by_key(|(video, budget)| (video.height, video.width, budget.peak_bps));
        let supplemental = self
            .variants
            .iter()
            .any(|video| video.supplemental_codecs.is_some());

        let mut out = format!(
            "#EXTM3U\n#EXT-X-VERSION:{}\n",
            if supplemental { 10 } else { 7 }
        );
        if self.independent_segments {
            out.push_str("#EXT-X-INDEPENDENT-SEGMENTS\n");
        }
        let default_language = audio
            .iter()
            .find(|track| track.default)
            .map(|track| language(track.language.as_deref()));
        let mut autoselect_languages = BTreeSet::new();
        autoselect_languages.extend(default_language);
        for (track, name) in audio.iter().zip(&names) {
            let _ = write!(
                out,
                "#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"{name}\""
            );
            let track_language = language(track.language.as_deref());
            if let Some(language) = &track_language {
                let _ = write!(out, ",LANGUAGE=\"{language}\"");
            }
            let autoselect = track.default || autoselect_languages.insert(track_language);
            let _ = write!(
                out,
                ",DEFAULT={},AUTOSELECT={}",
                if track.default { "YES" } else { "NO" },
                if autoselect { "YES" } else { "NO" },
            );
            if let Some(channels) = track.channels {
                let _ = write!(out, ",CHANNELS=\"{channels}\"");
            }
            let _ = writeln!(
                out,
                ",STABLE-RENDITION-ID=\"a{}\",URI=\"audio/{}/index.m3u8\"",
                track.source_index, track.rendition_id
            );
        }
        out.push_str(&self.subtitles);
        let mut audio_codecs: Vec<&str> = Vec::new();
        for track in &audio {
            if !audio_codecs.contains(&track.codec.as_str()) {
                audio_codecs.push(&track.codec);
            }
        }
        for (video, budget) in variants {
            let _ = write!(out, "#EXT-X-STREAM-INF:BANDWIDTH={}", budget.peak_bps);
            if let Some(average) = budget.average_bps {
                let _ = write!(out, ",AVERAGE-BANDWIDTH={average}");
            }
            if let (Some(width), Some(height)) = (video.width, video.height) {
                let _ = write!(out, ",RESOLUTION={width}x{height}");
            }
            if let Some(rate) = video.frame_rate {
                let _ = write!(out, ",FRAME-RATE={rate:.3}");
            }
            let mut codecs = vec![video.codec.as_str()];
            codecs.extend(audio_codecs.iter().copied());
            let _ = write!(out, ",CODECS=\"{}\"", codecs.join(","));
            if let Some(supplemental) = &video.supplemental_codecs {
                let _ = write!(out, ",SUPPLEMENTAL-CODECS=\"{supplemental}\"");
            }
            let _ = write!(out, ",VIDEO-RANGE={}", video.video_range.attribute());
            if !audio.is_empty() {
                out.push_str(",AUDIO=\"audio\"");
            }
            if !self.subtitles.is_empty() {
                out.push_str(",SUBTITLES=\"subs\"");
            }
            let _ = writeln!(out, "\nvideo/{}/index.m3u8", video.rendition_id);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(c: char) -> String {
        std::iter::repeat_n(c, 64).collect()
    }

    fn video(c: char, height: u32, peak: u64) -> SplitVideo {
        SplitVideo {
            rendition_id: id(c),
            codec: "avc1.640028".into(),
            supplemental_codecs: None,
            width: Some(height * 16 / 9),
            height: Some(height),
            frame_rate: Some(24_000.0 / 1001.0),
            video_range: VideoRange::Sdr,
            budget: Some(SplitBudget {
                peak_bps: peak,
                average_bps: None,
            }),
        }
    }

    fn audio(c: char, index: i64, language: &str, default: bool) -> SplitAudio {
        SplitAudio {
            rendition_id: id(c),
            source_index: index,
            title: None,
            language: Some(language.into()),
            default,
            channels: Some(2),
            codec: "mp4a.40.2".into(),
            budget: Some(SplitBudget {
                peak_bps: 160_000,
                average_bps: None,
            }),
        }
    }

    fn presentation() -> SplitPresentation {
        SplitPresentation {
            variants: vec![video('b', 1080, 8_000_000)],
            audio: vec![audio('d', 2, "fre", false), audio('c', 1, "eng", true)],
            bandwidth: SplitBandwidth::PerRole,
            subtitles: String::new(),
            independent_segments: false,
        }
    }

    #[test]
    fn split_master_writes_one_audio_group_in_source_order() {
        let master = presentation().master_text().expect("master");
        assert_eq!(
            master,
            format!(
                "#EXTM3U\n#EXT-X-VERSION:7\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"eng\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES,CHANNELS=\"2\",STABLE-RENDITION-ID=\"a1\",URI=\"audio/{c}/index.m3u8\"\n\
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"audio\",NAME=\"fre\",LANGUAGE=\"fr\",DEFAULT=NO,AUTOSELECT=YES,CHANNELS=\"2\",STABLE-RENDITION-ID=\"a2\",URI=\"audio/{d}/index.m3u8\"\n\
#EXT-X-STREAM-INF:BANDWIDTH=8160000,RESOLUTION=1920x1080,FRAME-RATE=23.976,CODECS=\"avc1.640028,mp4a.40.2\",VIDEO-RANGE=SDR,AUDIO=\"audio\"\n\
video/{b}/index.m3u8\n",
                b = id('b'),
                c = id('c'),
                d = id('d'),
            )
        );
        assert!(!master.contains("INDEPENDENT-SEGMENTS"));
    }

    #[test]
    fn split_master_names_are_unique_within_the_group() {
        let mut p = presentation();
        p.audio = vec![
            audio('c', 1, "eng", true),
            audio('d', 2, "eng", false),
            SplitAudio {
                language: None,
                ..audio('e', 3, "", false)
            },
        ];
        let master = p.master_text().expect("master");
        assert!(master.contains("NAME=\"eng\",LANGUAGE=\"en\""));
        assert!(master.contains("NAME=\"eng (2)\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=NO"));
        assert!(master.contains("NAME=\"Track 3\",DEFAULT=NO"));
        let titled = SplitAudio {
            title: Some("Commentary \"Dir\"".into()),
            ..audio('f', 4, "eng", false)
        };
        p.audio.push(titled);
        assert!(p
            .master_text()
            .expect("master")
            .contains("NAME=\"Commentary 'Dir'\""));
    }

    #[test]
    fn split_master_autoselects_the_default_over_an_earlier_track_in_the_same_language() {
        let mut p = presentation();
        p.audio = vec![
            audio('c', 1, "eng", false),
            audio('d', 2, "en", true),
            audio('e', 3, "fre", false),
            audio('f', 4, "fr", false),
        ];
        let master = p.master_text().expect("valid alternate soundtrack master");
        assert!(master.contains("NAME=\"eng\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=NO"));
        assert!(master.contains("NAME=\"en\",LANGUAGE=\"en\",DEFAULT=YES,AUTOSELECT=YES"));
        assert!(master.contains("NAME=\"fre\",LANGUAGE=\"fr\",DEFAULT=NO,AUTOSELECT=YES"));
        assert!(master.contains("NAME=\"fr\",LANGUAGE=\"fr\",DEFAULT=NO,AUTOSELECT=NO"));
    }

    #[test]
    fn split_master_without_audio_has_no_group() {
        let mut p = presentation();
        p.audio.clear();
        p.variants.push(video('a', 720, 3_000_000));
        let master = p.master_text().expect("master");
        assert!(!master.contains("TYPE=AUDIO"));
        assert!(!master.contains("AUDIO=\""));
        assert!(master.find("1280x720").expect("low") < master.find("1920x1080").expect("high"));
        assert!(master.contains("CODECS=\"avc1.640028\","));
    }

    #[test]
    fn split_master_refuses_unproved_or_conflicting_presentations() {
        let refused = |edit: &dyn Fn(&mut SplitPresentation)| {
            let mut p = presentation();
            edit(&mut p);
            p.master_text().is_err()
        };
        assert!(refused(&|p| p.variants.clear()));
        assert!(refused(&|p| p.audio[0].default = true));
        assert!(refused(&|p| p.audio[1].default = false));
        assert!(refused(&|p| p.audio[0].source_index = 1));
        assert!(refused(&|p| p.audio[0].rendition_id = id('b')));
        assert!(refused(&|p| p.audio[0].rendition_id = "source-abc".into()));
        assert!(refused(&|p| p.audio[0].budget = None));
        assert!(refused(&|p| p.variants[0].codec = "avc1\"x".into()));
        assert!(refused(&|p| p.variants[0].frame_rate = Some(f64::NAN)));
        assert!(refused(&|p| p.variants[0].width = None));
        assert!(refused(&|p| p.audio[0].channels = Some(0)));
        assert!(refused(
            &|p| p.subtitles = "#EXT-X-MEDIA:TYPE=AUDIO\n".into()
        ));
        assert!(refused(&|p| p.variants[0].budget = Some(SplitBudget {
            peak_bps: 1,
            average_bps: Some(2)
        })));
        assert!(refused(&|p| p.variants[0].budget = Some(SplitBudget {
            peak_bps: u64::MAX,
            average_bps: None
        })));
        assert!(refused(&|p| p.bandwidth =
            SplitBandwidth::Combined(SplitBudget {
                peak_bps: 9_000_000,
                average_bps: None
            })));
    }

    #[test]
    fn split_master_combined_budget_describes_the_native_projection() {
        let p = SplitPresentation {
            variants: vec![SplitVideo {
                codec: "hev1.2.4.L153.B0".into(),
                supplemental_codecs: Some("dvhe.08.06/db1p".into()),
                video_range: VideoRange::Pq,
                budget: None,
                ..video('b', 2160, 0)
            }],
            audio: vec![SplitAudio {
                budget: None,
                channels: Some(6),
                ..audio('c', 1, "eng", true)
            }],
            bandwidth: SplitBandwidth::Combined(SplitBudget {
                peak_bps: 40_000_000,
                average_bps: Some(25_000_000),
            }),
            subtitles: "#EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",LANGUAGE=\"en\",DEFAULT=NO,AUTOSELECT=YES,FORCED=NO,URI=\"subs/3/index.m3u8\"\n".into(),
            independent_segments: false,
        };
        let master = p.master_text().expect("master");
        assert!(master.starts_with("#EXTM3U\n#EXT-X-VERSION:10\n"));
        assert!(master.contains(
            "BANDWIDTH=40000000,AVERAGE-BANDWIDTH=25000000,RESOLUTION=3840x2160,FRAME-RATE=23.976,CODECS=\"hev1.2.4.L153.B0,mp4a.40.2\",SUPPLEMENTAL-CODECS=\"dvhe.08.06/db1p\",VIDEO-RANGE=PQ,AUDIO=\"audio\",SUBTITLES=\"subs\"\n"
        ));
        assert!(master.contains("CHANNELS=\"6\""));
        assert!(
            master.find("TYPE=SUBTITLES").expect("subs")
                < master.find("#EXT-X-STREAM-INF").expect("variant")
        );
    }

    #[test]
    fn split_master_average_needs_every_term() {
        let mut p = presentation();
        p.variants[0].budget = Some(SplitBudget {
            peak_bps: 8_000_000,
            average_bps: Some(5_000_000),
        });
        assert!(!p
            .master_text()
            .expect("master")
            .contains("AVERAGE-BANDWIDTH"));
        for track in &mut p.audio {
            track.budget = Some(SplitBudget {
                peak_bps: 160_000,
                average_bps: Some(128_000),
            });
        }
        assert!(p
            .master_text()
            .expect("master")
            .contains("BANDWIDTH=8160000,AVERAGE-BANDWIDTH=5128000,"));
    }

    #[test]
    fn video_range_comes_only_from_a_proved_transfer() {
        assert_eq!(VideoRange::from_transfer(16), Some(VideoRange::Pq));
        assert_eq!(VideoRange::from_transfer(18), Some(VideoRange::Hlg));
        assert_eq!(VideoRange::from_transfer(1), Some(VideoRange::Sdr));
        assert_eq!(VideoRange::from_transfer(2), None);
        assert_eq!(VideoRange::of_avc(2), VideoRange::Sdr);
        assert_eq!(VideoRange::of_avc(16), VideoRange::Pq);
    }
}
