# M7 M1–M3 blocker review — why the remainder is not accepted

**Status:** ready for independent review · **Reviews:** M1 readiness, M2
bounded materialization, and M3 seek coalescing from the M7 remainder ·
**Audited baseline:** fetched `origin/main` at `f91ea2bb` · **Written:**
2026-09-01

Companion to
[PLAYBACK-CONTROL-PROTOCOL-PLAN.md](PLAYBACK-CONTROL-PROTOCOL-PLAN.md)
§13.8 and the M7 remainder handoff in
[#740](https://github.com/pjunod/plurx/pull/740) — this is the independent
review brief for the work already attributed to
[#741](https://github.com/pjunod/plurx/pull/741),
[#742](https://github.com/pjunod/plurx/pull/742), and
[#754](https://github.com/pjunod/plurx/pull/754).

Read the handoff in full, then protocol-plan §13.8, then this document. Review
the blockers in milestone order. The standing instruction remains in force:
acceptance is not negotiable, green CI is not a substitute for the named
observable fact, and a later milestone cannot make an earlier red one green.
If the current design cannot meet an acceptance criterion, identify that
criterion and stop instead of weakening it.

M4 burn-join is deliberately outside this review. Its design review is
[M7-M4-BURN-JOIN-DESIGN-REVIEW.md](M7-M4-BURN-JOIN-DESIGN-REVIEW.md), and
Paul handed its implementation off separately. Marker prewarm is also outside
scope; [#743](https://github.com/pjunod/plurx/pull/743) belongs to its own
handoff and does not count toward any result below.

## 1. Verdict — the first red acceptance is still M1

The programme is not complete. M1 has useful source in `main`, but its own
acceptance is not fully evidenced. M2 then omitted several explicit contract
items. M3 built a predicate and one pre-spawn refusal, but it does not prove
or guarantee §8.1's settled-target production rule.

```text
 M1 source landed ──▶ M1 acceptance red ──▶ STOP
                              │
                              ├── M2 source nevertheless landed
                              └── M3 source nevertheless landed

 M4 implementation ──▶ separate handoff; not reviewed here
```

| Milestone | Source state | Acceptance state | Earliest reason it is red |
|---|---|---|---|
| M1 readiness | Merged as #741 | **Red** | Unknown-value coverage and the large-MKV observation are absent |
| M2 materialization | Merged as #742 | **Red** | The required server setting, window-aware readiness, and client retry are absent |
| M3 coalescing | Merged as #754 | **Red** | The test counts obsolete predicates; it does not prove only one production starts |
| M4 burn-join | Separate handoff | Not assessed | Deliberately excluded from this review |

**How to read this table:** “source state” says code exists. “Acceptance
state” says the milestone may be relied on by its successor. Those are
different facts; this handoff makes the distinction load-bearing.

## 2. Audit basis — current remote `main`, not the stale local checkout

The audit fetched `origin/main` and read the named contracts against commit
`f91ea2bb`. The local checkout was at `18886477`, 50 commits behind, with
unrelated modified and untracked files. It was not switched, reset, or used as
evidence for post-M2 behavior.

The evidence sources were:

1. The M7 remainder handoff from branch `m7-remainder-plan`, read in full.
2. Protocol-plan §13.8 from `origin/main`.
3. The implementation on `origin/main`, inspected with `git show` and
   `git grep` so the stale checkout could not change the answer.
4. PR bodies, commits, comments, reviews, and check conclusions for #741,
   #742, and #754.
5. [PLAYBACK-CONTROL-STATUS.md](PLAYBACK-CONTROL-STATUS.md), whose roadmap
   calls M4 the next buildable milestone and whose M3 section records the
   remaining sequence gap.

All three implementation PRs have green promotion checks. That establishes
the repository gate for the code that was submitted. It does not establish a
manual hardware fact, a missing test scenario, or behavior that the submitted
code does not implement.

## 3. M1 readiness — the field exists, but acceptance is incomplete

### 3.1 What #741 did establish

`DeliveryView.subtitle_readiness` exists in
[`playback_control.rs`](../../crates/plurxd/src/playback_control.rs) as an
optional additive `Option<String>`. It is populated on Live and VOD responses,
and non-native subtitle modes remain silent. The cache probe in
[`hls.rs`](../../crates/plurxd/src/http/hls.rs) is side-effect free: it does not
start extraction from a control exchange.

The submitted tests establish these useful pieces:

- `ready`, `warming`, and `unavailable` map from the internal verdict.
- Off, overlay, and burn modes serialize no readiness.
- An absent field is omitted, deserializes as `None`, and does not become
  `ready`.
- A present `ready` field survives a serialize/deserialize round trip.

Those are source facts, not a complete M1 acceptance result.

### 3.2 M1-B1 — unknown-value handling has no protocol test

**Contract:** M1 acceptance requires protocol tests for
present · absent · unknown-value handling. The field documentation also says
that a client treats an unrecognized value as absent.

**Current evidence:**
`a_delivery_without_subtitle_readiness_round_trips_as_absent` covers absence
and `ready`. No test constructs an unknown readiness value. The wire type is
`Option<String>`, so deserialization preserves an unknown string; it does not
normalize it to `None`. Deployed Apple and Android clients do not decode
`delivery` at all, which makes the field invisible rather than proving the
future consumer's required behavior.

**Why this is red:** the contract asks for the behavior before the M2 client
consumer depends on it. “No current client reads it” cannot prove how a client
that does read it handles an extension value.

**Evidence that clears it:** a retained protocol/client-boundary test that
sends a value outside `ready | warming | unavailable` and proves the receiving
decision treats it exactly like absence. The reviewer should decide where that
normalization belongs; it must not be inferred from `Option<String>` alone.

### 3.3 M1-B2 — the large-MKV transition has no recorded observation

**Contract:** a manual session against a large MKV must show
`warming → ready` across control exchanges with zero client changes.

**Current evidence:** PR #741 records validation-lint, operations-check, and
history-check. Its PR has no comments or reviews carrying the manual
observation, and no tracked evidence document on `origin/main` records it.
The PR explicitly says the build host had no Cargo and relied on CI for Rust.

**Why this is red:** this acceptance is an observable integration fact. Unit
mapping tests and a green Rust gate do not prove that the session route,
selected track, cache flight, and subsequent control exchange join in a real
large-file session.

**Evidence that clears it:** a dated, reproducible observation naming the
commit, file characteristics, selected native track, control sequences, and
the two readiness values. It need not deploy to the fleet; “no deploys” still
binds.

### 3.4 M1 review verdict

Confirm or reject both M1 blockers before considering M2. If either remains,
the ordered programme stops at M1 even though later source has merged.

## 4. M2 materialization — the bridge is only part of the contract

### 4.1 What #742 did establish

The subtitle segment path in
[`hls.rs`](../../crates/plurxd/src/http/hls.rs) now checks a window sidecar after
a whole-track miss and starts a detached window warmer before returning the
existing empty response. [`subtitles.rs`](../../crates/plurxd/src/subtitles.rs)
adds the following useful pieces:

- default/minimum/maximum window constants of 200 s · 30 s · 900 s;
- a clamp helper and a window key containing source fingerprint, track,
  anchor, and span;
- timestamp-base normalization and one minute of boundary slack;
- a separate 64-entry window budget;
- exact-window deduplication through the cache-path warmup registry; and
- a cost rule that declines a window at or past the source midpoint.

The empty `WEBVTT\n\n` + `no-store` fallback remains. The
`media_origin_seconds` shift remains. The whole-track `<track>` endpoint is
unchanged. The subtitle playlist still mirrors the video playlist.

Those guardrails are worth preserving. They do not satisfy the missing items
below.

### 4.2 M2-B1 — `WINDOW_SECONDS` is not a server setting

**Contract:** the default is 200 s and the stored server setting is clamped to
`[30 s, 900 s]`, following the analysis-knob shape and surfaced in the
settings API.

**Current evidence:** `bounded_window_seconds` exists, but all three handler
calls pass `WINDOW_SECONDS_DEFAULT` directly. No settings model, persistence
path, API field, or settings UI contains a subtitle-window setting. PR #742's
body explicitly says configurability is not included because window size did
not dominate the measured I/O cost.

**Why this is red:** the measurement may justify another design discussion,
but it does not remove a requirement Paul restated in the implementation
request. A clamp around a hard-coded literal is not a configurable server
setting.

**Evidence that clears it:** one named stored setting with default 200 s,
clamped read behavior at 30 s and 900 s, API round-trip coverage, and handler
use of the resolved value. The reviewer should also require validation-point
coverage for every new settings surface.

### 4.3 M2-B2 — readiness still means whole-track readiness

**Contract:** after M2, `ready` means the selected track's demand window is
servable from either a whole-track or window sidecar; `warming` means the
relevant flight is active.

**Current evidence:** the control path calls `subtitles::sidecar_state`, which
constructs only the whole-track `vtt_path`. It receives no demand anchor or
window length and never calls `read_cached_window`. The field's current doc
comment still says “until bounded materialization lands” even though #742 is
merged.

**Why this is red:** the server can serve real cues from a window while still
reporting `warming`. The field therefore cannot direct the retry behavior M2
claims to unlock.

**Evidence that clears it:** a demand-window-aware, side-effect-free readiness
probe used by the control response, with tests for whole-track hit · matching
window hit · unrelated-window hit · either flight active · negative memo.

### 4.4 M2-B3 — no client performs the directed retry

**Contract:** each platform re-fetches the temporary empty subtitle segment
when a control exchange changes the selected track's readiness to `ready`.
M2 owns the client changes and the corresponding build-number bumps.

**Current evidence:** #742 changes no web, Apple, or Android client source.
Repository search finds no client consumption of `subtitle_readiness`. The PR
therefore has no client build-number bumps. The requested
`node --test tests/playback` command is not recorded in its validation block.

**Why this is red:** the server field has no consumer, so retry remains on the
player's existing schedule rather than being directed by control readiness.

**Evidence that clears it:** one documented behavior per applicable platform,
tests proving unknown/absent values do nothing, a retry on the transition to
`ready`, no retry storm while readiness is unchanged, and build numbers above
the current merge target for every touched native client.

### 4.5 M2-B4 — the flight bound is per window key, not per session

**Contract:** at most one window extraction flight per session, with the M3
latch preventing a seek storm from fanning out production.

**Current evidence:** `warmups()` is a `HashSet<PathBuf>`. A window path
contains the anchor and span, so requests in one exact span deduplicate, while
requests at different anchors create different keys. The function receives no
session id and no control sequence. M3 did not change this function.

**Why this is red:** one playback seeking across 20 window anchors can still
own 20 distinct window keys and detached ffmpeg tasks. Exact-span deduplication
is useful, but it is not the plan's per-session bound.

**Evidence that clears it:** a retained concurrent test drives one session
through multiple anchors and proves the number of live window producers never
exceeds one. The implementation must use the shared settled-target authority,
not introduce a subtitle-only timer or second clock.

### 4.6 M2-B5 — extraction no longer has the planned index-seek shape

**Contract:** the handoff specifies `-ss` before the input so a seekable
container performs an index seek, then bounds the extracted span.

**Current evidence:** #742 deliberately uses an output seek: `-i <source>`
comes before `-ss <anchor> -to <end>`. The source comment records the measured
reason: the pre-input form produced the wrong timestamp/span behavior on the
tested ffmpeg versions. The consequence is equally explicit: ffmpeg reads
from the beginning, so cost grows with the anchor; the implementation declines
windowing at the midpoint.

**Why this needs a stop-or-prove decision:** the revised form may be the right
ffmpeg answer, but it no longer carries the performance property used to
justify “real cues from the second segment request onward.” A later seek can
require reading a large prefix, and past the midpoint the bridge deliberately
does not run.

**Evidence that clears it:** either measured acceptance showing the revised
form meets the publication-deadline scenario over the supported demand range,
or a precise statement that the design cannot meet the milestone criterion.
Do not redefine “bounded” to mean only that the output duration is bounded
while leaving input work unexamined.

### 4.7 M2-B6 — whole-track publication does not prune its windows

**Contract:** once the authoritative whole-track sidecar publishes, window
sidecars for that source fingerprint are dead weight and are pruned.

**Current evidence:** the general prune separates windows into a 64-entry
budget and evicts oldest windows only after that budget is exceeded. It does
not remove all matching windows when a whole-track sidecar publishes.

**Why this is red:** a cap prevents unbounded growth, but it is not the stated
supersession lifecycle. Stale bridge files remain until unrelated later cache
pressure happens to remove them.

**Evidence that clears it:** a test publishing windows for two fingerprints,
then the whole-track sidecar for one, and proving only that fingerprint's
windows disappear while the other fingerprint survives.

### 4.8 M2-B7 — the milestone acceptance fixture does not exist

**Contract:** an artificially slow whole-track extraction must serve real cues
within the publication deadline from the second segment request onward; first
touch remains empty and nonblocking; no advertised interval 404s; concurrent
segment requests deduplicate the window flight.

**Current evidence:** #742 adds pure tests for the cost rule, timestamp
normalization, anchor grid, path shape, and span-key identity. It adds no HLS
handler test and no async window-flight test. The existing async subtitle
tests exercise whole-track extraction safety and memoization, not this
acceptance scenario.

**Why this is red:** each helper can be correct while the HTTP path misses its
deadline, launches multiple windows, reports the wrong readiness, or falls
back forever. The acceptance is deliberately end-to-end across those joins.

**Evidence that clears it:** a deterministic handler-level fixture with an
injectable slow whole-track producer and counted window producer. Assert the
first and second HTTP bodies, status codes, cache headers, cue contents,
deadline, and spawn count.

### 4.9 M2 review verdict

M2 is not a small follow-up away from green: setting, control truth, client
consumption, production ownership, and acceptance evidence are all separate
missing contracts. Review each independently. A green result requires all of
them, not a majority.

## 5. M3 coalescing — the predicate is not the production guarantee

### 5.1 What #754 did establish

The rolling actor now records `SettledTarget { sequence, anchor_ms }` for each
accepted demand snapshot. `CreateSession` carries an optional
`control_sequence`; the web client copies the reporter's accepted sequence
into a session open. `hls::create` compares the request with the predecessor
session's settled target and returns typed `409 playback_target_superseded`
before spawning when a strictly later sequence names a sufficiently different
anchor.

The retained pure tests establish:

- 20 accepted actor snapshots leave the last target in the latch;
- the last latch considers 19 earlier sequence/anchor pairs obsolete;
- equal and future sequences are not superseded;
- a 1 s anchor slack does not classify settle jitter as a new destination;
  and
- an ordinary rendering snapshot advances the latch after seeking ends.

Those are useful predicate tests. They are not the §8.1 acceptance test.

### 5.2 M3-B1 — a 20-seek storm can still start more than one producer

**Non-negotiable acceptance:** a 20-seek storm starts work only for the
settled target.

**Current evidence:** the test first accepts all 20 snapshots and only then
asks how many old tuples the final latch would reject. It never calls
`hls::create`, never increments a production counter, and never starts or
cancels work. PR #754 says the HTTP refusal itself is not covered by a test.

The implementation also has an ordering window: a create that reaches the
server before the next control snapshot is accepted sees no later sequence and
passes. Repeating that interleaving can start work for every intermediate
target. The M3 status section itself describes the practical defect as “up to
twenty session creations the server honours.”

**Why this is red:** the source proves that the final latch can identify old
work after the storm. Acceptance requires preventing or cancelling that work
before all but the settled target consume production resources.

**Evidence that clears it:** a protocol/HTTP-level storm test with an
instrumented production boundary. Twenty target changes must result in exactly
one spawn or resource commit, and it must carry the final target. A count of 19
`supersedes` results is not equivalent evidence.

### 5.3 M3-B2 — the latch gates session creation only

**Contract:** the same sequence authority gates session restart and M2 window
extraction. Prewarm will use it under its own handoff and is not assessed here.

**Current evidence:** #754 adds the check only to `hls::create`. Subtitle
segment requests still compute an anchor and call `warm_vtt_window` without a
control sequence. `warm_vtt_window` has no session/latch parameter and did not
change in #754.

**Why this is red:** even if every obsolete session restart were refused, an
obsolete subtitle segment request can still launch a detached ffmpeg window
for an anchor the viewer abandoned.

**Evidence that clears it:** one shared authority and one storm test prove
both restart and window production refuse the same obsolete sequences. Do not
add a subtitle-specific debounce or infer demand from media-fetch arrival.

### 5.4 M3-B3 — already-running obsolete work is not cancelled

**Contract:** obsolete unpublished work is skipped before resource commit or,
when already running and cancellable, cancelled by sequence. Published work
retires through the existing lifecycle.

**Current evidence:** #754 adds a pre-spawn `409` for one request path. It adds
no cancellation token, task ownership, or sequence check inside an active
session/window producer. No test covers unpublished work becoming obsolete
after it starts.

**Why this is red:** “refuse if the newer snapshot won the race” is only one
interleaving. The acceptance also names the opposite race.

**Evidence that clears it:** deterministic tests for both sides of the
publication boundary: obsolete unpublished work is cancelled without
publishing, while already-published work uses the normal retirement path and
is never killed mid-write.

### 5.5 M3-B4 — the documented focused command selects no new test

**Contract command:**

```bash
cargo test -p plurxd seek_coalesc
```

**Current evidence:** the added test is named
`playback_control::tests::a_seek_storm_settles_on_one_target_and_supersedes_every_earlier_one`.
Its full test name contains no `seek_coalesc`, so Cargo's test-name filter
selects zero of the milestone's tests.

**Why this is red:** a command that exits successfully after running zero
relevant tests is not runnable acceptance evidence.

**Evidence that clears it:** give the retained production-boundary test a
stable name/module matched by the handoff command, then record command output
that names the test and its non-zero count.

### 5.6 What M3 did preserve

PR #754's CI shows the film-addressed VOD browser acceptance green. The
disabled VOD seek-storm case was not resurrected. Equal/lower sequence
behavior remains covered by the control actor's existing fencing tests. These
facts are necessary guardrails; they do not clear M3-B1 through M3-B4.

## 6. Repository mechanics — useful compliance does not erase the violations

### 6.1 PROC-B1 — implementation PRs were merged, not left open

Paul's instruction was one PR per milestone, opened and not merged. The
actual states are:

| Milestone | PR | State | Merge commit |
|---|---|---|---|
| M1 | [#741](https://github.com/pjunod/plurx/pull/741) | Merged | `60407147` |
| M2 | [#742](https://github.com/pjunod/plurx/pull/742) | Merged | `18886477` |
| M3 | [#754](https://github.com/pjunod/plurx/pull/754) | Merged | `952b8111` |
| M4 | Separate handoff | No implementation PR found | — |

Each landed through a merge commit and targeted `main`, which follows the
repository's merge-mode rule. The problem is the requested terminal state:
all three review units were merged before this acceptance audit.

### 6.2 PROC-B2 — milestone work advanced while prior acceptance was red

M1's first implementation commit was authored at 01:29 UTC. M2's first commit
was authored at 01:42 UTC, while M1 did not merge until 02:49 UTC and had no
recorded manual acceptance. M3 was later built after M2 merged, but M2-B1
through M2-B7 were still present.

The ordering rule is about acceptance, not merge timestamps. A later branch
can exist only after the earlier milestone is green; merging the earlier
source does not manufacture its missing evidence.

### 6.3 Mechanics that did hold

- The PRs target `main` and use merge commits rather than squashes.
- Main-promotion checks reported success on #741, #742, and #754.
- #742 added history-check receipts for its fix-shaped commits.
- No ansible role or detector work appears in the three milestone diffs.
- No client build bump was incorrectly fabricated; the problem is that the
  required M2 client work itself is absent.
- Image-publish jobs were skipped, and no deployment change is part of these
  PRs.

## 7. Guardrails — what Fable must preserve while reviewing the blockers

| Guardrail | Audit result | Review consequence |
|---|---|---|
| No detectors | Preserved | Do not propose detection work as a way to close readiness/materialization |
| No recovery opinions | Preserved | Keep M3 sequence gating separate from recovery decisions |
| Subtitle never replaces video | Preserved by M1/M2 | Do not pull M4's successor semantics into this review |
| Empty-segment fallback remains | Preserved | Every M2 fix must retain the nonblocking first-touch response |
| `media_origin_seconds` shift remains | Preserved | Window/readiness work must not move the GOP-lead correction |
| No advertised avoidable 404 | Structurally preserved, not covered by the new M2 fixture | Require the handler-level assertion rather than deleting the fallback |
| Disabled VOD storm stays disabled | Preserved | Use protocol/HTTP production instrumentation, not that browser case |
| `WINDOW_SECONDS` default 200 s and bounded setting | **Failed** | M2-B1 is mandatory |
| No deploys | No deployment is part of these PRs | Keep manual evidence local or otherwise non-deploying |
| Prewarm separate | Preserved as separate PR #743 | Do not count or modify prewarm in this review |

## 8. Requested Fable review — adjudicate, do not silently repair

For every blocker ID, return one of `confirmed` · `rejected` · `needs design
decision`, with a source/test citation and one paragraph of reasoning.

The review should answer these questions in order:

1. Does M1 have any retained evidence, missed by this audit, for unknown wire
   handling or the large-MKV `warming → ready` observation?
2. Can M2's output-seek extraction meet the second-request deadline over the
   demand range the contract claims? If not, name the exact criterion that
   cannot be met.
3. What existing settings path should own the bounded 200 s value, and does
   adding it require a client build bump or only a server API change?
4. Where should demand-window readiness be computed without letting a control
   exchange start work?
5. How can one per-session production owner cover different window anchors
   without inventing a subtitle-specific clock?
6. Can the current `control_sequence` ordering guarantee one production spawn
   under all 20-seek interleavings? If yes, produce the missing execution
   argument and test shape. If no, confirm M3-B1.
7. What is the smallest production boundary that can be instrumented for a
   real storm test covering both session restart and subtitle windows?
8. Do any proposed corrections cross into M4 burn-join or marker prewarm? If
   so, reject that scope expansion and leave the separate handoffs intact.

Do not answer “CI is green” to a behavior question. CI proves only the tests
that exist.

## 9. Exit criteria — what must be true before the programme advances

### 9.1 M1 may turn green only when

- present, absent, and unknown readiness handling all have retained tests;
- the relay/older-peer absence path remains green; and
- a dated large-MKV session records `warming → ready` across control
  exchanges without a client change or deployment.

### 9.2 M2 may turn green only when

- `WINDOW_SECONDS` is a stored, API-visible server setting with a 200 s
  default and `[30 s, 900 s]` clamp;
- readiness reports the selected demand window truthfully;
- directed retry exists on each applicable client and native build numbers
  are bumped where source changed;
- one session owns at most one window production flight across anchors;
- whole-track publication removes superseded windows for its fingerprint;
- the extraction form has measured deadline evidence or a named stop
  decision; and
- the slow-whole-track handler fixture proves second-request cues, first-touch
  fallback, no avoidable 404, and concurrent deduplication.

### 9.3 M3 may turn green only when

- a 20-seek protocol/HTTP storm starts exactly one production for the final
  target;
- session restart and subtitle-window production use the same sequence
  authority;
- obsolete unpublished work is skipped or cancelled, while published work
  retires normally;
- equal and lower sequences retain their existing rejection behavior;
- the focused acceptance command runs a non-zero matching test; and
- steady VOD and suspend/resume coverage remains green without resurrecting
  the disabled browser storm.

M4 remains separate regardless of the outcome. Nothing in this document is
authority to implement, amend, or merge the burn-join work.

## 10. Repository loop — apply only after review resolves the blockers

This document asks for review, not implementation. If a follow-up session is
authorized to change Rust, it must first establish the pinned Rust 1.97.1
compile loop from
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md#4-compile-before-ci-send-source-to-the-compiler-never-credentials)
before editing. Evidence against an older source archive does not count after
the intended base moves.

The handoff's focused commands remain the starting points, with the M3 filter
fixed so it actually selects the retained storm test:

```bash
cargo test -p plurxd playback_control  # M1 protocol/model behavior
cargo test -p plurxd subtitles         # M2 extraction/cache behavior
node --test tests/playback             # M2 web directed-retry behavior
cargo test -p plurxd <real-storm-name> # M3; must report a non-zero test count
```

Then run the repository validation required by the changed surfaces. Record
the exact focused commands and results in the milestone PR. Do not merge any
corrective PR as part of this review pass.
