//! Physical input-format start, observed in the SAME copy child's dump.
//! dump.c prints AVFormatContext.start_time with six decimal digits at
//! AV_TIME_BASE=1_000_000 (FFmpeg n7/n8). No stream-first-PTS fallback.
use plurx_core::segplan::ranges::SourceTimestamp;
const MAX_LINE: usize = 4096;
const MAX_PARSE: usize = 1024 * 1024;
pub(super) struct InputFormatStart {
    expected_suffix: String,
    line: Vec<u8>,
    bytes: usize,
    invalid: bool,
    matched_input: bool,
    summaries: u32,
    start: Option<SourceTimestamp>,
}
impl InputFormatStart {
    pub(super) fn new(input_url: &str) -> Self {
        Self {
            expected_suffix: format!(", from '{input_url}':"),
            line: Vec::new(),
            bytes: 0,
            invalid: false,
            matched_input: false,
            summaries: 0,
            start: None,
        }
    }
    pub(super) fn feed(&mut self, bytes: &[u8]) {
        if self.invalid {
            return;
        }
        for &byte in bytes {
            self.bytes = self.bytes.saturating_add(1);
            if self.bytes > MAX_PARSE {
                self.invalid = true;
                return;
            }
            if byte == b'\n' || byte == b'\r' {
                let line = std::mem::take(&mut self.line);
                match std::str::from_utf8(&line) {
                    Ok(line) => self.line(line),
                    Err(_) => self.invalid = true,
                }
            } else if self.line.len() == MAX_LINE {
                self.invalid = true;
                return;
            } else {
                self.line.push(byte);
            }
        }
    }
    fn line(&mut self, line: &str) {
        if line.starts_with("Input #") {
            if self.matched_input
                || !line.starts_with("Input #0, ")
                || !line.ends_with(&self.expected_suffix)
            {
                self.invalid = true;
            } else {
                self.matched_input = true;
            }
        }
        // Keep looking through EOF: metadata cannot spoof an Output marker
        // to hide the real Duration summary after a forged one. Duplicates
        // anywhere after the exact random Input URL fail closed.
        if self.matched_input && line.trim_start().starts_with("Duration: ") {
            self.summaries = self.summaries.saturating_add(1);
            if self.summaries != 1 {
                self.invalid = true;
                return;
            }
            self.start = line
                .split_once(", start: ")
                .and_then(|(_, rest)| rest.split_once(", bitrate: "))
                .and_then(|(start, _)| parse_start(start));
        }
    }
    pub(super) fn finish(self) -> Option<SourceTimestamp> {
        if self.invalid || !self.line.is_empty() || !self.matched_input || self.summaries != 1 {
            None
        } else {
            self.start
        }
    }
}
fn parse_start(value: &str) -> Option<SourceTimestamp> {
    let (negative, value) = value
        .strip_prefix('-')
        .map_or((false, value), |value| (true, value));
    let (seconds, micros) = value.split_once('.')?;
    if seconds.is_empty()
        || seconds.len() > 16
        || !seconds.bytes().all(|byte| byte.is_ascii_digit())
        || micros.len() != 6
        || !micros.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let exact = if negative {
        format!("-{value}")
    } else {
        value.to_owned()
    };
    match plurx_core::transcode::VodSourceClock::from_probe_start_time(Some(&exact)).ok()? {
        plurx_core::transcode::VodSourceClock::ReportedOrigin(micros) => Some(SourceTimestamp {
            dts: micros,
            time_base_num: 1,
            time_base_den: 1_000_000,
        }),
        plurx_core::transcode::VodSourceClock::ProducerDefaultZero => None,
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_random_input_and_single_signed_format_summary_only() {
        let mut parser = InputFormatStart::new("http://127.0.0.1:1/private-operation");
        parser.feed(b"Input #0, matroska, from 'http://127.0.0.1:1/private-operation':\n  Duration: 00:10:00.00, start: -0.083000, bitrate: 120 kb/s\nOutput #0, mp4, to 'pipe:1':\n");
        let start = parser.finish().expect("physical signed start");
        assert_eq!(start.dts, -83000);
        assert_eq!(start.time_base_den, 1_000_000);
        let mut wrong = InputFormatStart::new("different-operation");
        wrong.feed(
            b"Input #0, matroska, from 'other':\n  Duration: 1, start: 0.000000, bitrate: N/A\n",
        );
        assert!(wrong.finish().is_none());
        let mut duplicate = InputFormatStart::new("cap");
        duplicate.feed(b"Input #0, matroska, from 'cap':\n  Duration: N/A, start: 0.000000, bitrate: N/A\nOutput #0, mp4, to 'pipe:1':\n  Duration: N/A, start: 1.000000, bitrate: N/A\n");
        assert!(duplicate.finish().is_none());
        assert!(parse_start("N/A").is_none());
        assert!(parse_start("0.123").is_none());
        assert!(parse_start("0e6.000000").is_none());
    }
}
