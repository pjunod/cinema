//! Fail-closed, bounded analysis of FFmpeg's pre-filter HEVC trace.
//!
//! A proof is issued only by the caller after full-source completion and a
//! successful child exit. Missing/older proofs are never inferred from an
//! empty post-filter observation. The initial contract supports one VPS,
//! SPS and PPS, including identical repetitions, not configuration switches.
use serde::{Deserialize, Serialize};

pub const REVISION: u32 = 1;
const MAX_LINE: usize = 8192;
const MAX_PARAMETER: usize = 65536;
/// What makes a stderr line part of the trace. The index child shares one
/// stderr with everything else FFmpeg prints, so only lines carrying this are
/// evidence; `parse` ignores the rest.
const TRACE_PREFIX: &[u8] = b"[trace_headers @ ";

/// FFmpeg messages that mean the pass read a damaged source and went on.
/// `-xerror` used to turn each of them into a non-zero exit; the index argv no
/// longer carries it, because the same flag also made the mp4 muxer's
/// correctable `Non-monotonic DTS` fatal. Without it a truncated or corrupt
/// packet is dropped and the pass exits 0 with a short but well-formed
/// index, so these are matched explicitly. None of them is a muxer message.
///
/// Wording by FFmpeg release (packaging's floor is 5.1; the fleet runs newer):
/// the mov demuxer's `Packet corrupt` is in `mov.c` and does not change; the
/// CLI's `corrupt input packet in stream N` is in every release (5.1 prefixes
/// the input url); a bitstream-filter failure reads `Error applying bitstream
/// filters to an output packet` (5.1/6.0) or `... to a packet` (6.1+), both
/// matched by the shared prefix; a non-EOF demux error reads `Error during
/// demuxing` or `Error retrieving a packet from demuxer` (7+) and, through
/// `print_error` up to 6.x, only `<input>: <strerror>`, so the two
/// `AVERROR_INVALIDDATA`/`EIO` strerror texts are matched as well. Checked
/// against 6.1 on a truncated fragmented MP4; the older wording is from the
/// release sources and is not exercised here.
const SOURCE_FAULTS: &[&str] = &[
    // Demuxer flagged the packet (truncated fragment, "partial file").
    "corrupt input packet",
    "Packet corrupt",
    // trace_headers could not parse a packet, which is then dropped.
    "Error applying bitstream filters",
    // A demux error other than EOF, which ffmpeg otherwise treats as EOF.
    "Error during demuxing",
    "Error retrieving a packet from demuxer",
    "Invalid data found when processing input",
    "Input/output error",
];
/// A line is searched whenever this many bytes are buffered, and again at its
/// end, so a message glued onto a long unterminated trace line is still seen.
const FAULT_CHUNK: usize = 1024;
/// What survives a mid-line search: longer than any marker, so one split
/// across two searches is still whole in the next.
const FAULT_KEEP: usize = 64;

/// Line-oriented, bounded scan of all stderr for [`SOURCE_FAULTS`]. It sees
/// every byte, including after the trace has been refused.
#[derive(Clone, Default)]
struct FaultScan {
    line: Vec<u8>,
    fault: Option<&'static str>,
}

impl FaultScan {
    fn search(&mut self) {
        let text = String::from_utf8_lossy(&self.line);
        self.fault = SOURCE_FAULTS.iter().copied().find(|m| text.contains(m));
    }

    fn feed(&mut self, bytes: &[u8]) {
        if self.fault.is_some() {
            return;
        }
        for &byte in bytes {
            if byte == b'\n' || byte == b'\r' {
                self.search();
                self.line.clear();
            } else {
                self.line.push(byte);
                if self.line.len() >= FAULT_CHUNK {
                    self.search();
                    let keep = self.line.len() - FAULT_KEEP;
                    self.line.drain(..keep);
                }
            }
            if self.fault.is_some() {
                return;
            }
        }
    }
}

/// Streaming search of one line for anything that could be trace output:
/// the [`TRACE_PREFIX`], or a trace field's ` = <integer>`. FFmpeg omits the
/// prefix on a message that follows an unterminated one, so a field glued onto
/// a foreign line has only its value to show for itself. The tee banner
/// escapes every space and the progress line writes `q=-1.0`, so neither
/// carries the field form.
#[derive(Clone, Copy, Default)]
struct Evidence {
    prefix: usize,
    field: usize,
}

impl Evidence {
    /// Advance by one byte; true once either signature has been seen.
    fn advance(&mut self, byte: u8) -> bool {
        // `[` occurs only at the start of the prefix, so a mismatch can
        // restart only on `[` itself.
        self.prefix = if byte == TRACE_PREFIX[self.prefix] {
            self.prefix + 1
        } else if byte == TRACE_PREFIX[0] {
            1
        } else {
            0
        };
        let field_value = self.field == 3 && (byte.is_ascii_digit() || byte == b'-');
        self.field = match (self.field, byte) {
            (_, _) if field_value => 0,
            (1, b'=') | (3, b'=') => 2,
            (2, b' ') => 3,
            (_, b' ') => 1,
            _ => 0,
        };
        field_value || self.prefix == TRACE_PREFIX.len()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Proof {
    pub revision: u32,
    pub source_object_version: String,
    /// Local stamps are meaningful only in their issuing node's namespace.
    #[serde(default)]
    pub source_node_id: String,
    /// Whole-source attestation, never the sampled source-cache digest.
    #[serde(default)]
    pub source_sha256: Option<String>,
    pub stream_index: u32,
    pub slices: u64,
    /// None means verified; a reason means this complete scan refused copy.
    pub refusal: Option<String>,
}

impl Proof {
    /// Called only after authenticated blob/pipeline verification and a
    /// local full-source attestation using this proof revision's regime.
    pub fn bind_equivalent_source(&mut self, digest: &str, node: &str, object: &str) -> bool {
        if self.revision != REVISION
            || digest.len() != 64
            || !digest.bytes().all(|b| b.is_ascii_hexdigit())
            || self.source_sha256.as_deref() != Some(digest)
            || node.is_empty()
            || object.is_empty()
        {
            return false;
        }
        self.source_node_id = node.to_owned();
        self.source_object_version = object.to_owned();
        true
    }

    /// A completed refusal is still a current analysis; do not rescan it
    /// until source identity or analyzer revision changes.
    pub fn current_on_node(&self, object: &str, node: &str) -> bool {
        self.revision == REVISION
            && !object.is_empty()
            && !node.is_empty()
            && self.source_node_id == node
            && self.source_object_version == object
    }

    pub fn permits_on_node(&self, object: &str, node: &str) -> bool {
        self.current_on_node(object, node) && self.permits(object)
    }

    pub fn permits(&self, object_version: &str) -> bool {
        self.revision == REVISION
            && !object_version.is_empty()
            && self.source_object_version == object_version
            && self.slices > 0
            && self.refusal.is_none()
    }
}

/// One closed, inspected source packet. This is range-local evidence, never
/// a whole-source [`Proof`] or authorization to read or publish media.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PacketInspection {
    pub sequence: u64,
    pub slices: u64,
    pub configuration_sha256: String,
}

/// Inspection of the packets actually drained from one range child. A seeked
/// range can return this successfully without inspecting any preceding bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RangeInspection {
    pub packets: u64,
    pub slices: u64,
    pub configuration_sha256: String,
}

/// `Clone` lets a whole-file pass close a copy as a range
/// ([`Trace::finish_range`]) and still finish the original as its proof.
#[derive(Clone, Default)]
pub struct Trace {
    line: Vec<u8>,
    sets: [Option<Vec<u8>>; 3],
    ids: [Option<i64>; 3],
    parents: [Option<i64>; 3],
    section: Option<usize>,
    fields: Vec<u8>,
    id: Option<i64>,
    parent: Option<i64>,
    ended: bool,
    slice: bool,
    slice_reference: bool,
    slices: u64,
    packets: u64,
    packet_slices: u64,
    configuration_sha256: Option<String>,
    closed_packet: Option<PacketInspection>,
    /// Discarding the rest of an over-long line that is not trace output.
    skipping: bool,
    /// Search state across the bytes of the current over-long line, so trace
    /// content anywhere in it still fails closed.
    evidence: Evidence,
    refusal: Option<String>,
    faults: FaultScan,
}

impl Trace {
    /// The first FFmpeg message showing the pass read a damaged source, if
    /// any. Independent of the trace verdict: the caller refuses the index.
    pub fn source_fault(&self) -> Option<&'static str> {
        self.faults.fault
    }

    fn reject(&mut self, reason: &str) {
        if self.refusal.is_none() {
            self.refusal = Some(reason.to_owned());
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) {
        self.feed_with_packets(bytes, |_| {});
    }

    /// Emits a packet only after the following packet header closes all of its
    /// slice/parameter sections. No unbounded event queue is retained.
    pub fn feed_with_packets(&mut self, bytes: &[u8], mut emit: impl FnMut(PacketInspection)) {
        self.faults.feed(bytes);
        // A rejection is terminal: keep draining stderr, but bound parsing work.
        if self.refusal.is_some() {
            return;
        }
        for &byte in bytes {
            // FFmpeg ends its progress line with `\r`; it is a line of its own,
            // not a prefix of whatever is printed next.
            if byte == b'\n' || byte == b'\r' {
                if self.skipping {
                    self.skipping = false;
                    self.evidence = Evidence::default();
                    continue;
                }
                let line = std::mem::take(&mut self.line);
                match std::str::from_utf8(&line) {
                    Ok(line) => {
                        self.parse(line);
                        if let Some(packet) = self.closed_packet.take() {
                            emit(packet);
                        }
                    }
                    Err(_) => self.reject("invalid trace encoding"),
                }
            } else if self.skipping {
                if self.evidence.advance(byte) {
                    self.reject("trace line exceeds the parser limit");
                    return;
                }
            } else if self.line.len() < MAX_LINE {
                self.line.push(byte);
            } else {
                // The bound protects parsing work, and only trace lines are
                // parsed. An over-long line that could carry trace output is
                // still refused; any other one -- FFmpeg's own output banner names
                // every tee slave, one per subtitle track -- is not evidence
                // either way, so it is dropped unbuffered instead of refusing
                // the source.
                let mut evidence = Evidence::default();
                let found = self
                    .line
                    .iter()
                    .chain(std::iter::once(&byte))
                    .any(|&byte| evidence.advance(byte));
                if found {
                    self.reject("trace line exceeds the parser limit");
                    return;
                }
                self.line.clear();
                self.skipping = true;
                self.evidence = evidence;
            }
        }
    }

    fn end_section(&mut self) {
        if self.slice && !self.slice_reference {
            self.reject("slice has no parsed PPS reference");
        }
        self.slice = false;
        self.slice_reference = false;
        if let Some(kind) = self.section.take() {
            if self.id.is_none() || !self.ended || (kind > 0 && self.parent.is_none()) {
                self.reject("incomplete parameter-set trace");
            } else if let Some(previous) = &self.sets[kind] {
                if previous != &self.fields {
                    self.reject("HEVC parameter sets change; immutable copy is unsafe");
                }
            } else {
                self.sets[kind] = Some(std::mem::take(&mut self.fields));
                self.ids[kind] = self.id;
                self.parents[kind] = self.parent;
            }
        }
        self.fields.clear();
        self.id = None;
        self.parent = None;
        self.ended = false;
    }

    fn parse(&mut self, line: &str) {
        let Some((_, line)) = line.split_once("[trace_headers @ ") else {
            return;
        };
        let Some((_, line)) = line.split_once("] ") else {
            self.reject("unrecognized trace prefix");
            return;
        };
        let line = line.trim();
        if !line.as_bytes().first().is_some_and(u8::is_ascii_digit) {
            self.end_section();
            if line.starts_with("Packet:") {
                if self.sets.iter().any(Option::is_none) {
                    self.reject("source extradata lacks a complete decoder configuration");
                }
                self.close_packet();
                if self.configuration_sha256.is_none()
                    && self.refusal.is_none()
                    && self.sets.iter().all(Option::is_some)
                {
                    use sha2::{Digest, Sha256};
                    let mut digest = Sha256::new();
                    digest.update(b"plurx.hevc-inspected-configuration.v1\0");
                    for fields in self.sets.iter().flatten() {
                        digest.update((fields.len() as u64).to_be_bytes());
                        digest.update(fields);
                    }
                    self.configuration_sha256 = Some(format!("{:x}", digest.finalize()));
                }
                self.packets += 1;
                self.packet_slices = 0;
            } else if !matches!(
                line,
                "Extradata"
                    | "Video Parameter Set"
                    | "Sequence Parameter Set"
                    | "Picture Parameter Set"
                    | "Slice Segment Header"
                    | "Access Unit Delimiter"
                    | "Prefix Supplemental Enhancement Information"
                    | "Suffix Supplemental Enhancement Information"
                    | "Active Parameter Sets"
                    | "Buffering Period"
                    | "Picture Timing"
                    | "Mastering Display Colour Volume"
                    | "Content Light Level Information"
                    | "User Data Unregistered"
                    | "User Data Registered ITU-T T.35"
                    | "Decoded Picture Hash"
                    | "Recovery Point"
                    | "Filler Data"
                    | "End of Sequence"
                    | "End of Bitstream"
            ) {
                self.reject("unrecognized or failed HEVC trace section");
            }
            self.section = match line {
                "Video Parameter Set" => Some(0),
                "Sequence Parameter Set" => Some(1),
                "Picture Parameter Set" => Some(2),
                _ => None,
            };
            self.slice = line == "Slice Segment Header";
            return;
        }
        let mut parts = line.split_whitespace();
        let _position = parts.next();
        let Some(name) = parts.next() else {
            self.reject("malformed trace field");
            return;
        };
        let Some((_, value)) = line.rsplit_once(" = ") else {
            self.reject("malformed trace value");
            return;
        };
        let Ok(value) = value.parse::<i64>() else {
            self.reject("invalid trace value");
            return;
        };
        if (self.section.is_some() || self.slice) && name == "nuh_layer_id" && value != 0 {
            self.reject("multilayer HEVC configuration is not qualified for copy");
        }
        if let Some(kind) = self.section {
            if self.fields.len() + name.len() + 32 > MAX_PARAMETER {
                self.reject("parameter set exceeds the parser limit");
                return;
            }
            self.fields.extend_from_slice(name.as_bytes());
            self.fields.push(0);
            self.fields.extend_from_slice(&value.to_le_bytes());
            match (kind, name) {
                (0, "vps_video_parameter_set_id")
                | (1, "sps_seq_parameter_set_id")
                | (2, "pps_pic_parameter_set_id") => self.id = Some(value),
                (1, "sps_video_parameter_set_id") | (2, "pps_seq_parameter_set_id") => {
                    self.parent = Some(value)
                }
                (_, "rbsp_stop_one_bit") if value == 1 => self.ended = true,
                _ => (),
            }
        }
        if self.slice && name == "slice_pic_parameter_set_id" {
            if self.sets.iter().any(Option::is_none)
                || self.ids[2] != Some(value)
                || self.parents[2] != self.ids[1]
                || self.parents[1] != self.ids[0]
            {
                self.reject("slice references an unproved decoder configuration");
            }
            self.slice_reference = true;
            self.slices += 1;
            self.packet_slices += 1;
        }
    }

    fn close_packet(&mut self) {
        if self.packets == 0 {
            return;
        }
        if self.packet_slices == 0 {
            self.reject("video packet has no inspected slices");
        }
        if self.refusal.is_none() {
            if let Some(configuration_sha256) = &self.configuration_sha256 {
                self.closed_packet = Some(PacketInspection {
                    sequence: self.packets - 1,
                    slices: self.packet_slices,
                    configuration_sha256: configuration_sha256.clone(),
                });
            }
        }
    }

    fn finish_input(&mut self) {
        if !self.line.is_empty() || self.skipping {
            self.reject("truncated trace line");
        }
        self.end_section();
        if self.slices == 0
            || self.packets == 0
            || self.packet_slices == 0
            || self.sets.iter().any(Option::is_none)
        {
            self.reject("no complete pre-filter HEVC configuration scan");
        }
        self.close_packet();
    }

    /// Close a drained range. The caller still checks successful child exit
    /// and matches packet sequence/clock/sample coverage. This method cannot
    /// mint a full-source proof, even when the range child exited at EOF.
    pub fn finish_range(
        mut self,
        mut emit: impl FnMut(PacketInspection),
    ) -> Result<RangeInspection, String> {
        self.finish_input();
        if let Some(reason) = self.refusal {
            return Err(reason);
        }
        if let Some(packet) = self.closed_packet.take() {
            emit(packet);
        }
        Ok(RangeInspection {
            packets: self.packets,
            slices: self.slices,
            configuration_sha256: self
                .configuration_sha256
                .ok_or_else(|| "missing inspected configuration".to_owned())?,
        })
    }

    pub fn finish(mut self, source_object_version: String, stream_index: u32) -> Proof {
        self.finish_input();
        Proof {
            revision: REVISION,
            source_object_version,
            stream_index,
            source_node_id: String::new(),
            source_sha256: None,
            slices: self.slices,
            refusal: self.refusal,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn line(s: &str) -> String {
        format!("[trace_headers @ 0x1] {s}\n")
    }
    fn fixture() -> String {
        [
            "Extradata",
            "Video Parameter Set",
            "0 vps_video_parameter_set_id 0 = 0",
            "4 rbsp_stop_one_bit 1 = 1",
            "Sequence Parameter Set",
            "0 sps_video_parameter_set_id 0 = 0",
            "4 sps_seq_parameter_set_id 0 = 0",
            "8 rbsp_stop_one_bit 1 = 1",
            "Picture Parameter Set",
            "0 pps_pic_parameter_set_id 0 = 0",
            "4 pps_seq_parameter_set_id 0 = 0",
            "8 pps_cb_qp_offset 0 = -6",
            "16 rbsp_stop_one_bit 1 = 1",
            "Packet: 100 bytes, key frame",
            "Slice Segment Header",
            "0 slice_pic_parameter_set_id 0 = 0",
        ]
        .map(line)
        .concat()
    }
    #[test]
    fn range_events_close_packets_in_order_across_arbitrary_stderr_chunks() {
        let input = fixture()
            + &line("Packet: 80 bytes")
            + &line("Slice Segment Header")
            + &line("0 slice_pic_parameter_set_id 0 = 0");
        let mut trace = Trace::default();
        let mut packets = Vec::new();
        for chunk in input.as_bytes().chunks(7) {
            trace.feed_with_packets(chunk, |packet| packets.push(packet));
        }
        assert_eq!(packets.len(), 1, "last packet is still open");
        let range = trace
            .finish_range(|packet| packets.push(packet))
            .expect("range inspected");
        assert_eq!(range.packets, 2);
        assert_eq!(range.slices, 2);
        assert_eq!(
            packets.iter().map(|p| p.sequence).collect::<Vec<_>>(),
            vec![0, 1]
        );
        assert!(packets
            .iter()
            .all(|p| p.slices == 1 && p.configuration_sha256 == range.configuration_sha256));
        assert_eq!(range.configuration_sha256.len(), 64);
    }

    #[test]
    fn missing_or_changed_late_inspection_never_emits_that_packet() {
        for suffix in [
            line("Packet: 80 bytes"),
            fixture().replace("pps_cb_qp_offset 0 = -6", "pps_cb_qp_offset 0 = -1"),
        ] {
            let mut trace = Trace::default();
            let mut packets = Vec::new();
            trace.feed_with_packets(fixture().as_bytes(), |packet| packets.push(packet));
            trace.feed_with_packets(suffix.as_bytes(), |packet| packets.push(packet));
            assert!(trace.finish_range(|packet| packets.push(packet)).is_err());
            assert!(packets.iter().all(|p| p.sequence == 0));
        }
    }

    #[test]
    fn stable_repeats_are_proved_but_updates_and_distinct_ids_are_refused() {
        for change in [
            None,
            Some(("= -6", "= -1")),
            Some((
                "pps_pic_parameter_set_id 0 = 0",
                "pps_pic_parameter_set_id 0 = 1",
            )),
        ] {
            let mut trace = Trace::default();
            trace.feed(fixture().as_bytes());
            let next = match change {
                Some((a, b)) => fixture().replace(a, b),
                None => fixture(),
            };
            // A split anywhere in stderr must not change the verdict.
            for chunk in next.as_bytes().chunks(7) {
                trace.feed(chunk);
            }
            let proof = trace.finish("source".into(), 0);
            assert_eq!(proof.permits("source"), change.is_none());
            assert!(!proof.permits("replaced-source"));
        }
    }
    /// A shared index pass writes several outputs from one demuxer and
    /// traces only the first. The other outputs' log lines, the muxers'
    /// banners and the progress line land between the trace's fields, and
    /// must not change the proof.
    #[test]
    fn other_outputs_lines_between_trace_fields_do_not_change_the_proof() {
        let mut clean = Trace::default();
        clean.feed(fixture().as_bytes());
        let clean = clean.finish("source".into(), 0);
        assert!(clean.permits("source"));
        let noisy: String = fixture()
            .lines()
            .flat_map(|line| {
                [
                    format!("{line}\n"),
                    "[out#1/mp4 @ 0x2] video:1kB audio:0kB subtitle:0kB\n".to_owned(),
                    "[hevc_mp4toannexb @ 0x3] packet processed\n".to_owned(),
                    "frame=   10 fps=0.0 q=-1.0 size=       1kB time=00:00:00.41 speed=N/A\r"
                        .to_owned(),
                ]
            })
            .collect();
        let mut trace = Trace::default();
        for chunk in noisy.as_bytes().chunks(5) {
            trace.feed(chunk);
        }
        assert_eq!(trace.finish("source".into(), 0), clean);
    }

    /// The mutation the shared pass's argv rules out: a trace on every
    /// output interleaves two traces into one parser, which must refuse
    /// rather than prove whichever it happened to assemble.
    #[test]
    fn two_traces_interleaved_into_one_stderr_are_refused() {
        let first = fixture();
        let second = first.replace("@ 0x1]", "@ 0x2]");
        let interleaved: String = first
            .lines()
            .zip(second.lines())
            .flat_map(|(a, b)| [format!("{a}\n"), format!("{b}\n")])
            .collect();
        let mut trace = Trace::default();
        trace.feed(interleaved.as_bytes());
        assert!(!trace.finish("source".into(), 0).permits("source"));
    }

    #[test]
    fn peer_proof_requires_full_digest_and_preserves_refusals() {
        let mut trace = Trace::default();
        trace.feed(fixture().as_bytes());
        let mut proof = trace.finish("peer-object".into(), 0);
        proof.source_node_id = "peer".into();
        assert!(!proof.permits_on_node("peer-object", "local"));
        let digest = "a".repeat(64);
        assert!(!proof.bind_equivalent_source(&digest, "local", "local-object"));
        proof.source_sha256 = Some(digest.clone());
        assert!(!proof.bind_equivalent_source(&"b".repeat(64), "local", "local-object"));
        assert!(proof.bind_equivalent_source(&digest, "local", "local-object"));
        assert!(proof.permits_on_node("local-object", "local"));
        proof.refusal = Some("changed PPS".into());
        assert!(proof.bind_equivalent_source(&digest, "another", "another-object"));
        assert!(!proof.permits_on_node("another-object", "another"));
    }
    #[test]
    fn trace_errors_and_oversized_lines_fail_closed() {
        for bad in [
            line("Invalid NAL unit, skipping"),
            // What `trace_headers` logs when a unit will not parse; with
            // `-xerror` gone from the index argv this is what refuses it.
            line("Failed to read unit 0 (type 33)."),
            line("0 unrecognized field"),
            "x".repeat(MAX_LINE + 1),
        ] {
            let mut trace = Trace::default();
            trace.feed(fixture().as_bytes());
            trace.feed(bad.as_bytes());
            trace.feed(fixture().as_bytes());
            assert!(!trace.finish("source".into(), 0).permits("source"));
        }
    }
    /// With `-xerror` gone, a damaged source is caught by message, and the
    /// muxer's correctable DTS warning is not mistaken for one.
    #[test]
    fn damaged_source_messages_are_faults_and_muxer_dts_is_not() {
        for text in [
            "[mov,mp4,m4a,3gp,3g2,mj2 @ 0x1] Packet corrupt (stream = 0, dts = 75328).\n",
            "[in#0/mov,mp4 @ 0x1] corrupt input packet in stream 0\n",
            "[vost#0:0/copy @ 0x1] Error applying bitstream filters to a packet\n",
            "[in#0/matroska @ 0x1] Error during demuxing: Invalid data\n",
            "in.mkv: Invalid data found when processing input\n",
            "in.mkv: Input/output error\n",
            "[out#0 @ 0x1] Error applying bitstream filters to an output packet for stream #0:0\n",
        ] {
            for split in [text.len(), 7] {
                let mut trace = Trace::default();
                trace.feed(fixture().as_bytes());
                trace.feed(&text.as_bytes()[..split.min(text.len())]);
                trace.feed(&text.as_bytes()[split.min(text.len())..]);
                assert!(trace.source_fault().is_some(), "{text}");
            }
        }
        // A fault glued onto a long unterminated line is still found, even
        // when it straddles the mid-line search boundary.
        for pad in [FAULT_CHUNK - 5, FAULT_CHUNK + 40, 5 * FAULT_CHUNK + 3] {
            let mut trace = Trace::default();
            trace.feed(fixture().as_bytes());
            trace.feed("x".repeat(pad).as_bytes());
            trace.feed(b"Packet corrupt (stream = 0, dts = 1).\n");
            assert!(trace.source_fault().is_some(), "pad {pad}");
        }
        let mut trace = Trace::default();
        trace.feed(fixture().as_bytes());
        trace.feed(
            b"[mp4 @ 0x1] Non-monotonic DTS in output stream 0:0; previous: 2000, current: 672; changing to 2001.\n",
        );
        assert!(trace.source_fault().is_none());
    }
    /// The index pass's stderr also carries FFmpeg's own output banner, and
    /// with one tee slave per PGS track that single line outgrows `MAX_LINE`
    /// (29 tracks under a 79-byte cache root printed 9,300 bytes). It is not
    /// trace output, so it must neither refuse the proof nor hide trace
    /// content that shares its line.
    #[test]
    fn oversized_lines_refuse_only_when_they_carry_trace_output() {
        let tee = format!(
            "Output #1, tee, to '{}[f=null]-':\n",
            "[select=0:f=sup:onfail=ignore]/cache/.stage-0/s0.sup|".repeat(MAX_LINE / 40)
        );
        let progress =
            "frame=  240 fps= 48 q=-1.0 size=N/A time=00:00:10.01 bitrate=N/A speed=2x    \r"
                .repeat(MAX_LINE / 40);
        for (foreign, chunk) in [
            (tee.as_str(), 7),
            (tee.as_str(), 4096),
            (progress.as_str(), 13),
        ] {
            let mut trace = Trace::default();
            let bytes = [fixture().as_str(), foreign, fixture().as_str()].concat();
            for part in bytes.as_bytes().chunks(chunk) {
                trace.feed(part);
            }
            assert!(trace.finish("source".into(), 0).permits("source"));
        }
        let long_value = line(&format!("0 pps_cb_qp_offset 0 = {}", "9".repeat(MAX_LINE)));
        let straddling = format!("{}{}", "y".repeat(MAX_LINE - 5), line("Extradata"));
        let buried = format!("{}{}", "z".repeat(MAX_LINE * 2), line("Extradata"));
        // A `[` that does not start the prefix must not hide the one that does.
        let restarted = format!("{}[{}", "z".repeat(MAX_LINE * 2), line("Extradata"));
        // FFmpeg drops the prefix after an unterminated message, so a field
        // can arrive glued to a foreign line with only its value to show.
        let glued = format!(
            "{}0 vps_video_parameter_set_id 0 = 1\n",
            "v".repeat(MAX_LINE)
        );
        let glued_negative = format!("{}8 pps_cb_qp_offset 0 = -1\n", "v".repeat(MAX_LINE * 2));
        for (bad, chunk) in [
            (long_value, 4096),
            (straddling.clone(), 7),
            (straddling, MAX_LINE * 4),
            (buried.clone(), 7),
            (buried, MAX_LINE * 4),
            (restarted, 7),
            (glued, 7),
            (glued_negative, 4096),
            ("w".repeat(MAX_LINE * 2), 13),
        ] {
            let mut trace = Trace::default();
            trace.feed(fixture().as_bytes());
            for part in bad.as_bytes().chunks(chunk) {
                trace.feed(part);
            }
            assert!(
                !trace.finish("source".into(), 0).permits("source"),
                "{}",
                &bad[..40]
            );
        }
    }
    #[test]
    fn absent_truncated_and_unresolved_are_not_proof() {
        for bytes in [
            String::new(),
            fixture().trim_end().into(),
            fixture().replace(
                "slice_pic_parameter_set_id 0 = 0",
                "slice_pic_parameter_set_id 0 = 2",
            ),
        ] {
            let mut trace = Trace::default();
            trace.feed(bytes.as_bytes());
            assert!(!trace.finish("source".into(), 0).permits("source"));
        }
    }
}
