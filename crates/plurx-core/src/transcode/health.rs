//! What one producer attempt's observation concluded.
//!
//! The receipt lives in `plurx-core` rather than in the daemon because the
//! generation manifest carries it, and the manifest is core's. Everything that
//! *produces* a receipt — the grammar, the accumulator, the reader — stays in
//! the daemon: this module is the shape a cache reader has to authenticate,
//! and nothing more.

use serde::{Deserialize, Serialize};

/// A diagnostic contract identifier names a stanza in a table this build
/// ships, so it is short by construction. The bound lives here, next to the
/// receipt that carries the identifier, because two places enforce it: the
/// loader that reads the table, and the manifest that has to store the result.
/// Keeping one constant is what stops a contract that loads fine from being a
/// receipt that cannot be published.
pub const MAX_DIAGNOSTIC_CONTRACT_BYTES: usize = 128;

/// Whether a contract identifier is one a manifest can carry.
pub fn safe_diagnostic_contract_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_DIAGNOSTIC_CONTRACT_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

/// What went wrong, at the granularity a recovery decision needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// How this attempt's process ended, as far as observation can tell.
///
/// Separate from the exit status because the two disagree in the case this
/// whole effort exists for: a producer that failed to decode and exited zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitDisposition {
    /// Ran to the end of its input and stopped.
    CleanEnd,
    /// Stopped early because the caller asked it to — a yield to offline
    /// production, a preemption. Not a failure, and not end-of-input either:
    /// §6.2 requires the receipt to say which of the two it was.
    IntentionalYield,
    /// Terminated by failure, signal, or a deadline the caller enforced.
    FailedTermination,
}

impl ExitDisposition {
    /// How well this ending is thought of, as an order. A failure is the worst
    /// news and a clean end the best; an intentional yield sits between them
    /// because it is normal in a resumable pipeline and still means this
    /// process did not reach the end of its input.
    fn rank(self) -> u8 {
        match self {
            Self::FailedTermination => 0,
            Self::IntentionalYield => 1,
            Self::CleanEnd => 2,
        }
    }

    /// The worse of two endings.
    pub fn weaker_of(self, other: Self) -> Self {
        if other.rank() < self.rank() {
            other
        } else {
            self
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::CleanEnd => "clean_end",
            Self::IntentionalYield => "intentional_yield",
            Self::FailedTermination => "failed_termination",
        }
    }
}

/// What the attempt's output may be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Qualification {
    /// Complete observation, no selected-video decode record, no fault. The
    /// bytes may be retained and served as a cache artifact.
    Qualified,
    /// Nothing was proved wrong, but something was not observed — a truncated
    /// stream, an unqualified build, an incomplete drain. The bytes may still
    /// be served to the viewer who is waiting for them; they may not become a
    /// durable artifact.
    Unqualified,
    /// Something was proved wrong. A decode fault was observed on the selected
    /// video stream.
    Rejected,
}

impl Qualification {
    pub fn name(self) -> &'static str {
        match self {
            Self::Qualified => "qualified",
            Self::Unqualified => "unqualified",
            Self::Rejected => "rejected",
        }
    }

    /// Whether this attempt's bytes may be kept as a reusable artifact.
    pub fn permits_reuse(self) -> bool {
        matches!(self, Self::Qualified)
    }

    /// How much this conclusion permits, as an order. `Rejected` is the least
    /// permissive and `Qualified` the most.
    fn rank(self) -> u8 {
        match self {
            Self::Rejected => 0,
            Self::Unqualified => 1,
            Self::Qualified => 2,
        }
    }

    /// The more restrictive of two conclusions.
    pub fn weaker_of(self, other: Self) -> Self {
        if other.rank() < self.rank() {
            other
        } else {
            self
        }
    }
}

/// The version of the receipt schema. A reader that does not recognize it
/// refuses the artifact rather than guessing at the fields it can see.
pub const PRODUCER_HEALTH_RECEIPT_VERSION: u32 = 1;

/// What one producer attempt's observation concluded.
///
/// §6.2's field list, and the thing a cache reader has to authenticate before
/// calling anything reusable. It is deliberately small and flat: it is stored
/// beside media objects, read on a hot path, and has to survive a build that
/// predates whatever writes it next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProducerHealthReceipt {
    pub receipt_version: u32,
    /// Which plan produced these bytes. A receipt that does not name this
    /// attempt's plan is evidence about some other work.
    pub plan_digest: String,
    /// The contract id whose grammar read this stream, when one covered the
    /// build. `None` means the build was not qualified, which is an
    /// observation-only attempt however clean its output looked.
    pub diagnostic_contract: Option<String>,
    pub observation_complete: bool,
    /// Structural primary records on the selected video stream. §7.3: isolated
    /// errors below the fault threshold still make the output unqualified.
    pub video_decode_error_records: u64,
    /// The subset of those that also carried the build receipt.
    pub contract_qualified_error_records: u64,
    pub terminal_fault: Option<DecodeFaultKind>,
    pub exit_disposition: ExitDisposition,
    pub qualification: Qualification,
}

/// The version of the retained part record. A reader that does not recognize
/// it treats the part as unobserved rather than guessing at what it means.
pub const RETAINED_PART_RECEIPT_VERSION: u32 = 1;

/// One part's receipt, written beside the part and bound to its bytes.
///
/// A generation is encoded across many preempted passes, and a pass that
/// cannot read back what an earlier pass observed has to call every inherited
/// part unobserved — which makes every film long enough to need two passes
/// permanently uncertifiable. That is not a conservative default, it is a
/// broken one: it means the qualified artifact namespace could never hold a
/// long title at all.
///
/// So the observation is written down where the bytes are. What makes it
/// evidence rather than an assertion is `part_shape`: the receipt is bound to
/// the exact segment list, sizes and durations it was settled over, so it
/// cannot be carried onto a part that was re-encoded, truncated, or replaced.
/// `record_digest` covers the rest, so a torn or rotted record is refused
/// instead of read. It is an unkeyed digest over public inputs, so it is
/// corruption-evident, not tamper-evident: it says nothing to anyone who can
/// write into the part directory. That is the right bound for where these
/// live — a node-local staging tree — and the generation manifest, which is
/// the artifact peers actually read, authenticates its own copy separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedPartReceipt {
    pub record_version: u32,
    /// Digest of the part's shape, as [`part_shape_digest`] computes it.
    pub part_shape: String,
    pub receipt: ProducerHealthReceipt,
    pub record_digest: String,
}

/// The join of every observed attempt on one generation that contributed no
/// bytes, carried across passes at the staging root.
///
/// A part's record can only describe a part that exists. The attempt this
/// whole effort is named after produces no part: FFmpeg drops every frame and
/// exits zero, `read_part` returns nothing, and the directory is reused by the
/// retry. Within one pass that receipt is still recorded, so the generation is
/// refused; across a pass boundary it would be forgotten, and whether a film
/// certified would depend on where preemption happened to fall. This is where
/// it is kept instead.
///
/// It is a monotone weakening accumulator: each pass joins its own
/// non-producing attempts into whatever it read and writes the result back, so
/// the value can only ever become more restrictive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetainedGenerationHealth {
    pub record_version: u32,
    pub receipt: ProducerHealthReceipt,
    pub record_digest: String,
}

#[derive(Serialize)]
struct RetainedGenerationBody<'a> {
    record_version: u32,
    receipt: &'a ProducerHealthReceipt,
}

impl RetainedGenerationHealth {
    pub fn seal(receipt: ProducerHealthReceipt) -> Result<Self, String> {
        let record_digest = Self::body_digest(RETAINED_PART_RECEIPT_VERSION, &receipt)?;
        Ok(Self {
            record_version: RETAINED_PART_RECEIPT_VERSION,
            receipt,
            record_digest,
        })
    }

    fn body_digest(record_version: u32, receipt: &ProducerHealthReceipt) -> Result<String, String> {
        use sha2::{Digest, Sha256};

        let encoded = serde_json::to_vec(&RetainedGenerationBody {
            record_version,
            receipt,
        })
        .map_err(|error| format!("serializing a retained generation record: {error}"))?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }

    /// The carried receipt, if this record is one this build wrote for this
    /// plan and its digest still checks out.
    ///
    /// Unlike a part record, there is no shape to bind to — the attempts this
    /// describes left no bytes. The plan digest carried inside the receipt is
    /// the binding, and the staging tree it lives in is per-recipe.
    pub fn opened(&self, plan_digest: &str) -> Option<&ProducerHealthReceipt> {
        (self.record_version == RETAINED_PART_RECEIPT_VERSION
            && self.receipt.plan_digest == plan_digest
            && Self::body_digest(self.record_version, &self.receipt)
                .is_ok_and(|actual| actual == self.record_digest))
        .then_some(&self.receipt)
    }
}

#[derive(Serialize)]
struct RetainedPartBody<'a> {
    record_version: u32,
    part_shape: &'a str,
    receipt: &'a ProducerHealthReceipt,
}

/// What a part's bytes look like, at the granularity that decides whether a
/// receipt still describes them.
///
/// Names, sizes and durations rather than content. The segment *content* is
/// hashed by the generation manifest at publication and verified on every
/// serve, so hashing it again here would buy nothing and cost a full read of
/// the film on every resume — the exact cost resuming exists to avoid. What
/// this has to catch is a receipt outliving the part it was settled over, and
/// a re-encoded part cannot keep every segment's byte count and duration.
pub fn part_shape_digest(plan_digest: &str, segments: &[(String, u64, i64)]) -> String {
    use sha2::{Digest, Sha256};

    let mut digest = Sha256::new();
    let mut feed = |name: &str, value: &[u8]| {
        digest.update((name.len() as u32).to_le_bytes());
        digest.update(name.as_bytes());
        digest.update((value.len() as u32).to_le_bytes());
        digest.update(value);
    };
    feed(
        "shape_version",
        RETAINED_PART_RECEIPT_VERSION.to_string().as_bytes(),
    );
    feed("plan", plan_digest.as_bytes());
    feed("segments", segments.len().to_string().as_bytes());
    for (name, bytes, duration_ms) in segments {
        feed("segment", name.as_bytes());
        feed("bytes", bytes.to_string().as_bytes());
        feed("duration_ms", duration_ms.to_string().as_bytes());
    }
    hex::encode(digest.finalize())
}

impl RetainedPartReceipt {
    /// Bind a settled receipt to the shape of the part it describes.
    pub fn seal(part_shape: String, receipt: ProducerHealthReceipt) -> Result<Self, String> {
        let record_digest =
            Self::body_digest(RETAINED_PART_RECEIPT_VERSION, &part_shape, &receipt)?;
        Ok(Self {
            record_version: RETAINED_PART_RECEIPT_VERSION,
            part_shape,
            receipt,
            record_digest,
        })
    }

    /// The version is digested from the record rather than from the constant,
    /// so the field is covered by its own digest. It is redundant while
    /// `opened` accepts exactly one version, and it is the difference between
    /// safe and silently unauthenticated the day a reader accepts two.
    fn body_digest(
        record_version: u32,
        part_shape: &str,
        receipt: &ProducerHealthReceipt,
    ) -> Result<String, String> {
        use sha2::{Digest, Sha256};

        let encoded = serde_json::to_vec(&RetainedPartBody {
            record_version,
            part_shape,
            receipt,
        })
        .map_err(|error| format!("serializing a retained part receipt: {error}"))?;
        Ok(hex::encode(Sha256::digest(encoded)))
    }

    /// The receipt this record carries, if it still describes `part_shape`.
    ///
    /// `None` for a record from another version, a record whose own digest
    /// does not check out, or a record bound to a different part. Every one of
    /// those means the caller must treat the part as unobserved, which is what
    /// it would have done before this record existed.
    pub fn opened(&self, part_shape: &str) -> Option<&ProducerHealthReceipt> {
        (self.record_version == RETAINED_PART_RECEIPT_VERSION
            && self.part_shape == part_shape
            && Self::body_digest(self.record_version, &self.part_shape, &self.receipt)
                .is_ok_and(|actual| actual == self.record_digest))
        .then_some(&self.receipt)
    }
}

impl ProducerHealthReceipt {
    /// A receipt for work this daemon did not observe at all.
    ///
    /// Used where a process exists but no diagnostic stream was owned — the
    /// paths M3b has not reached yet. It is `Unqualified` by construction, so
    /// a missing observer can never be mistaken for a clean one.
    pub fn unobserved(plan_digest: String, exit_disposition: ExitDisposition) -> Self {
        Self {
            receipt_version: PRODUCER_HEALTH_RECEIPT_VERSION,
            plan_digest,
            diagnostic_contract: None,
            observation_complete: false,
            video_decode_error_records: 0,
            contract_qualified_error_records: 0,
            terminal_fault: None,
            exit_disposition,
            qualification: Qualification::Unqualified,
        }
    }

    pub fn permits_reuse(&self) -> bool {
        self.receipt_version == PRODUCER_HEALTH_RECEIPT_VERSION
            && self.qualification.permits_reuse()
    }

    /// Reduce every part's receipt to the one receipt the generation presents.
    ///
    /// Weakest wins in every field, because a generation is one artifact: its
    /// bytes are reusable only if *every* part that contributed to them was
    /// observed and found clean. One resumed part that this run did not watch
    /// carries an `unobserved` receipt, and that alone makes the whole
    /// generation unqualified — which is the point. The alternative, ignoring
    /// the parts nobody watched, certifies a generation from a minority of its
    /// own bytes.
    ///
    /// `plan_digest` is the generation's. A part naming a different plan is
    /// evidence about other work, so it cannot certify these bytes; it weakens
    /// the join rather than being quietly dropped.
    ///
    /// `None` for an empty list: a generation with no receipts at all carries
    /// none, and a reader treats an absent receipt exactly as it treats an
    /// unqualified one.
    pub fn join(plan_digest: &str, parts: &[Self]) -> Option<Self> {
        let (first, rest) = parts.split_first()?;
        let mut observation_complete = true;
        let mut video_decode_error_records = 0_u64;
        let mut contract_qualified_error_records = 0_u64;
        let mut terminal_fault = None;
        let mut exit_disposition = ExitDisposition::CleanEnd;
        let mut qualification = Qualification::Qualified;
        // `Some` only while every part so far named the same contract. Parts
        // read by different grammars have not been read by one grammar, and
        // the joined receipt must not claim they were.
        let mut diagnostic_contract = first.diagnostic_contract.clone();
        for part in std::iter::once(first).chain(rest) {
            if part.receipt_version != PRODUCER_HEALTH_RECEIPT_VERSION
                || part.plan_digest != plan_digest
            {
                // A part naming another plan, or written by another schema, is
                // evidence about work this generation cannot claim. Its
                // conclusion still weakens the join — dropping it would let a
                // mixture certify itself — but its contract identifier is not
                // attributable, so the generation names none. Its counters,
                // fault and ending are merged below for the same reason its
                // qualification is: every one of them can only make the answer
                // more restrictive.
                qualification = qualification.weaker_of(Qualification::Unqualified);
                observation_complete = false;
                diagnostic_contract = None;
            }
            if part.diagnostic_contract != diagnostic_contract {
                // Parts read by different grammars have not been read by one
                // grammar, and the generation cannot say which read it. It is
                // reachable in ordinary operation now that a part survives a
                // pass: an FFmpeg upgrade between two passes of the same film
                // gives two parts two contracts under one plan digest, which
                // does not name the build. Erasing the name without weakening
                // would leave `Qualified` beside `diagnostic_contract: None` —
                // indistinguishable, to a reader, from a receipt nobody's
                // grammar ever covered.
                diagnostic_contract = None;
                qualification = qualification.weaker_of(Qualification::Unqualified);
            }
            observation_complete = observation_complete && part.observation_complete;
            video_decode_error_records =
                video_decode_error_records.saturating_add(part.video_decode_error_records);
            contract_qualified_error_records = contract_qualified_error_records
                .saturating_add(part.contract_qualified_error_records);
            terminal_fault = terminal_fault.or(part.terminal_fault);
            exit_disposition = exit_disposition.weaker_of(part.exit_disposition);
            qualification = qualification.weaker_of(part.qualification);
        }
        if terminal_fault.is_some() {
            qualification = qualification.weaker_of(Qualification::Rejected);
        }
        Some(Self {
            receipt_version: PRODUCER_HEALTH_RECEIPT_VERSION,
            plan_digest: plan_digest.to_owned(),
            diagnostic_contract,
            observation_complete,
            video_decode_error_records,
            contract_qualified_error_records,
            terminal_fault,
            exit_disposition,
            qualification,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAN: &str = "1111111111111111111111111111111111111111111111111111111111111111";
    const OTHER_PLAN: &str = "2222222222222222222222222222222222222222222222222222222222222222";

    fn clean(plan: &str) -> ProducerHealthReceipt {
        ProducerHealthReceipt {
            receipt_version: PRODUCER_HEALTH_RECEIPT_VERSION,
            plan_digest: plan.to_owned(),
            diagnostic_contract: Some("ffmpeg-test-v1".to_owned()),
            observation_complete: true,
            video_decode_error_records: 0,
            contract_qualified_error_records: 0,
            terminal_fault: None,
            exit_disposition: ExitDisposition::CleanEnd,
            qualification: Qualification::Qualified,
        }
    }

    #[test]
    fn a_generation_with_no_receipts_carries_none() {
        assert_eq!(ProducerHealthReceipt::join(PLAN, &[]), None);
    }

    #[test]
    fn every_part_clean_qualifies_the_generation() {
        let joined = ProducerHealthReceipt::join(PLAN, &[clean(PLAN), clean(PLAN), clean(PLAN)])
            .expect("joined receipt");
        assert_eq!(joined.qualification, Qualification::Qualified);
        assert!(joined.permits_reuse());
        assert!(joined.observation_complete);
        assert_eq!(joined.plan_digest, PLAN);
        assert_eq!(
            joined.diagnostic_contract.as_deref(),
            Some("ffmpeg-test-v1")
        );
    }

    #[test]
    fn one_unobserved_part_unqualifies_the_whole_generation() {
        // The resumed-part case, which is the entire reason the join exists: a
        // film picked up from an earlier pass must not be certified from the
        // tail this pass happened to watch.
        let parts = [
            ProducerHealthReceipt::unobserved(PLAN.to_owned(), ExitDisposition::CleanEnd),
            clean(PLAN),
            clean(PLAN),
        ];
        let joined = ProducerHealthReceipt::join(PLAN, &parts).expect("joined receipt");
        assert_eq!(joined.qualification, Qualification::Unqualified);
        assert!(!joined.permits_reuse());
        assert!(!joined.observation_complete);
        // An unobserved part names no contract, so the generation names none.
        assert_eq!(joined.diagnostic_contract, None);
    }

    #[test]
    fn a_rejected_part_rejects_the_generation_and_keeps_its_fault() {
        let mut bad = clean(PLAN);
        bad.qualification = Qualification::Rejected;
        bad.terminal_fault = Some(DecodeFaultKind::VideoDecodeFailure);
        bad.video_decode_error_records = 5;
        bad.contract_qualified_error_records = 5;
        let joined =
            ProducerHealthReceipt::join(PLAN, &[clean(PLAN), bad, clean(PLAN)]).expect("joined");
        assert_eq!(joined.qualification, Qualification::Rejected);
        assert_eq!(
            joined.terminal_fault,
            Some(DecodeFaultKind::VideoDecodeFailure)
        );
        assert_eq!(joined.video_decode_error_records, 5);
        assert_eq!(joined.contract_qualified_error_records, 5);
    }

    #[test]
    fn structural_records_below_the_fault_threshold_still_unqualify() {
        // §7.3: isolated errors that never latched a fault are still enough to
        // refuse reuse. The part's own receipt says so; the join must not
        // launder it back to qualified by summing counters and forgetting the
        // conclusion.
        let mut noisy = clean(PLAN);
        noisy.qualification = Qualification::Unqualified;
        noisy.video_decode_error_records = 2;
        let joined = ProducerHealthReceipt::join(PLAN, &[clean(PLAN), noisy]).expect("joined");
        assert_eq!(joined.qualification, Qualification::Unqualified);
        assert_eq!(joined.terminal_fault, None);
        assert_eq!(joined.video_decode_error_records, 2);
    }

    #[test]
    fn a_part_from_another_plan_cannot_certify_these_bytes() {
        let joined =
            ProducerHealthReceipt::join(PLAN, &[clean(PLAN), clean(OTHER_PLAN)]).expect("joined");
        assert_eq!(joined.qualification, Qualification::Unqualified);
        assert!(!joined.observation_complete);
        assert_eq!(joined.plan_digest, PLAN);
    }

    #[test]
    fn parts_read_by_different_grammars_name_no_contract_and_do_not_qualify() {
        // An FFmpeg upgrade between two passes of one film reaches this.
        let mut other = clean(PLAN);
        other.diagnostic_contract = Some("ffmpeg-other-v1".to_owned());
        let joined = ProducerHealthReceipt::join(PLAN, &[clean(PLAN), other]).expect("joined");
        assert_eq!(joined.diagnostic_contract, None);
        assert_eq!(joined.qualification, Qualification::Unqualified);
        assert!(
            !joined.permits_reuse(),
            "`Qualified` with no contract named is indistinguishable from never having had one"
        );
    }

    #[test]
    fn a_failed_part_makes_the_generation_a_failed_termination() {
        let mut failed = clean(PLAN);
        failed.exit_disposition = ExitDisposition::FailedTermination;
        failed.qualification = Qualification::Unqualified;
        let joined = ProducerHealthReceipt::join(PLAN, &[clean(PLAN), failed]).expect("joined");
        assert_eq!(joined.exit_disposition, ExitDisposition::FailedTermination);
    }

    #[test]
    fn an_intentional_yield_is_normal_and_still_qualifies() {
        // Every part of a long film in this pipeline ends by yielding. If a
        // yield weakened the join, nothing would ever be certified.
        let mut yielded = clean(PLAN);
        yielded.exit_disposition = ExitDisposition::IntentionalYield;
        let joined = ProducerHealthReceipt::join(PLAN, &[yielded, clean(PLAN)]).expect("joined");
        assert_eq!(joined.exit_disposition, ExitDisposition::IntentionalYield);
        assert_eq!(joined.qualification, Qualification::Qualified);
        assert!(joined.permits_reuse());
    }

    #[test]
    fn an_unrecognized_receipt_version_never_permits_reuse() {
        let mut future = clean(PLAN);
        future.receipt_version = PRODUCER_HEALTH_RECEIPT_VERSION + 1;
        assert!(!future.permits_reuse());
        let joined = ProducerHealthReceipt::join(PLAN, &[clean(PLAN), future]).expect("joined");
        assert_eq!(joined.qualification, Qualification::Unqualified);
        assert!(!joined.permits_reuse());
    }

    #[test]
    fn each_counter_sums_its_own_field_and_saturates_there() {
        let mut left = clean(PLAN);
        left.qualification = Qualification::Unqualified;
        left.video_decode_error_records = u64::MAX;
        left.contract_qualified_error_records = 1;
        let mut right = clean(PLAN);
        right.qualification = Qualification::Unqualified;
        right.video_decode_error_records = 4;
        right.contract_qualified_error_records = 2;
        let joined = ProducerHealthReceipt::join(PLAN, &[left, right]).expect("joined");
        // Saturated on its own field, and the qualified count summed on its
        // own — a join that read the wrong field would report `u64::MAX` here.
        assert_eq!(joined.video_decode_error_records, u64::MAX);
        assert_eq!(joined.contract_qualified_error_records, 3);
        assert!(joined.contract_qualified_error_records <= joined.video_decode_error_records);
    }

    #[test]
    fn a_mismatched_plan_weakens_every_field_it_can() {
        let mut foreign = clean(OTHER_PLAN);
        foreign.exit_disposition = ExitDisposition::FailedTermination;
        foreign.video_decode_error_records = 3;
        let joined = ProducerHealthReceipt::join(PLAN, &[clean(PLAN), foreign]).expect("joined");
        assert_eq!(joined.qualification, Qualification::Unqualified);
        assert!(!joined.observation_complete);
        // Not attributable to this generation's grammar, even though both
        // parts happened to name the same contract.
        assert_eq!(joined.diagnostic_contract, None);
        // Still weakening, because every one of these can only restrict.
        assert_eq!(joined.exit_disposition, ExitDisposition::FailedTermination);
        assert_eq!(joined.video_decode_error_records, 3);
    }

    #[test]
    fn the_contract_identifier_bound_is_the_one_a_manifest_can_store() {
        assert!(safe_diagnostic_contract_id(
            "ffmpeg-8.0.1-3ubuntu2-rawvideo-v1"
        ));
        assert!(safe_diagnostic_contract_id(
            &"c".repeat(MAX_DIAGNOSTIC_CONTRACT_BYTES)
        ));
        assert!(!safe_diagnostic_contract_id(
            &"c".repeat(MAX_DIAGNOSTIC_CONTRACT_BYTES + 1)
        ));
        assert!(!safe_diagnostic_contract_id(""));
        // The shape a Debian epoch would produce, which is exactly how this
        // bound gets violated by an innocent table edit.
        assert!(!safe_diagnostic_contract_id(
            "ffmpeg-7:6.1.1-3ubuntu5-h264-v1"
        ));
    }

    fn shape() -> Vec<(String, u64, i64)> {
        vec![
            ("seg00000.ts".to_owned(), 1_048_576, 2_000),
            ("seg00001.ts".to_owned(), 1_040_000, 1_960),
        ]
    }

    #[test]
    fn the_part_shape_moves_when_any_of_the_bytes_it_describes_do() {
        let base = part_shape_digest(PLAN, &shape());
        assert_eq!(base, part_shape_digest(PLAN, &shape()));
        assert_ne!(base, part_shape_digest(OTHER_PLAN, &shape()));

        let mut renamed = shape();
        renamed[1].0 = "seg00002.ts".to_owned();
        assert_ne!(base, part_shape_digest(PLAN, &renamed));

        let mut resized = shape();
        resized[0].1 += 1;
        assert_ne!(base, part_shape_digest(PLAN, &resized));

        let mut retimed = shape();
        retimed[0].2 += 1;
        assert_ne!(base, part_shape_digest(PLAN, &retimed));

        let mut truncated = shape();
        truncated.pop();
        assert_ne!(base, part_shape_digest(PLAN, &truncated));

        // Length-prefixed, so no two different shapes can concatenate to the
        // same byte stream.
        let mut smeared = shape();
        smeared[0].0 = "seg00000.ts1048576".to_owned();
        smeared[0].1 = 0;
        assert_ne!(base, part_shape_digest(PLAN, &smeared));
    }

    #[test]
    fn a_sealed_record_opens_only_for_the_shape_it_was_sealed_over() {
        let digest = part_shape_digest(PLAN, &shape());
        let record = RetainedPartReceipt::seal(digest.clone(), clean(PLAN)).expect("seal");
        assert_eq!(record.opened(&digest), Some(&clean(PLAN)));

        let mut resized = shape();
        resized[0].1 += 1;
        assert_eq!(record.opened(&part_shape_digest(PLAN, &resized)), None);
        assert_eq!(
            record.opened(&part_shape_digest(OTHER_PLAN, &shape())),
            None
        );
    }

    #[test]
    fn a_record_from_another_version_is_not_read() {
        let digest = part_shape_digest(PLAN, &shape());
        let mut record = RetainedPartReceipt::seal(digest.clone(), clean(PLAN)).expect("seal");
        record.record_version = RETAINED_PART_RECEIPT_VERSION + 1;
        assert_eq!(record.opened(&digest), None);
    }

    #[test]
    fn a_promoted_receipt_no_longer_matches_its_own_record_digest() {
        // The record lives in a staging directory beside the bytes. Its digest
        // is what makes a torn or edited record refuse to open rather than
        // hand back a conclusion nobody reached.
        let digest = part_shape_digest(PLAN, &shape());
        let mut unqualified = clean(PLAN);
        unqualified.qualification = Qualification::Unqualified;
        let mut record = RetainedPartReceipt::seal(digest.clone(), unqualified).expect("seal");
        assert!(record.opened(&digest).is_some());
        record.receipt.qualification = Qualification::Qualified;
        assert_eq!(record.opened(&digest), None);
    }

    #[test]
    fn a_record_whose_shape_field_was_edited_to_match_still_fails_its_digest() {
        // Rewriting `part_shape` to the shape actually on disk is the obvious
        // way to make a stale record open. It changes the body, so it changes
        // the digest.
        let mut resized = shape();
        resized[0].1 += 1;
        let sealed_over = part_shape_digest(PLAN, &shape());
        let actual = part_shape_digest(PLAN, &resized);
        let mut record = RetainedPartReceipt::seal(sealed_over, clean(PLAN)).expect("seal");
        record.part_shape = actual.clone();
        assert_eq!(record.opened(&actual), None);
    }
}
