//! Frozen rate and coded-level budget for completed HDR enhancements.
//! A decoder/link ceiling constrains the peak, never the selected target.
mod bitstream;
pub use bitstream::validate_encoded_enhancement_envelope;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnhancementRateProvenance {
    HeldVideoStream,
    ContainerAverageVideoUpperBound,
    SizeDurationVideoUpperBound,
    ExplicitEncode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EnhancementRateEnvelope {
    target_kbps: u32,
    peak_kbps: u32,
    buffer_kbits: u32,
    hevc_level_idc: u8,
    dv_level: Option<u8>,
    provenance: EnhancementRateProvenance,
    bounded_peak: bool,
    bounded_buffer: bool,
    source_preference: bool,
}

/// Only independently observed declarations belong here. Missing declarations
/// are unknown, not an invented device maximum or a reason to reduce video.
#[derive(Debug, Clone, Copy, Default)]
pub struct EnhancementRateLimits {
    pub video_peak_bps: Option<u64>,
    pub buffer_bits: Option<u64>,
    /// A source's already-signalled level is a preference, not a client cap.
    pub preferred_hevc_level: Option<&'static str>,
    pub preferred_dv_level: Option<u8>,
}

#[derive(Clone, Copy)]
struct HevcLevel {
    name: &'static str,
    idc: u8,
    picture: u64,
    samples: u64,
    high_kbps: u32,
    high_cpb_kbits: u32,
}

// ITU-T H.265 Table A.8, conservative Main10 High-tier BR/CPB values.
const HEVC: [HevcLevel; 8] = [
    HevcLevel {
        name: "4",
        idc: 120,
        picture: 2_228_224,
        samples: 66_846_720,
        high_kbps: 30_000,
        high_cpb_kbits: 30_000,
    },
    HevcLevel {
        name: "4.1",
        idc: 123,
        picture: 2_228_224,
        samples: 133_693_440,
        high_kbps: 50_000,
        high_cpb_kbits: 50_000,
    },
    HevcLevel {
        name: "5",
        idc: 150,
        picture: 8_912_896,
        samples: 267_386_880,
        high_kbps: 100_000,
        high_cpb_kbits: 100_000,
    },
    HevcLevel {
        name: "5.1",
        idc: 153,
        picture: 8_912_896,
        samples: 534_773_760,
        high_kbps: 160_000,
        high_cpb_kbits: 160_000,
    },
    HevcLevel {
        name: "5.2",
        idc: 156,
        picture: 8_912_896,
        samples: 1_069_547_520,
        high_kbps: 240_000,
        high_cpb_kbits: 240_000,
    },
    HevcLevel {
        name: "6",
        idc: 180,
        picture: 35_651_584,
        samples: 1_069_547_520,
        high_kbps: 240_000,
        high_cpb_kbits: 240_000,
    },
    HevcLevel {
        name: "6.1",
        idc: 183,
        picture: 35_651_584,
        samples: 2_139_095_040,
        high_kbps: 480_000,
        high_cpb_kbits: 480_000,
    },
    HevcLevel {
        name: "6.2",
        idc: 186,
        picture: 35_651_584,
        samples: 4_278_190_080,
        high_kbps: 800_000,
        high_cpb_kbits: 800_000,
    },
];

// FFmpeg 8.1.3 dovi_rpuenc.c dv_levels, High-tier bitrate in kb/s.
const DV: [(u64, u32, u32); 13] = [
    (1280 * 720 * 24, 1280, 50_000),
    (1280 * 720 * 30, 1280, 50_000),
    (1920 * 1080 * 24, 1920, 70_000),
    (1920 * 1080 * 30, 2560, 70_000),
    (1920 * 1080 * 60, 3840, 70_000),
    (3840 * 2160 * 24, 3840, 130_000),
    (3840 * 2160 * 30, 3840, 130_000),
    (3840 * 2160 * 48, 3840, 130_000),
    (3840 * 2160 * 60, 3840, 130_000),
    (3840 * 2160 * 120, 3840, 240_000),
    (3840 * 2160 * 120, 7680, 240_000),
    (7680 * 4320 * 60, 7680, 450_000),
    (7680 * 4320 * 120, 7680, 800_000),
];

/// Rate from one selected physical/probe stream, never another track.
pub fn enhancement_stream_rate(document: &serde_json::Value, absolute_stream: u32) -> Option<u64> {
    let streams = document.get("streams")?.as_array()?;
    // A BL-only track is not the combined FEL video budget. Without complete
    // independently identified layer rates use the container upper bound.
    if streams
        .iter()
        .filter(|stream| {
            stream.get("codec_name").and_then(serde_json::Value::as_str) == Some("hevc")
        })
        .count()
        > 1
    {
        return None;
    }
    streams
        .iter()
        .find(|stream| {
            stream.get("index").and_then(serde_json::Value::as_u64)
                == Some(u64::from(absolute_stream))
        })?
        .get("bit_rate")
        .and_then(|value| {
            value
                .as_str()
                .and_then(|v| v.parse().ok())
                .or_else(|| value.as_u64())
        })
        .filter(|v| *v > 0)
}

pub fn enhancement_source_budget(
    source: &crate::domain::MediaFile,
    held_video_bps: Option<u64>,
) -> Result<(u32, EnhancementRateProvenance), &'static str> {
    let (kbps, provenance) = if let Some(bps) = held_video_bps.filter(|v| *v > 0) {
        (
            u128::from(bps).div_ceil(1000),
            EnhancementRateProvenance::HeldVideoStream,
        )
    } else if let Some(bps) = source.bitrate.filter(|v| *v > 0) {
        (
            u128::try_from(bps)
                .map_err(|_| "source video budget overflow")?
                .div_ceil(1000),
            EnhancementRateProvenance::ContainerAverageVideoUpperBound,
        )
    } else if let Some(ms) = source.duration_ms.filter(|v| *v > 0 && source.size > 0) {
        (
            u128::try_from(source.size)
                .map_err(|_| "source size budget overflow")?
                .checked_mul(8)
                .ok_or("source size budget overflow")?
                .div_ceil(u128::try_from(ms).map_err(|_| "source duration budget overflow")?),
            EnhancementRateProvenance::SizeDurationVideoUpperBound,
        )
    } else {
        return Err("enhancement source video budget unresolved: no positive held stream/container rate or size-duration measurement");
    };
    let target =
        u32::try_from(kbps).map_err(|_| "enhancement source video budget exceeds encoder range")?;
    if target == 0 {
        return Err("enhancement source video budget is zero");
    }
    Ok((target, provenance))
}

impl EnhancementRateEnvelope {
    /// Whether a level name belongs to the envelope's Main10 High-tier table.
    /// Raster, rate, buffer and client limits still require their own validation.
    pub fn supports_hevc_level_name(name: &str) -> bool {
        HEVC.iter().any(|level| level.name == name)
    }

    pub fn resolve(
        target_kbps: u32,
        provenance: EnhancementRateProvenance,
        width: u32,
        height: u32,
        rate: (u32, u32),
        profile81: bool,
        limits: EnhancementRateLimits,
    ) -> Result<Self, &'static str> {
        if target_kbps == 0 || width == 0 || height == 0 || rate.0 == 0 || rate.1 == 0 {
            return Err("enhancement rate envelope has no positive budget/raster/cadence");
        }
        let picture = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or("enhancement raster overflow")?;
        let samples = picture
            .checked_mul(u64::from(rate.0))
            .ok_or("enhancement cadence overflow")?
            .div_ceil(u64::from(rate.1));
        let preferred_peak = target_kbps
            .checked_mul(3)
            .ok_or("enhancement peak overflow")?
            / 2;
        let preferred_buffer = target_kbps
            .checked_mul(2)
            .ok_or("enhancement CPB overflow")?;
        let mut peak = limits.video_peak_bps.map_or(preferred_peak, |v| {
            u32::try_from(v / 1000)
                .unwrap_or(u32::MAX)
                .min(preferred_peak)
        });
        let mut buffer = limits.buffer_bits.map_or(preferred_buffer, |v| {
            u32::try_from(v / 1000)
                .unwrap_or(u32::MAX)
                .min(preferred_buffer)
        });
        if peak < target_kbps {
            return Err("enhancement source budget conflicts with declared decoder/delivery peak; target cannot be lowered");
        }
        if buffer == 0 {
            return Err("enhancement CPB conflicts with declared buffer limit");
        }
        let mut source_preference = false;
        if let Some(level) = limits
            .preferred_hevc_level
            .and_then(|name| HEVC.iter().find(|level| level.name == name))
            .filter(|level| {
                picture <= level.picture
                    && u64::from(width) * u64::from(width) <= 8 * level.picture
                    && u64::from(height) * u64::from(height) <= 8 * level.picture
                    && samples <= level.samples
                    && target_kbps <= level.high_kbps
            })
        {
            peak = peak.min(level.high_kbps);
            buffer = buffer.min(level.high_cpb_kbits);
            source_preference = true;
        }
        if profile81 {
            if let Some(level) = limits
                .preferred_dv_level
                .and_then(|v| v.checked_sub(1))
                .and_then(|v| DV.get(usize::from(v)))
                .filter(|(pps, max_width, bps)| {
                    samples <= *pps && width <= *max_width && target_kbps <= *bps
                })
            {
                peak = peak.min(level.2);
                source_preference = true;
            }
        }
        let admits_geometry = |level: &HevcLevel| {
            picture <= level.picture
                && u64::from(width) * u64::from(width) <= 8 * level.picture
                && u64::from(height) * u64::from(height) <= 8 * level.picture
                && samples <= level.samples
        };
        let level = if let Some(level) = HEVC.iter().find(|level| {
            admits_geometry(level) && peak <= level.high_kbps && buffer <= level.high_cpb_kbits
        }) {
            level
        } else {
            // The 1.5x peak and 2x CPB are preferences. At the highest legal
            // coded level they must not become an implicit source-rate cap.
            // Declared upper bounds were applied above and are never raised.
            let level = HEVC
                .iter()
                .rev()
                .find(|level| admits_geometry(level) && target_kbps <= level.high_kbps)
                .ok_or("no legal HEVC High-tier level admits the unchanged enhancement target")?;
            peak = peak.min(level.high_kbps);
            buffer = buffer.min(level.high_cpb_kbits);
            level
        };
        let dv_level = if profile81 {
            Some(
                u8::try_from(
                    DV.iter()
                        .position(|(pps, max_width, bps)| {
                            samples <= *pps && width <= *max_width && peak <= *bps
                        })
                        .ok_or(
                            "no legal Dolby Vision level admits the unchanged enhancement peak",
                        )?
                        + 1,
                )
                .map_err(|_| "Dolby Vision level overflow")?,
            )
        } else {
            None
        };
        Ok(Self {
            target_kbps,
            peak_kbps: peak,
            buffer_kbits: buffer,
            hevc_level_idc: level.idc,
            dv_level,
            provenance,
            bounded_peak: peak != preferred_peak,
            bounded_buffer: buffer != preferred_buffer,
            source_preference,
        })
    }
    pub fn validate_picture(
        self,
        width: u32,
        height: u32,
        rate: (u32, u32),
    ) -> Result<(), &'static str> {
        if self.target_kbps == 0
            || self.peak_kbps < self.target_kbps
            || self.buffer_kbits == 0
            || width == 0
            || height == 0
            || rate.0 == 0
            || rate.1 == 0
        {
            return Err("enhancement picture geometry/cadence absent");
        }
        let picture = u64::from(width)
            .checked_mul(u64::from(height))
            .ok_or("enhancement picture overflow")?;
        let samples = picture
            .checked_mul(u64::from(rate.0))
            .ok_or("enhancement sample rate overflow")?
            .div_ceil(u64::from(rate.1));
        let level = HEVC
            .iter()
            .find(|level| level.idc == self.hevc_level_idc)
            .ok_or("enhancement HEVC level unknown")?;
        if picture > level.picture
            || u64::from(width) * u64::from(width) > 8 * level.picture
            || u64::from(height) * u64::from(height) > 8 * level.picture
            || samples > level.samples
            || self.peak_kbps > level.high_kbps
            || self.buffer_kbits > level.high_cpb_kbits
        {
            return Err("frozen enhancement envelope cannot admit this picture");
        }
        if let Some(dv) = self.dv_level {
            let (pps, width_limit, peak) = DV
                .get(usize::from(
                    dv.checked_sub(1).ok_or("enhancement DV level unknown")?,
                ))
                .ok_or("enhancement DV level unknown")?;
            if samples > *pps || width > *width_limit || self.peak_kbps > *peak {
                return Err("frozen Dolby Vision envelope cannot admit this picture");
            }
        }
        Ok(())
    }
    pub fn target_kbps(self) -> u32 {
        self.target_kbps
    }
    pub fn peak_kbps(self) -> u32 {
        self.peak_kbps
    }
    pub fn peak_bps(self) -> u64 {
        u64::from(self.peak_kbps) * 1000
    }
    pub fn buffer_kbits(self) -> u32 {
        self.buffer_kbits
    }
    pub fn hevc_level(self) -> &'static str {
        HEVC.iter()
            .find(|level| level.idc == self.hevc_level_idc)
            .map_or("invalid", |level| level.name)
    }
    pub fn hevc_level_idc(self) -> u8 {
        self.hevc_level_idc
    }
    pub fn dv_level(self) -> Option<u8> {
        self.dv_level
    }
    pub fn provenance(self) -> EnhancementRateProvenance {
        self.provenance
    }
    pub fn identity(self) -> String {
        serde_json::to_string(&(
            "enhancement-video-envelope-v2",
            self.target_kbps,
            self.peak_kbps,
            self.buffer_kbits,
            "high",
            self.hevc_level_idc,
            self.dv_level,
        ))
        .expect("rate envelope serialization")
    }
    pub fn apply_args(self, args: &mut Vec<String>, encoder: super::Encoder) {
        for (key, value) in [
            ("-b:v", format!("{}k", self.target_kbps)),
            ("-maxrate", format!("{}k", self.peak_kbps)),
            ("-bufsize", format!("{}k", self.buffer_kbits)),
        ] {
            if let Some(index) = args.iter().position(|arg| arg == key) {
                args[index + 1] = value;
            } else {
                args.extend([key.to_owned(), value]);
            }
        }
        if encoder != super::Encoder::Software {
            if let Some(index) = args.iter().position(|arg| arg == "-level:v") {
                args[index + 1] = self.hevc_level().to_owned();
            } else {
                args.extend(["-level:v".into(), self.hevc_level().to_owned()]);
            }
        }
        if encoder == super::Encoder::Software {
            let extra = format!("high-tier=1:level-idc={}:hrd=1", self.hevc_level());
            if let Some(index) = args.iter().position(|arg| arg == "-x265-params") {
                args[index + 1].push(':');
                args[index + 1].push_str(&extra);
            } else {
                args.extend(["-x265-params".to_owned(), extra]);
            }
        } else if let Some(index) = args.iter().position(|arg| arg == "-tier:v") {
            args[index + 1] = "high".into();
        } else {
            args.extend(["-tier:v".into(), "high".into()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn envelope(
        target: u32,
        profile81: bool,
        limits: EnhancementRateLimits,
    ) -> EnhancementRateEnvelope {
        EnhancementRateEnvelope::resolve(
            target,
            EnhancementRateProvenance::ContainerAverageVideoUpperBound,
            3840,
            2160,
            (24, 1),
            profile81,
            limits,
        )
        .expect("legal envelope")
    }
    #[test]
    fn source_budget_envelope_raises_levels_instead_of_capping_video() {
        let sixty = envelope(60_000, true, Default::default());
        assert_eq!(
            (
                sixty.target_kbps(),
                sixty.peak_kbps(),
                sixty.buffer_kbits(),
                sixty.hevc_level(),
                sixty.dv_level()
            ),
            (60_000, 90_000, 120_000, "5.1", Some(6))
        );
        let ninety = envelope(90_000, true, Default::default());
        assert_eq!(
            (
                ninety.target_kbps(),
                ninety.peak_kbps(),
                ninety.buffer_kbits(),
                ninety.hevc_level(),
                ninety.dv_level()
            ),
            (90_000, 135_000, 180_000, "5.2", Some(10))
        );
        let low_raster = EnhancementRateEnvelope::resolve(
            90_000,
            EnhancementRateProvenance::HeldVideoStream,
            1920,
            1080,
            (24, 1),
            false,
            Default::default(),
        )
        .expect("raise coded level");
        assert_eq!(low_raster.hevc_level(), "5.2");
    }
    #[test]
    fn source_budget_envelope_bounds_only_preferences_at_the_highest_legal_level() {
        let resolve = |target, profile81, limits| {
            EnhancementRateEnvelope::resolve(
                target,
                EnhancementRateProvenance::HeldVideoStream,
                64,
                64,
                (24, 1),
                profile81,
                limits,
            )
        };
        let full = resolve(400_000, false, Default::default()).expect("full preferences fit");
        assert_eq!(
            (full.target_kbps(), full.peak_kbps(), full.buffer_kbits()),
            (400_000, 600_000, 800_000)
        );
        assert!(!full.bounded_peak && !full.bounded_buffer);
        let high = resolve(450_000, false, Default::default()).expect("legal source target");
        assert_eq!(
            (
                high.target_kbps(),
                high.peak_kbps(),
                high.buffer_kbits(),
                high.hevc_level()
            ),
            (450_000, 675_000, 800_000, "6.2")
        );
        assert!(!high.bounded_peak && high.bounded_buffer);
        let p81 = resolve(450_000, true, Default::default()).expect("legal P81 target");
        assert_eq!(p81.dv_level(), Some(13));
        assert!(p81.validate_picture(64, 64, (24, 1)).is_ok());

        for profile81 in [false, true] {
            let top =
                resolve(800_000, profile81, Default::default()).expect("highest legal target");
            assert_eq!(
                (
                    top.target_kbps(),
                    top.peak_kbps(),
                    top.buffer_kbits(),
                    top.hevc_level()
                ),
                (800_000, 800_000, 800_000, "6.2")
            );
            assert_eq!(top.dv_level(), profile81.then_some(13));
            assert!(top.validate_picture(64, 64, (24, 1)).is_ok());
            assert!(resolve(800_001, profile81, Default::default()).is_err());
        }
        let declared = resolve(
            450_000,
            true,
            EnhancementRateLimits {
                video_peak_bps: Some(650_000_000),
                ..Default::default()
            },
        )
        .expect("declared peak remains authoritative");
        assert_eq!(
            (
                declared.target_kbps(),
                declared.peak_kbps(),
                declared.buffer_kbits()
            ),
            (450_000, 650_000, 800_000)
        );
        assert!(resolve(
            450_000,
            false,
            EnhancementRateLimits {
                video_peak_bps: Some(449_999_999),
                ..Default::default()
            }
        )
        .is_err());
        assert!(resolve(
            450_000,
            false,
            EnhancementRateLimits {
                buffer_bits: Some(0),
                ..Default::default()
            }
        )
        .is_err());
    }

    #[test]
    fn source_budget_envelope_adapts_only_peak_cpb_and_reports_target_conflicts() {
        let limited = envelope(
            90_000,
            true,
            EnhancementRateLimits {
                video_peak_bps: Some(130_000_999),
                buffer_bits: Some(160_000_000),
                ..Default::default()
            },
        );
        assert_eq!(
            (
                limited.target_kbps(),
                limited.peak_kbps(),
                limited.buffer_kbits(),
                limited.hevc_level(),
                limited.dv_level()
            ),
            (90_000, 130_000, 160_000, "5.1", Some(6))
        );
        assert!(EnhancementRateEnvelope::resolve(
            90_000,
            EnhancementRateProvenance::HeldVideoStream,
            3840,
            2160,
            (24, 1),
            true,
            EnhancementRateLimits {
                video_peak_bps: Some(89_999_999),
                ..Default::default()
            }
        )
        .is_err());
        let preferred = envelope(
            90_000,
            true,
            EnhancementRateLimits {
                preferred_hevc_level: Some("5.1"),
                preferred_dv_level: Some(6),
                ..Default::default()
            },
        );
        assert_eq!(
            limited.identity(),
            preferred.identity(),
            "byte-equivalent reasons do not split qualification"
        );
        let promoted = envelope(
            180_000,
            true,
            EnhancementRateLimits {
                preferred_hevc_level: Some("5.1"),
                preferred_dv_level: Some(6),
                ..Default::default()
            },
        );
        assert_eq!(promoted.target_kbps(), 180_000);
        assert_eq!(promoted.dv_level(), Some(12));
    }
    #[test]
    fn source_budget_envelope_uses_checked_ceiling_and_selected_video_facts() {
        let mut file = crate::transcode::tests::file(Some("hdr10"));
        file.bitrate = Some(90_893_001);
        assert_eq!(
            enhancement_source_budget(&file, None).expect("container").0,
            90_894
        );
        assert_eq!(
            enhancement_source_budget(&file, Some(83_515_001)).expect("video"),
            (83_516, EnhancementRateProvenance::HeldVideoStream)
        );
        file.bitrate = None;
        file.size = 1;
        file.duration_ms = Some(3);
        assert_eq!(
            enhancement_source_budget(&file, None).expect("ceil size").0,
            3
        );
        file.size = 0;
        assert!(enhancement_source_budget(&file, None).is_err());
        let doc = serde_json::json!({"streams":[{"index":0,"bit_rate":"10"},{"index":2,"bit_rate":"83515001"}]});
        assert_eq!(enhancement_stream_rate(&doc, 2), Some(83_515_001));
        assert_eq!(enhancement_stream_rate(&doc, 1), None);
        let layers = serde_json::json!({"streams":[{"index":0,"codec_name":"hevc","bit_rate":"80000000"},{"index":2,"codec_name":"hevc","bit_rate":"10000000"}]});
        assert_eq!(
            enhancement_stream_rate(&layers, 0),
            None,
            "BL-only rate cannot downshift the combined source budget"
        );
    }
    #[test]
    fn source_budget_envelope_freezes_actual_encoder_flags_and_odd_peak() {
        let e = envelope(43_567, false, Default::default());
        assert_eq!(e.peak_kbps(), 65_350);
        for encoder in [
            super::super::Encoder::Vaapi,
            super::super::Encoder::Nvenc,
            super::super::Encoder::Software,
        ] {
            let mut args = Vec::new();
            e.apply_args(&mut args, encoder);
            for pair in [
                ["-b:v", "43567k"],
                ["-maxrate", "65350k"],
                ["-bufsize", "87134k"],
            ] {
                assert!(args.windows(2).any(|p| p == pair));
            }
            if encoder == super::super::Encoder::Software {
                assert!(args
                    .iter()
                    .any(|a| a.contains("high-tier=1:level-idc=5:hrd=1")));
            } else {
                assert!(args.windows(2).any(|p| p == ["-tier:v", "high"]));
            }
        }
    }
}
