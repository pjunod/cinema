# Native adaptive quality — the build plan

**Status:** ready for review · **Executes:**
[NATIVE-ADAPTIVE-QUALITY-DESIGN.md](NATIVE-ADAPTIVE-QUALITY-DESIGN.md)'s D4
· **Written:** 2026-09-23 against `main` @ `8839cc72`

**Board:** row **A-05** on the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) — claim
there before starting; record model and session id there and in the Execution
log below.

Read [NATIVE-ADAPTIVE-QUALITY-DESIGN.md](NATIVE-ADAPTIVE-QUALITY-DESIGN.md)
end to end first. This document sequences its work; it does not restate its
reasoning, and where the two appear to differ the design wins on *what* and
this plan wins on *when*.

The one sentence that governs everything below, from the review's §3.8 and
repeated in the design's §5.4: **a platform's controller ships disabled until
its shaped-network trace shows stalled seconds down and unexpected SDR
transitions at zero against that platform's own D3 baseline.** A green
`auto-quality-policy.json` run is necessary and nowhere near sufficient. This
is not a feature gate: `playback_auto_abr` is a replicated setting that
already exists and is already surfaced in Settings → Developer with advisory
readiness, and readiness never gates enablement. "Ships disabled" means the
controller tick is not written into the platform's loop until the trace
exists — not that a flag hides it.

## 1. What is already here

- `tests/playback/auto-quality-policy.json` — the shared policy fixture at
  schema 1, driven today by `node tests/playback/web-policy.test.js`.
  29 cases, 11 controller-gate rows.
- The design's §8 — the per-platform adapter specification, with the six
  fields that are not simply available and the seven corrections it made to
  §3.1.
- The stall-reopen wire, on all three sides: `previous_session_id` and
  `reopen_reason` on `POST /files/{id}/hls/sessions`, `ReopenReason` with its
  single `Stall` variant, the server's two consumers, Apple's
  `PlayerOpenIntent.stallReopen`/`StallReopenTicket`/`applyOpenIntent`/
  `unboundStallRetry`/`StallReopenBudget`, and Android's
  `subtitleSessionBody` parameters.
- The web controller, which is the only implementation and the only thing the
  constants were tuned against.

## 2. What is missing, and it is not mostly code

Four disagreements between the design and the shipped browser, each pinned in
the fixture with a written finding (design §8.4, §7.6), and **no shaped-network
measurement on any platform**. M0 and the D3 baseline are therefore the whole
critical path; the adapters are the easy part and must not start first.

## 3. Milestones

### M0 — settle the four disagreements, in the web, with the fixture as the ledger

No native work. Each item lands as a web change plus the fixture case moving
from `web_current` back to plain `expect`, which is the acceptance: a case
that no longer needs a `web_current` is a disagreement that has been settled.

1. **A control verdict in force must gate the controller.** `autoCauseEvidence`
   gains a `control-hold` kind fed by the verdict the client already holds, and
   `decideRung` adds it to `namedSuppression`. Today a hold verdict with a
   healthy buffer and a fast sample does not stop an upgrade on any platform.
   This is the one item that is a live web defect rather than a gap, so it is
   first.
2. **The voluntary-switch gap is named once and correctly.** `voluntaryAllowed`
   is `cooldownMs && dwellMs`, so the real minimum is 60 s while §3.4 reads as
   20 s. Either correct the document to `max(cooldownMs, dwellMs)` or return
   `dwellMs` to being only the restart-cost horizon its own §2.2 row says it
   governs. Changing the number is a behaviour change and needs a trace behind
   it; correcting the document does not. Prefer the document unless D3 says
   otherwise.
3. **A decode class inside the policy.** `decideRung` gains a `decodeStalls`
   input and a `decode` cause, and its decision gains the height to block, so
   the blocking stops happening outside the policy where two platforms cannot
   share it. Apple must **not** feed `numberOfStalls` into this (design
   §8.4.3): it is a playback-stall counter that a delivery failure moves
   first, and routing delivery starvation into a class whose action is a
   permanent per-playback block would cost a viewer a rung for a link blip.
4. **Decide whether `link:` carries a publication refusal** (design §7.6).
   The likely answer is that it does not and the refusal belongs with
   `hold:`. Whichever way it goes, §3.2's table and the fixture move together.

**Acceptance:** all four fixture cases carry `expect` alone, with no
`web_current`; `node tests/playback/web-policy.test.js` and `make web-check`
green; each behavioural item has a test that fails with the change reverted,
proved by reverting the change under test rather than its call site.

### M1 — type the cause on the wire, in the change that first sends one

`ReopenReason` widens from `Stall` to the five causes of §3.2. Deliberately
**not** done under A-04, and deliberately not done here either until M3 is
ready to send one: four variants that all behave like `Stall` would make the
field's name a lie, and four with distinct behaviour is a server change that
needs the trace behind it. The server work is the interesting half:

- `auto_height_from_prior` steps the ladder for `link:` and `encode:` only.
- `decode:` must block a rung rather than step it, which the prior has no
  representation for today.
- `hold:` and `authority:` must do nothing at all to the rung, which means
  they must also not feed the network prior — a session the client no longer
  owns is not evidence about the link.
- The prepared-switch suppression in `hls.rs` keys off "is this a client
  adapting to a failure", which stays true for all five.

**Acceptance:** an unknown future value is still refused rather than coerced
(the existing property, kept); each of the five causes has a server test that
fails when its arm is reverted; no client sends a new value yet.

### M2 — the second and third runners of the shared fixture

A Swift test in `clients/apple/Tests/` and a JVM test under `make
android-test`, both reading `tests/playback/auto-quality-policy.json` and both
driving a port of `decideRung`. One JSON, three runners.

This is where §7.2's question gets its first cheap answer: if a constant has
to differ per platform it becomes per-platform **data in the fixture**, never
a branch in three codebases.

**Acceptance:** all three runners green on the same file; a case added to the
JSON fails all three until all three are updated.

### M3 — the first adapter, one platform, disabled until measured

Android first, for three reasons the design's §8 establishes: its bandwidth
meter is one line of wiring away, it is the only platform that already tracks
the age of its status sample, and it is the only platform where the control
verdict — the `hold:` row nobody implements — is already in hand.

The adapter is §8.3's table, the tick location and guards named there, and
nothing else. It sends the typed cause from M1.

**Acceptance:** the platform's shaped-network trace shows stalled seconds down
and unexpected SDR transitions at zero against its own D3 baseline. Not a
green fixture run. Not a green build.

### M4 — the second adapter

Apple, and only after M3 has a trace that improves on its baseline.

**Apple's blocker is real and is not effort** (design §8.4.1, §7.7):
`decideRung`'s emergency branch fires only on a per-completed-transfer
throughput sample, and Apple has none, so the `link:` class has no Apple
evidence and the emergency branch is unreachable there. Whether successive
access-log events yield a usable sample is D3's first Apple question. If the
answer is no, M4 ships an Apple controller that can suppress, hold, mildly
downgrade and upgrade but cannot react to a cliff — and the plan says so out
loud rather than weakening the branch to accept a smoothed estimate, which is
the input the browser's own `upgradeSpeedFloor` comment explains is wrong at a
cliff.

### M5 — the decision record

Whatever M0 settled, §3.2's and §3.4's tables in the design document are
corrected to match, and the fixture is the proof they agree.

## 4. Guardrails

Every non-goal in the design's §4 applies here unchanged: no multi-variant
manifest, no native-player ABR, Android's `onStall` refusal preserved, real
per-player measurements rather than a modelled link, no new recovery ladder,
A6's 2 s poll cadence untouched, no in-code feature gate, viewer intent never
silently overridden, Live TV out of scope.

Two more, specific to the sequence:

- **No adapter before M0.** Writing three adapters against a policy with four
  known disagreements would bake them into three codebases.
- **No enablement on a green fixture.** Said three times in the design and
  once more here because it is the failure mode this whole plan exists to
  avoid.

## 5. Verification

`make web-check` for M0, `make unit` for M1, the Apple test target and `make
android-test` for M2, and the shaped-network traces of the design's §5.3 and
§6 for M3 and M4. The traces are device evidence: a session that cannot take
them records `needs:` in the execution log with the design's GPT prompt and
does not mark a milestone done from a measurement nobody has made.

## 6. Open questions this plan inherits

The design's §7.2 (do the constants transfer), §7.3 (budget or rate), §7.4
(does this need the honest master playlist first), §7.5 (what drives the
Android shaped run), §7.6 (does `link:` carry a refusal) and §7.7 (can Apple
see a cliff at all). §7.6 is M0's; §7.7 is M4's; the rest are D3's to inform.

---

## Execution log

Executing sessions append one row per milestone PR (see the
[work board](../reviews/ARCHITECTURE-REVIEW-2026-09-20-WORKBOARD.md) for the
claim protocol). **Model** is the runtime's exact model identifier;
**Session** is the session id or URL; the same two values are commit trailers
`Agent-Model:` / `Agent-Session:` on every commit of the branch.

| Date | Model | Session | Milestone | PR | Outcome / evidence |
|---|---|---|---|---|---|
| | | | | | |
