//! Bounded canonical WebVTT cues; translation changes text, never cue identity/time.
use serde::{Deserialize, Serialize};
pub(crate) const MAX_BYTES: usize = 1024 * 1024;
const MAX_CUES: usize = 20_000;
const MAX_END: i64 = 86_400_000;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct TextCue {
    pub id: String,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}
fn stamp(value: &str) -> Option<i64> {
    let bits: Vec<_> = value.split(':').collect();
    let (hours, minutes, tail) = match bits.as_slice() {
        [m, s] => (0, m.parse::<i64>().ok()?, *s),
        [h, m, s] => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?, *s),
        _ => return None,
    };
    let (seconds, millis) = tail.split_once('.')?;
    if millis.len() != 3 || !millis.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let seconds = seconds.parse::<i64>().ok()?;
    let millis = millis.parse::<i64>().ok()?;
    if hours < 0 || !(0..60).contains(&minutes) || !(0..60).contains(&seconds) {
        return None;
    }
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes * 60_000 + seconds * 1000 + millis)
        .filter(|ms| *ms <= MAX_END)
}
fn plain(value: &str) -> Result<String, String> {
    let mut output = String::new();
    let mut tag = false;
    for c in value.chars() {
        match c {
            '<' => tag = true,
            '>' if tag => tag = false,
            _ if !tag => output.push(c),
            _ => {}
        }
    }
    if tag {
        return Err("subtitle_markup_invalid".into());
    }
    // VTT entities are decoded once before model inference; output remains literal text.
    Ok(output
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&lrm;", "")
        .replace("&rlm;", "")
        .replace("&amp;", "&"))
}
pub(crate) fn parse(input: &[u8]) -> Result<Vec<TextCue>, String> {
    if input.is_empty() || input.len() > MAX_BYTES {
        return Err("subtitle_too_large".into());
    }
    let raw = std::str::from_utf8(input).map_err(|_| "subtitle_encoding_invalid")?;
    if raw.contains('\0') {
        return Err("subtitle_encoding_invalid".into());
    }
    let normalized = raw
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let mut blocks = normalized.split("\n\n");
    let header = blocks.next().ok_or("subtitle_format_invalid")?;
    if header
        .lines()
        .next()
        .is_none_or(|l| l != "WEBVTT" && !l.starts_with("WEBVTT "))
    {
        return Err("subtitle_format_invalid".into());
    }
    // Segmented transport timestamps require the existing extraction owner to normalize first.
    if header.lines().any(|l| l.starts_with("X-TIMESTAMP-MAP")) {
        return Err("subtitle_clock_not_normalized".into());
    }
    let mut cues = Vec::new();
    for block in blocks {
        let mut lines = block.lines();
        let Some(first) = lines.next() else {
            continue;
        };
        if first.is_empty()
            || first == "STYLE"
            || first == "REGION"
            || first == "NOTE"
            || first.starts_with("NOTE ")
        {
            continue;
        }
        let id = if first.contains("-->") {
            format!("cue-{}", cues.len())
        } else {
            first.to_owned()
        };
        if id.len() > plurx_core::store::subtitle_text_translation::MAX_TEXT_CUE_ID_BYTES
            || id.chars().any(char::is_control)
        {
            return Err("subtitle_identifier_invalid".into());
        }
        let timing = if first.contains("-->") {
            first
        } else {
            lines.next().ok_or("subtitle_format_invalid")?
        };
        let (start, end) = timing.split_once("-->").ok_or("subtitle_format_invalid")?;
        let start_ms = stamp(start.trim()).ok_or("subtitle_timestamp_invalid")?;
        let end_ms = stamp(
            end.split_whitespace()
                .next()
                .ok_or("subtitle_timestamp_invalid")?,
        )
        .ok_or("subtitle_timestamp_invalid")?;
        if end_ms <= start_ms {
            return Err("subtitle_timestamp_invalid".into());
        }
        let text = plain(&lines.collect::<Vec<_>>().join("\n"))?;
        if text.trim().is_empty() {
            continue;
        }
        if text.len() > 16 * 1024 || cues.len() >= MAX_CUES {
            return Err("subtitle_too_large".into());
        }
        cues.push(TextCue {
            id,
            start_ms,
            end_ms,
            text,
        });
    }
    if cues.is_empty() {
        return Err("subtitle_has_no_text".into());
    }
    Ok(cues)
}
fn timestamp(ms: i64) -> String {
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}
pub(crate) fn render(cues: &[TextCue]) -> Result<Vec<u8>, String> {
    if cues.is_empty() || cues.len() > MAX_CUES {
        return Err("subtitle_output_invalid".into());
    }
    let mut out = String::from("WEBVTT\n\n");
    for cue in cues {
        if cue.start_ms < 0
            || cue.end_ms <= cue.start_ms
            || cue.end_ms > MAX_END
            || cue.text.trim().is_empty()
            || cue.text.contains('\0')
        {
            return Err("subtitle_output_invalid".into());
        }
        let text = cue
            .text
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;");
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            cue.id,
            timestamp(cue.start_ms),
            timestamp(cue.end_ms),
            text.replace("\n\n", "\n")
        ));
        if out.len() > MAX_BYTES {
            return Err("subtitle_output_too_large".into());
        }
    }
    Ok(out.into_bytes())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn timing_survives_translation_and_markup_is_literal() {
        let mut cues = parse(
            b"WEBVTT\n\na\n00:01.250 --> 00:03.750 align:start\n<b>Bonjour &amp; merci</b>\n\n",
        )
        .expect("valid source cues");
        assert_eq!(cues[0].text, "Bonjour & merci");
        cues[0].text = "Hello <world> & thank you".into();
        let bytes = render(&cues).expect("render translated cues");
        let decoded = parse(&bytes).expect("parse rendered captions");
        assert_eq!((decoded[0].start_ms, decoded[0].end_ms), (1250, 3750));
        assert_eq!(decoded[0].text, cues[0].text);
    }
    #[test]
    fn caption_identifiers_share_the_durable_slice_byte_limit() {
        let limit = plurx_core::store::subtitle_text_translation::MAX_TEXT_CUE_ID_BYTES;
        let id = "é".repeat(limit / 2);
        let source = format!("WEBVTT\n\n{id}\n00:00.000 --> 00:01.000\nBonjour\n");
        let cues = parse(source.as_bytes()).expect("full durable identifier accepted");
        assert_eq!(cues[0].id, id);
        assert_eq!(
            parse(&render(&cues).expect("render identifier")).expect("reopen identifier"),
            cues
        );
        let too_long = source.replacen(&id, &(id.clone() + "x"), 1);
        assert!(parse(too_long.as_bytes()).is_err());
    }
    #[test]
    fn malformed_or_transport_relative_cues_never_publish() {
        for bytes in [
            b"WEBVTT\n\n00:61.000 --> 00:62.000\na\n".as_slice(),
            b"WEBVTT\nX-TIMESTAMP-MAP=LOCAL:00:00.000,MPEGTS:9000\n\n00:00.000 --> 00:01.000\na\n",
        ] {
            assert!(parse(bytes).is_err());
        }
    }
}
