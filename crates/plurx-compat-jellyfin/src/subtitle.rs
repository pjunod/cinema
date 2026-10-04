//! Bounded representation conversion for native extracted WebVTT.
//! Source times are preserved; no resume origin is added by this boundary.
use thiserror::Error;
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SubtitleError {
    #[error("subtitle exceeds the representation bound")]
    TooLarge,
    #[error("unsupported or malformed WebVTT representation")]
    Unsupported,
}
fn timestamp(value: &str) -> Result<(u64, String), SubtitleError> {
    let fields = value.split(':').collect::<Vec<_>>();
    let (hours, minutes, seconds) = match fields.as_slice() {
        [minutes, seconds] => ("00", *minutes, *seconds),
        [hours, minutes, seconds] => (*hours, *minutes, *seconds),
        _ => return Err(SubtitleError::Unsupported),
    };
    let (seconds, millis) = seconds.split_once('.').ok_or(SubtitleError::Unsupported)?;
    let integer = |value: &str| -> Result<u64, SubtitleError> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
            return Err(SubtitleError::Unsupported);
        }
        value.parse().map_err(|_| SubtitleError::Unsupported)
    };
    let (hours, minutes, seconds, millis_value) = (
        integer(hours)?,
        integer(minutes)?,
        integer(seconds)?,
        integer(millis)?,
    );
    if minutes >= 60 || seconds >= 60 || millis.len() != 3 || hours > 9999 {
        return Err(SubtitleError::Unsupported);
    }
    Ok((
        (hours * 3600 + minutes * 60 + seconds) * 1000 + millis_value,
        format!("{hours:02}:{minutes:02}:{seconds:02},{millis_value:03}"),
    ))
}
pub fn vtt_to_srt(bytes: &[u8]) -> Result<Vec<u8>, SubtitleError> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(SubtitleError::TooLarge);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SubtitleError::Unsupported)?;
    let normalized = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let (header, body) = normalized
        .split_once('\n')
        .ok_or(SubtitleError::Unsupported)?;
    if header != "WEBVTT" || !body.starts_with('\n') {
        return Err(SubtitleError::Unsupported);
    }
    let mut output = String::new();
    let mut count = 0u32;
    for block in body.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let mut lines = block.trim_matches('\n').lines();
        let first = lines.next().ok_or(SubtitleError::Unsupported)?;
        if first.starts_with("NOTE") {
            continue;
        }
        let timing = if first.contains("-->") {
            first
        } else {
            if first == "STYLE" || first == "REGION" {
                return Err(SubtitleError::Unsupported);
            }
            lines.next().ok_or(SubtitleError::Unsupported)?
        };
        let (start, end) = timing
            .split_once(" --> ")
            .ok_or(SubtitleError::Unsupported)?;
        // Positioning, ruby, voice and class rules have no equivalent in this
        // representation. Refuse instead of silently losing their semantics.
        if end.split_whitespace().count() != 1 {
            return Err(SubtitleError::Unsupported);
        }
        let (start, end) = (timestamp(start)?, timestamp(end)?);
        if start.0 > end.0 {
            return Err(SubtitleError::Unsupported);
        }
        let (start, end) = (start.1, end.1);
        let payload = lines.collect::<Vec<_>>().join("\n");
        if payload.is_empty() || payload.contains("-->") {
            return Err(SubtitleError::Unsupported);
        }
        // Only markup with the same meaning in SRT is representable. This
        // also rejects WebVTT timestamp tags rather than dropping karaoke.
        let mut remainder = payload.as_str();
        while let Some(open) = remainder.find('<') {
            if remainder[..open].contains('>') {
                return Err(SubtitleError::Unsupported);
            }
            let close = remainder[open..]
                .find('>')
                .ok_or(SubtitleError::Unsupported)?
                + open;
            if !matches!(
                &remainder[open..=close],
                "<b>" | "</b>" | "<i>" | "</i>" | "<u>" | "</u>"
            ) {
                return Err(SubtitleError::Unsupported);
            }
            remainder = &remainder[close + 1..];
        }
        if remainder.contains('>') {
            return Err(SubtitleError::Unsupported);
        }
        count = count.checked_add(1).ok_or(SubtitleError::TooLarge)?;
        if count > 100_000 {
            return Err(SubtitleError::TooLarge);
        }
        output.push_str(&format!("{count}\n{start} --> {end}\n{payload}\n\n"));
        if output.len() > 20 * 1024 * 1024 {
            return Err(SubtitleError::TooLarge);
        }
    }
    Ok(output.into_bytes())
}
/// The text formats this facade can deliver as a sidecar resource. Every one
/// is produced from the native WebVTT extraction; aliases name the same
/// writer, as in Jellyfin's `TryGetWriter`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubtitleFormat {
    Vtt,
    Srt,
}
impl SubtitleFormat {
    /// `vtt`/`webvtt` and `srt`/`subrip`, case-insensitively; nothing else.
    pub fn parse(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("vtt") || name.eq_ignore_ascii_case("webvtt") {
            Some(Self::Vtt)
        } else if name.eq_ignore_ascii_case("srt") || name.eq_ignore_ascii_case("subrip") {
            Some(Self::Srt)
        } else {
            None
        }
    }
    pub fn content_type(self) -> &'static str {
        match self {
            Self::Vtt => "text/vtt; charset=utf-8",
            Self::Srt => "application/x-subrip; charset=utf-8",
        }
    }
}

fn vtt_time(ms: u64) -> String {
    let (hours, rest) = (ms / 3_600_000, ms % 3_600_000);
    format!(
        "{hours:02}:{:02}:{:02}.{:03}",
        rest / 60_000,
        (rest % 60_000) / 1000,
        rest % 1000
    )
}

/// Jellyfin's subtitle window (`SubtitleEncoder.FilterEvents`, 10.11.11) over
/// native WebVTT: the leading run of cues that start or end before `start_ms`
/// is dropped, cues after the first one starting past `end_ms` are dropped,
/// and unless `preserve` (Jellyfin's `copyTimestamps`) every remaining cue is
/// moved earlier by `start_ms`. A zero start with no end returns the input
/// unchanged. Header, NOTE, STYLE and REGION blocks and cue settings survive.
pub fn window_vtt(
    bytes: &[u8],
    start_ms: u64,
    end_ms: Option<u64>,
    preserve: bool,
) -> Result<Vec<u8>, SubtitleError> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(SubtitleError::TooLarge);
    }
    if start_ms == 0 && end_ms.is_none() {
        return Ok(bytes.to_vec());
    }
    let text = std::str::from_utf8(bytes).map_err(|_| SubtitleError::Unsupported)?;
    let normalized = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let (header, body) = normalized
        .split_once('\n')
        .ok_or(SubtitleError::Unsupported)?;
    if header != "WEBVTT" || !body.starts_with('\n') {
        return Err(SubtitleError::Unsupported);
    }
    let mut output = String::from("WEBVTT\n\n");
    let mut leading = true;
    for block in body.split("\n\n").filter(|b| !b.trim().is_empty()) {
        let block = block.trim_matches('\n');
        let mut lines = block.lines();
        let first = lines.next().ok_or(SubtitleError::Unsupported)?;
        let (identifier, timing) = if first.contains("-->") {
            (None, first)
        } else if first.starts_with("NOTE") || first == "STYLE" || first == "REGION" {
            output.push_str(block);
            output.push_str("\n\n");
            continue;
        } else {
            (Some(first), lines.next().ok_or(SubtitleError::Unsupported)?)
        };
        let (start, rest) = timing
            .split_once(" --> ")
            .ok_or(SubtitleError::Unsupported)?;
        let (end, settings) = rest
            .split_once(' ')
            .map_or((rest, None), |(e, s)| (e, Some(s)));
        let (start, end) = (timestamp(start)?.0, timestamp(end)?.0);
        if start > end {
            return Err(SubtitleError::Unsupported);
        }
        if leading && (start < start_ms || end < start_ms) {
            continue;
        }
        leading = false;
        if end_ms.is_some_and(|limit| limit > 0 && start > limit) {
            break;
        }
        let shift = if preserve { 0 } else { start_ms };
        if let Some(identifier) = identifier {
            output.push_str(identifier);
            output.push('\n');
        }
        output.push_str(&vtt_time(start - shift));
        output.push_str(" --> ");
        output.push_str(&vtt_time(end - shift));
        if let Some(settings) = settings {
            output.push(' ');
            output.push_str(settings);
        }
        output.push('\n');
        for line in lines {
            output.push_str(line);
            output.push('\n');
        }
        output.push('\n');
        if output.len() > 20 * 1024 * 1024 {
            return Err(SubtitleError::TooLarge);
        }
    }
    Ok(output.into_bytes())
}

/// Jellyfin `SubtitleDeliveryMethod` values this facade can honour.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryMethod {
    /// The client reads the track from the delivered container itself.
    Embed,
    /// A separate subtitle resource at `DeliveryUrl`.
    External,
    /// A rendition inside the HLS master.
    Hls,
    /// Burned into the video; selecting it requires a transcode.
    Encode,
}
impl DeliveryMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Embed => "Embed",
            Self::External => "External",
            Self::Hls => "Hls",
            Self::Encode => "Encode",
        }
    }
}

/// One `DeviceProfile.SubtitleProfiles` entry. Unknown methods (Drop, and any
/// future value) are kept so their position in the list is preserved, but
/// never match: an unknown method cannot grant a delivery.
#[derive(Clone, Debug)]
pub struct SubtitleRule {
    pub format: String,
    pub method: Option<DeliveryMethod>,
    pub language: Option<String>,
    pub container: Option<String>,
}

/// Parse the profile's `SubtitleProfiles`, bounded. An absent or null list is
/// empty (every track then resolves to Encode, as in Jellyfin).
pub fn subtitle_rules(
    value: Option<&serde_json::Value>,
) -> Result<Vec<SubtitleRule>, SubtitleError> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_array()
        .filter(|v| v.len() <= 64)
        .ok_or(SubtitleError::Unsupported)?;
    let text = |entry: &serde_json::Value, key: &str| -> Result<Option<String>, SubtitleError> {
        match entry.get(key) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(s)) if s.len() <= 256 => {
                Ok(Some(s.clone()).filter(|s| !s.is_empty()))
            }
            Some(_) => Err(SubtitleError::Unsupported),
        }
    };
    entries
        .iter()
        .map(|entry| {
            if !entry.is_object() {
                return Err(SubtitleError::Unsupported);
            }
            let format = text(entry, "Format")?.ok_or(SubtitleError::Unsupported)?;
            let method = match text(entry, "Method")?.as_deref() {
                Some("Embed") => Some(DeliveryMethod::Embed),
                Some("External") => Some(DeliveryMethod::External),
                Some("Hls") => Some(DeliveryMethod::Hls),
                Some("Encode") => Some(DeliveryMethod::Encode),
                _ => None,
            };
            Ok(SubtitleRule {
                format,
                method,
                language: text(entry, "Language")?,
                container: text(entry, "Container")?,
            })
        })
        .collect()
}

/// How the negotiated media reaches the client.
#[derive(Clone, Copy, Debug)]
pub enum Transport<'a> {
    /// The source file itself, in this container.
    Direct { container: Option<&'a str> },
    /// Native HLS; `manifest` when the chosen output can carry VTT renditions.
    Hls { manifest: bool },
}

/// The facts about one subtitle stream that the decision reads.
#[derive(Clone, Copy, Debug)]
pub struct TrackFacts<'a> {
    pub codec: &'a str,
    pub language: Option<&'a str>,
    /// A text track the native extractor can turn into WebVTT. Bitmap tracks
    /// can only be embedded (direct play) or burned.
    pub text: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub method: DeliveryMethod,
    /// The resource extension for External, the profile format otherwise.
    pub format: String,
}

/// Jellyfin's `StreamBuilder.GetSubtitleProfile` (10.11.11) restricted to what
/// Plurx can produce. Order matters and is Jellyfin's: an exact embedded
/// match (direct play only), then External/Hls entries in profile order whose
/// format equals the codec, then the same entries allowing conversion, then
/// Encode. Sidecars are produced only as VTT or SRT and manifest renditions
/// only as VTT, so an entry naming another format never matches.
pub fn delivery(
    rules: &[SubtitleRule],
    track: TrackFacts<'_>,
    transport: Transport<'_>,
) -> Delivery {
    let codec = jellyfin_codec(track.codec);
    let language = |rule: &SubtitleRule| {
        crate::profile::selector_matches(
            rule.language.as_deref(),
            Some(track.language.filter(|l| !l.is_empty()).unwrap_or("und")),
        )
    };
    if let Transport::Direct { container } = transport {
        if let Some(rule) = rules.iter().find(|rule| {
            rule.method == Some(DeliveryMethod::Embed)
                && language(rule)
                && crate::profile::selector_matches(rule.container.as_deref(), container)
                && rule.format.eq_ignore_ascii_case(codec)
        }) {
            return Delivery {
                method: DeliveryMethod::Embed,
                format: rule.format.clone(),
            };
        }
    }
    let manifest = matches!(transport, Transport::Hls { manifest: true });
    let candidate = |rule: &SubtitleRule, convert: bool| -> Option<Delivery> {
        let produced = SubtitleFormat::parse(&rule.format)?;
        let method = match rule.method? {
            DeliveryMethod::External => DeliveryMethod::External,
            DeliveryMethod::Hls if manifest && produced == SubtitleFormat::Vtt => {
                DeliveryMethod::Hls
            }
            _ => return None,
        };
        (track.text && language(rule) && (convert || rule.format.eq_ignore_ascii_case(codec))).then(
            || Delivery {
                method,
                format: rule.format.to_ascii_lowercase(),
            },
        )
    };
    for convert in [false, true] {
        if let Some(found) = rules.iter().find_map(|rule| candidate(rule, convert)) {
            return found;
        }
    }
    Delivery {
        method: DeliveryMethod::Encode,
        format: codec.to_owned(),
    }
}

/// Jellyfin names bitmap codecs by its own vocabulary (`ProbeResultNormalizer`),
/// and clients' profiles use those names; text codecs keep ffprobe's name.
pub fn jellyfin_codec(codec: &str) -> &str {
    match codec {
        "hdmv_pgs_subtitle" => "pgssub",
        "dvd_subtitle" => "dvdsub",
        "dvb_subtitle" => "dvbsub",
        "dvb_teletext" => "dvbtxt",
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn srt_representation_preserves_zero_nonzero_and_hour_source_times_without_an_origin() {
        let source = b"WEBVTT\r\n\r\ncue-zero\r\n00:00.000 --> 00:02.125\r\nFirst\r\n\r\n01:02:03.456 --> 01:02:05.789\r\nSecond\r\nline\r\n";
        assert_eq!(String::from_utf8(vtt_to_srt(source).expect("plain native VTT")).expect("UTF8"),
            "1\n00:00:00,000 --> 00:00:02,125\nFirst\n\n2\n01:02:03,456 --> 01:02:05,789\nSecond\nline\n\n");
    }
    #[test]
    fn srt_representation_refuses_malformed_or_unrepresentable_cues_without_empty_success() {
        for source in [
            "WEBVTT\n\n00:00.000 --> 00:02.000 align:start\ntext\n",
            "WEBVTT\n\n00:00.000 --> 00:02.000\n<v name>text\n",
            "WEBVTT\n\n00:60.000 --> 00:62.000\ntext\n",
            "WEBVTT\n\n00:00.000 --> 00:02.000\n<00:01.000>text\n",
            "WEBVTT\n\n100:00:00.000 --> 99:00:00.000\ntext\n",
            "WEBVTT\n\nSTYLE\n::cue { color:red }\n",
            "WEBVTT\n\n00:02.000 --> 00:01.000\ntext\n",
        ] {
            assert_eq!(
                vtt_to_srt(source.as_bytes()),
                Err(SubtitleError::Unsupported)
            );
        }
        assert!(String::from_utf8(
            vtt_to_srt(b"WEBVTT\n\n99:00:00.000 --> 100:00:00.000\n<b>text</b>\n")
                .expect("long forward cue")
        )
        .expect("UTF8")
        .contains("99:00:00,000 --> 100:00:00,000"));
        assert!(vtt_to_srt(b"WEBVTT\n\n")
            .expect("a natively empty track")
            .is_empty());
    }

    #[test]
    fn subtitle_window_follows_jellyfin_filter_events_and_keeps_zero_start_byte_identical() {
        let source = "WEBVTT\n\nNOTE kept\n\n00:00:01.000 --> 00:00:03.000\nearly\n\ncue-2\n00:02:00.000 --> 00:02:02.500 align:start\nmiddle\n\n00:05:30.000 --> 00:05:31.000\nlate\n";
        assert_eq!(
            window_vtt(source.as_bytes(), 0, None, false).expect("identity"),
            source.as_bytes(),
            "a zero start is the native extraction, unchanged"
        );
        let shifted =
            String::from_utf8(window_vtt(source.as_bytes(), 60_000, None, false).expect("window"))
                .expect("UTF8");
        assert!(
            !shifted.contains("early"),
            "a cue before the start is dropped: {shifted}"
        );
        assert!(shifted.contains("NOTE kept"));
        assert!(
            shifted.contains("cue-2\n00:01:00.000 --> 00:01:02.500 align:start\nmiddle"),
            "{shifted}"
        );
        assert!(shifted.contains("00:04:30.000 --> 00:04:31.000\nlate"));
        let copied = String::from_utf8(
            window_vtt(source.as_bytes(), 60_000, Some(200_000_u64), true).expect("window"),
        )
        .expect("UTF8");
        assert!(copied.contains("00:02:00.000 --> 00:02:02.500"), "{copied}");
        assert!(
            !copied.contains("late"),
            "cues after the end are dropped: {copied}"
        );
        // The shifted window still converts for an SRT request.
        let srt = String::from_utf8(
            vtt_to_srt(
                &window_vtt(
                    b"WEBVTT\n\n00:02:00.000 --> 00:02:01.000\nx\n",
                    60_000,
                    None,
                    false,
                )
                .expect("window"),
            )
            .expect("srt"),
        )
        .expect("UTF8");
        assert_eq!(srt, "1\n00:01:00,000 --> 00:01:01,000\nx\n\n");
    }

    #[test]
    fn subtitle_formats_are_the_implemented_aliases_only() {
        for (name, format) in [
            ("vtt", Some(SubtitleFormat::Vtt)),
            ("WebVTT", Some(SubtitleFormat::Vtt)),
            ("srt", Some(SubtitleFormat::Srt)),
            ("subrip", Some(SubtitleFormat::Srt)),
            ("ass", None),
            ("ttml", None),
            ("", None),
        ] {
            assert_eq!(SubtitleFormat::parse(name), format, "{name}");
        }
    }

    fn rules(value: serde_json::Value) -> Vec<SubtitleRule> {
        subtitle_rules(Some(&value)).expect("rules")
    }
    const SRT: TrackFacts<'static> = TrackFacts {
        codec: "subrip",
        language: Some("eng"),
        text: true,
    };
    const PGS: TrackFacts<'static> = TrackFacts {
        codec: "hdmv_pgs_subtitle",
        language: None,
        text: false,
    };

    #[test]
    fn delivery_matches_the_pinned_clients_profiles() {
        // Jellyfin Android TV 0.19.10 (J0 trace): Embed, External, Hls per format.
        let android = rules(serde_json::json!([
            {"Format":"vtt","Method":"Embed"},{"Format":"vtt","Method":"External"},{"Format":"vtt","Method":"Hls"},
            {"Format":"srt","Method":"Embed"},{"Format":"srt","Method":"External"},
            {"Format":"subrip","Method":"Embed"},{"Format":"subrip","Method":"External"},
            {"Format":"pgssub","Method":"Embed"},{"Format":"pgssub","Method":"Encode"},
            {"Format":"ass","Method":"Encode"}]));
        let direct = Transport::Direct {
            container: Some("mkv"),
        };
        let hls = Transport::Hls { manifest: true };
        assert_eq!(
            delivery(&android, SRT, direct).method,
            DeliveryMethod::Embed
        );
        // HLS skips the embedded pass; External precedes Hls in Android's list.
        assert_eq!(
            delivery(&android, SRT, hls),
            Delivery {
                method: DeliveryMethod::External,
                format: "subrip".into()
            }
        );
        // A text codec with no exact entry converts to the first producible one.
        let ass = TrackFacts {
            codec: "ass",
            language: None,
            text: true,
        };
        assert_eq!(
            delivery(&android, ass, hls),
            Delivery {
                method: DeliveryMethod::External,
                format: "vtt".into()
            }
        );
        // A bitmap track is never a sidecar: Encode unless embedded as-is.
        assert_eq!(delivery(&android, PGS, hls).method, DeliveryMethod::Encode);
        // Matched under Jellyfin's codec name, as the profile spells it.
        assert_eq!(
            delivery(&android, PGS, direct).method,
            DeliveryMethod::Embed
        );
        // Infuse 8.5.6: External vtt/ass/ssa only.
        let infuse = rules(serde_json::json!([
            {"Format":"vtt","Method":"External","AllowChunkedResponse":true},
            {"Format":"ass","Method":"External"},{"Format":"ssa","Method":"External"}]));
        assert_eq!(
            delivery(&infuse, SRT, direct),
            Delivery {
                method: DeliveryMethod::External,
                format: "vtt".into()
            }
        );
        // `ass` is not a format Plurx produces, so an ASS track converts to VTT.
        assert_eq!(delivery(&infuse, ass, direct).format, "vtt");
    }

    #[test]
    fn delivery_never_grants_what_the_profile_or_output_cannot_carry() {
        let hls_only = rules(serde_json::json!([{"Format":"vtt","Method":"Hls"}]));
        assert_eq!(
            delivery(&hls_only, SRT, Transport::Hls { manifest: true }).method,
            DeliveryMethod::Hls
        );
        // An output without manifest renditions cannot take an Hls entry.
        assert_eq!(
            delivery(&hls_only, SRT, Transport::Hls { manifest: false }).method,
            DeliveryMethod::Encode
        );
        // Hls entries never apply to direct play.
        assert_eq!(
            delivery(
                &hls_only,
                SRT,
                Transport::Direct {
                    container: Some("mkv")
                }
            )
            .method,
            DeliveryMethod::Encode
        );
        // Unknown methods and unimplemented formats never match.
        let odd = rules(serde_json::json!([
            {"Format":"vtt","Method":"Drop"},{"Format":"ttml","Method":"External"}]));
        assert_eq!(
            delivery(&odd, SRT, Transport::Direct { container: None }).method,
            DeliveryMethod::Encode
        );
        // An embedded entry restricted to another container does not apply.
        let mp4_only =
            rules(serde_json::json!([{"Format":"subrip","Method":"Embed","Container":"mp4"}]));
        assert_eq!(
            delivery(
                &mp4_only,
                SRT,
                Transport::Direct {
                    container: Some("mkv")
                }
            )
            .method,
            DeliveryMethod::Encode
        );
        // A language-restricted entry skips other languages.
        let french =
            rules(serde_json::json!([{"Format":"vtt","Method":"External","Language":"fre"}]));
        assert_eq!(
            delivery(&french, SRT, Transport::Direct { container: None }).method,
            DeliveryMethod::Encode
        );
        assert_eq!(subtitle_rules(None).expect("absent").len(), 0);
        assert!(subtitle_rules(Some(&serde_json::json!({"Format":"vtt"}))).is_err());
        assert!(subtitle_rules(Some(&serde_json::json!([{"Method":"External"}]))).is_err());
    }
}
