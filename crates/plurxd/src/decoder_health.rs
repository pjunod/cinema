//! Whether a decode actually worked, decided from what FFmpeg said about it.
//!
//! A producer that emits segments and exits zero looks identical, from the
//! outside, to one that spent the whole film failing to decode and wrote green
//! frames. Both advance the playlist; both finish. The difference is in stderr,
//! and stderr was being logged and thrown away — which is why a cache can hold
//! a broken title indefinitely and every counter reads healthy.
//!
//! Reading it is not the hard part. The hard part is that a log line is not
//! evidence unless you know exactly which build produced it, because the same
//! failure is spelled differently across FFmpeg versions and only some of those
//! spellings say *which stream* failed. FFmpeg 5.1 reports
//! `Error while decoding stream #0:0` with no decoder context; FFmpeg 8 reports
//! it through `[vist#0:0/h264 @ …] [dec:h264 @ …]`. Acting automatically on the
//! first would mean acting on a guess about attribution, so this module refuses
//! to: a build whose grammar has not been qualified against a retained fixture
//! produces observations and no actions.
//!
//! ## Two tiers, and why there have to be two
//!
//! §7.2 draws a line this module implements literally. A line that has the
//! FFmpeg 8 *shape* — the `vist#` and `[dec:]` contexts, the selected stream,
//! the exact message — is a **structural** primary record: it counts toward the
//! window, it disqualifies the output from reuse, and it is worth logging. It
//! is not, on its own, permission to kill a session. For that the line must
//! additionally match a **versioned diagnostic contract**: this FFmpeg version,
//! this binary and buildconf, these log flags, this codec and decoder, the
//! severity label in the right place, the exact detail text, and the context
//! addresses when the contract demands them. "A tolerant structural match
//! without that external build receipt is observation only."
//!
//! The retained #913 capture is exactly why. Its records have the shape and
//! name the right stream, but FFmpeg 7.1.4 emitted them without severity
//! labels, so they are structural records that latch a fault and refuse a cache
//! receipt — and never an automatic action.
//!
//! ## Three rules do the rest of the work
//!
//! Each exists because of a specific way this could report a healthy stream as
//! broken:
//!
//! **Only the selected video stream counts.** An audio decoder failing, an
//! unselected video stream failing, the *encoder* failing, or a filename that
//! happens to contain the word `error` are all things that appear in the same
//! stream of text and none of them is a decode fault on the picture being
//! served.
//!
//! **A repeat summary disqualifies the attempt from automatic action.**
//! `Last message repeated 8 times` means the log was compressed: the times are
//! gone, so the sliding window cannot be evaluated honestly. Counting the
//! summary as one record understates; expanding it to eight invents timestamps
//! that were never observed. Neither is a basis for killing a viewer's session,
//! so the attempt is observed and never acted on — and the summary's
//! provenance is kept, because "repeated after the selected stream failed" and
//! "repeated after an audio line" are different facts and §7.2 asks for both.
//!
//! **Progress never clears a fault.** A decoder that fails and then recovers
//! enough to emit frames is still producing a film with holes in it, and the
//! frames arriving afterwards are exactly what made this invisible before.

// M3a lands the grammar, the accumulator and their whole test suite as one
// reviewable piece; M3b is what wires them into the producer. Until then
// nothing outside these tests calls any of it, which is the point of the split
// — the subtle logic (window boundary, repeat provenance, saturation) gets
// reviewed on its own, before it is threaded through a 36,000-line file.
#![allow(dead_code)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far back the accumulator looks. Errors that fall out of it stop
/// counting, so a title that fails once a minute for two hours never latches —
/// which is right, because that is a scratched disc, not a broken decoder.
pub const VIDEO_DECODE_ERROR_WINDOW: Duration = Duration::from_millis(2_000);

/// Primary errors inside the window that mean the decode is not working.
/// One through four are suspicion; the fifth is a decision.
pub const VIDEO_DECODE_ERROR_LIMIT: usize = 5;

/// How long a settling attempt waits for stderr to reach EOF after the child
/// exits. Bounded because a reader that never finishes must not hold a
/// publication open forever; what it produces instead is an incomplete
/// observation, which is a refusal to qualify rather than a hang.
pub const DIAGNOSTIC_DRAIN_BUDGET: Duration = Duration::from_millis(2_000);

/// The most of one line the reader retains. A malformed stream can emit
/// megabytes without a newline; keeping all of it would make memory a function
/// of the stream rather than of the parser.
pub const MAX_DIAGNOSTIC_LINE_BYTES: usize = 16 * 1024;

/// Automatic decoder actions for one recovery epoch — shared across the
/// initial retry and any later automatic replacement, not one per attempt.
/// One, so a failing plan is replaced once and not repeatedly.
pub const AUTOMATIC_PRODUCER_RECOVERY_LIMIT: u32 = 1;

/// The message that opens every qualified primary record. Matched as a literal
/// prefix rather than searched for anywhere in the line: `Error initializing
/// filters: Invalid data found when processing input` carries the contract's
/// detail text and the contract's contexts, and is not a decode failure.
const PRIMARY_MESSAGE: &str = "Error submitting packet to decoder:";

/// The subordinate FFmpeg emits after the failure it elaborates on.
const SUBORDINATE_MESSAGE: &str = "No frame decoded?";

/// What went wrong, at the granularity a recovery decision needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecodeFaultKind {
    /// The selected video stream failed to decode often enough, close enough
    /// together, that the output cannot be trusted.
    VideoDecodeFailure,
    /// The decode backend itself said it was finished — a fatal the caller
    /// does not need five samples to believe. Only a contract that names a
    /// backend-fault family can produce this: no M0 evidence qualifies one, so
    /// on the retained builds it is unreachable by construction.
    DecodeBackendUnavailable,
}

impl DecodeFaultKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::VideoDecodeFailure => "video_decode_failure",
            Self::DecodeBackendUnavailable => "decode_backend_unavailable",
        }
    }
}

/// What one line of stderr turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticRecord {
    /// A decode error with the qualified shape, on the selected video stream.
    ///
    /// `contract_qualified` is the second tier: the line additionally matched
    /// the versioned contract's severity, codec, decoder, detail and address
    /// requirements. A structural record still counts and still refuses a
    /// cache receipt; only a contract-qualified window may drive an action.
    PrimarySelectedVideoError { contract_qualified: bool },
    /// The same shape, on a stream this session is not decoding. Kept distinct
    /// from `Unrelated` because a repeat summary that follows one has
    /// unambiguous — and unrelated — provenance.
    OtherStreamPrimaryError,
    /// A detail line that elaborates on a primary error rather than being one
    /// — `No frame decoded?` follows the failure it describes. Counting it
    /// would double every fault.
    SubordinateDetail,
    /// The decode backend reporting its own terminal failure, in the exact
    /// wording a contract named.
    QualifiedFatalDecodeFailure,
    /// `Last message repeated N times`. Proof the log was compressed.
    RepeatSummary { times: u64 },
    /// A line that looks like something this grammar reads and is not: a
    /// repeat summary whose count is not a bounded integer, for instance.
    /// Unreadable input is not evidence of health, so it costs the attempt its
    /// completeness rather than being silently dropped.
    Malformed,
    /// Anything else: audio, another stage, the encoder, a filename, a
    /// warning, a blank line.
    Unrelated,
}

impl DiagnosticRecord {
    /// The provenance label a following repeat summary attaches itself to.
    fn provenance(&self) -> RepeatProvenance {
        match self {
            Self::PrimarySelectedVideoError { .. } => RepeatProvenance::SelectedPrimary,
            Self::RepeatSummary { .. } => RepeatProvenance::Ambiguous,
            Self::Malformed => RepeatProvenance::Ambiguous,
            Self::OtherStreamPrimaryError
            | Self::SubordinateDetail
            | Self::QualifiedFatalDecodeFailure
            | Self::Unrelated => RepeatProvenance::Other,
        }
    }
}

/// What the record before a repeat summary was, which is the only thing that
/// makes the summary attributable at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepeatProvenance {
    /// Nothing classified yet, or the previous line was itself a summary: the
    /// summary cannot be attributed to anything.
    Ambiguous,
    SelectedPrimary,
    Other,
}

/// One build's spelling of a decode failure, bound tightly enough that
/// matching it is evidence rather than a guess.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticContract {
    pub id: String,
    pub host: String,
    pub ffmpeg_version: String,
    pub binary_sha256: String,
    pub buildconf_sha256: String,
    pub stderr_mode: String,
    pub input_codec: String,
    pub decoder: String,
    pub require_context_addresses: bool,
    pub error_detail: String,
    pub fixture: String,
    pub fixture_sha256: String,
    /// Free text recording what the contract was qualified against. Carried so
    /// a reader cannot mistake a host-grammar qualification for a statement
    /// about the deployed producer.
    pub scope: String,
    /// The exact `[fatal]` detail this build emits when the decode backend
    /// itself is gone, when such a family has been qualified for it.
    ///
    /// Absent everywhere in M0, deliberately. `Decode error rate 1 exceeds
    /// maximum 0.666667` is FFmpeg abandoning a corrupt *input*, not a backend
    /// that has become unavailable, and swapping decoders cannot repair it —
    /// so no retained contract names a fatal family, and until one is
    /// qualified this grammar cannot emit a backend fault at all.
    #[serde(default)]
    pub backend_fault_detail: Option<String>,
}

/// The identity of the process that produced the diagnostics, as observed —
/// not as configured. Every field of it is compared, because every one of them
/// can change what a line means.
#[derive(Debug, Clone, Copy)]
pub struct ObservedBuild<'a> {
    pub ffmpeg_version: &'a str,
    pub binary_sha256: &'a str,
    pub buildconf_sha256: &'a str,
    /// The `-loglevel` value the argument builder actually emitted. A contract
    /// qualified under `repeat+level+error` says nothing about a producer
    /// launched with `error`: without `level` there are no severity labels, and
    /// without `repeat` the log is compressed.
    pub stderr_mode: &'a str,
    pub input_codec: &'a str,
    /// The decoder the plan named, which is the point of this whole effort: a
    /// contract for `h264` is not a contract for `h264_qsv`, and a grammar
    /// looking for `[dec:h264 @` sees nothing at all in a `[dec:h264_qsv @`
    /// stream — silently, which is the failure mode that matters.
    pub decoder: &'a str,
}

/// The retained contract table, as it appears on disk.
#[derive(Debug, serde::Deserialize)]
struct DiagnosticContractFile {
    version: u32,
    #[serde(default)]
    contracts: Vec<DiagnosticContract>,
}

/// The only table version this build understands.
pub const DIAGNOSTIC_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq)]
pub enum ContractLoadError {
    Malformed(String),
    UnsupportedVersion(u32),
    DuplicateId(String),
}

impl std::fmt::Display for ContractLoadError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Malformed(detail) => {
                write!(formatter, "diagnostic contracts are malformed: {detail}")
            }
            Self::UnsupportedVersion(version) => write!(
                formatter,
                "diagnostic contract table version {version} is not version {DIAGNOSTIC_CONTRACT_VERSION}"
            ),
            Self::DuplicateId(id) => write!(
                formatter,
                "diagnostic contract {id} is defined more than once"
            ),
        }
    }
}

impl DiagnosticContract {
    /// Parse the retained contract table.
    ///
    /// A version this build does not understand is a refusal, not a
    /// best-effort read: the table's meaning is what makes a match evidence,
    /// and reading a v2 table with v1 rules would produce confident matches
    /// against rules that had changed underneath.
    ///
    /// Two contracts sharing an id are refused for the same reason. Which of
    /// them a caller got would decide whether a session lives, and "whichever
    /// was listed first" is not a decision procedure.
    pub fn load(table: &str) -> Result<Vec<Self>, ContractLoadError> {
        let parsed: DiagnosticContractFile = toml::from_str(table)
            .map_err(|error| ContractLoadError::Malformed(error.to_string()))?;
        if parsed.version != DIAGNOSTIC_CONTRACT_VERSION {
            return Err(ContractLoadError::UnsupportedVersion(parsed.version));
        }
        for (index, contract) in parsed.contracts.iter().enumerate() {
            if parsed.contracts[..index]
                .iter()
                .any(|earlier| earlier.id == contract.id)
            {
                return Err(ContractLoadError::DuplicateId(contract.id.clone()));
            }
        }
        Ok(parsed.contracts)
    }

    /// Whether this contract describes the build that is actually running.
    ///
    /// Every field that can change what a line *means* has to match. A
    /// contract qualified against one binary says nothing about another binary
    /// that reports the same version string — distributions patch FFmpeg, and
    /// a patched build can change both the wording and the attribution. The
    /// same is true one level up: the same binary run with different log flags,
    /// or asked for a different decoder, produces a different grammar.
    pub fn covers_build(&self, build: &ObservedBuild<'_>) -> bool {
        self.ffmpeg_version == build.ffmpeg_version
            && self.binary_sha256 == build.binary_sha256
            && self.buildconf_sha256 == build.buildconf_sha256
            && self.stderr_mode == build.stderr_mode
            && self.input_codec == build.input_codec
            && self.decoder == build.decoder
    }
}

/// One `[…]` context at the head of a line, split into its name and the
/// address FFmpeg prints after it.
struct Context<'a> {
    name: &'a str,
    address: Option<&'a str>,
}

/// Take a leading `[…]` context and return it with the rest of the line.
///
/// A context name never contains `]` or `@`; the address, when present, is the
/// ` @ …` tail inside the same brackets. Returning `None` for anything else is
/// what keeps `[error] opening /media/green-error-name.mkv` from being read as
/// a context at all.
fn take_context(line: &str) -> Option<(Context<'_>, &str)> {
    let body = line.strip_prefix('[')?;
    let end = body.find(']')?;
    let (inside, rest) = body.split_at(end);
    let rest = &rest[1..];
    let (name, address) = match inside.split_once(" @ ") {
        Some((name, address)) if is_address(address) => (name, Some(address)),
        Some(_) => return None,
        None => (inside, None),
    };
    if name.contains('@') || name.is_empty() {
        return None;
    }
    Some((Context { name, address }, rest.trim_start()))
}

/// `0x…` as FFmpeg prints it, or the `<address>` the retained fixtures carry
/// in its place. Sanitizing a capture must not make it unreadable.
fn is_address(value: &str) -> bool {
    if value == "<address>" {
        return true;
    }
    match value.strip_prefix("0x") {
        Some(digits) => !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_hexdigit()),
        None => false,
    }
}

/// A line carrying full decode-stage attribution, taken apart.
struct AttributedLine<'a> {
    input: u32,
    stream: u32,
    codec: &'a str,
    decoder: &'a str,
    stream_address: bool,
    decoder_address: bool,
    severity: Option<&'a str>,
    message: &'a str,
}

/// `[vist#<input>:<stream>/<codec>[ @ addr]] [dec:<decoder>[ @ addr]] [<sev>] <message>`
fn parse_attributed(line: &str) -> Option<AttributedLine<'_>> {
    let (stream_context, rest) = take_context(line)?;
    let selector = stream_context.name.strip_prefix("vist#")?;
    let (indices, codec) = selector.split_once('/')?;
    let (input, stream) = indices.split_once(':')?;
    let input = input.parse().ok()?;
    let stream = stream.parse().ok()?;
    if codec.is_empty() {
        return None;
    }

    let (decoder_context, rest) = take_context(rest)?;
    let decoder = decoder_context.name.strip_prefix("dec:")?;
    if decoder.is_empty() {
        return None;
    }

    let (severity, message) = match take_context(rest) {
        Some((severity, remainder)) if is_severity(severity.name) => {
            (Some(severity.name), remainder)
        }
        _ => (None, rest),
    };

    Some(AttributedLine {
        input,
        stream,
        codec,
        decoder,
        stream_address: stream_context.address.is_some(),
        decoder_address: decoder_context.address.is_some(),
        severity,
        message,
    })
}

fn is_severity(name: &str) -> bool {
    matches!(
        name,
        "error" | "warning" | "fatal" | "info" | "verbose" | "debug" | "trace"
    )
}

/// Reads lines against one contract, for one selected stream.
#[derive(Debug, Clone)]
pub struct DiagnosticGrammar {
    contract: DiagnosticContract,
    /// The absolute input *file* and stream index being decoded, as FFmpeg
    /// spells them in `vist#<file>:<index>`. Both halves, because a session
    /// with an overlay or a concat has a second input and `0:0` would then
    /// attribute another file's failures to this picture.
    selected_input: u32,
    selected_stream: u32,
}

impl DiagnosticGrammar {
    pub fn new(contract: DiagnosticContract, selected_input: u32, selected_stream: u32) -> Self {
        Self {
            contract,
            selected_input,
            selected_stream,
        }
    }

    pub fn contract(&self) -> &DiagnosticContract {
        &self.contract
    }

    /// Classify one line.
    ///
    /// Deliberately conservative in one direction only: anything this cannot
    /// positively attribute to the selected video stream's decoder is
    /// `Unrelated`. A missed fault is a title that stays in the cache; a
    /// fabricated one is a viewer's session killed for an audio glitch.
    pub fn classify(&self, line: &str) -> DiagnosticRecord {
        let line = line.trim_end_matches(['\r', '\n']);
        let trimmed = line.trim_start();
        if trimmed.trim_end().is_empty() {
            return DiagnosticRecord::Unrelated;
        }

        // The repeat forms come first, and a line that opens like one and is
        // not one is malformed rather than ordinary: it is the one shape whose
        // misreading silently changes the window's denominator.
        match repeat_summary(trimmed) {
            RepeatMatch::Summary(times) => return DiagnosticRecord::RepeatSummary { times },
            RepeatMatch::Malformed => return DiagnosticRecord::Malformed,
            RepeatMatch::None => {}
        }

        if is_subordinate(trimmed) {
            return DiagnosticRecord::SubordinateDetail;
        }

        let Some(attributed) = parse_attributed(trimmed) else {
            return DiagnosticRecord::Unrelated;
        };

        if let Some(detail) = self.contract.backend_fault_detail.as_deref() {
            if attributed.severity == Some("fatal")
                && attributed.message.trim() == detail
                && self.is_selected(&attributed)
                && self.matches_contract_identity(&attributed)
            {
                return DiagnosticRecord::QualifiedFatalDecodeFailure;
            }
        }

        let Some(detail) = attributed.message.strip_prefix(PRIMARY_MESSAGE) else {
            return DiagnosticRecord::Unrelated;
        };
        if !self.is_selected(&attributed) {
            return DiagnosticRecord::OtherStreamPrimaryError;
        }
        DiagnosticRecord::PrimarySelectedVideoError {
            contract_qualified: attributed.severity == Some("error")
                && detail.trim() == self.contract.error_detail
                && self.matches_contract_identity(&attributed),
        }
    }

    fn is_selected(&self, attributed: &AttributedLine<'_>) -> bool {
        attributed.input == self.selected_input && attributed.stream == self.selected_stream
    }

    /// The build-bound half: the codec and decoder the contract was qualified
    /// for, and the context addresses when it says those are load-bearing.
    fn matches_contract_identity(&self, attributed: &AttributedLine<'_>) -> bool {
        attributed.codec.trim() == self.contract.input_codec
            && attributed.decoder.trim() == self.contract.decoder
            && (!self.contract.require_context_addresses
                || (attributed.stream_address && attributed.decoder_address))
    }
}

/// `[<name> @ addr] [error] No frame decoded?`, with either context part
/// optional in the ways FFmpeg actually varies them.
fn is_subordinate(line: &str) -> bool {
    let Some((_, rest)) = take_context(line) else {
        return false;
    };
    let rest = match take_context(rest) {
        Some((context, remainder)) if is_severity(context.name) => remainder,
        _ => rest,
    };
    rest.starts_with(SUBORDINATE_MESSAGE)
}

enum RepeatMatch {
    Summary(u64),
    /// Opened with the repeat wording and did not finish as a repeat summary.
    Malformed,
    None,
}

/// `Last message repeated N times`, in either of the shapes FFmpeg emits it.
///
/// Anchored, not searched. Searching finds it inside
/// `[error] opening /media/Last message repeated 8 times.mkv: I/O error`, and
/// a filename would then disqualify a title from automatic recovery forever.
fn repeat_summary(line: &str) -> RepeatMatch {
    const MARKER: &str = "Last message repeated ";
    let body = match take_context(line) {
        Some((context, rest)) if is_severity(context.name) => rest,
        _ => line,
    };
    let Some(rest) = body.strip_prefix(MARKER) else {
        return RepeatMatch::None;
    };
    let Some(digits) = rest.strip_suffix(" times") else {
        return RepeatMatch::Malformed;
    };
    // Bounded before parsing: an unbounded run of digits that overflows would
    // otherwise fall through as an ordinary line, and a compressed log would
    // read as an uncompressed one.
    if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return RepeatMatch::Malformed;
    }
    match digits.parse() {
        Ok(times) => RepeatMatch::Summary(times),
        Err(_) => RepeatMatch::Malformed,
    }
}

/// The running verdict for one attempt.
///
/// Latches are sticky by construction: there is no method that clears one.
/// That is the property §7.4 is about — a fault that arrives before a burst of
/// healthy progress must still be the answer when the attempt is classified,
/// and every previous version of this logic lost exactly that.
#[derive(Debug)]
pub struct HealthAccumulator {
    /// Each primary record's observation time, and whether it was
    /// contract-qualified. The flag travels with the timestamp because the
    /// question an action asks is about the *triggering window*, not about the
    /// attempt as a whole.
    window: VecDeque<(Instant, bool)>,
    primary_error_records: u64,
    contract_qualified_records: u64,
    fault: Option<DecodeFaultKind>,
    /// Whether every record in the window that latched the fault was
    /// contract-qualified. Set once, when the fault latches.
    triggering_window_contract_qualified: bool,
    selected_repeat_summaries: u64,
    selected_repeated_messages: u64,
    unrelated_repeat_summaries: u64,
    ambiguous_repeat_summaries: u64,
    malformed_lines: u64,
    oversized_lines: u64,
    invalid_utf8_lines: u64,
    /// Cleared by a reader that could not finish, or by input it could not
    /// read. An incomplete observation can never qualify an artifact, whatever
    /// it did or did not see.
    observation_complete: bool,
    previous: RepeatProvenance,
}

impl Default for HealthAccumulator {
    // Hand-written rather than derived: the derived value would start with
    // `observation_complete: false`, so a caller writing the idiomatic
    // `HealthAccumulator::default()` would get an accumulator that can never
    // qualify anything and never act, silently and with no error anywhere.
    fn default() -> Self {
        Self::new()
    }
}

impl HealthAccumulator {
    pub fn new() -> Self {
        Self {
            window: VecDeque::new(),
            primary_error_records: 0,
            contract_qualified_records: 0,
            fault: None,
            triggering_window_contract_qualified: false,
            selected_repeat_summaries: 0,
            selected_repeated_messages: 0,
            unrelated_repeat_summaries: 0,
            ambiguous_repeat_summaries: 0,
            malformed_lines: 0,
            oversized_lines: 0,
            invalid_utf8_lines: 0,
            observation_complete: true,
            previous: RepeatProvenance::Ambiguous,
        }
    }

    /// Feed one classified line. Returns the fault if this line is what
    /// latched it, so a caller can raise the barrier exactly once.
    pub fn observe(&mut self, at: Instant, record: &DiagnosticRecord) -> Option<DecodeFaultKind> {
        // Observation time is monotonic in production because it comes from
        // `Instant::now()` on one reader. Stated so that a caller feeding
        // constructed times sees the invariant break in a debug build rather
        // than getting a window whose entries are no longer time-sorted.
        debug_assert!(
            self.window.back().is_none_or(|(last, _)| at >= *last),
            "records are observed in the order they were read"
        );
        let latched = match record {
            DiagnosticRecord::RepeatSummary { times } => {
                match self.previous {
                    RepeatProvenance::SelectedPrimary => {
                        self.selected_repeat_summaries =
                            self.selected_repeat_summaries.saturating_add(1);
                        self.selected_repeated_messages =
                            self.selected_repeated_messages.saturating_add(*times);
                    }
                    RepeatProvenance::Ambiguous => {
                        self.ambiguous_repeat_summaries =
                            self.ambiguous_repeat_summaries.saturating_add(1);
                    }
                    RepeatProvenance::Other => {
                        self.unrelated_repeat_summaries =
                            self.unrelated_repeat_summaries.saturating_add(1);
                    }
                }
                None
            }
            DiagnosticRecord::Malformed => {
                self.malformed_lines = self.malformed_lines.saturating_add(1);
                self.observation_complete = false;
                None
            }
            DiagnosticRecord::QualifiedFatalDecodeFailure => {
                self.triggering_window_contract_qualified = true;
                self.latch(DecodeFaultKind::DecodeBackendUnavailable)
            }
            DiagnosticRecord::PrimarySelectedVideoError { contract_qualified } => {
                self.primary_error_records = self.primary_error_records.saturating_add(1);
                if *contract_qualified {
                    self.contract_qualified_records =
                        self.contract_qualified_records.saturating_add(1);
                }
                // The window is half-open: `(at - 2s, at]`. A record at exactly
                // the lower bound has aged out. Stated as a comparison on the
                // elapsed time rather than a subtraction so it cannot drift.
                while self.window.front().is_some_and(|(first, _)| {
                    at.saturating_duration_since(*first) >= VIDEO_DECODE_ERROR_WINDOW
                }) {
                    self.window.pop_front();
                }
                self.window.push_back((at, *contract_qualified));
                // Only the newest few can ever matter: if five records are in
                // the window then the five *most recent* are, because the
                // window is a suffix in time. Keeping more would let a child
                // flooding stderr decide how much memory the parser uses —
                // bounded by the error rate rather than by the parser, which
                // is the same failure in a slower form.
                while self.window.len() > VIDEO_DECODE_ERROR_LIMIT {
                    self.window.pop_front();
                }
                if self.window.len() >= VIDEO_DECODE_ERROR_LIMIT && self.fault.is_none() {
                    self.triggering_window_contract_qualified =
                        self.window.iter().all(|(_, qualified)| *qualified);
                    self.latch(DecodeFaultKind::VideoDecodeFailure)
                } else {
                    None
                }
            }
            DiagnosticRecord::OtherStreamPrimaryError
            | DiagnosticRecord::SubordinateDetail
            | DiagnosticRecord::Unrelated => None,
        };
        self.previous = record.provenance();
        latched
    }

    /// First fault wins. A backend fatal arriving after a decode failure does
    /// not relabel the attempt, and neither does the reverse: what matters
    /// downstream is that the attempt is faulted, and re-latching would emit a
    /// second barrier for one failure.
    fn latch(&mut self, kind: DecodeFaultKind) -> Option<DecodeFaultKind> {
        if self.fault.is_some() {
            return None;
        }
        self.fault = Some(kind);
        Some(kind)
    }

    pub fn fault(&self) -> Option<DecodeFaultKind> {
        self.fault
    }

    pub fn primary_error_records(&self) -> u64 {
        self.primary_error_records
    }

    pub fn contract_qualified_records(&self) -> u64 {
        self.contract_qualified_records
    }

    pub fn errors_in_window(&self) -> usize {
        self.window.len()
    }

    pub fn selected_repeat_summaries(&self) -> u64 {
        self.selected_repeat_summaries
    }

    pub fn selected_repeated_messages(&self) -> u64 {
        self.selected_repeated_messages
    }

    pub fn unrelated_repeat_summaries(&self) -> u64 {
        self.unrelated_repeat_summaries
    }

    pub fn ambiguous_repeat_summaries(&self) -> u64 {
        self.ambiguous_repeat_summaries
    }

    pub fn malformed_lines(&self) -> u64 {
        self.malformed_lines
    }

    pub fn invalid_utf8_lines(&self) -> u64 {
        self.invalid_utf8_lines
    }

    pub fn observation_complete(&self) -> bool {
        self.observation_complete
    }

    pub fn oversized_lines(&self) -> u64 {
        self.oversized_lines
    }

    /// Record that the diagnostic stream could not be read to its end.
    pub fn mark_observation_incomplete(&mut self) {
        self.observation_complete = false;
    }

    /// A line whose tail was discarded. The retained head may still classify,
    /// but the attempt no longer saw everything the child said, so it can
    /// neither qualify an artifact nor drive an action.
    pub fn note_oversized_line(&mut self) {
        self.oversized_lines = self.oversized_lines.saturating_add(1);
        self.observation_complete = false;
    }

    /// A line that was not valid UTF-8. It is still read lossily, because a
    /// decoder choking on a corrupt file sometimes prints the bytes it choked
    /// on and that is the diagnostic that matters — but the replacement
    /// characters mean the observation is no longer exact.
    pub fn note_invalid_utf8_line(&mut self) {
        self.invalid_utf8_lines = self.invalid_utf8_lines.saturating_add(1);
        self.observation_complete = false;
    }

    pub fn compressed_log(&self) -> bool {
        self.selected_repeat_summaries > 0
            || self.unrelated_repeat_summaries > 0
            || self.ambiguous_repeat_summaries > 0
    }

    /// Whether this attempt may drive an automatic decoder action.
    ///
    /// Requires a fault, a complete observation, an uncompressed log, and —
    /// the part that is easy to lose — a *triggering window* every record of
    /// which matched the versioned contract. §7.2: a tolerant structural match
    /// without the build receipt is observation only.
    ///
    /// Note what this is *not*: it is not "is the attempt healthy". An attempt
    /// can be disqualified from acting and still be disqualified from the
    /// cache — the two questions have different answers and conflating them is
    /// how an unqualified build ends up killing sessions.
    pub fn automatic_action_allowed(&self) -> bool {
        self.fault.is_some()
            && self.triggering_window_contract_qualified
            && !self.compressed_log()
            && self.observation_complete
    }

    /// Whether the observation permits calling the output reusable.
    ///
    /// Strictly stronger than the action question, and every clause earns its
    /// place. Complete, because an attempt that did not see the whole log
    /// cannot say the log was clean. Uncompressed, because `Last message
    /// repeated 36 times` is the shape the originating failure arrived in and
    /// reading it as silence is how a broken title got cached for months. No
    /// structural record and no fault, because §7.3 is explicit that isolated
    /// errors below the threshold still make the output unqualified.
    pub fn qualifies_reuse(&self) -> bool {
        self.observation_complete
            && !self.compressed_log()
            && self.fault.is_none()
            && self.primary_error_records == 0
    }
}

/// Reads a child's stderr into classified records without letting the child
/// decide how much memory that takes.
#[derive(Debug)]
pub struct BoundedDiagnosticReader {
    line: Vec<u8>,
    /// True while the current line has already exceeded the retention bound
    /// and its tail is being discarded. Draining continues — stopping would
    /// block the child on a full pipe, which turns a logging problem into a
    /// stalled encode.
    discarding: bool,
}

impl Default for BoundedDiagnosticReader {
    fn default() -> Self {
        Self::new()
    }
}

impl BoundedDiagnosticReader {
    pub fn new() -> Self {
        Self {
            line: Vec::with_capacity(256),
            discarding: false,
        }
    }

    /// Feed a chunk of bytes; returns the complete lines it contained.
    ///
    /// Non-UTF-8 is replaced rather than rejected: a decoder failing on a
    /// corrupt file sometimes prints the bytes it choked on, and refusing to
    /// read the line would lose the diagnostic that matters most. The
    /// substitution is recorded, because a lossy read is not an exact one.
    pub fn push(&mut self, chunk: &[u8], accumulator: &mut HealthAccumulator) -> Vec<String> {
        let mut lines = Vec::new();
        for &byte in chunk {
            if byte == b'\n' {
                lines.push(self.take_line(accumulator));
                continue;
            }
            if self.line.len() >= MAX_DIAGNOSTIC_LINE_BYTES {
                self.discarding = true;
                continue;
            }
            self.line.push(byte);
        }
        lines
    }

    /// Whatever was left when the stream ended without a final newline.
    pub fn finish(&mut self, accumulator: &mut HealthAccumulator) -> Option<String> {
        if self.line.is_empty() && !self.discarding {
            return None;
        }
        Some(self.take_line(accumulator))
    }

    fn take_line(&mut self, accumulator: &mut HealthAccumulator) -> String {
        if self.discarding {
            accumulator.note_oversized_line();
        }
        let line = match std::str::from_utf8(&self.line) {
            Ok(text) => text.to_owned(),
            Err(_) => {
                accumulator.note_invalid_utf8_line();
                String::from_utf8_lossy(&self.line).into_owned()
            }
        };
        self.line.clear();
        self.discarding = false;
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTRACTS: &str =
        include_str!("../../../tests/playback/decoder-health/diagnostic-contracts.toml");
    const QUALIFIED_FFMPEG_8: &str =
        include_str!("../../../tests/playback/decoder-health/qualified-ffmpeg-8.stderr");
    const UNQUALIFIED_FFMPEG_5: &str =
        include_str!("../../../tests/playback/decoder-health/unqualified-ffmpeg-5.stderr");
    const TOLERANT_CONTROLS: &str =
        include_str!("../../../tests/playback/decoder-health/tolerant-controls.stderr");
    const REPEAT_CONTROLS: &str =
        include_str!("../../../tests/playback/decoder-health/repeat-attribution-controls.stderr");
    const ISSUE_913_LEGACY: &str =
        include_str!("../../../tests/playback/decoder-health/issue-913-legacy.stderr");

    /// The retained fixtures are `<milliseconds>\t<line>`, so a test can replay
    /// real output at the offsets it was really observed at. Wall-clock sleeps
    /// would make these tests slow and flaky and would prove nothing extra.
    fn replay(fixture: &str, grammar: &DiagnosticGrammar) -> HealthAccumulator {
        let base = Instant::now();
        let mut accumulator = HealthAccumulator::new();
        for line in fixture.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let (offset, text) = line
                .split_once('\t')
                .expect("every fixture line carries its observation offset");
            let at = base + Duration::from_millis(offset.trim().parse().expect("offset"));
            accumulator.observe(at, &grammar.classify(text));
        }
        accumulator
    }

    fn contract(decoder: &str, detail: &str) -> DiagnosticContract {
        DiagnosticContract {
            id: format!("{decoder}-test"),
            host: "test".to_owned(),
            ffmpeg_version: "8.0.1-3ubuntu2".to_owned(),
            binary_sha256: "b".repeat(64),
            buildconf_sha256: "c".repeat(64),
            stderr_mode: "repeat+level+error".to_owned(),
            input_codec: decoder.to_owned(),
            decoder: decoder.to_owned(),
            require_context_addresses: true,
            error_detail: detail.to_owned(),
            fixture: "test".to_owned(),
            fixture_sha256: "d".repeat(64),
            scope: "test".to_owned(),
            backend_fault_detail: None,
        }
    }

    fn build(contract: &DiagnosticContract) -> ObservedBuild<'_> {
        ObservedBuild {
            ffmpeg_version: &contract.ffmpeg_version,
            binary_sha256: &contract.binary_sha256,
            buildconf_sha256: &contract.buildconf_sha256,
            stderr_mode: &contract.stderr_mode,
            input_codec: &contract.input_codec,
            decoder: &contract.decoder,
        }
    }

    fn h264() -> DiagnosticGrammar {
        DiagnosticGrammar::new(contract("h264", "corrupt input packet"), 0, 0)
    }

    const H264_PRIMARY: &str = "[vist#0:0/h264 @ 0x1] [dec:h264 @ 0x2] [error] Error submitting packet to decoder: corrupt input packet";

    fn primary(grammar: &DiagnosticGrammar, accumulator: &mut HealthAccumulator, at: Instant) {
        let record = grammar.classify(H264_PRIMARY);
        assert_eq!(
            record,
            DiagnosticRecord::PrimarySelectedVideoError {
                contract_qualified: true
            }
        );
        accumulator.observe(at, &record);
    }

    /// One through four are a scratched disc. The fifth inside two seconds is
    /// a decoder that is not working.
    #[test]
    fn four_errors_are_suspicion_and_the_fifth_is_a_decision() {
        let grammar = h264();
        let base = Instant::now();
        let mut accumulator = HealthAccumulator::new();
        for step in 0..4 {
            primary(
                &grammar,
                &mut accumulator,
                base + Duration::from_millis(step * 100),
            );
            assert_eq!(accumulator.fault(), None, "record {step} must not latch");
        }
        primary(
            &grammar,
            &mut accumulator,
            base + Duration::from_millis(400),
        );
        assert_eq!(
            accumulator.fault(),
            Some(DecodeFaultKind::VideoDecodeFailure)
        );
        assert_eq!(accumulator.primary_error_records(), 5);
        assert!(accumulator.automatic_action_allowed());
        assert!(!accumulator.qualifies_reuse());
    }

    /// The window is `(now - 2s, now]`. A record at exactly the lower bound has
    /// aged out — stated as a test because an off-by-one here is the difference
    /// between "five errors in two seconds" and "five errors, ever".
    #[test]
    fn the_lower_boundary_of_the_window_is_excluded() {
        let grammar = h264();
        let base = Instant::now();
        let mut accumulator = HealthAccumulator::new();
        for offset in [0, 300, 600, 900] {
            primary(
                &grammar,
                &mut accumulator,
                base + Duration::from_millis(offset),
            );
        }
        // Exactly 2000ms after the first record: the first has expired, so this
        // is the fourth in the window, not the fifth.
        primary(
            &grammar,
            &mut accumulator,
            base + Duration::from_millis(2_000),
        );
        assert_eq!(accumulator.fault(), None);
        assert_eq!(accumulator.errors_in_window(), 4);
        assert_eq!(
            accumulator.primary_error_records(),
            5,
            "but every record is still counted for the receipt"
        );
    }

    /// `No frame decoded?` follows the failure it describes. Counting it as a
    /// record of its own would halve the effective threshold.
    #[test]
    fn a_subordinate_detail_does_not_double_count_its_primary_error() {
        let grammar = h264();
        let subordinate = "[h264 @ 0x3] [error] No frame decoded?";
        assert_eq!(
            grammar.classify(subordinate),
            DiagnosticRecord::SubordinateDetail
        );
        let base = Instant::now();
        let mut accumulator = HealthAccumulator::new();
        for step in 0..4 {
            primary(
                &grammar,
                &mut accumulator,
                base + Duration::from_millis(step * 100),
            );
            // Through the parser, so that classification and counting are one
            // path: hand-feeding the record would leave a `classify` that
            // called this a primary error entirely undetected.
            let record = grammar.classify(subordinate);
            accumulator.observe(base + Duration::from_millis(step * 100 + 1), &record);
        }
        assert_eq!(
            accumulator.fault(),
            None,
            "eight lines, four errors, no decision"
        );
        assert_eq!(accumulator.primary_error_records(), 4);
    }

    /// The retained fatal is FFmpeg abandoning a corrupt input, not a backend
    /// that has gone away, and no M0 contract names a backend-fault family. It
    /// must therefore classify as nothing at all — a decoder swap cannot repair
    /// a file, and this is the line that would have asked for one.
    #[test]
    fn an_unqualified_fatal_is_not_a_backend_fault() {
        let grammar = DiagnosticGrammar::new(
            contract("rawvideo", "Invalid data found when processing input"),
            0,
            0,
        );
        let fatal = "[vist#0:0/rawvideo @ 0x1] [dec:rawvideo @ 0x2] [fatal] Decode error rate 1 exceeds maximum 0.666667";
        assert_eq!(grammar.classify(fatal), DiagnosticRecord::Unrelated);
        assert!(
            grammar.contract().backend_fault_detail.is_none(),
            "no retained contract qualifies a fatal family"
        );
    }

    /// A contract that does name one needs no corroboration for it: the
    /// backend saying it is finished is not a sampling question.
    #[test]
    fn a_contract_qualified_backend_fatal_latches_without_five_errors() {
        let mut qualified = contract("h264_qsv", "corrupt input packet");
        qualified.backend_fault_detail = Some("Device creation failed".to_owned());
        let grammar = DiagnosticGrammar::new(qualified, 0, 0);
        let fatal = "[vist#0:0/h264_qsv @ 0x1] [dec:h264_qsv @ 0x2] [fatal] Device creation failed";
        assert_eq!(
            grammar.classify(fatal),
            DiagnosticRecord::QualifiedFatalDecodeFailure
        );
        let mut accumulator = HealthAccumulator::new();
        let latched = accumulator.observe(Instant::now(), &grammar.classify(fatal));
        assert_eq!(latched, Some(DecodeFaultKind::DecodeBackendUnavailable));
        assert!(accumulator.automatic_action_allowed());
        assert_eq!(
            accumulator.primary_error_records(),
            0,
            "a fatal is not a sample of a rate"
        );
        // Another build's fatal wording, under the same contract, is not it.
        assert_eq!(
            grammar.classify(
                "[vist#0:0/h264_qsv @ 0x1] [dec:h264_qsv @ 0x2] [fatal] Decode error rate 1 exceeds maximum 0.666667"
            ),
            DiagnosticRecord::Unrelated
        );
    }

    /// Everything in the same stream of text that is not this decode failing —
    /// including the lines a looser matcher gets wrong, which are the ones that
    /// cost a viewer a session rather than costing a title a cache entry.
    #[test]
    fn other_streams_stages_and_stagewise_lookalikes_are_not_decode_faults() {
        let grammar = h264();
        for line in [
            "[aist#0:1/aac @ 0x1] [dec:aac @ 0x2] [error] Error submitting packet to decoder: invalid data",
            "[vost#0:0/h264 @ 0x1] [enc:h264_videotoolbox @ 0x2] [error] Error submitting frame to encoder",
            "[error] opening /media/green-error-name.mkv: I/O error",
            "[info] frame= 120 fps=24",
            "",
            // The whole reason the message is a literal prefix rather than a
            // search: this line carries the contract's contexts, its severity
            // and its exact detail text, and is a filter-graph failure.
            "[vist#0:0/h264 @ 0x1] [dec:h264 @ 0x2] [error] Error initializing filters: corrupt input packet",
            // And the reason the marker is anchored: a filename can say
            // anything at all.
            "[error] opening /media/Last message repeated 8 times.mkv: I/O error",
        ] {
            assert_eq!(grammar.classify(line), DiagnosticRecord::Unrelated, "{line}");
        }
        // Shaped like a primary record, on a stream this session is not
        // decoding: named, so a following repeat summary is attributable, and
        // never counted.
        assert_eq!(
            grammar.classify(
                "[vist#0:2/h264 @ 0x1] [dec:h264 @ 0x2] [error] Error submitting packet to decoder: corrupt input packet"
            ),
            DiagnosticRecord::OtherStreamPrimaryError
        );
    }

    /// The second tier. A line with the right shape on the right stream still
    /// counts and still refuses a cache receipt; without the build receipt it
    /// may not end a session.
    #[test]
    fn a_structural_match_without_the_build_receipt_is_observation_only() {
        let grammar = h264();
        let base = Instant::now();
        let mut accumulator = HealthAccumulator::new();
        // No severity label: the #913 shape, from a build that predates them.
        let unlabelled = "[vist#0:0/h264 @ 0x1] [dec:h264 @ 0x2] Error submitting packet to decoder: corrupt input packet";
        assert_eq!(
            grammar.classify(unlabelled),
            DiagnosticRecord::PrimarySelectedVideoError {
                contract_qualified: false
            }
        );
        for step in 0..5 {
            accumulator.observe(
                base + Duration::from_millis(step * 100),
                &grammar.classify(unlabelled),
            );
        }
        assert_eq!(
            accumulator.fault(),
            Some(DecodeFaultKind::VideoDecodeFailure),
            "the fault is observed"
        );
        assert!(
            !accumulator.automatic_action_allowed(),
            "and never acted on"
        );
        assert!(!accumulator.qualifies_reuse());
        assert_eq!(accumulator.contract_qualified_records(), 0);

        // Each remaining contract field, alone, is enough to withdraw the
        // receipt while leaving the structural record intact.
        for line in [
            // wrong detail
            "[vist#0:0/h264 @ 0x1] [dec:h264 @ 0x2] [error] Error submitting packet to decoder: something else",
            // wrong decoder — the case this whole effort creates
            "[vist#0:0/h264 @ 0x1] [dec:h264_qsv @ 0x2] [error] Error submitting packet to decoder: corrupt input packet",
            // wrong codec
            "[vist#0:0/hevc @ 0x1] [dec:h264 @ 0x2] [error] Error submitting packet to decoder: corrupt input packet",
            // addresses the contract requires, missing
            "[vist#0:0/h264] [dec:h264] [error] Error submitting packet to decoder: corrupt input packet",
        ] {
            assert_eq!(
                grammar.classify(line),
                DiagnosticRecord::PrimarySelectedVideoError {
                    contract_qualified: false
                },
                "{line}"
            );
        }
    }

    /// A decoder that fails and then emits frames again is still producing a
    /// film with holes in it — and the frames arriving afterwards are exactly
    /// what made this invisible before.
    #[test]
    fn progress_and_success_never_clear_a_latched_fault() {
        let grammar = h264();
        let base = Instant::now();
        let mut accumulator = HealthAccumulator::new();
        for step in 0..5 {
            primary(
                &grammar,
                &mut accumulator,
                base + Duration::from_millis(step * 100),
            );
        }
        assert_eq!(
            accumulator.fault(),
            Some(DecodeFaultKind::VideoDecodeFailure)
        );
        for later in 0..50 {
            accumulator.observe(
                base + Duration::from_secs(60 + later),
                &DiagnosticRecord::Unrelated,
            );
        }
        assert_eq!(
            accumulator.fault(),
            Some(DecodeFaultKind::VideoDecodeFailure),
            "an hour of healthy output does not un-break the first two seconds"
        );
    }

    /// A compressed log has lost the times the window is evaluated against,
    /// and *which* record was compressed is a different fact from *that* one
    /// was. The retained control fixture places a summary in all five
    /// positions precisely so a bool cannot pass for an answer.
    #[test]
    fn repeat_summaries_keep_their_provenance_and_never_become_an_action() {
        let grammar = h264();
        let accumulator = replay(REPEAT_CONTROLS, &grammar);
        assert_eq!(
            accumulator.selected_repeat_summaries(),
            1,
            "one summary follows the selected stream's own failure"
        );
        assert_eq!(
            accumulator.selected_repeated_messages(),
            6,
            "and it says how many times, without inventing six timestamps"
        );
        assert_eq!(
            accumulator.unrelated_repeat_summaries(),
            2,
            "one after the audio failure, one after the subordinate line"
        );
        assert_eq!(
            accumulator.ambiguous_repeat_summaries(),
            2,
            "the leading summary, and the summary that follows a summary"
        );
        assert!(accumulator.compressed_log());
        assert!(!accumulator.automatic_action_allowed());
        assert!(
            !accumulator.qualifies_reuse(),
            "a compressed log is not a clean one"
        );
        assert!(
            accumulator.observation_complete(),
            "the log was compressed, not truncated"
        );
    }

    /// A line that opens like a repeat summary and does not finish like one is
    /// unreadable, not ordinary. Falling through to `Unrelated` would report a
    /// compressed stream as an uncompressed one.
    #[test]
    fn a_repeat_marker_without_a_bounded_count_is_malformed() {
        let grammar = h264();
        for line in [
            "Last message repeated many times",
            "[error] Last message repeated 999999999999999999999999 times",
            "Last message repeated 8 tim",
        ] {
            assert_eq!(
                grammar.classify(line),
                DiagnosticRecord::Malformed,
                "{line}"
            );
        }
        let mut accumulator = HealthAccumulator::new();
        accumulator.observe(Instant::now(), &DiagnosticRecord::Malformed);
        assert_eq!(accumulator.malformed_lines(), 1);
        assert!(
            !accumulator.observation_complete(),
            "input this parser cannot read is not evidence that the decode was fine"
        );
        assert!(!accumulator.qualifies_reuse());
    }

    /// Memory has to be a function of the parser, not of what the child
    /// decided to print — and a stream that overran the bound was not fully
    /// observed, whatever the retained head of it said.
    #[test]
    fn long_lines_are_bounded_and_a_truncated_stream_cannot_qualify() {
        let mut accumulator = HealthAccumulator::new();
        let mut reader = BoundedDiagnosticReader::new();
        let mut chunk = vec![b'x'; MAX_DIAGNOSTIC_LINE_BYTES * 3];
        chunk.push(b'\n');
        let lines = reader.push(&chunk, &mut accumulator);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), MAX_DIAGNOSTIC_LINE_BYTES);
        assert_eq!(accumulator.oversized_lines(), 1);
        assert!(!accumulator.observation_complete());
        assert!(
            !accumulator.qualifies_reuse(),
            "the tail that was discarded is exactly where the failure would have been"
        );

        let mut accumulator = HealthAccumulator::new();
        let lines = reader.push(b"[error] \xff\xfe not utf-8\n", &mut accumulator);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("not utf-8"), "read lossily, not dropped");
        assert_eq!(accumulator.invalid_utf8_lines(), 1);
        assert!(!accumulator.qualifies_reuse());

        // A line split across reads is still one line.
        let mut accumulator = HealthAccumulator::new();
        assert!(reader
            .push(b"[vist#0:0/h264 @ 0x1] ", &mut accumulator)
            .is_empty());
        let lines = reader.push(b"[dec:h264 @ 0x2] [error] x\n", &mut accumulator);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("[vist#0:0/h264"));
        assert!(lines[0].ends_with("[error] x"));
        assert!(
            accumulator.observation_complete(),
            "a rejoined line is not a truncated one"
        );
    }

    /// A stream that ends without a trailing newline still yields its last
    /// line — which on a crashing child is often the only one that matters.
    #[test]
    fn a_final_line_without_a_newline_is_not_lost() {
        let mut accumulator = HealthAccumulator::new();
        let mut reader = BoundedDiagnosticReader::new();
        assert!(reader
            .push(b"[fatal] the last thing it said", &mut accumulator)
            .is_empty());
        assert_eq!(
            reader.finish(&mut accumulator).as_deref(),
            Some("[fatal] the last thing it said")
        );
        assert_eq!(reader.finish(&mut accumulator), None);
    }

    /// One barrier per attempt, however loud the failure is.
    #[test]
    fn a_flood_latches_once_and_keeps_counting() {
        let grammar = h264();
        let base = Instant::now();
        let mut accumulator = HealthAccumulator::new();
        let mut latches = 0;
        for step in 0..10_000u64 {
            // Through the parser, not around it: a flood has to stay bounded
            // in classification as well as in accounting.
            let record = grammar.classify(H264_PRIMARY);
            if accumulator
                .observe(base + Duration::from_millis(step), &record)
                .is_some()
            {
                latches += 1;
            }
        }
        assert_eq!(latches, 1, "one failure, one barrier");
        assert_eq!(accumulator.primary_error_records(), 10_000);
        assert!(
            accumulator.errors_in_window() <= VIDEO_DECODE_ERROR_LIMIT,
            "the window is bounded, so memory does not grow with the stream"
        );
    }

    /// An incomplete observation cannot qualify anything, whatever it saw.
    #[test]
    fn an_unfinished_reader_cannot_qualify_an_artifact() {
        let mut accumulator = HealthAccumulator::new();
        assert!(
            accumulator.qualifies_reuse(),
            "a clean complete read qualifies"
        );
        accumulator.mark_observation_incomplete();
        assert!(!accumulator.qualifies_reuse());
        assert!(!accumulator.automatic_action_allowed());
    }

    /// `HealthAccumulator::default()` is the idiomatic thing for M3b to write,
    /// and the derived value would be an accumulator that can never qualify
    /// anything and never act — silently, with no error anywhere.
    #[test]
    fn the_default_accumulator_is_the_one_new_returns() {
        let defaulted = HealthAccumulator::default();
        assert!(defaulted.observation_complete());
        assert!(defaulted.qualifies_reuse());
        assert_eq!(defaulted.primary_error_records(), 0);
    }

    /// The real thing: output this exact build actually produced, replayed at
    /// the offsets it was actually observed at. A fake process proves lifecycle
    /// behaviour; only a retained fixture proves decoder semantics.
    #[test]
    fn the_retained_qualified_fixture_latches_on_its_fifth_record() {
        let contracts = DiagnosticContract::load(CONTRACTS).expect("retained contracts parse");
        let contract = contracts
            .iter()
            .find(|contract| contract.input_codec == "rawvideo")
            .expect("the retained rawvideo contract")
            .clone();
        assert_eq!(contract.ffmpeg_version, "8.0.1-3ubuntu2");
        assert_eq!(
            contract.fixture, "qualified-ffmpeg-8.stderr",
            "the contract names the fixture this test replays"
        );
        let accumulator = replay(QUALIFIED_FFMPEG_8, &DiagnosticGrammar::new(contract, 0, 0));
        assert_eq!(
            accumulator.fault(),
            Some(DecodeFaultKind::VideoDecodeFailure),
            "five attributed records inside the window"
        );
        assert_eq!(accumulator.primary_error_records(), 5);
        assert_eq!(
            accumulator.contract_qualified_records(),
            5,
            "and every one of them carried the build receipt"
        );
        assert!(accumulator.automatic_action_allowed());
        assert!(!accumulator.qualifies_reuse());
    }

    /// The capture this whole effort was created from. It latches, it refuses
    /// the cache, and it never acts: FFmpeg 7.1.4 wrote no severity labels and
    /// compressed thirty-six records into one line, so neither the build
    /// receipt nor the window's timing survives.
    #[test]
    fn the_originating_capture_is_diagnosed_and_never_certified_reusable() {
        let grammar = DiagnosticGrammar::new(contract("mpeg4", "Unknown error occurred"), 0, 0);
        let accumulator = replay(ISSUE_913_LEGACY, &grammar);
        assert_eq!(
            accumulator.fault(),
            Some(DecodeFaultKind::VideoDecodeFailure),
            "five attributed records on the selected stream"
        );
        assert_eq!(
            accumulator.contract_qualified_records(),
            0,
            "none of them carries a severity label"
        );
        assert!(accumulator.compressed_log());
        assert!(
            !accumulator.automatic_action_allowed(),
            "an unqualified grammar observes and does not act"
        );
        assert!(
            !accumulator.qualifies_reuse(),
            "and the output of this attempt is exactly what must not be cached"
        );
    }

    /// The build the fleet actually runs does not attribute this failure to a
    /// stream at all, so nothing in its output may become an action.
    #[test]
    fn the_deployed_unqualified_build_produces_no_attributed_fault() {
        let grammar = DiagnosticGrammar::new(
            contract("rawvideo", "Invalid data found when processing input"),
            0,
            0,
        );
        let accumulator = replay(UNQUALIFIED_FFMPEG_5, &grammar);
        assert_eq!(
            accumulator.fault(),
            None,
            "`Error while decoding stream #0:0` names no decoder context"
        );
        assert_eq!(accumulator.primary_error_records(), 0);
        assert!(
            UNQUALIFIED_FFMPEG_5.contains("Error while decoding stream #0:0"),
            "the fixture really does carry a decode failure"
        );
        assert!(
            accumulator.qualifies_reuse(),
            "which this grammar cannot see — the point of the two tiers, and \
             the reason enabling actions on this build is refused elsewhere"
        );
    }

    /// Five records on the selected stream, spread so that only four are ever
    /// in the window, mixed with every kind of line that must not count.
    #[test]
    fn the_tolerant_controls_fixture_never_latches() {
        let accumulator = replay(TOLERANT_CONTROLS, &h264());
        assert_eq!(accumulator.fault(), None);
        assert_eq!(
            accumulator.primary_error_records(),
            5,
            "five attributed records were seen"
        );
        assert_eq!(accumulator.errors_in_window(), 4, "but never five at once");
        assert!(!accumulator.qualifies_reuse(), "five is not zero");
    }

    /// A table this build does not understand is a refusal. Reading a later
    /// version with these rules would produce confident matches against rules
    /// that had changed underneath.
    #[test]
    fn an_unsupported_or_ambiguous_contract_table_is_refused() {
        let table = CONTRACTS.replacen("version = 1", "version = 2", 1);
        assert_eq!(
            DiagnosticContract::load(&table),
            Err(ContractLoadError::UnsupportedVersion(2))
        );
        assert!(matches!(
            DiagnosticContract::load("version = 1\n[[contracts]]\nid = 1\n"),
            Err(ContractLoadError::Malformed(_))
        ));
        assert!(
            matches!(
                DiagnosticContract::load(
                    "version = 1\n[[contracts]]\nid = \"x\"\nunexpected = \"y\"\n"
                ),
                Err(ContractLoadError::Malformed(_))
            ),
            "a field this build does not know about is not silently ignored"
        );
        let (header, entry) = CONTRACTS
            .split_once("[[contracts]]")
            .expect("the retained table has one entry");
        let doubled = format!("{header}[[contracts]]{entry}\n[[contracts]]{entry}");
        assert_eq!(
            DiagnosticContract::load(&doubled),
            Err(ContractLoadError::DuplicateId(
                "ffmpeg-8.0.1-3ubuntu2-rawvideo-v1".to_owned()
            )),
            "which of two contracts for one id applies is not 'whichever was listed first'"
        );
    }

    /// A contract is evidence about one build, and every field of that build
    /// can change what a line means. The decoder is the one this effort
    /// changes on purpose, and a contract that still claimed to cover the
    /// build afterwards would report a silent all-clear forever.
    #[test]
    fn a_contract_covers_one_exact_build_and_one_exact_grammar() {
        let contract = contract("h264", "detail");
        assert!(contract.covers_build(&build(&contract)));
        for mutate in [
            |build: &mut ObservedBuild<'_>| build.ffmpeg_version = "8.0.2",
            |build: &mut ObservedBuild<'_>| build.binary_sha256 = "e",
            |build: &mut ObservedBuild<'_>| build.buildconf_sha256 = "e",
            |build: &mut ObservedBuild<'_>| build.stderr_mode = "error",
            |build: &mut ObservedBuild<'_>| build.input_codec = "hevc",
            |build: &mut ObservedBuild<'_>| build.decoder = "h264_qsv",
        ] {
            let mut observed = build(&contract);
            mutate(&mut observed);
            assert!(
                !contract.covers_build(&observed),
                "{observed:?} is not the build this contract was qualified against"
            );
        }
    }

    /// The second input a session gains from an overlay or a concat is a
    /// different picture, and `0:0` would attribute its failures to this one.
    #[test]
    fn the_input_file_index_is_part_of_the_attribution() {
        let grammar = DiagnosticGrammar::new(contract("h264", "corrupt input packet"), 1, 0);
        assert_eq!(
            grammar.classify(H264_PRIMARY),
            DiagnosticRecord::OtherStreamPrimaryError,
            "input 0 is not input 1"
        );
        assert_eq!(
            grammar.classify(
                "[vist#1:0/h264 @ 0x1] [dec:h264 @ 0x2] [error] Error submitting packet to decoder: corrupt input packet"
            ),
            DiagnosticRecord::PrimarySelectedVideoError {
                contract_qualified: true
            }
        );
    }
}
