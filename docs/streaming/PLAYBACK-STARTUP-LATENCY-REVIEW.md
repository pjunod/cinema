# Playback startup review — prove the transition before lowering the gate

**Status:** revised plan ready for M0–M2 staged investigation; runtime policy
unselected ·
**Reviewed:** 2026-09-29 EDT · **Reviewer:** independent adversarial agent ·
**Reviewed source:** `b5fa758644d413ef73000ded00cedc15766eba02`.

Companion to the [implementation plan](PLAYBACK-STARTUP-LATENCY-IMPLEMENTATION.md).
This document challenges that plan's causal claims, transport assumptions,
state transitions and acceptance criteria. It does not claim a runtime fix,
fresh production reproduction or physical-device qualification. Findings
refer to the initial draft; record reconciliation below rather than erasing
the original review.

## 1. Verdict — revised investigation contract accepted; no runtime acceptance

The revised plan is ready for M0–M2 staged investigation. R1–R3 are addressed
in the written contract as recorded in §6; their runtime proof remains work
for those milestones. M3 remains blocked until M2 selects a policy from
transport evidence. This verdict does not accept an implementation or
justify changing 48 seconds to an arbitrary smaller constant today.

The initial findings below are preserved. They were plan corrections, not
evidence that the proposed implementation already contained a bug. Native
feasibility was already explicitly gated and is not counted again as an
unresolved implementation defect.

| Finding | Priority | Required disposition |
|---|---|---|
| R1: bootstrap has no proved convergence or expiry outcome | P1 | Define states and admission/expiry decisions before selecting M2 policy |
| R2: legal fixed-target timing and removal constraints are implicit | P2 | Add numeric experiment constraints and protocol assertions |
| R3: rate endpoints and immutable-policy semantics are incomplete | P2 | Cover supported extremes and specify which state may change |

## 2. R1 — a producer can sustain playback without reaching steady reserve

**Plan references:** §4.2 policy shape and source-class bound; §4.3 finite
bootstrap deadline and steady transition; §7.1 barely sufficient producers;
§8 two-minute startup runs.

**Failure:** §4.3 requires both enough served reserve for a full steady cycle
and a finite bootstrap deadline derived from existing startup budgets. It
does not say what happens if playback has started successfully but reserve
has not grown enough by that deadline. Sustaining playback does not prove
that this transition can finish promptly.

For example, assume a candidate serves 24 usable media seconds and needs 48
for its selected steady policy. At 1× consumption and 1.05× production, even
perfect continuous production accumulates those extra 24 seconds in roughly
480 wall seconds. At 1.2× it takes 120 seconds. Segment rounding, transfer
cost and I/O pauses can only worsen this lower bound. A two-minute stall-free
trace can therefore pass while the selected state transition remains
unproved. The example is conditional on those candidate values; M2 may
choose a different, demonstrated steady requirement.

**Source evidence:**
[RollingStartupState](../../crates/plurxd/src/playback_control.rs) starts the
30-second presentation budget at first served media and removes that
deadline after accepted presentation progress. That deadline guards failure
to start, not reserve accumulation after starting.
[evaluate_flow](../../crates/plurxd/src/transcode/rolling/flow.rs) also stops
granting presentation startup protection after that transition. The current
[publication cycle](../../crates/plurxd/src/transcode/rolling/session.rs)
then schedules a target-period update and maintains a separate publication
hard deadline. Reusing one of these timers for a new meaning changes the
lifecycle contract.

**Required correction:** distinguish first-response readiness,
presentation-start protection and reserve accumulation. Give each proposed
bootstrap state an explicit entry, exit, deadline and expiry action. M2
must prove the transition bound from admitted production and consumption,
or retain the existing readiness policy for that class before its first
response. An unknown or briefly fast producer cannot acquire a stronger
capacity guarantee merely from an early speed sample. If continued bounded
bootstrap after successful presentation is a candidate, specify its own
resource/time contract and test it; do not silently keep presentation
startup protection alive. A timeout must neither jump the served edge nor
switch cadence with inadequate runway nor turn a previously successful
class into systematic playback failures.

**Acceptance:** deterministic traces at 1.05× and 1.2× run through actual
cutover or the explicitly selected expiry disposition, including a pause,
a rate increase and a refused scratch grant. The M2 receipt identifies how
the required class is known before release. Physical continuity evidence
covers the full selected transition, even when it exceeds two minutes.

## 3. R2 — encode protocol timing before measuring candidate wins

**Plan references:** §4.2 runway sweep; §4.3 bootstrap cadence and retention;
§4.4 fixed target; §7.1 decision receipt; §7.2 legal cadence requirement.

**Failure:** the draft says cadence must be legal and native reload timing
must be measured, but leaves the actual limits implicit. That permits an
experiment to report an attractive result from a schedule it cannot ship.
It also leaves early-prefix removal unspecified while exploring playlists
shorter than the historical 48 seconds.

**Protocol evidence:** [RFC 8216 §6.2.1](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.2.1)
bounds non-final segment-bearing updates to half through 1.5 target
durations: 8–24 seconds at a 16-second target.
[§6.3.4](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.3.4)
specifies a target-duration wait after an initial or changed playlist and
half a target after an unchanged reload. Thus an 8-second server update does
not prove an 8-second discovery bound.
[§6.3.3](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.3.3)
advises initial live selection away from the final three target durations;
this is a client-behavior risk, not a universal ban on short initial
playlists. [§6.2.2](https://www.rfc-editor.org/rfc/rfc8216.html#section-6.2.2)
prohibits segment removal that would leave a non-final playlist shorter than
three targets. Keep the startup and removal rules distinct.

**Source evidence:** the existing
[publication constants](../../crates/plurxd/src/transcode/rolling/publication.rs)
are 8-second earliest, 16-second ordinary and 24-second hard timing, with a
separate 250 ms worker poll. The
[snapshot renderer](../../crates/plurxd/src/transcode/rolling/segment_index.rs)
fixes the target at 16 seconds and validates actual durations. Existing
retention in the [publication cycle](../../crates/plurxd/src/transcode/rolling/session.rs)
can choose a first segment from retention and protected-position state.

**Required correction:** prefilter M2 schedules by these limits, assert them
in the harness and record actual reload behavior separately. Forbid prefix
removal while it would leave less than 48 seconds in a non-final playlist.
Include an unchanged-reload phase and the worst phase alignment between
server updates and client requests. Treat failed native start selection as
a failed candidate; §4.4 already correctly requires a separate design for
changing the fixed target.

**Acceptance:** a latency win obtained by a too-fast update, a forced
non-shipped reload loop, a shortened target or premature prefix removal is
rejected before policy selection. Generated traces distinguish those
protocol assertions from observed physical-client behavior.

## 4. R3 — cover the whole admitted rate range and freeze the right facts

**Plan references:** §4.2 attempt-created policy; §4.3 recomputation on rate
changes; §8 consumption matrix; §9.3 policy applies to new attempts.

**Failure:** the proposed rate sweep covers 0.5× through 2×, but the current
flow policy accepts a clamped 0.25×–4× range. Its initial runway also has a
48-second lower floor and a retention-derived upper cap. Replacing that
function can change behavior at endpoints the plan never requires testing.
Additionally, an immutable policy resolved at attempt creation and a policy
recomputed after a rate change are different contracts unless the document
distinguishes immutable coefficients from effective derived bounds.

**Source evidence:**
[rolling_playback_rate and rolling_initial_runway_ms](../../crates/plurxd/src/transcode/rolling/flow.rs)
implement those clamps. The
[publication budget](../../crates/plurxd/src/transcode/rolling/publication.rs)
uses accepted demand and media origin; the actor's presentation baseline in
[playback_control.rs](../../crates/plurxd/src/playback_control.rs) additionally
fences generation, owner epoch, producer attempt and timeline sequence.

**Required correction:** add deterministic 0.25× and 4× cases, both clamp
boundaries, and rate increases/decreases before and after first publication.
Describe which values are frozen for the attempt (transport qualification,
target and selected policy version), which derive from accepted rate, and
which reset on a new producer attempt versus a seek in the same attempt.
Existing snapshots and resource grants cannot shrink retroactively. Unknown
transport must explicitly preserve the qualified old policy rather than
becoming a permissive default after a state change.

**Acceptance:** tests prove all derived bounds fit the retained window and
charged scratch at the rate extremes, or yield the existing truthful
capacity refusal. They also prove that rate changes neither reset first
availability/deadlines nor weaken exact-attempt response admission.

## 5. Challenges the draft already answers

The incident attribution is properly bounded: the six variable durations
sum to 58.558 seconds and the requested-origin-adjusted gate is 55.965
seconds. This supports the sixth-segment alignment without pretending the
first segment could have appeared within one second. M0 still owns a
controlled old-tree reproduction.

The index backlog is not presented as proved starvation. M1 checks exact
recipe identity, eligibility, resource occupancy and repeated worker cycles,
and permits a diagnosis without a scheduler patch. This is the correct
response to a point-in-time queue snapshot.

The plan preserves actor response admission, source identity, init
validation, scratch ownership and cleanup. It avoids replacing every shared
runway use blindly. Missing physical-client evidence and a need for a new
target duration are already explicit gates. Neither is grounds for calling
the current draft unsafe by default; they are experiments it has not yet
performed.

The effort branch and exact-integrated-candidate compiler evidence are
specified. The named test filters identify existing tests, and the plan
requires checking nonzero selections. Runtime compilation and production
tests were not run for this documentation review.

## 6. Reconciliation — preserve the review and record corrections

The initial reviewed draft's SHA-256, retained by the planning agent before
revision, was
`0adb3f71f42b9f1e93807fe65c00b4e3095caa484dea19ad87dbf2316071e7ec`.
On 2026-09-29 EDT the reviewer reread revised §4.2–§4.3, M2/M3 and §8 against
the original findings and source contracts. The following dispositions
accept the corrected investigation requirements, not unperformed tests.

| Finding | Disposition after revision | Verification |
|---|---|---|
| R1 | Addressed in plan | §4.3 separates prepublication, awaiting presentation, active low reserve and steady state. Active low reserve has no reserve-growth expiry or renewed startup protection; ordinary lease, capacity and watchdog limits still apply. M2 must prove sustained operation or retain the old policy before first response. M2/§8 require actual-transition or stable-non-transition traces, including longer physical runs. |
| R2 | Addressed in plan | §4.2 makes the fixed-target update, reload and minimum-removal-window constraints explicit. M2 rejects protocol-invalid candidates and tests unchanged reloads and phase alignment. The native-selection risk remains a qualification condition. |
| R3 | Addressed in plan | §4.2 and §8 include 0.25×/4× endpoints and clamp boundaries. §4.3 distinguishes frozen policy facts from derived bounds, same-attempt rate/seek changes and new-attempt state; old-policy defaults and existing promises remain fenced. |

**Final verdict:** ready for M0–M2 staged investigation; runtime policy
remains unselected, with no implementation acceptance. No material correction
from R1–R3 remains missing from the revised plan. The actual policy, any
transport classification mechanism and active-low-reserve stability must
still earn their M2 evidence. The final implementation needs review against
its actual code and measurements; this documentation review neither proves
deployment readiness nor supplies production authorization.
