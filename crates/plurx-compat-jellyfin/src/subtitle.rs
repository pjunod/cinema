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
}
