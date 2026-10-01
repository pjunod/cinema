# Automatic quality review — adversarial findings and their disposition

**Status:** planning findings reconciled and rechecked before implementation;
GPT-6.1 Sol implementation underway · **Written:** 2026-09-30

Companion to [the implementation plan](DISPLAY-AWARE-AUTO-QUALITY-PLAN.md).
A separate Codex agent, `adversarial_quality_review`, reviewed the initial draft
against repository source. Its initial verdict was **directionally sound, but
revise before implementation**. The five findings below are that review's
ranked findings, with the author's subsequent dispositions recorded separately.
No runtime changes, encoder benchmarks or physical playback tests were made
during that planning review.
The [independent Opus review](DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.md) was
subsequently supplied by Paul; its [source text](DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.txt)
is preserved unchanged behind a lifecycle wrapper. Its verdict was
**revise first, narrowly**. §4 records this later reconciliation; the earlier
internal findings below are historical dispositions, not a claim that Opus
approved the revised plan. The [handoff](DISPLAY-AWARE-AUTO-QUALITY-OPUS-HANDOFF.md)
now provides a follow-up review prompt if another Opus pass is desired.

## 1. Findings and changes

### R1 · P1 — an Auto height does not choose a delivery route

**Finding:** `plan_preparation_candidate` in
[preparation.rs](../../crates/plurxd/src/http/hls/preparation.rs) resolves
compatibility and `copy` before applying the requested height.
[CreateSession](../../crates/plurxd/src/http/hls/create.rs) ignores height for
copy, and legacy `candidate_request` in
[playback_control.rs](../../crates/plurxd/src/playback_control.rs) preserves
delivery method for Auto. A compatible but heavy original could ignore a
1440 Auto request; the reverse transition is also not expressible merely by
changing height.

**Disposition: accepted.** Plan §4.2 now requires a negotiated Auto candidate
request, covering copy/encode route, source revision, geometry, grade and
recipe identity, with server-side validation. It defines absent-field legacy
behavior, unchanged old digests, new candidate digest inclusion, replay,
obsolete-candidate handling and manual supersession. M1/M4 must exercise
copy→1440 encode→copy while retaining Auto intent. A response-only candidate
ID is expressly insufficient. Final wire types and planning-function tests
are a named M0 prerequisite, not work presumed complete.

### R2 · P1 — geometry promises outran source facts and decoder constraints

**Finding:** `MediaFile` has coded width/height but no SAR or rotation;
`output_size` in [core transcode](../../crates/plurx-core/src/transcode/mod.rs)
uses only those dimensions. [VideoCaps](../../crates/plurx-core/src/playback/caps.rs)
lacks a width/rate envelope, and [Android probing](../../clients/android/app/src/main/java/tv/plurx/app/data/Caps.kt)
checks standard 16:9 sizes at 30 fps. Source frame rate does already exist in
`DecodeFacts::frame_rate` in
[decode.rs](../../crates/plurx-core/src/transcode/decode.rs).
A geometry helper could pass synthetic tests while the production planner and
encoder still disagree, or a wide frame exceeds the decoder's proved width.

**Disposition: accepted.** Plan §4.4 now owns the source-facts adapter, reuses
DecodeFacts, carries facts through planning/preparation/remote execution, and
requires source-revision-scoped caching. It explicitly normalizes new
rotated/anamorphic candidates to upright square pixels and accounts for
ffmpeg autorotation once. Decoder width/rate/profile limits, conservative
unknowns, actual output/manifests, old-source behavior and conditional schema
migration tests are milestone work. Additional catalog columns are not
assumed necessary; M0 chooses the minimal transport/storage after inspection.

### R3 · P1 — Apple cannot manufacture fresh cliff evidence

**Finding:** [A-04 §§7.7 and 8.4.1](../clients/NATIVE-ADAPTIVE-QUALITY-DESIGN.md)
records no proved per-completed-transfer Apple sample. Smoothed
`observedBitrate` cannot simply populate the fresh sample field. A reducer can
pass synthetic fixtures while its emergency branch is unreachable in the
real adapter, making the common ten-second cliff target misleading.

**Disposition: accepted.** Plan §§2.1 and 4.4 explicitly depend on A-05's
observation work. M0 must measure the source of fresh evidence before M5.
Absent that source, initial display-aware selection can work, but full
cross-platform cliff/recovery completion remains blocked. The plan prohibits
marking that acceptance N/A to manufacture parity; reducing scope requires a
separate user decision. No Apple measurement is claimed.

### R4 · P1 — the polling limit would slow an existing recovery owner

**Finding:** The draft's blanket five-second server-status limit conflicts
with Apple's two-second `startStatusPolling`, which supplies
`observeDeliveryStarvation` and preparation measurements. A-04 §2.6 protects
that cadence; it is not merely stats polling.

**Disposition: accepted.** Plan §4.3 now distinguishes the web policy polling
limit from existing native owner cadences, preserves Apple's two-second poll,
and requires observation reuse instead of a second poller. Evidence freshness
is separately defined. The plan no longer instructs implementers to slow
existing recovery.

### R5 · P2 — “reuse web behavior” concealed actual policy differences

**Finding:** `decideRung` uses `max(cooldownMs, dwellMs)` = 60 seconds for
voluntary changes, not merely the 20-second cooldown constant. Current
`predictedSpeed(null)` permits upgrades, while the draft's missing-evidence
rule does not. The [shared fixture](../../tests/playback/auto-quality-policy.json)
already records design disagreements, including stall-scoped holds versus
healthy producer pacing. Blindly treating all holds as unhealthy can suppress
upgrades forever.

**Disposition: accepted.** Plan §2.1 explicitly extends A-04/A-05, reuses the
existing fixture and preserves disagreements until each receives a decision.
§4.3 states the real 60-second voluntary gap and labels stricter missing-speed
behavior as a proposed policy change requiring fixtures and a measurement
path. Routine paced holds are not stall evidence. A-05 owns cause classification
and native adapters; its shaped-trace-before-controller-merge requirement is
inherited by M4/M5. The six-switch budget remains a proposal, not a fabricated
current setting. §8.3 makes unknown-speed/paced-producer upward recovery a
specific experiment that must close before implementation claims completeness.

## 2. Questions originally retained for Opus and qualification

**Historical internal-review table.** These questions were sent to Opus before
Paul chose the combined effort and mid-play restoration. The current answers
and execution contract are in §4 and plan §6.1; this table does not reopen
those product decisions. These were not dismissed by accepting the internal
findings:

| Question | Plan response / remaining decision |
|---|---|
| Is smallest display-covering encode genuinely the best picture? | §3.1 makes it a starting hypothesis. §8 compares outputs rendered at target size; a material improvement from a larger sustainable encode must change the preference rather than be ignored. Opus should challenge the criterion before implementation. |
| Can reduced playback return to original without guessing bandwidth? | §4.3 proposes passive already-produced transfer evidence or bounded preparation, a healthy-window requirement, single candidate, cancellation on lost runway and five-minute failure backoff. M0 must prove the evidence path is attainable without harming the incumbent. |
| Are two native-controller plans competing? | §2.1 assigns existing A-05 work its ownership, requires board coordination and one effort or qualified dependency. This document supplies new display/route requirements; it is not a second independent controller project. |
| Is the scope proportional? | Android SDR/tablet is first, but the requested automatic behavior spans supported clients. Unsupported 1440 HDR combinations may remain unadvertised with tested fallback. Apple cliff parity cannot be silently dropped. Opus should assess whether the effort needs an explicit product split. |
| Can the tablet run the temporary second decoder? | §4.2 and §8 require device evidence and admission; a dual-player flag alone is insufficient. Failed voluntary preparation retains the incumbent. This remains a physical qualification obligation. |

## 3. Verification record

This is a documentation-only planning pass. The repository documentation-index
suite and an additional relative-link/index check covering the new untracked
documents are the relevant checks. Runtime suites and physical acceptance
remain future implementation work; no missing measurement has been reported
as zero or passing. Final checks on 2026-09-30:

- `python3 -m unittest discover -s tests/operations -p test_docs_index.py`:
  **4 tests passed**.
- Additional index and relative-link validation for all three new documents:
  **passed**. This matters because the repository suite discovers documents
  through `git ls-files`, while these new documents are not yet committed.
- `git diff --check`: **passed**.

The same independent reviewer re-read the revisions and verified each R1–R5
disposition. Its revised verdict: **all five findings meaningfully addressed
at the planning level; ready for external Opus review, not implementation
approval**. It found no residual contradiction in those dispositions that
would make the handoff misleading. Candidate wire details, attainable upgrade
evidence, Apple telemetry and physical decoder capacity remain explicit
prerequisites.


## 4. Independent Opus review — all findings accounted for

**Received:** 2026-09-30 · **Reviewer identified in supplied text:**
claude-opus-5-5 (Cowork) · **Its inspected baseline:** `ceb7dd8cc`.
The original text is [preserved here](DISPLAY-AWARE-AUTO-QUALITY-OPUS-REVIEW.txt).
Reconciliation spot-checked relevant source at `b8f3c7587` after the user's
checkout moved to another task branch; it did not fetch or claim current-main
evidence. No unrelated files were changed.

Paul answered three product questions explicitly: **one combined effort**,
**up to 10% enlargement**, and **mid-play returns to original when safely
proved**. Those decisions override the review's recommended feature split and
boundary-only recovery baseline. The integration and evidence risks remain
recorded rather than being treated as resolved by the decision itself.

| Finding | Disposition and plan changes |
|---|---|
| P1-1 · Mixed-version strict relays | **Accepted, strengthened.** §4.1 enumerates nested parsers, owner/ingress/remote/replay paths, three deployment stages, fleet-wide tolerance receipt, owner start advertisement, semantic-worker eligibility and rollback parser floor. Merely accepting optional fields is not permission for an old worker to ignore candidate semantics. Tests distinguish pre-floor and tolerant peers. B-R1 is a behavior-preserving prerequisite; qualified R2 server artifacts precede R3 clients. |
| P1-2 · Initial feature blocked by A-05 | **Risk accepted by explicit user choice; split recommendation declined.** §§2.1, 6.1 and 7 keep one combined feature effort and make A/B workstreams rather than releases. Existing A-05 ownership is integrated into its ledger. Apple/native evidence blocks full promotion; an A checkpoint cannot ship independently. The parser-only infrastructure prerequisite is not a partial feature release. |
| P1-3 · Ordinary HDR tone-map ceiling misses 1440 | **Accepted.** §3.4 names `capability_height_for_encoder`, SDR-source, HDR10→SDR, per-profile DV→SDR and preserved-HDR proofs separately. M0-A records real tablet HDR/profile/level facts; M2-A tests each production worker/burn route under concurrent load before M3-A. Unsupported workers retain truthful lower fallback. An SDR-source pass cannot qualify the HDR-source case. |
| P1-4 · Original recovery stranded by pacing | **Accepted, with broader scope selected by Paul.** §4.3 defines active-production timing excluding known pacing, deterministic re-plan on committed user seek/≥60 s pause/next episode, fresh-link veto and retained decoder limits. Safely proved mid-play return is mandatory: bounded speculative candidate evidence, source-cost/decode proof, incumbent runway protection, five-minute failure backoff and no voluntary reopen. Failure to establish attainable telemetry is a blocker, not a silent boundary-only downgrade. |
| P2-1 · Priors skip 1440 / misattribute encode pressure | **Accepted with arithmetic correction.** §3.3 explicitly replaces both prior lookup branches with route candidates and requires cause provenance for negative network records, including legacy-row coexistence. At a 12 Mb/s target, 1.5× peak plus 160 kb/s audio is 18.16 Mb/s: **14 Mb/s cannot fit 1440**. Fixtures use 20 Mb/s→1440 and 14 Mb/s→1080, while preserving the 2160-starvation→1440 case when that route is eligible. |
| P2-2 · New bitrate band silently changes scope/fps | **Accepted via explicit-profile alternative.** §3.3 uses resolved width/height/fps/codec/grade/version profiles. Legacy nonstandard-height rates stay pinned initially; any correction is separate, measured and versioned. A distinct 60 fps profile must be qualified; comparisons include 3840×1600 source-rung burns. No blind match-band movement or universal linear pixel-rate assumption. |
| P2-3 · Small coverage miss causes a large jump | **Accepted by user decision.** §3.2 uses maximum local enlargement 1.10 on both axes (equivalently coverage ≥1/1.10, not 0.90). Fixtures pin 2000×1200→1080 and 2400×1600→1440. §8 requires equal-total-wire-cost comparisons plus actual production-rate comparisons. |
| P2-4 · Ignores usable pretranscode cache | **Accepted.** §3.3 treats compatible complete immutable cache recipes as zero incremental encode-cost candidates; eligible 2160 with equal/better fidelity can beat live 1440. Wrong-grade/track/source-revision, incomplete and inaccessible entries are excluded; network/decode/storage cost still matters. Pretranscode target-generation policy stays unchanged. |
| P2-5 · Weaker 250 ms oracle | **Accepted.** §8.2 inherits A-04 D3 harness/oracles, including 100 ms gap and zero hitch/stall/restart conditions where required. Forced-reopen measurements stay separate and never excuse D3 failure. No 99.9% aggregate substitute. The 250 ms startup-regression allowance is a different metric, not a presentation-gap threshold. |
| P2-6 · No switch for initial policy | **Accepted.** §5 proposes `playback.display_aware_auto`, Developer card “Fit Auto to the display,” advisory source-grade/device evidence and named graduation. Saved choices are authoritative. `playback.auto_abr` remains A-05's runtime switch; neither card silently replaces the other. |
| P2-7 · String candidate breaks Copy | **Accepted.** §4.2 chooses `CandidateId([u8; 16])`, 32 lowercase hex on the wire, with validation/canonicalization/full recipe checks; optional enums remain `Copy`. Missing identity preserves old digest bytes. |
| P2-8 · Voluntary upgrade consumes/preempts slots | **Accepted, strengthened.** §4.2 requires speculative admission and additionally forbids voluntary predecessor-yield/handoff-permit requests, foreground waiters and fallback reopen. Current speculative priority alone is insufficient because handoff code can ask the incumbent to yield. Full-slots Live TV+viewer fixture preserves both foreground sessions. |
| P2-9 · Height-only web cap misses portrait fit | **Accepted.** M4-A shares the two-axis fitted-picture calculation with current web policy inputs; portrait 1080×2340 with a 2160 source chooses at most 720 when encoding. Existing web behavior changes retain D3 obligations. |
| P2-10 · Feature-branch baseline | **Accepted.** Plan header records both local snapshots as non-main evidence and requires authoritative Forgejo main SHA/census at M0, then exact-base qualification before pushing or promotion. No current-main claim or unnecessary branch checkout was made during documentation work. |

### 4.1 Corrections made while accepting the review

Two examples in Opus need explicit qualification. A 12 Mb/s **target** is not
a 12 Mb/s **peak** under current rate control, so its suggested 14 Mb/s→1440
fixture would be unsafe. Also, a viewer seek or long-pause resume can use the
existing buffered session without reopening; returning to original can add
latency there. The plan measures that cost, coalesces to one owner transaction
and avoids claiming “zero extra interruption” by definition. Audio
incompatibility alone, including TrueHD, must continue to allow video copy;
it does not supply a missing reason to encode the picture.

The enlarged mid-play scope is deliberately not reported as already solved.
M0-B must demonstrate its observation path, and B5 includes a no-seek long-title
restoration trace. Physical evidence and the combined integration dependency
remain outstanding implementation work.


## 5. Final reconciliation check and implementation authorization

The original adversarial agent checked all four Opus P1 and ten P2 dispositions
against the revised plan. It confirmed the combined promotion/staged protocol
rollout, mandatory no-seek mid-play restoration, 1.10 enlargement rule and
corrected rate arithmetic. It identified only stale historical scope wording
and an ambiguous singular rollback switch; both are now corrected. This is a
Codex reconciliation check, not a new Opus verdict.

Final documentation verification covers **all four new documents**, including
the preserved Opus review: repository docs-index suite (4 tests), additional
new-file relative-link/index checks, exact byte comparison of the supplied
review against its preserved copy, and `git diff --check`. All passed on
2026-09-30. No runtime build or physical measurement is claimed by these checks.

Paul then explicitly requested a **GPT-6.1 Sol agent to build the reconciled
plan**. Implementation starts with current-main census, the pinned compiler
loop and the named evidence prerequisites; none of the physical acceptance or
release gates is waived by that authorization.
