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

// The receipt and its vocabulary live in `plurx-core`, because the generation
// manifest carries them and the manifest is core's. Re-exported here so every
// caller keeps naming them where they are produced.
pub use plurx_core::transcode::health::{
    DecodeFaultKind, ExitDisposition, ProducerHealthReceipt, Qualification,
    MAX_DIAGNOSTIC_CONTRACT_BYTES, PRODUCER_HEALTH_RECEIPT_VERSION,
};

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

// The message that opens a primary record used to be a constant here. It is a
// contract field now, and the reason is not hypothetical: FFmpeg 9 does not
// contain the string `Error submitting packet to decoder` at all — the binary's
// only decode-error format is `Decoding error: %s`. A grammar carrying the
// FFmpeg 8 wording matches nothing on FFmpeg 9, and a grammar that matches
// nothing reports every stream as clean, which is the exact failure this whole
// effort exists to prevent, reached by an ordinary upgrade.
//
// Everything else about a build was already a contract field. The wording is
// no different in kind, and it was the one that was not.

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
    /// The literal prefix that opens a primary record on this build.
    ///
    /// Matched as a prefix rather than searched for anywhere in the line:
    /// `Error initializing filters: Invalid data found when processing input`
    /// carries the contract's detail text and the contract's contexts, and is
    /// not a decode failure.
    pub primary_message: String,
    /// The detail line this build emits after the failure it elaborates on,
    /// when it emits one. Counting it would double every fault.
    ///
    /// `None` for a build that emits none, which is the honest state rather
    /// than a default: FFmpeg 8 prints `No frame decoded?` and FFmpeg 9 does
    /// not print it at all.
    #[serde(default)]
    pub subordinate_message: Option<String>,
    /// Whether this build was shown to attribute *every* failed packet.
    ///
    /// The windowed rule counts records inside two seconds, which only means
    /// what it says on a build that emits one per failure. FFmpeg 8 was
    /// measured doing exactly that: five records, five failures. FFmpeg 9's
    /// attributed record count for a corrupted h264 source was measured across
    /// several inputs at zero, one and two — so it announces far fewer records
    /// than there are failed packets, and five inside two seconds is not
    /// something that build has been shown to produce.
    ///
    /// Deliberately a statement about what was measured, not a claim that the
    /// build *cannot* emit five. It gates the automatic action, never the
    /// latch: a burst that does occur still faults, still raises the barrier's
    /// evidence and still refuses the artifact. What an unqualified build may
    /// not do is kill a session on the strength of a rule nobody qualified for
    /// it.
    pub attributes_every_failure: bool,
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

/// The contract table this build was tested against, compiled in.
///
/// Embedded rather than read from disk on purpose. A contract is evidence
/// about one binary, and the evidence a node applies has to be the evidence
/// its own build was qualified with — a file beside the daemon can be edited,
/// can go missing, and can describe a build nobody ran.
pub const RETAINED_DIAGNOSTIC_CONTRACTS: &str =
    include_str!("../../../tests/playback/decoder-health/diagnostic-contracts.toml");

/// The only table version this build understands.
pub const DIAGNOSTIC_CONTRACT_VERSION: u32 = 2;

#[derive(Debug, PartialEq, Eq)]
pub enum ContractLoadError {
    Malformed(String),
    UnsupportedVersion(u32),
    DuplicateId(String),
    /// An identifier a published manifest could not carry.
    UnsafeId(String),
    /// A matcher that would match everything, or nothing it should.
    UnusableMatcher(String),
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
            Self::UnusableMatcher(id) => write!(
                formatter,
                "diagnostic contract {id} carries a message matcher that is empty or padded; \
                 an empty prefix matches every line ever printed, and a padded one matches \
                 none — which certifies every broken stream as clean"
            ),
            Self::UnsafeId(id) => write!(
                formatter,
                "diagnostic contract id {id:?} is not one a generation manifest can carry: \
                 at most {MAX_DIAGNOSTIC_CONTRACT_BYTES} bytes of ASCII alphanumerics, \
                 `-`, `_` or `.`"
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
    ///
    /// An id a published manifest could not carry is refused here, where the
    /// table is authored, rather than at publication. A receipt naming it
    /// would be dropped on the way into the generation manifest — so a fleet
    /// whose table used, say, a Debian epoch (`ffmpeg-7:6.1.1-…`) would
    /// publish every generation uncertified with nothing but a log line to
    /// say why. The bound belongs to the identifier, so it is checked where
    /// the identifier is introduced.
    pub fn load(table: &str) -> Result<Vec<Self>, ContractLoadError> {
        let parsed: DiagnosticContractFile = toml::from_str(table)
            .map_err(|error| ContractLoadError::Malformed(error.to_string()))?;
        if parsed.version != DIAGNOSTIC_CONTRACT_VERSION {
            return Err(ContractLoadError::UnsupportedVersion(parsed.version));
        }
        for (index, contract) in parsed.contracts.iter().enumerate() {
            if !plurx_core::transcode::health::safe_diagnostic_contract_id(&contract.id) {
                return Err(ContractLoadError::UnsafeId(contract.id.clone()));
            }
            // A build that prints no subordinate line says so by omitting the
            // key. Spelling it as an empty string would be a prefix that
            // matches every line, which would classify the whole stream as
            // subordinate detail and count nothing at all.
            // Empty is the obvious one and the *safe* one: an empty prefix
            // over-matches, which is noisy. Padded is the dangerous one, and
            // it is one stray character away in a hand-written file. A leading
            // space makes `strip_prefix` fail against a message this reader
            // has already trimmed, so the grammar matches nothing — and a
            // grammar that matches nothing certifies every broken stream as
            // clean, which is the failure this whole effort exists to prevent.
            let usable = |message: &str| !message.is_empty() && message == message.trim();
            if !usable(&contract.primary_message)
                || contract
                    .subordinate_message
                    .as_deref()
                    .is_some_and(|message| !usable(message))
            {
                return Err(ContractLoadError::UnusableMatcher(contract.id.clone()));
            }
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

    /// The absolute input stream index this grammar attributes failures to.
    pub fn selected_stream(&self) -> u32 {
        self.selected_stream
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

        if is_subordinate(trimmed, self.contract.subordinate_message.as_deref()) {
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

        let Some(detail) = attributed
            .message
            .strip_prefix(self.contract.primary_message.as_str())
        else {
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
    /// Whether this build attributes every failed packet, and therefore
    /// whether the windowed fault rule can fire against it at all.
    pub fn attributes_every_failure(&self) -> bool {
        self.contract.attributes_every_failure
    }

    fn matches_contract_identity(&self, attributed: &AttributedLine<'_>) -> bool {
        attributed.codec.trim() == self.contract.input_codec
            && attributed.decoder.trim() == self.contract.decoder
            && (!self.contract.require_context_addresses
                || (attributed.stream_address && attributed.decoder_address))
    }
}

/// `[<name> @ addr] [error] <the build's subordinate wording>`, with either
/// context part optional in the ways FFmpeg actually varies them.
///
/// A build that emits no subordinate line has none to recognize, and matching
/// nothing is the right answer for it — not a default wording it never prints.
fn is_subordinate(line: &str, subordinate: Option<&str>) -> bool {
    let Some(subordinate) = subordinate else {
        return false;
    };
    let Some((_, rest)) = take_context(line) else {
        return false;
    };
    let rest = match take_context(rest) {
        Some((context, remainder)) if is_severity(context.name) => remainder,
        _ => rest,
    };
    rest.starts_with(subordinate)
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
    /// Cleared when no contract covered the build that produced this stream.
    /// The lines are still read and still logged; none of them is evidence,
    /// so the attempt cannot certify its own output.
    grammar_available: bool,
    /// Whether this build's attribution frequency has been qualified for the
    /// windowed rule.
    ///
    /// The rule counts records inside two seconds, which only means what it
    /// says on a build that emits one per failed packet. FFmpeg 8 does. FFmpeg
    /// 9's attributed record count for a corrupted source was measured at zero,
    /// one and two depending on the input — so five inside two seconds is not
    /// something that build has been shown to produce, and a window of five
    /// there is not the evidence the rule was designed around.
    ///
    /// It gates the *action*, not the latch. Refusing to latch would be the
    /// worse trade: a genuinely burst-failing stream would then leave no fault,
    /// no barrier, and a receipt saying `Unqualified` with no terminal fault
    /// where it should say `Rejected` — losing the strongest thing the
    /// observation had to say. Latching still refuses the artifact; what an
    /// unqualified build may not do is kill a session over it.
    windowed_action_qualified: bool,
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
    /// A reader with no grammar. The safe default, because it is the answer
    /// for every build nobody has qualified — and because a caller that
    /// forgets to say otherwise must not be handed an accumulator willing to
    /// certify a stream it could not read.
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
            grammar_available: false,
            // True with no contract, because that is what this accumulator has
            // always done and a latch without a grammar drives nothing:
            // `automatic_action_allowed` already gates the action on the
            // grammar. Only a contract that says its build cannot support the
            // rule turns it off, and only for the qualified path.
            windowed_action_qualified: true,
            previous: RepeatProvenance::Ambiguous,
        }
    }

    /// An accumulator reading a stream a versioned contract covers.
    ///
    /// The only constructor that can produce a qualifying observation, so the
    /// grammar and the permission to certify are one decision rather than two
    /// that can drift.
    pub fn with_qualified_grammar(windowed_action_qualified: bool) -> Self {
        Self {
            grammar_available: true,
            windowed_action_qualified,
            ..Self::new()
        }
    }

    /// Whether this build's attribution frequency was qualified for the
    /// windowed rule, and therefore whether a window of five here is the
    /// evidence that rule was designed around.
    pub fn windowed_action_qualified(&self) -> bool {
        self.windowed_action_qualified
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

    /// Whether a contract covered the build this stream came from.
    ///
    /// Distinct from an incomplete observation: the stream may have been read
    /// perfectly. What is missing is the grammar that would make any of it
    /// mean something, and an attempt whose output nobody could read cannot
    /// certify that output as clean.
    pub fn grammar_available(&self) -> bool {
        self.grammar_available
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
            && self.windowed_action_qualified
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
    /// A build no contract covers is the last clause, and it is the one that
    /// keeps this honest on the fleet as it is deployed today: FFmpeg 5.1.9
    /// prints its decode failures without naming a stream, so a title that
    /// fails on it produces a clean-looking stream and must still not be
    /// certified from it.
    pub fn qualifies_reuse(&self) -> bool {
        self.observation_complete
            && self.grammar_available
            && !self.compressed_log()
            && self.fault.is_none()
            && self.primary_error_records == 0
    }

    /// Settle this observation into the receipt the manifest carries.
    ///
    /// The derivation lives on the accumulator, not on the receipt, because
    /// the accumulator is the only thing that saw the stream. `plurx-core`
    /// owns the receipt's *shape* so a cache reader can authenticate it; it
    /// deliberately owns no way to decide that an attempt was clean.
    pub fn settle_receipt(
        &self,
        plan_digest: String,
        diagnostic_contract: Option<String>,
        exit_disposition: ExitDisposition,
    ) -> ProducerHealthReceipt {
        let qualification = if self.fault.is_some() {
            Qualification::Rejected
        } else if exit_disposition == ExitDisposition::FailedTermination || !self.qualifies_reuse()
        {
            Qualification::Unqualified
        } else {
            Qualification::Qualified
        };
        ProducerHealthReceipt {
            receipt_version: PRODUCER_HEALTH_RECEIPT_VERSION,
            plan_digest,
            diagnostic_contract,
            observation_complete: self.observation_complete,
            video_decode_error_records: self.primary_error_records,
            contract_qualified_error_records: self.contract_qualified_records(),
            terminal_fault: self.fault,
            exit_disposition,
            qualification,
        }
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
    ///
    /// A partial trailing line is a *truncated* stream, not a short one: the
    /// child was killed mid-write, and the record that was being written is
    /// gone. A producer preempted or deadlined mid-line is the ordinary case,
    /// and §7.2 is explicit that a truncated diagnostic stream disallows a
    /// qualified receipt — otherwise an `Error submitting packet to decoder:`
    /// straddling the kill is silently dropped and the receipt says the stream
    /// was clean.
    pub fn finish(&mut self, accumulator: &mut HealthAccumulator) -> Option<String> {
        if self.line.is_empty() && !self.discarding {
            return None;
        }
        accumulator.mark_observation_incomplete();
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

// ---------------------------------------------------------------------------
// M3b — owned observation
//
// Everything above decides what a *line* means. Everything below decides who
// owns the reading of it, and what an attempt is allowed to claim afterwards.
// ---------------------------------------------------------------------------

/// The facts about the FFmpeg binary that is actually going to run.
///
/// Measured, not configured. A contract is evidence about one binary, and the
/// version string alone does not identify one — distributions patch FFmpeg,
/// and a patched build can change both the wording of a diagnostic and the
/// attribution it carries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MeasuredBuild {
    pub ffmpeg_version: String,
    pub binary_sha256: String,
    pub buildconf_sha256: String,
}

impl MeasuredBuild {
    /// Hash the bytes that will be executed and the build configuration they
    /// report.
    ///
    /// Every failure is `None` rather than an error: a node that cannot
    /// measure its own FFmpeg has an unqualified grammar, which costs it
    /// automatic decoder actions and costs it nothing else. Refusing to start
    /// would turn a diagnostic capability into a availability requirement.
    pub async fn measure(bin: &str) -> Option<Self> {
        let version = Self::first_banner_line(&Self::run(bin, &["-version"]).await?)?;
        let buildconf = Self::run(bin, &["-buildconf"]).await?;
        Some(Self {
            ffmpeg_version: version,
            binary_sha256: Self::file_digest(&Self::resolve(bin).await?).await?,
            buildconf_sha256: hex_digest(&buildconf),
        })
    }

    /// Hash a file without holding it in memory.
    ///
    /// A static FFmpeg is a hundred megabytes, and this runs on the startup
    /// path.
    async fn file_digest(path: &std::path::Path) -> Option<String> {
        use sha2::Digest;
        use tokio::io::AsyncReadExt;

        let mut file = tokio::fs::File::open(path).await.ok()?;
        let mut hasher = sha2::Sha256::new();
        let mut chunk = vec![0_u8; 64 * 1024];
        loop {
            match file.read(&mut chunk).await.ok()? {
                0 => break,
                read => hasher.update(&chunk[..read]),
            }
        }
        Some(hex::encode(hasher.finalize()))
    }

    async fn run(bin: &str, args: &[&str]) -> Option<Vec<u8>> {
        let output = tokio::process::Command::new(bin)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .ok()?;
        // A build that does not understand `-buildconf` exits non-zero and
        // prints an error. Hashing that error text would give
        // `buildconf_sha256` a value that is not a build configuration, and a
        // field that lies is worse than a node with no measured build at all.
        if !output.status.success() {
            return None;
        }
        // FFmpeg writes the banner to stdout and older builds to stderr; take
        // whichever is non-empty rather than assuming.
        if output.stdout.is_empty() {
            Some(output.stderr)
        } else {
            Some(output.stdout)
        }
    }

    /// The path whose bytes will actually be executed.
    ///
    /// A bare name is resolved through `PATH` the same way the spawn will
    /// resolve it, because hashing the wrong file is worse than hashing none:
    /// it produces a confident identity for a binary that is not running.
    async fn resolve(bin: &str) -> Option<std::path::PathBuf> {
        let candidate = std::path::Path::new(bin);
        if candidate.components().count() > 1 {
            return Some(candidate.to_path_buf());
        }
        for directory in std::env::split_paths(&std::env::var_os("PATH")?) {
            let path = directory.join(bin);
            // `execvp` skips a non-executable entry and keeps searching, so a
            // stale unexecutable `ffmpeg` earlier in PATH is not the binary
            // that will run — and hashing it would give a confident identity
            // for a file nobody executes.
            if Self::is_executable_file(&path).await {
                return Some(path);
            }
        }
        None
    }

    async fn is_executable_file(path: &std::path::Path) -> bool {
        let Ok(meta) = tokio::fs::metadata(path).await else {
            return false;
        };
        if !meta.is_file() {
            return false;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            true
        }
    }

    fn first_banner_line(stdout: &[u8]) -> Option<String> {
        String::from_utf8_lossy(stdout)
            .lines()
            .next()
            .map(|line| line.trim().to_owned())
            .filter(|line| !line.is_empty())
    }
}

fn hex_digest(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// What this node knows about reading its own FFmpeg's diagnostics.
///
/// Held once, resolved at startup, consulted per attempt. A node with no
/// measured build, or with a build no retained contract covers, still reads
/// stderr — it just reads it without a grammar, which produces observations
/// and no actions.
#[derive(Debug, Clone, Default)]
pub struct DiagnosticPolicy {
    build: Option<MeasuredBuild>,
    contracts: Vec<DiagnosticContract>,
}

impl DiagnosticPolicy {
    pub fn new(build: Option<MeasuredBuild>, contracts: Vec<DiagnosticContract>) -> Self {
        Self { build, contracts }
    }

    pub fn measured_build(&self) -> Option<&MeasuredBuild> {
        self.build.as_ref()
    }

    /// The contract covering this node's build for one codec and decoder, if
    /// there is exactly one.
    ///
    /// The log flags matter as much as the codec: a contract qualified under
    /// `repeat+level+error` says nothing about a producer launched with
    /// `error`, because without `level` there are no severity labels to match
    /// and without `repeat` the log is compressed. So the *intended* flags are
    /// an input here, and the caller uses the answer to decide which flags to
    /// emit — a build with no contract keeps the flags it has always had.
    pub fn contract_for(
        &self,
        input_codec: &str,
        decoder: &str,
        stderr_mode: &str,
    ) -> Option<&DiagnosticContract> {
        let build = self.build.as_ref()?;
        let observed = ObservedBuild {
            ffmpeg_version: &build.ffmpeg_version,
            binary_sha256: &build.binary_sha256,
            buildconf_sha256: &build.buildconf_sha256,
            stderr_mode,
            input_codec,
            decoder,
        };
        let mut covering = self
            .contracts
            .iter()
            .filter(|contract| contract.covers_build(&observed));
        let first = covering.next()?;
        // Two contracts covering one build with different rules is a coin
        // flip about whether a session lives. `load` refuses duplicate ids;
        // this refuses duplicate *coverage*, which is the property that
        // actually matters here.
        covering.next().is_none().then_some(first)
    }
}

/// The policy this process resolved for its own FFmpeg, installed once.
///
/// Process-scoped because that is what it describes: one daemon runs one
/// FFmpeg binary, and which contract covers it is a property of that binary
/// rather than of any session. Threading it through every manager, runner and
/// session constructor would say otherwise, and would say it about a dozen
/// call sites that have no opinion on the matter.
static INSTALLED_POLICY: std::sync::OnceLock<DiagnosticPolicy> = std::sync::OnceLock::new();

/// What a node has before it has measured anything: no build, no contracts,
/// therefore no grammar and no automatic actions.
static UNQUALIFIED_POLICY: DiagnosticPolicy = DiagnosticPolicy {
    build: None,
    contracts: Vec::new(),
};

/// Install the measured policy. Returns `false` if one is already installed,
/// which a second daemon start inside one process would be.
pub fn install_diagnostic_policy(policy: DiagnosticPolicy) -> bool {
    INSTALLED_POLICY.set(policy).is_ok()
}

/// The installed policy, or the unqualified one. Never `None`: a node that has
/// not measured its build still reads stderr, it just reads it without a
/// grammar.
pub fn diagnostic_policy() -> &'static DiagnosticPolicy {
    INSTALLED_POLICY.get().unwrap_or(&UNQUALIFIED_POLICY)
}

/// The log flags a qualified grammar requires, per §7.1.
///
/// `repeat` stops FFmpeg compressing repeated messages into a summary whose
/// timestamps the window cannot evaluate; `level` supplies the severity labels
/// the contract matches on.
pub const QUALIFIED_STDERR_MODE: &str = "repeat+level+error";

/// What every unqualified build has always used, and keeps using.
pub const LEGACY_STDERR_MODE: &str = "error";

/// One attempt's diagnostic stream, owned rather than detached.
///
/// §7.1: "Reader task failure must be observable; detached best-effort logging
/// is insufficient for cache qualification." The producer holds this handle,
/// and until it is settled the attempt has no receipt — which is the whole
/// point, because an attempt with no receipt cannot qualify an artifact.
pub struct ObservedDiagnostics {
    plan_digest: String,
    contract_id: Option<String>,
    /// The reader task. `None` once settled, or if the child was spawned
    /// without a stderr pipe at all — which is itself an incomplete
    /// observation rather than a clean one.
    reader: Option<tokio::task::JoinHandle<HealthAccumulator>>,
    /// The progress reader, owned for the same reason: a progress task that
    /// died is a session whose telemetry silently stopped, and joining it is
    /// how that becomes visible.
    progress: Option<tokio::task::JoinHandle<()>>,
    /// Set when there was never a reader to join.
    unobserved: bool,
}

impl std::fmt::Debug for ObservedDiagnostics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ObservedDiagnostics")
            .field("plan_digest", &self.plan_digest)
            .field("contract_id", &self.contract_id)
            .field("settled", &self.reader.is_none())
            .finish()
    }
}

impl ObservedDiagnostics {
    pub(crate) fn new(
        plan_digest: String,
        contract_id: Option<String>,
        reader: Option<tokio::task::JoinHandle<HealthAccumulator>>,
        progress: Option<tokio::task::JoinHandle<()>>,
    ) -> Self {
        Self {
            plan_digest,
            contract_id,
            unobserved: reader.is_none(),
            reader,
            progress,
        }
    }

    pub fn plan_digest(&self) -> &str {
        &self.plan_digest
    }

    pub fn diagnostic_contract(&self) -> Option<&str> {
        self.contract_id.as_deref()
    }

    /// Wait for the diagnostic stream to reach EOF, bounded, and produce the
    /// attempt's receipt.
    ///
    /// The budget is what stops a reader that never finishes from holding a
    /// publication open forever. What expiry produces is an *incomplete*
    /// observation — a refusal to qualify — rather than a hang or a guess.
    /// A reader that panicked produces the same answer for the same reason:
    /// §7.1 requires reader failure to be observable, and the observable form
    /// of it is an attempt that cannot claim its output is clean.
    pub async fn settle(
        mut self,
        budget: std::time::Duration,
        exit_disposition: ExitDisposition,
    ) -> ProducerHealthReceipt {
        // One deadline for the whole settle, not one per reader. Two sequential
        // budgets would let a teardown spend twice what the caller was told it
        // could, and the caller is holding a publication open behind this.
        let deadline = tokio::time::Instant::now() + budget;
        if let Some(mut progress) = self.progress.take() {
            // The progress reader ends when the pipe closes, which the child's
            // exit already guaranteed. Awaited by reference so that expiry can
            // *abort* it: dropping a `JoinHandle` detaches its task, which is
            // precisely the reader-outlives-its-producer shape this milestone
            // exists to remove.
            if tokio::time::timeout_at(deadline, &mut progress)
                .await
                .is_err()
            {
                tracing::warn!("producer progress reader did not finish inside the drain budget");
                progress.abort();
            }
        }
        let Some(mut reader) = self.reader.take() else {
            return ProducerHealthReceipt::unobserved(self.plan_digest.clone(), exit_disposition);
        };
        let accumulator = match tokio::time::timeout_at(deadline, &mut reader).await {
            Ok(Ok(accumulator)) => accumulator,
            Ok(Err(error)) => {
                // Panicked or cancelled. There is no accumulator to read, and
                // the honest receipt is one that says nothing was observed.
                tracing::warn!("producer diagnostic reader failed: {error}");
                return ProducerHealthReceipt::unobserved(
                    self.plan_digest.clone(),
                    exit_disposition,
                );
            }
            Err(_) => {
                tracing::warn!(
                    budget_ms = budget.as_millis(),
                    "producer diagnostics did not reach EOF inside the drain budget"
                );
                reader.abort();
                return ProducerHealthReceipt::unobserved(
                    self.plan_digest.clone(),
                    exit_disposition,
                );
            }
        };
        accumulator.settle_receipt(
            self.plan_digest.clone(),
            self.contract_id.clone(),
            exit_disposition,
        )
    }

    /// Whether this attempt ever had a diagnostic stream to read.
    pub fn is_unobserved(&self) -> bool {
        self.unobserved
    }
}

impl Drop for ObservedDiagnostics {
    fn drop(&mut self) {
        // A handle dropped without settling means its producer was torn down
        // before classification. Abort rather than detach: a reader that
        // outlives the thing that owns it is exactly the "detached
        // best-effort logging" §7.1 refuses, and nobody is left to read what
        // it would find.
        if let Some(reader) = self.reader.take() {
            reader.abort();
        }
        if let Some(progress) = self.progress.take() {
            progress.abort();
        }
    }
}

/// Read one child's stderr to EOF, classifying as it goes and logging what it
/// reads.
///
/// This is the body of the owned reader task. It returns the accumulator
/// rather than mutating a shared one, so the only way to obtain an attempt's
/// health is to join its reader — which is what makes "the reader died" and
/// "the stream was clean" different answers instead of the same silence.
pub(crate) async fn read_diagnostics<R>(
    stream: R,
    grammar: Option<DiagnosticGrammar>,
    log: impl FnMut(&str),
    progress: impl FnMut(&str) -> bool,
) -> HealthAccumulator
where
    R: tokio::io::AsyncRead + Unpin,
{
    read_diagnostics_reporting(stream, grammar, log, progress, |_, _| {}).await
}

/// The same read, reporting a latch the moment it happens.
///
/// §7.4's ordering is the reason this is not simply read off the returned
/// accumulator: the fault has to reach its owner *before* the success facts
/// that would otherwise be the last word about this attempt, and the
/// accumulator is not returned until stderr reaches EOF — which, for a
/// producer that stops decoding and keeps running, is the rest of the film.
pub(crate) async fn read_diagnostics_reporting<R>(
    stream: R,
    grammar: Option<DiagnosticGrammar>,
    mut log: impl FnMut(&str),
    mut progress: impl FnMut(&str) -> bool,
    mut on_fault: impl FnMut(DecodeFaultKind, u64),
) -> HealthAccumulator
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;

    // Decided once, before anything is read, and by the same value that
    // decides whether any line is classified: without a contract this attempt
    // has no grammar, so nothing it prints is evidence and its output cannot
    // become a durable artifact however clean it looks.
    let mut accumulator = match grammar.as_ref() {
        Some(grammar) => {
            HealthAccumulator::with_qualified_grammar(grammar.attributes_every_failure())
        }
        None => HealthAccumulator::new(),
    };
    let mut reader = BoundedDiagnosticReader::new();
    let mut stream = stream;
    let mut chunk = [0_u8; 8 * 1024];
    loop {
        let read = match stream.read(&mut chunk).await {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) => {
                // The stream ended in a way that is not EOF. Whatever the
                // child said after this point is unknown, so the attempt is
                // not fully observed.
                tracing::warn!("reading producer diagnostics: {error}");
                accumulator.mark_observation_incomplete();
                break;
            }
        };
        for line in reader.push(&chunk[..read], &mut accumulator) {
            observe_line(
                &line,
                grammar.as_ref(),
                &mut accumulator,
                &mut log,
                &mut progress,
                &mut on_fault,
            );
        }
    }
    if let Some(line) = reader.finish(&mut accumulator) {
        observe_line(
            &line,
            grammar.as_ref(),
            &mut accumulator,
            &mut log,
            &mut progress,
            &mut on_fault,
        );
    }
    accumulator
}

fn observe_line(
    line: &str,
    grammar: Option<&DiagnosticGrammar>,
    accumulator: &mut HealthAccumulator,
    log: &mut impl FnMut(&str),
    progress: &mut impl FnMut(&str) -> bool,
    on_fault: &mut impl FnMut(DecodeFaultKind, u64),
) {
    // A `-progress` block on this stream is telemetry, not a diagnostic. It is
    // sorted out before classification so a key=value line can never become a
    // decode record, and before logging so the log is not drowned in it.
    if progress(line) {
        return;
    }
    if let Some(grammar) = grammar {
        let record = grammar.classify(line);
        // `observe` answers with the fault only on the line that latched it,
        // which is what makes one barrier per attempt a property of the
        // accumulator rather than of the caller remembering to ask once.
        if let Some(fault) = accumulator.observe(std::time::Instant::now(), &record) {
            on_fault(fault, accumulator.primary_error_records());
        }
    }
    // Without a grammar the line is still read, still bounded and still
    // logged; it is simply not evidence. `mark_grammar_unavailable` said so
    // once, before the first read.
    log(line);
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTRACTS: &str = RETAINED_DIAGNOSTIC_CONTRACTS;
    const QUALIFIED_FFMPEG_9: &str =
        include_str!("../../../tests/playback/decoder-health/qualified-ffmpeg-9.stderr");
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
        let mut accumulator =
            HealthAccumulator::with_qualified_grammar(grammar.attributes_every_failure());
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
            primary_message: "Error submitting packet to decoder:".to_owned(),
            subordinate_message: Some("No frame decoded?".to_owned()),
            attributes_every_failure: true,
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
        assert!(
            !accumulator.observation_complete(),
            "a stream that ended mid-line was cut, and the record being written is gone"
        );
        assert!(!accumulator.qualifies_reuse());

        // A stream that ended *on* a newline was not cut.
        let mut whole = HealthAccumulator::with_qualified_grammar(true);
        let mut reader = BoundedDiagnosticReader::new();
        assert_eq!(reader.push(b"[info] done\n", &mut whole).len(), 1);
        assert_eq!(reader.finish(&mut whole), None);
        assert!(whole.observation_complete());
        assert!(whole.qualifies_reuse());
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
        let mut accumulator = HealthAccumulator::with_qualified_grammar(true);
        assert!(
            accumulator.qualifies_reuse(),
            "a clean complete read of a qualified build qualifies"
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
        assert_eq!(defaulted.primary_error_records(), 0);
        assert!(
            !defaulted.grammar_available(),
            "a caller that says nothing about a grammar has not got one"
        );
        assert!(
            !defaulted.qualifies_reuse(),
            "and an accumulator nobody gave a grammar certifies nothing"
        );
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

    /// The retained FFmpeg 8 contract cannot cover any running build, and the
    /// reason is a convention, not a hash.
    ///
    /// `MeasuredBuild::measure` takes `ffmpeg_version` from the first banner
    /// line — `ffmpeg version 8.0.1-3ubuntu2 Copyright …` — and `covers_build`
    /// compares it for equality. The retained FFmpeg 8 contract records the
    /// bare `8.0.1-3ubuntu2` instead, so it can never match, which makes it
    /// inert on the very host it was qualified against.
    ///
    /// It is pinned rather than fixed because fixing it means re-capturing
    /// that host's banner line, and inventing one would be exactly the
    /// confident-but-unmeasured value this evidence set exists to refuse. The
    /// FFmpeg 9 contract uses the measured convention, so a future capture has
    /// a correct example to follow.
    #[test]
    fn the_retained_ffmpeg_8_contract_records_a_version_no_build_reports() {
        let contracts = DiagnosticContract::load(CONTRACTS).expect("retained contracts parse");
        let ffmpeg8 = contracts
            .iter()
            .find(|contract| contract.input_codec == "rawvideo")
            .expect("the retained rawvideo contract");
        assert!(
            !ffmpeg8.ffmpeg_version.starts_with("ffmpeg version "),
            "owed: this contract predates the measured-banner convention and cannot cover a \
             running build until its host is re-captured"
        );
        let ffmpeg9 = contracts
            .iter()
            .find(|contract| contract.input_codec == "h264")
            .expect("the retained h264 contract");
        assert!(
            ffmpeg9.ffmpeg_version.starts_with("ffmpeg version "),
            "a contract's version is the banner line `MeasuredBuild` reads, not a package version"
        );
    }

    /// A matcher that is empty, or padded, is refused at load.
    ///
    /// Empty is the obvious one and the safe one: it over-matches. Padded is
    /// the dangerous one and it is one stray character away in a hand-written
    /// file — a leading space makes the prefix fail against a message this
    /// reader has already trimmed, so the grammar matches nothing, and a
    /// grammar that matches nothing certifies every broken stream as clean.
    #[test]
    fn an_empty_or_padded_matcher_is_refused_at_load() {
        for (field, value) in [
            ("primary_message", ""),
            ("primary_message", " Error submitting packet to decoder:"),
            ("primary_message", "Error submitting packet to decoder: "),
            ("primary_message", "   "),
            ("subordinate_message", ""),
            ("subordinate_message", " No frame decoded?"),
        ] {
            let table = CONTRACTS.replacen(
                &format!("{field} = \"Error submitting packet to decoder:\""),
                &format!("{field} = \"{value}\""),
                1,
            );
            let table = if table == CONTRACTS {
                CONTRACTS.replacen(
                    &format!("{field} = \"No frame decoded?\""),
                    &format!("{field} = \"{value}\""),
                    1,
                )
            } else {
                table
            };
            assert_ne!(table, CONTRACTS, "the fixture must actually change {field}");
            assert!(
                matches!(
                    DiagnosticContract::load(&table),
                    Err(ContractLoadError::UnusableMatcher(_))
                ),
                "{field} = {value:?} must be refused"
            );
        }
        // And the retained table itself passes the guard, so the test is about
        // the guard rather than about the table being unloadable.
        assert!(DiagnosticContract::load(CONTRACTS).is_ok());
    }

    /// The same grammar, on a build whose wording is different and whose
    /// attribution is once per session rather than once per failure.
    ///
    /// This is the finding that made the primary message a contract field.
    /// FFmpeg 9 does not contain the string `Error submitting packet to
    /// decoder` anywhere in its binary — its only decode-error format is
    /// `Decoding error: %s` — so the FFmpeg 8 wording matches nothing on it,
    /// and a grammar that matches nothing reports every stream as clean. An
    /// ordinary upgrade would have reached that silently.
    ///
    /// The two tiers separate cleanly here, and in the safe direction: one
    /// structural record refuses the cache artifact, and the windowed action
    /// cannot fire because this build never emits five of anything.
    #[test]
    fn the_ffmpeg_9_capture_counts_a_record_and_cannot_act_on_it() {
        let contracts = DiagnosticContract::load(CONTRACTS).expect("retained contracts parse");
        let contract = contracts
            .iter()
            .find(|contract| contract.id == "ffmpeg-9.0.1-homebrew-h264-v1")
            .expect("the retained ffmpeg 9 contract")
            .clone();
        assert_eq!(contract.primary_message, "Decoding error:");
        assert_eq!(contract.subordinate_message, None);
        assert!(!contract.attributes_every_failure);
        assert_eq!(contract.fixture, "qualified-ffmpeg-9.stderr");

        let accumulator = replay(QUALIFIED_FFMPEG_9, &DiagnosticGrammar::new(contract, 0, 0));
        assert_eq!(accumulator.primary_error_records(), 1);
        assert_eq!(
            accumulator.contract_qualified_records(),
            1,
            "the record carried the build receipt"
        );
        assert!(accumulator.observation_complete());
        assert!(
            !accumulator.qualifies_reuse(),
            "one structural record is already enough to refuse the artifact"
        );
        assert_eq!(
            accumulator.fault(),
            None,
            "one record is not a window of five"
        );
        assert!(!accumulator.windowed_action_qualified());
    }

    /// A build whose attribution was not qualified still latches, and still
    /// may not act on it.
    ///
    /// The first draft of this milestone blocked the *latch* for such a build,
    /// and that was the wrong trade: a genuinely burst-failing stream would
    /// then produce no fault, no barrier, and a receipt saying `Unqualified`
    /// with no terminal fault where it should say `Rejected` — throwing away
    /// the strongest thing the observation had to say in order to avoid
    /// overclaiming about a *different* thing. Latching refuses the artifact;
    /// what an unqualified build may not do is kill a session over it.
    #[test]
    fn an_unqualified_attribution_still_latches_and_still_may_not_act() {
        let contracts = DiagnosticContract::load(CONTRACTS).expect("retained contracts parse");
        let contract = contracts
            .iter()
            .find(|contract| contract.id == "ffmpeg-9.0.1-homebrew-h264-v1")
            .expect("the retained ffmpeg 9 contract")
            .clone();
        let grammar = DiagnosticGrammar::new(contract, 0, 0);
        let base = Instant::now();
        let mut accumulator = HealthAccumulator::with_qualified_grammar(false);
        let line = "[vist#0:0/h264 @ 0x1] [dec:h264 @ 0x2] [error] \
                    Decoding error: Invalid data found when processing input";
        for step in 0..20 {
            accumulator.observe(
                base + Duration::from_millis(step * 10),
                &grammar.classify(line),
            );
        }
        assert_eq!(accumulator.primary_error_records(), 20);
        assert_eq!(
            accumulator.fault(),
            Some(DecodeFaultKind::VideoDecodeFailure),
            "a burst is still a fault, and refusing to see it would lose the receipt's \
             strongest statement"
        );
        assert!(
            !accumulator.automatic_action_allowed(),
            "but a build whose attribution was never qualified may not kill a session"
        );
        assert!(!accumulator.qualifies_reuse());

        // And the same twenty records on a build whose attribution *was*
        // qualified do permit the action, so the refusal above is about the
        // contract rather than about anything else in this fixture.
        let mut qualified = HealthAccumulator::with_qualified_grammar(true);
        for step in 0..20 {
            qualified.observe(
                base + Duration::from_millis(step * 10),
                &grammar.classify(line),
            );
        }
        assert!(qualified.automatic_action_allowed());
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
        let table = CONTRACTS.replacen("version = 2", "version = 3", 1);
        assert_eq!(
            DiagnosticContract::load(&table),
            Err(ContractLoadError::UnsupportedVersion(3))
        );
        // And the version this build no longer reads. Version 1 tables carried
        // the primary message as a constant in this file rather than as a
        // field, so reading one with these rules would match every build's
        // lines against one build's wording.
        assert_eq!(
            DiagnosticContract::load(&CONTRACTS.replacen("version = 2", "version = 1", 1)),
            Err(ContractLoadError::UnsupportedVersion(1))
        );
        assert!(matches!(
            DiagnosticContract::load("version = 2\n[[contracts]]\nid = 1\n"),
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

    // -----------------------------------------------------------------------
    // M3b — owned observation
    // -----------------------------------------------------------------------

    fn qualified_policy() -> DiagnosticPolicy {
        let contract = contract("h264", "corrupt input packet");
        DiagnosticPolicy::new(
            Some(MeasuredBuild {
                ffmpeg_version: contract.ffmpeg_version.clone(),
                binary_sha256: contract.binary_sha256.clone(),
                buildconf_sha256: contract.buildconf_sha256.clone(),
            }),
            vec![contract],
        )
    }

    /// The policy answers for one exact build, one codec, one decoder, and one
    /// set of log flags — and says no to everything else.
    #[test]
    fn a_policy_covers_the_build_it_measured_and_nothing_else() {
        let policy = qualified_policy();
        assert!(policy
            .contract_for("h264", "h264", QUALIFIED_STDERR_MODE)
            .is_some());
        assert!(
            policy
                .contract_for("h264", "h264", LEGACY_STDERR_MODE)
                .is_none(),
            "a contract qualified under repeat+level+error says nothing about a \
             child launched without severity labels"
        );
        assert!(policy
            .contract_for("hevc", "hevc", QUALIFIED_STDERR_MODE)
            .is_none());
        assert!(
            policy
                .contract_for("h264", "h264_qsv", QUALIFIED_STDERR_MODE)
                .is_none(),
            "the decoder this effort swaps in is not the decoder that was qualified"
        );
        assert!(
            DiagnosticPolicy::default()
                .contract_for("h264", "h264", QUALIFIED_STDERR_MODE)
                .is_none(),
            "a node that measured nothing covers nothing"
        );
    }

    /// Two contracts covering one build is a coin flip about whether a session
    /// lives. It answers no rather than picking one.
    #[test]
    fn two_contracts_covering_one_build_answer_neither() {
        let one = contract("h264", "corrupt input packet");
        let mut two = one.clone();
        two.id = "second".to_owned();
        two.error_detail = "something else entirely".to_owned();
        let policy = DiagnosticPolicy::new(
            Some(MeasuredBuild {
                ffmpeg_version: one.ffmpeg_version.clone(),
                binary_sha256: one.binary_sha256.clone(),
                buildconf_sha256: one.buildconf_sha256.clone(),
            }),
            vec![one, two],
        );
        assert!(policy
            .contract_for("h264", "h264", QUALIFIED_STDERR_MODE)
            .is_none());
    }

    /// The whole point of the receipt: a producer that exits zero having
    /// failed to decode cannot qualify its own output.
    #[tokio::test]
    async fn a_producer_that_failed_to_decode_and_exited_zero_is_rejected() {
        let grammar = h264();
        let (mut writer, reader) = tokio::io::duplex(4096);
        let handle = tokio::spawn(crate::decoder_health::read_diagnostics(
            reader,
            Some(grammar),
            |_| {},
            |_| false,
        ));
        for _ in 0..VIDEO_DECODE_ERROR_LIMIT {
            use tokio::io::AsyncWriteExt;
            writer
                .write_all(format!("{H264_PRIMARY}\n").as_bytes())
                .await
                .expect("write a primary record");
        }
        drop(writer);
        let diagnostics = ObservedDiagnostics::new(
            "plan-digest".to_owned(),
            Some("id".to_owned()),
            Some(handle),
            None,
        );
        // A clean end: the process said it finished, and it is the receipt
        // that disagrees.
        let receipt = diagnostics
            .settle(DIAGNOSTIC_DRAIN_BUDGET, ExitDisposition::CleanEnd)
            .await;
        assert_eq!(receipt.qualification, Qualification::Rejected);
        assert!(!receipt.permits_reuse());
        assert_eq!(
            receipt.terminal_fault,
            Some(DecodeFaultKind::VideoDecodeFailure)
        );
        assert_eq!(receipt.video_decode_error_records, 5);
        assert_eq!(receipt.contract_qualified_error_records, 5);
        assert_eq!(receipt.plan_digest, "plan-digest");
    }

    /// A clean stream from a qualified build is the one case that may become a
    /// durable artifact.
    #[tokio::test]
    async fn a_clean_observation_from_a_qualified_build_qualifies() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        let handle = tokio::spawn(crate::decoder_health::read_diagnostics(
            reader,
            Some(h264()),
            |_| {},
            |_| false,
        ));
        {
            use tokio::io::AsyncWriteExt;
            writer
                .write_all(b"[info] frame= 120 fps=24\n[aist#0:1/aac @ 0x1] [dec:aac @ 0x2] [error] Error submitting packet to decoder: invalid data\n")
                .await
                .expect("write unrelated lines");
        }
        drop(writer);
        let receipt =
            ObservedDiagnostics::new("d".to_owned(), Some("id".to_owned()), Some(handle), None)
                .settle(DIAGNOSTIC_DRAIN_BUDGET, ExitDisposition::CleanEnd)
                .await;
        assert_eq!(receipt.qualification, Qualification::Qualified);
        assert!(receipt.permits_reuse());
        assert_eq!(receipt.video_decode_error_records, 0);
    }

    /// A stream this node has no grammar for is read, bounded and logged — and
    /// certifies nothing. This is the deployed fleet's state, and the reason
    /// "it looked clean" is not an answer.
    #[tokio::test]
    async fn a_build_no_contract_covers_reads_without_qualifying() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        let handle = tokio::spawn(async move {
            let mut lines = Vec::new();
            let accumulator = crate::decoder_health::read_diagnostics(
                reader,
                None,
                |line| lines.push(line.to_owned()),
                |_| false,
            )
            .await;
            (accumulator, lines)
        });
        {
            use tokio::io::AsyncWriteExt;
            writer
                .write_all(b"[error] Error while decoding stream #0:0: Invalid data found when processing input\n")
                .await
                .expect("write the deployed build's spelling");
        }
        drop(writer);
        let (accumulator, logged) = handle.await.expect("join");
        assert_eq!(logged.len(), 1, "the line is still read and still logged");
        assert!(!accumulator.grammar_available());
        assert!(
            !accumulator.qualifies_reuse(),
            "a stream nobody could read is not a stream that was clean"
        );
        assert_eq!(accumulator.primary_error_records(), 0);
    }

    /// §7.1: reader failure must be observable. The observable form of it is
    /// an attempt that cannot claim its output is clean.
    #[tokio::test]
    async fn a_reader_that_never_reaches_eof_is_aborted_rather_than_detached() {
        /// Fires when the reader task is dropped — which, if the budget path
        /// detaches instead of aborting, never happens.
        struct Cancelled(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Cancelled {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        // A duplex whose writer is never dropped: the reader cannot reach EOF.
        let (_writer, reader) = tokio::io::duplex(64);
        let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = Cancelled(std::sync::Arc::clone(&gone));
        let handle = tokio::spawn(async move {
            let _guard = guard;
            crate::decoder_health::read_diagnostics(reader, Some(h264()), |_| {}, |_| false).await
        });
        let receipt = ObservedDiagnostics::new("d".to_owned(), None, Some(handle), None)
            .settle(Duration::from_millis(50), ExitDisposition::CleanEnd)
            .await;
        assert_eq!(receipt.qualification, Qualification::Unqualified);
        assert!(!receipt.observation_complete);
        assert!(!receipt.permits_reuse());

        // Aborting is not instantaneous, but it is bounded: yield until the
        // runtime has dropped the task, and fail rather than hang.
        for _ in 0..1_000 {
            if gone.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!(
            "the reader outlived the attempt that owned it — dropping a JoinHandle \
             detaches its task, which is the shape §7.1 refuses"
        );
    }

    /// A reader that panicked is the same answer for the same reason — and it
    /// must not be the same answer as a clean stream.
    #[tokio::test]
    async fn a_reader_that_panicked_settles_as_unobserved() {
        let handle = tokio::spawn(async { panic!("the reader died") });
        let receipt = ObservedDiagnostics::new("d".to_owned(), None, Some(handle), None)
            .settle(DIAGNOSTIC_DRAIN_BUDGET, ExitDisposition::CleanEnd)
            .await;
        assert_eq!(receipt.qualification, Qualification::Unqualified);
        assert!(!receipt.observation_complete);
    }

    /// An attempt with no diagnostic stream at all is unqualified, never
    /// silently clean.
    #[tokio::test]
    async fn an_attempt_with_no_reader_is_unqualified() {
        let diagnostics = ObservedDiagnostics::new("d".to_owned(), None, None, None);
        assert!(diagnostics.is_unobserved());
        let receipt = diagnostics
            .settle(DIAGNOSTIC_DRAIN_BUDGET, ExitDisposition::CleanEnd)
            .await;
        assert_eq!(receipt.qualification, Qualification::Unqualified);
    }

    /// A failed termination never qualifies, however clean the stream was: the
    /// bytes it produced are a fragment of work that did not finish.
    #[tokio::test]
    async fn a_failed_termination_is_never_qualified() {
        let (writer, reader) = tokio::io::duplex(64);
        drop(writer);
        let handle = tokio::spawn(crate::decoder_health::read_diagnostics(
            reader,
            Some(h264()),
            |_| {},
            |_| false,
        ));
        let receipt =
            ObservedDiagnostics::new("d".to_owned(), Some("id".to_owned()), Some(handle), None)
                .settle(DIAGNOSTIC_DRAIN_BUDGET, ExitDisposition::FailedTermination)
                .await;
        assert_eq!(receipt.qualification, Qualification::Unqualified);
        assert!(
            receipt.observation_complete,
            "the stream was read to its end"
        );
    }

    /// A yield is not a failure and not an end of input, and the receipt says
    /// which — §6.2 keeps the segments a yielded part finished.
    #[tokio::test]
    async fn a_yield_is_recorded_as_a_yield() {
        let (writer, reader) = tokio::io::duplex(64);
        drop(writer);
        let handle = tokio::spawn(crate::decoder_health::read_diagnostics(
            reader,
            Some(h264()),
            |_| {},
            |_| false,
        ));
        let receipt =
            ObservedDiagnostics::new("d".to_owned(), Some("id".to_owned()), Some(handle), None)
                .settle(DIAGNOSTIC_DRAIN_BUDGET, ExitDisposition::IntentionalYield)
                .await;
        assert_eq!(receipt.exit_disposition, ExitDisposition::IntentionalYield);
        assert_eq!(
            receipt.qualification,
            Qualification::Qualified,
            "a clean yield may still be reused; the receipt records why it stopped"
        );
    }

    /// Progress blocks share the copy session's stderr with the log. A
    /// `key=value` line must never reach classification, and must not drown
    /// the log either.
    #[tokio::test]
    async fn progress_blocks_are_sorted_out_before_classification() {
        let (mut writer, reader) = tokio::io::duplex(4096);
        let handle = tokio::spawn(async move {
            let mut progress = Vec::new();
            let mut logged = Vec::new();
            let accumulator = crate::decoder_health::read_diagnostics(
                reader,
                Some(h264()),
                |line| logged.push(line.to_owned()),
                |line| {
                    if line.starts_with("out_time_ms=") || line.starts_with("progress=") {
                        progress.push(line.to_owned());
                        true
                    } else {
                        false
                    }
                },
            )
            .await;
            (accumulator, progress, logged)
        });
        {
            use tokio::io::AsyncWriteExt;
            // The third line is a telemetry line the *grammar* would also have
            // an opinion about: as a diagnostic it is a malformed repeat
            // marker. It is claimed by the progress filter, so the accumulator
            // must never see it — which is what "sorted out *before*
            // classification" means, and what moving the filter after
            // `classify` would break.
            writer
                .write_all(
                    format!(
                        "out_time_ms=1000\nprogress=continue\nprogress=Last message repeated x times\n{H264_PRIMARY}\n"
                    )
                    .as_bytes(),
                )
                .await
                .expect("write a mixed stream");
        }
        drop(writer);
        let (accumulator, progress, logged) = handle.await.expect("join");
        assert_eq!(progress.len(), 3);
        assert_eq!(logged.len(), 1, "telemetry does not drown the log");
        assert_eq!(accumulator.primary_error_records(), 1);
        assert_eq!(
            accumulator.malformed_lines(),
            0,
            "a telemetry line never reaches the grammar"
        );
        assert!(accumulator.observation_complete());
    }

    /// Memory is a function of the parser, not of what the child printed, and
    /// a stream that overran the bound says so in its receipt.
    #[tokio::test]
    async fn a_flooding_line_is_bounded_and_costs_the_attempt_its_receipt() {
        let (mut writer, reader) = tokio::io::duplex(8192);
        let handle = tokio::spawn(crate::decoder_health::read_diagnostics(
            reader,
            Some(h264()),
            |_| {},
            |_| false,
        ));
        let flood = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let chunk = vec![b'x'; 4096];
            for _ in 0..8 {
                if writer.write_all(&chunk).await.is_err() {
                    return;
                }
            }
            let _ = writer.write_all(b"\n").await;
        });
        flood.await.expect("flood");
        // The bound itself, not only the flag: 32 KiB written, 16 KiB kept.
        let mut retained = HealthAccumulator::with_qualified_grammar(true);
        let mut bounded = BoundedDiagnosticReader::new();
        let mut wide = vec![b'x'; MAX_DIAGNOSTIC_LINE_BYTES * 2];
        wide.push(b'\n');
        let lines = bounded.push(&wide, &mut retained);
        assert_eq!(lines[0].len(), MAX_DIAGNOSTIC_LINE_BYTES);
        let receipt =
            ObservedDiagnostics::new("d".to_owned(), Some("id".to_owned()), Some(handle), None)
                .settle(DIAGNOSTIC_DRAIN_BUDGET, ExitDisposition::CleanEnd)
                .await;
        assert_eq!(receipt.qualification, Qualification::Unqualified);
        assert!(!receipt.observation_complete);
    }

    /// The receipt round-trips, and a version this build does not know is not
    /// a reusable artifact — §6.2's readers reject an unsupported receipt
    /// rather than reading the fields they happen to recognize.
    #[test]
    fn a_receipt_of_an_unknown_version_permits_nothing() {
        let receipt = ProducerHealthReceipt {
            receipt_version: PRODUCER_HEALTH_RECEIPT_VERSION,
            plan_digest: "d".to_owned(),
            diagnostic_contract: Some("id".to_owned()),
            observation_complete: true,
            video_decode_error_records: 0,
            contract_qualified_error_records: 0,
            terminal_fault: None,
            exit_disposition: ExitDisposition::CleanEnd,
            qualification: Qualification::Qualified,
        };
        let encoded = serde_json::to_string(&receipt).expect("serialize");
        assert_eq!(
            serde_json::from_str::<ProducerHealthReceipt>(&encoded).expect("round trip"),
            receipt
        );
        assert!(receipt.permits_reuse());

        let future = ProducerHealthReceipt {
            receipt_version: PRODUCER_HEALTH_RECEIPT_VERSION + 1,
            ..receipt
        };
        assert!(
            !future.permits_reuse(),
            "a receipt whose schema this build cannot read is not evidence it can act on"
        );
    }

    /// The unobserved receipt is unqualified by construction. Nothing can
    /// assemble one that says otherwise.
    #[test]
    fn an_unobserved_receipt_cannot_be_made_to_qualify() {
        let receipt = ProducerHealthReceipt::unobserved("d".to_owned(), ExitDisposition::CleanEnd);
        assert_eq!(receipt.qualification, Qualification::Unqualified);
        assert!(!receipt.observation_complete);
        assert!(!receipt.permits_reuse());
        assert!(receipt.diagnostic_contract.is_none());
    }

    /// What the measurement reads, and what it refuses to read.
    ///
    /// `execvp` skips a non-executable entry and keeps searching, so hashing
    /// one would give a confident identity for a file nobody executes; and a
    /// build that does not understand `-buildconf` exits non-zero, so hashing
    /// its error text would put something that is not a build configuration
    /// into a field named after one.
    #[tokio::test]
    async fn the_measurement_reads_the_binary_that_will_actually_run() {
        let root = tempfile::tempdir().expect("temp root");
        let unexecutable = root.path().join("ffmpeg");
        tokio::fs::write(&unexecutable, b"not a program")
            .await
            .expect("write");
        assert!(
            !MeasuredBuild::is_executable_file(&unexecutable).await,
            "a file without an executable bit is not what PATH resolution finds"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&unexecutable, std::fs::Permissions::from_mode(0o755))
                .await
                .expect("chmod");
            assert!(MeasuredBuild::is_executable_file(&unexecutable).await);
        }
        assert!(
            !MeasuredBuild::is_executable_file(root.path()).await,
            "a directory on PATH is not the binary either"
        );

        // The digest is of the file's bytes, streamed rather than slurped.
        let digest = MeasuredBuild::file_digest(&unexecutable)
            .await
            .expect("digest");
        assert_eq!(digest, hex_digest(b"not a program"));

        // A command that fails is not a measurement.
        assert!(
            MeasuredBuild::run("plurxd-no-such-binary-anywhere", &["-version"])
                .await
                .is_none()
        );
        assert!(
            MeasuredBuild::run("false", &[]).await.is_none(),
            "a non-zero exit is a refusal, not a value to hash"
        );
        assert_eq!(
            MeasuredBuild::first_banner_line(b"ffmpeg version 8.0.1\nbuilt with\n").as_deref(),
            Some("ffmpeg version 8.0.1")
        );
        assert_eq!(MeasuredBuild::first_banner_line(b"   \n").as_deref(), None);
    }

    /// The unqualified policy is what a node has before it has measured
    /// anything, and it is what `diagnostic_policy()` answers with.
    #[test]
    fn a_node_that_measured_nothing_still_has_a_policy() {
        let policy = diagnostic_policy();
        assert!(
            policy
                .contract_for("h264", "h264", QUALIFIED_STDERR_MODE)
                .is_none()
                || policy.measured_build().is_some(),
            "either nothing was installed for this test process, or whatever \
             was installed measured a build"
        );
    }
}
