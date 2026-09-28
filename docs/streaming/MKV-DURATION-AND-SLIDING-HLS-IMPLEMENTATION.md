# MKV duration and sliding HLS — implementation handoff for Sol

**Status:** implementation in progress on `effort/mkv-duration-sliding-hls`;
no deployment or production requeue approved · **Written:** 2026-09-19 UTC ·
**Executes:** R1–R11 of the
[reviewed RCA](MKV-DURATION-AND-APPLE-SLIDING-HLS-RCA-AND-FIX.md).

The live package ledger and verification evidence are in
[MKV-DURATION-AND-SLIDING-HLS-STATUS.md](MKV-DURATION-AND-SLIDING-HLS-STATUS.md).

Start with the RCA's evidence and §15 review disposition. This handoff owns
implementation order, boundaries, and acceptance. Execute the work packages
sequentially on one effort, recording actual commands and results as each
package finishes. Never mark a device test passed from a synthetic trace.

The review's central corrections are accepted: startup can deadlock after
the server burst, regular time holds drain the reserve before resuming, and
missing-duration index rows are terminal. Three suggested shortcuts are
qualified: a download does not prove startup, a long hold does not erase the
existing recent-speed EWMA, and “ceiling plus one GOP” is not a bound without a
bound on the GOP. The plan below is authoritative where those suggestions
differ.

## 1. Outcome — accept both duration and forced rolling playback

Completion means all of these are true:

1. A valid MKV without selected-video duration can build a verified immutable
   video index through bounded packet evidence.
2. Existing exact-identity terminal rows can be explicitly rebuilt and become
   ready; new code alone is not reported as repairing those rows.
3. A fresh rolling client cannot be time-held merely because the server
   published a startup burst before rendering began.
4. Active and intentionally paused rolling presentations either publish on
   schedule, finish truthfully, or fail through the existing bounded recovery
   protocol. Controlled failure is reported as failure, not accepted playback.
5. The target duration stays fixed and every advertised segment fits it.
6. Removed and retired presentation objects honor their serving promises,
   stay charged to storage, and cannot authorize stale producer operations.
7. Operators see terminal index state and usable diagnostics on both store
   backends, including the node-local telemetry database.

The reference incident is neutral fixture F: file 6109, H.264, time base
1/1000, container duration 6423.712 s, selected metadata absent, packet span
6423.666 s. The implementation must not depend on this file ID or private
library path. Use generated media fixtures and sanitized trace data.

## 2. Integration — preserve the shared checkout and qualify one effort

### 2.1 Establish the base and compiler before Rust edits

The handoff was reconciled with locally available origin/main
`a2d9c2fb7b26e142a39b70da8cc0e57bbbdf878a`. Fable reviewed
`395ce591`; incident deployment was `a5454c40`. These are evidence anchors,
not instructions to build on an old branch.

At handoff creation the shared checkout was on
`codex/restore-tvos-cinematic-detail`, with these documents and an unrelated
untracked temporary directory. Use an isolated checkout from freshly fetched
main. Do not switch, clean, stage wholesale, or commit unrelated work there.
Copy only these two documents and their index rows into the implementation
checkout, after verifying their final content.

Read [the contributor workflow](../../AGENTS.md),
[development pipeline](../DEVELOPMENT_PIPELINE.md), and
[agent compile loop](../ci/AGENT-COMPILE-LOOP.md). Record:

```bash
git status --short                     # identify existing work before isolation
git rev-parse HEAD                     # record actual checkout
git rev-parse origin/main              # record fetched intended base
rustc --version                        # must be repository-pinned 1.97.1
cargo --version                        # confirm the selected toolchain
```

If the host cannot run the pin, establish the documented source-only compile
loop first. Archive committed source with `git archive`; transfer no .git
directory or credentials. Re-archive and verify after applying a patch onto a
new base. CI is not the compiler.

### 2.2 Branches and review boundaries

Create `effort/mkv-duration-sliding-hls` from current main. Each task branch
uses `codex/mkv-hls-<package>` and targets the effort's current tip. Files
overlap, so the file-disjoint main exception does not apply.

W0.5 is the first separate runtime review unit. Do not bypass effort
qualification to satisfy the review's “ship first” wording. If a separate
startup-only release becomes necessary, qualify that smaller candidate through
the normal workflow and merge it into the effort before continuing. That is
a release decision, not a reason to merge an unqualified partial effort.

Each task PR records changed behavior, the smallest failing-old/passing-new
test, compiler evidence, source SHA, and unresolved acceptance. Finish through
`Main promotion gate` and its current-tree receipt. This handoff authorizes
building a reviewable result; production deployment and cohort requeue require
the applicable explicit operational authorization.

### 2.3 Package ownership and dependencies

Paths below are relative to the repository; reverify symbols on the fetched
base. Ownership is per sequential task, not concurrent permission to edit the
same file.

| Package | Primary files / boundary | Depends on |
|---|---|---|
| W0 | Existing tests in [fragindex](../../crates/plurxd/src/fragindex.rs), [transcode](../../crates/plurxd/src/transcode.rs), [playback_control](../../crates/plurxd/src/playback_control.rs); sanitized fixture data | Compiler loop |
| W0.5 | transcode startup predicate/latch, playback_control accepted client evidence and bounded timer | W0 |
| W1a | [content_analysis](../../crates/plurx-core/src/content_analysis.rs), [store/fragindex](../../crates/plurx-core/src/store/fragindex.rs), [telemetry](../../crates/plurx-core/src/store/telemetry.rs), [hiqlite](../../crates/plurx-core/src/store/hiqlite.rs), SQLite caller | W0.5 |
| W1b | fragindex, [ffmpeg](../../crates/plurxd/src/ffmpeg.rs), content_analysis | W1a |
| W1c | [state](../../crates/plurxd/src/state.rs), [analysis HTTP](../../crates/plurxd/src/http/analysis.rs), store contract tests and both cluster backends | W1b |
| W2a | [fmp4](../../crates/plurx-core/src/fmp4.rs), [copyseg](../../crates/plurxd/src/copyseg.rs), [transcode constants/argv](../../crates/plurx-core/src/transcode/mod.rs), rolling writer adapters | W1c |
| W2b | playback_control actor/scheduler, transcode produced/served snapshots, [HLS HTTP](../../crates/plurxd/src/http/hls.rs), ffmpeg publication observations | W2a |
| W3 | transcode retention/index/accounting, playback_control retirement promises, HLS authorization, [media_sessions](../../crates/plurxd/src/media_sessions.rs) relay/takeover | W2b |
| W4 | [vodserve](../../crates/plurxd/src/vodserve.rs), analysis/Activity, Apple PlayerView and tests, Android PlayerScreen and tests, web playback module/comment and tests | W3 |
| W5 | Focused regression catalog, package receipts, device evidence, rollout/requeue preview | W4 |

W1a can be reviewed as compatibility/history work before enabling W1b.
W2b must not be enabled on live sessions without W3: retirement and publication
make promises whose bytes still require retention. Avoid introducing new
administrator settings for every internal tuning constant.

## 3. W0 — freeze the old behavior as reproducible inputs

Build three deterministic reproductions before changing the predicates:

- **Duration:** the selected mapped video has index 0, time base 1/1000,
  no duration fields/tags, and container duration. Assert the current exact
  `Unverified("the selected video has no trustworthy duration")` result.
  Generate an MKV with B-frames and remove selected-video duration metadata
  through a deterministic fixture recipe. Assert its actual FFprobe output
  has the intended shape; do not assume a muxer's metadata defaults.
- **Startup:** request origin 4,694,789 ms, zero initial runway, a 32-second
  publish burst at t+1 s, then 20 seconds fetched with Starting/Waiting render
  state and no position progress. The old flow enters a time hold. Retain a
  contrasting trace that starts with 72/84 seconds of inventory.
- **Steady state:** at 1× with contiguous runway, choose target ≥60 seconds.
  Prove the old release threshold is target−30 and regular client fetch
  progress drains the reserve before release. Add a client that stops fetching
  but continues control reports. Drive time explicitly, without sleeps.

Retain regression inputs while replacing assertions with desired behavior
in their implementing packages; do not keep tests that require the bug.

**Acceptance:** the exact old outcome is reproducible on the recorded base.
The missing-duration parser case exists even if a fixture-generation issue
still blocks the real-media case; W1b cannot finish until both run.

## 4. W0.5 — end startup protection on presentation evidence

### 4.1 Change the predicate and its owning latch together

Existing sites are `FlowInputs.startup_grant_spent`,
`evaluate_flow`, and `apply_ahead_window` in transcode. The current
`starting = !spent && published < 12s` cannot be repaired by changing the
latch alone: its published-media comparison also ends the exemption.

Move the authoritative admission state into the rolling actor, or expose a
projection whose writes all originate there. Suggested internal shape:

```rust
enum RollingStartupState {
    AwaitingPresentation {
        first_served_at: Option<Instant>,
        baseline: Option<StartupPositionObservation>,
    },
    Presented,
    Expired,
}
```

This is a proposed type, not existing code. Include presentation identity,
owner epoch and accepted control sequence in the observation. Tie evidence to
the current installed attempt where the available protocol permits it. A
source-only internal retry may preserve the remaining presentation startup
budget; it must not mint a fresh one.

Use existing `PlaybackDemandSnapshot` fields: `render_state`,
`position_ms`, `seek_target_ms`, `demand`, and validated buffer bounds.
A qualifying observation is current accepted Active/Rendering evidence plus
forward presentation progress across two increasing control sequences for the
same settled timeline. Use 250 ms advancement as the initial testable policy
constant. A seek jump, duplicated sequence, predecessor report, requested
resume position, first completed download, or positive buffered-through value
does not qualify by itself. Use existing settled-target validation to exclude
seek transitions; reset the position baseline on a seek, not the deadline.

If a supported client cannot report Rendering and progress accurately, fix
its existing reporter in W4 or supply a tested equivalent from the established
protocol. Do not silently substitute server output or add an unreviewed wire
field that old relays reject.

### 4.2 Keep admission bounded

While AwaitingPresentation, bypass time holds. Preserve End, terminal state,
lease expiry and byte/global limits. Preserve the startup exception for a
Holding report that represents native startup; after presentation, Hold uses
the pause policy.

Initial policy: startup protection expires 30 seconds after the first media
playlist becomes externally available. Existing earlier lease/process
deadlines still win. Reloads and status polling do not renew it. On expiry,
retire through the existing bounded recovery route, with a startup-specific
reason. The intent is a finite admission budget, not a second producer
watchdog. The 30-second value is proposed and must appear in measurements and
release notes.

Before the first playlist, retain the existing input/probe/start watchdog and
hard limits; do not create an unbounded prepublication exemption.

### 4.3 Acceptance

The 32/20/0 trace never enters a time hold while presentation is unproved.
Rendering plus 250 ms progress spends the grant once. A seek position alone
does not. End and byte caps still stop production. An abandoned client reaches
the bounded terminal path. Old accepted sequences and replacement callbacks
cannot spend or extend a successor's grant.

Physical iPad replay must show first frame and continuing progress for the
previously failing shape. Until that evidence exists, call W0.5 a tested
policy mitigation rather than claiming the incident is fixed.

## 5. W1a — make diagnostic rollback and local outcome writes work first

### 5.1 Unknown provenance must not erase the whole diagnostic

Today `IndexDiagnostic::decode_bounded` deserializes
`Option<CompletionProvenance>`; a future enum string fails the whole decode.
Separate permissive diagnostic representation from strict completion
authority. A diagnostic may retain an unknown bounded provenance string; an
unknown value must never construct an accepted completion expectation.

Retain version, size, field and stderr bounds. Tests must decode current,
future-provenance, malformed, and oversized documents. Malformed required
fields stay rejected. Add PacketTimeline emission only after this reader
behavior is available as the rollback baseline.

### 5.2 Resolve retry settings outside the telemetry database transaction

The generic local `record_typed_outcome` queries settings after writing its
outcome within one transaction; a telemetry database without settings rolls
the transaction back. Introduce a bounded resolved policy argument (for
example `IndexRetryPolicy { max_attempts }`) passed by the store adapter.

Trace and update SQLite, Hiqlite and telemetry callers. Hiqlite resolves
authoritative settings before entering the local database closure. SQLite can
resolve from its authoritative store. Preserve existing attempts, deadlines,
diagnostic bounds and transient rules. Do not create a local settings table
or make a query failure look like success.

**Acceptance:** a real minimal telemetry database with its normal schema
records a typed outcome and diagnostic with no settings table. Configured
max attempts apply equally across backends. Forced rollback of the local
transaction remains atomic. Authoritative cluster outcome is still fenced.

## 6. W1b — selected-video packet timeline with bounded work

### 6.1 Metadata resolution and proposed interfaces

Existing authority type, copied from content_analysis:

```rust
pub struct VideoCompletionExpectation {
    pub stream_index: u32,
    pub duration_num: u64,
    pub duration_den: u64,
    pub provenance: CompletionProvenance,
    pub source_object_version: String,
}
```

Keep successful artifact bytes and source/recipe identity rules unchanged.
Collect all valid duration tick, decimal and tag candidates for the exact
mapped stream. Compare max−min with two seconds using checked rational math;
the current comparison against only the first value misses candidates on
opposite sides of it. Include both supported tags when both are present.
Preserve precedence when all agree. Conflict or absence invokes packet
fallback; invalid stream identity or attached-picture selection retains its
current refusal.

Proposed ffmpeg-layer interface:

```rust
async fn held_source_video_packet_bounds(
    source: &std::fs::File,
    absolute_stream_index: u32,
    time_base: (u64, u64),
    format_seek_hint_ms: Option<i64>,
    budget: PacketProbeBudget,
) -> Result<PacketTimelineBounds, PacketProbeError>;
```

Use typed outcomes rather than matching stderr strings for new policy.
The source is already held; Unix inherits the descriptor, Windows uses
existing guarded reopen and before/after identity checks. Serialize reads
sharing a file offset and reset to zero even after error or cancellation.
Cancellation must kill/reap the probe through the existing process owner.

### 6.2 Initial resource budget and actual EOF

These are initial constants, not new settings or evidence of measured limits:

| Bound | Initial value / behavior |
|---|---|
| Packet probe total wall time | min(30 s, remaining whole-index budget) |
| Head | first 256 selected packets; 1 MiB output cap |
| Tail seeks | hint−128 s, hint−512 s, hint−2048 s, clamped to start; deduplicate |
| Tail count | at most 3, used only if prior EOF contains no selected-video packets |
| Tail stdout | 1 MiB per attempt; 4 MiB aggregate including head |
| Missing usable seek hint | terminal unverified; no unbounded full-file scan |
| Output cap | kill/reap; unverified, never successful partial bounds |
| Process / stderr | existing bounded process and stderr budgets |

Use minimal `-show_entries packet=stream_index,pts,dts,duration`, integer
fields, and a bounded JSON reader. Record actual bytes and packet count.
A head count cap is acceptable because it establishes a prefix. A tail has
an open end (`START%`), requires natural zero exit, and must not be stopped
at a packet/time count and then treated as source EOF. The outer timeout and
byte cap can terminate it only as failure.

FFprobe seek position is approximate. Compute bounds from observed packets,
not the requested seek coordinate. A widened interval that exceeds budget
remains unverified. Short sources may fit wholly within the same small cap;
that is not permission for a general whole-library full-packet prepass.
If selected packets are present but timestamps are inconsistent, refuse
rather than repeatedly seeking until one subset happens to pass.

### 6.3 Signed rational arithmetic and normalization

Use checked i128 arithmetic for PTS, origin, end and differences; reduce
fractions before multiplication and reject overflow. PTS may be negative,
DTS may be missing, and the last row need not have maximal PTS.

```text
origin = minimum selected PTS in the bounded prefix
end    = maximum selected (PTS + positive duration) in an EOF-proved tail
span   = end - origin > 0
```

Every packet that could determine the maximum needs a usable duration.
Do not ignore missing-duration tail packets and choose a convenient smaller
maximum. Do not substitute DTS for missing PTS silently.

Document the supported reorder/origin model. A bounded head is not a proof of
arbitrary future negative timestamp excursions. Reject unsupported timestamp
resets, discontinuities, or unresolved origin semantics. Retain packet
origin/end evidence and compare with the index's actual normalized sample
timeline. Test nonzero/negative starts and timestamp gaps against real
FFmpeg; do not compare a wall-clock span to summed durations without proving
the index uses the same coordinate model.

For PacketTimeline only require absolute difference between normalized index
coverage and expected span ≤2000 ms. Short output uses
`index_video_shortfall`; excess output uses
`index_completion_unverified` with a bounded discrepancy diagnostic.
Exactly two seconds passes; two seconds plus one tick fails. Metadata
expectations retain the existing lower-bound semantics.

### 6.4 Exact failure matrix and fixtures

Use the matrix in RCA §6 verbatim. Nonzero FFprobe exit remains terminal
`index_process_failed` unless an explicit existing transient rule applies.
Timeout uses the existing retryable timeout/budget code; resource cap and
unproved endpoint are terminal unverified. A changed source or lost lease
never commits.

**Acceptance:** real FFprobe/FFmpeg fixture without selected duration publishes
through PacketTimeline, while longer-audio metadata fixtures remain accepted
by the old precedence. Add negative origin, B-frame last-row trap, missing
tail duration, empty late seek, aggregate cap, natural EOF versus artificial
cap, timeout, process error, and source-replaced tests. Include metadata
candidates 10/8.5/11.5 seconds: each outer candidate is near the first but
the full range conflicts. Missing DTS with valid PTS succeeds.

## 7. W1c — terminal repair is part of duration acceptance

Exercise the existing force-analysis path in a disposable store, using
`request_file_analysis_for_identity` and the public analysis handler.
Preserve exact source version, node/fence/lease identity and request dedup.

Required transition:

```text
no-duration source
  → failed index_completion_unverified (not_before = i64::MAX)
  → normal duplicate request remains terminal
  → explicit current-identity force request
  → one queued/running job with PacketTimeline
  → ready artifact
  → playback chooses immutable VOD
```

Also test duplicate force requests, source replacement before claim and
before commit, lease loss, and a valid existing positive artifact. Forced
repair must not erase unrelated successful indexes or failure history.

Build a read-only production preview procedure using authoritative rows.
It emits exact identities and exclusions, not SQL updates based on the
52-file log count. Include failed state, typed code, current source match,
existing positive artifact, request dedup key, and intended policy revision.
A production requeue action is executed only with authorization after
deployment; test the same authorization/identity boundary in disposable data.

**Acceptance:** both relevant store backends pass the transition, and the
preview contains no unrelated code, stale identity, or existing success.

## 8. W2a — make the rolling target immutable and enforce its bound

### 8.1 Keep immutable plans isolated from rolling policy

`Segmenter::cut_before` already separates planned from unplanned generation.
Do not change persisted immutable-VOD boundaries, plan versions or recipe
fingerprints as a side effect of rolling repair. A planned artifact uses its
own completed plan's covering target.

For unplanned rolling copy, retain the nominal 6-second floor, 2-second
first floor and 15-second preferred ceiling where they fit. Introduce an
explicit strict advertised bound: initially 16 seconds. Pass it separately
from the preferred cut ceiling; do not imply that an unknown GOP is one
second long.

Before adding an incoming fragment, check the prospective actual duration.
If it exceeds the strict bound, flush a nonempty pending run first, even if
the ordinary floor would delay that cut. Preserve truthful clean/ceiling
classification. If a single fragment or sample cannot fit, return a typed
unsupported/contract failure; do not publish it or grow the target. Sample
splitting is a separate implementation choice requiring proof of correct
fMP4 tables, timestamps and random-access semantics.

Check actual finalized EXTINF too, including audio-only EOF tails. Tail
splitting must use the strict rolling bound. Keep
`EXT-X-INDEPENDENT-SEGMENTS` absent where forced cuts are not independently
decodable. Record extra ceiling cuts and startup latency as tradeoffs.

### 8.2 Freeze before the first served response

Pass a fixed covering target into `SessionDir`; replace “longest so far”
comments and tests with “every segment fits the fixed target.” The initial
16-second bound allows ceiling-15 overshoot only when it truly fits 16.
Do not round 16.4 seconds down and treat it as fitting the strict bound even
though a standards-only nearest-integer check might.

Other rolling writers, including ordinary FFmpeg HLS, need a fixed contract
too. Enumerate the actual writer routes in the task receipt. Wrap their
served manifests with an immutable declared target and validation, or configure
an enforced bound in that writer. An oversized future segment retires before
it becomes externally visible; a changing raw target is not forwarded.
Immutable cached VOD remains on its existing path.

**Acceptance:** variable GOP, crossing fragment, fragment-alone oversize,
audio tail, short EOF, H.264/HEVC forced cuts, and all rolling writer paths
either fit a single target throughout or return the typed failure without
advertising invalid output. Existing immutable plan tests remain unchanged.

## 9. W2b — make publication a scheduled actor action

### 9.1 Separate produced inventory from the served snapshot

Existing `RollingPublicationObservation` includes:

```rust
pub producer_attempt: u64,
pub playlist_ready: bool,
pub published_segment: Option<i64>,
pub published_end_ms: Option<i64>,
pub next_media_sequence: i64,
pub resolved_fetched_segment: Option<i64>,
pub resolved_fetched_end_ms: Option<i64>,
```

Extend the internal observation; do not repurpose fields so existing delivery
accounting silently switches meaning. Distinguish produced/staged media from
externally published media. Suggested new contract:

```text
identity = generation + owner epoch + response incarnation + producer attempt
target_duration_ms                       fixed for presentation
served_revision                         monotone within identity
served_last_segment / served_end_ms      only externally available media
last_segment_advanced_at                 monotonic server time
next_publish_at / hard_deadline          independent of HTTP reloads
maintenance_state                       idle / scheduled / producing / retiring
pause_started_at / startup_deadline      finite and non-renewing
```

Commit one immutable playlist snapshot under the actor's authority. The
availability time is when that snapshot becomes retrievable, not when a
particular client asks for it. First HTTP publication activates the contract
if the snapshot was previously private. Later requests read the current
snapshot and do not advance its clock.

Coalesce retention-prefix removal into the next scheduled segment-bearing
snapshot. Do not regenerate a prune-only response on each request or let
MEDIA-SEQUENCE changes postpone the new-media deadline. Finished snapshots
and explicit retirement use their own terminal rules.

Copy and ordinary HLS writers send completed-media observations promptly.
A 15-second global repair pass is only a backstop. Use the actor's bounded
timer/event loop for normal wakeups. File notifications or polling may
discover completion, but only an accepted exact-attempt actor observation can
authorize publication or signals.

Preserve response publication fencing. Validate sequence and attempt before
both snapshot commit and signal application. Stale observations cannot change
the successor's target, clock, retained object promise or startup grant.

### 9.2 Publication cadence and production pacing are separate constraints

For fixed T use a target publication time near previous availability+T,
earliest new segment-bearing version at +0.5T, and hard deadline at +1.5T.
An ended presentation can publish its truthful completion promptly. Schedule
before the hard limit with an initial safety margin of max(500 ms, 0.1T).

At each cycle choose a completed-media batch, not necessarily one segment.
Track cumulative media demand and already produced lead; carry segment
overshoot into the next cycle rather than rounding up independently forever.

```text
desired cumulative end at next publish
    = settled playback anchor + projected consumption to that time
      + bounded client runway/reserve target

media still needed = max(0, desired end - produced end)
run budget        = conservative wall time to produce that media
resume by         = desired publish time - run budget - safety margin
```

Use actual staged durations and checked arithmetic. For an unknown next
segment reserve the strict maximum duration and byte envelope. Integrate
rate over accepted active intervals; reset/rebase on settled seek and
replacement. Unknown measurement means resume now and learn from production,
not grant a long hold.

| Scenario | Required schedule property |
|---|---|
| T=6 s, 6 s segments, 2×, 1× playback | About 3 s run plus 3 s hold when safely ahead |
| T=16 s, 16 s segments, 2×, 1× playback | About 8 s run plus 8 s hold |
| T=16 s, 6 s segments, 1× playback | Batch short segments; roughly 12/18 s media batches with carried surplus, not one 6 s segment every 16 s |
| 1× producer, 1× playback | Little or no hold allowance; publication still scheduled |
| Fast burst | Stage and cap inventory; do not leak every raw writer revision before 0.5T |
| Fetch-stopped client | Publication timer still runs; finite reserve/cap eventually causes explicit recovery |
| Paused client | At least one segment per allowed update until grace expiry, with bounded scratch |
| Faster playback | Batch enough media for that rate or take explicit insufficient-capacity path |

Replace rolling time-hold release with this policy for both explicit and
legacy rolling routes, or explicitly disable a route until it conforms.
Do not leave `time_release_threshold` as a competing decision during a
maintenance cycle. It may remain for unrelated methods only if tests show
they are outside this contract.

Staged inventory is charged immediately. A fast producer may be held once the
bounded batch/lead is ready even when its speed is unknown. The publication
timer can expose staged media without resuming FFmpeg. Thus unknown speed
does not require unbounded production.

The explicit time hold therefore requires both conditions: one whole batch
staged past the published end, and produced media reaching what the next
publication will ask for -- the clock's desired end plus a rate-scaled
two-exchange guard, capped at its `allowed_end_ms` -- so the hold cannot stop
the producer just short of the next snapshot's floor.

### 9.3 Initial runway and window length

The first response starts at the oldest advertised segment because of the
zero offset. Default initial gate for unfinished rolling media is:

```text
max(existing 12 s gate,
    3 × T,
    supported playback rate × (T + 1 s delivery margin))
```

This is a conservative initial admission rule; at T=16 it requires 48 seconds
rather than the old 12. Measure cold/warm TTFF and report the cost. Retain the
initial burst so the gate fills at actual available throughput. If this
exceeds current request/watchdog budgets on supported hardware, solve the
admission budget coherently before enabling; do not raise one timeout
silently or lower inventory below its proof.

A finished short source publishes with ENDLIST and bypasses the unfinished
window gate. Preserve at least 3T in every later unfinished served snapshot.
Do not turn a large fixed target into a repeat of the old first-reload drain.

### 9.4 Producer rate and watchdog behavior

`Progress::note_out_time` discards a sample spanning a long gap but retains
`recent_milli`. Preserve that tested behavior. Add freshness and current
attempt to any rate used for deadline scheduling. Where necessary measure
active wall time per completed batch, excluding intentional holds.

Configured `-readrate` can constrain an optimistic estimate but is not
guaranteed throughput. Slow NAS, CPU or decoder performance must cause earlier
production or a classified miss. Keep observed/proposed rates distinct in
telemetry.

Held→Running rearms Advancing and an on-time Held acknowledgement clears its
live deadline. Add tests for run intervals shorter than a progress block,
late hold after an already-due deadline, stale signal acknowledgement, and
many cycles with no progress. Repeated resume must not forgive an already
won timeout. The independent publication deadline catches “signals work but
no segment ever completes.”

### 9.5 Pause and retirement

Proposed internal `ROLLING_PAUSE_GRACE = 180 s`, starting at the first
accepted post-startup Hold. Repeated Hold, playlist reload and status request
do not renew it. Resume cancels it only through a valid newer Active command.
End terminates promptly; EOF cancels future publication obligations.

During grace, keep the publication clock but do not project active consumption
at 1× indefinitely. Stage only what the minimum valid next update requires.
On expiry or hard-cap refusal, retire and retain position/recipe for the
existing replacement path. A paused client must remain paused: do not
automatically reopen on every expired playlist error while Hold persists.

W3 owns read-only object grace after retirement. Clients learn the typed
retirement through the established control/response path; keep existing
response allowlists and relay compatibility in sync. No unconditional
`ENDLIST` on pause, hold, failure, or storage exhaustion.

Physical process EOF is not enough to publish ENDLIST if completed segments
remain staged. Publish all remaining ordered media and the end tag in the
same final snapshot; never strand the staged tail behind an ended playlist.

**Acceptance:** virtual-time tests cover every row in §9.2, startup, pause
boundaries, all flow reevaluations, delayed discovery, EOF and replacement.
Wire snapshot tests prove the header is constant and each unfinished update
has new media within its cadence. Explicit retirement is separately counted
and never satisfies the “uninterrupted playback” test.

## 10. W3 — object promises survive prune and retirement

### 10.1 Define three lifetimes and update every consumer

Proposed internal lifecycle:

```rust
enum SegmentVisibility {
    Advertised,
    Grace { removed_at: Instant, serve_until: Instant },
    Deleted,
}
```

Keep bytes on the object record until actual unlink; separately account for
produced-but-unadvertised staged objects. Init objects inherit the maximum
promise of dependent segments. Existing attempt-private garbage names remain
useful only after serving grace ends.

| Consumer | New meaning |
|---|---|
| `first_retained_index` and served MEDIA-SEQUENCE | First Advertised segment |
| `prunable` | Split eligibility for removal from eligibility for deletion |
| `server_ready` | State explicitly whether coverage is reachable from the current playlist; do not advertise Grace-only bytes as new-client runway |
| Takeover baseline | Begins from the advertised sequence, while old URIs stay bound to old objects |
| `segment_was_pruned` and segment-open path | Grace is still servable to an authorized old URI; Deleted is absent |
| `live_bytes`, ahead and garbage accounting | Count each physical object once; do not release space at logical removal |
| Refresh/rebuild | Preserve promises by exact object identity; never reset grace from writer history |
| Retirement/directory cleanup | Preserve promised objects without retaining producer authority |

Store the longest actual distributed playlist duration containing each
segment, or a conservative bounded equivalent. A global session upper bound
is acceptable only if its memory/disk cost is measured and it cannot grow with
the full append-only writer history.

When removing a segment, serve until removal time + segment duration +
longest containing playlist duration. For whole-presentation removal, retain
all objects for at least the removed playlist's duration; use the maximum of
that deadline and any earlier per-segment promise. See
[RFC removal requirements](https://www.rfc-editor.org/rfc/rfc8216#section-6.2.1).
Read-only grace responses cannot renew the retired producer, refresh its
lease, or invoke stale flow signals. Respect explicit authorization revocation.

### 10.2 Reserve storage before making promises

Audit actual charged quantities. Per-session ahead bytes and global live
scratch are different. Grace, staged batches, in-progress output, detached
garbage, initialization data and replacement overlap must be included in the
appropriate hard accounting, without double counting.

Maintain bounded admission headroom for a worst-case batch and committed
grace. Check before starting production and before publication. Never promise
bytes and then delete them early to satisfy a cap. If reservations cannot be
obtained, deny new admission or retire an existing presentation while honoring
its already-reserved promises.

The retained 180-second window plus ahead and grace can roughly double
steady-bitrate footprint; use actual variable-bitrate tests, concurrent
viewers, and paused sessions. Do not silently increase HLS ahead/global
limits. Shortening future advertised history is an explicit alternative only
with rewind/seek acceptance and a minimum 3T window; it never reduces old
promises retroactively.

Account for hard limits being operational caps with bounded in-flight write
envelopes: document and test the exact worst-case overshoot or reserve it in
advance. “Hard” must not mean “checked after arbitrary output already exists.”

### 10.3 Concurrency and acceptance

At snapshot removal commit, atomically record sequence advancement and grace.
Before physical deletion, recheck identity and deadline under the existing
producer-transition boundary. Never hold that lock across slow payload
unlink. A failed unlink stays charged and is retried by bounded cleanup.

Tests request a URI from snapshot N after N+1 removes it, at deadline−1 ms,
deadline, and after cleanup. Include range requests, init segments, owner
takeover, process replacement reusing filenames, delayed HTTP opens, actor
retirement, explicit End policy, authorization revocation and failed unlink.
A predecessor cleanup must not delete or serve a successor object.

Use a stable object binding/retention owner that can outlive the producer
actor. A filename reused in the same URI namespace cannot be resolved to
different bytes during a promised lifetime; isolate the old presentation
namespace or prevent such reuse until the promise ends.

**Acceptance:** admitted presentations retain all promised media within their
reservation, and over-cap admissions fail visibly without deadlock. A retired
actor does not prevent authorized grace reads or keep production alive.

## 11. W4 — expose accurate state and make client recovery bounded

### 11.1 Operator diagnostics

Fix local outcome persistence before relying on the analysis UI. Keep
`vod_index_pending` compatibility for old callers if changing its category
would disable their rolling fallback, but project an explicit authoritative
state and last error code for operators: queued/running, retry-wait,
failed-terminal, ready, or unavailable authority.

Do not infer failed-terminal solely from a missing local history row. Read
the authoritative job/request status and exact source identity. Update
Activity, analysis and refusal detail together; test old clients/new server
and relay parsing with additive fields. If a new wire refusal code is used,
enumerate every existing switch/allowlist before changing it.

Suggested bounded telemetry fields:

```text
startup_state / startup_remaining_ms / presentation_progress_seen
produced_end_ms / served_end_ms / staged_bytes
playlist_target_ms / served_revision / last_segment_advanced_idle_ms
next_publication_in_ms / publication_deadline_remaining_ms
maintenance_state / rate_estimate_source / estimate_active_speed
pause_grace_remaining_ms / retirement_reason
advertised_bytes / grace_bytes / reserved_bytes / live_bytes
index_expected_ms / index_covered_ms / expectation_provenance
index_terminal_code / packet_probe_bytes / packet_probe_elapsed_ms
```

Keep raw IDs and exact coordinates in bounded structured events, not
high-cardinality metric labels. Logs and docs use reference film F and media1.

### 11.2 Clients and platform-specific proof

Reverify paths on the implementation base:

- Apple: [PlayerView](../../clients/apple/Sources/Views/PlayerView.swift)
  and [AppleClientTests](../../clients/apple/Tests/AppleClientTests.swift).
- Android: [PlayerScreen](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt)
  and player policy tests. Media3 is pinned in
  [the version catalog](../../clients/android/gradle/libs.versions.toml).
- Web: [index.html](../../crates/plurxd/src/web/index.html) and
  [playback tests](../../tests/playback). If the web-shell split has landed,
  follow the moved playback module and do not duplicate logic in index.html.

Correct the web comment claiming every HLS session is VOD. Preserve existing
quality/recipe attribution and bounded reopen budgets. A new server retirement
must produce at most the existing allowed recovery, preserve film position,
and not cause a paused client to reopen repeatedly.

Apple's observed -11866/-12888 is native content-not-updated evidence.
Media3 1.10.1's default tracker detects stuck playlists after 3.5T, corrected
from the review's 3T estimate; source semantics do not substitute for device
results. Test hls.js separately: lack of the same native fatal does not prove
absence of reserve drain.

**Acceptance:** failed-terminal index state is visible, diagnostics survive
reader rollback, startup reports distinguish requested position from rendered
progress, and paused retirement waits for Resume without a reopen storm.

## 12. W5 — acceptance commands, receipts, and device qualification

### 12.1 Focused checks during development

Run the relevant filters below after adding regressions, and record their
matched test counts. Names containing `mkv_hls_` are proposed new test
prefixes, not claims that tests already exist.

```bash
cargo fmt --all -- --check
cargo check -p plurxd --all-targets --locked
cargo clippy -p plurxd --all-targets --locked -- -D warnings
cargo test -p plurxd --bin plurxd mkv_hls_startup
cargo test -p plurxd --bin plurxd mkv_hls_packet
cargo test -p plurxd --bin plurxd mkv_hls_publication
cargo test -p plurxd --bin plurxd mkv_hls_retention
cargo test -p plurxd --bin plurxd recent_speed_survives_a_hold
cargo test -p plurx-core mkv_hls
cargo test -p plurx-core --test store_contract mkv_hls
python3 -m unittest discover -s tests/operations -p test_docs_index.py
git diff --check
```

Use `cargo test … -- --list` to confirm focused filters matched intended
tests. A zero-test exit is not evidence. Register behavior regressions through
the repository's existing validation/history mechanism. Do not list a test
under two package receipts as though it were two independent proofs.

Run affected client commands on supported hosts:

```bash
make web-check                         # policy and embedded-JS contracts
make apple-build                       # iOS and tvOS compilation
make apple-test                        # affected Apple behavioral proof
make android                           # pinned Android compile
make android-test                      # JVM/lint proof
```

If the checkout cannot run a surface, use its documented build environment
and report the remaining device requirement. Never mark a skipped surface
green.

### 12.2 Integration matrix

| Dimension | Required cases |
|---|---|
| Duration | Valid absent metadata; longer audio/subtitle; conflicting candidates; invalid packet EOF; signed origin; output short and long; changed source |
| Startup | 32 s burst/20 s fetched/no render; healthy 72/84 s traces; slow cold source; End; hard cap; seek/replacement |
| Publication | T6 and T16; short/long mixed segments; early burst; fetch-stopped; 0.5×/1×/2× playback where supported; unknown/slow producer |
| Pause | Hold during startup; Hold after rendering; repeated Hold; Active at 179.999 s; expiry at 180 s; Resume; no repeated reopen |
| Ownership | Stale observations/timers; queued signals; late process output; same-name replacement; remote owner takeover |
| Storage | Grace at high bitrate; concurrent viewers; staged batches; in-flight writes; hard cap; failed unlink; whole retirement |
| Clients | Physical iPad Pro, Android Media3, and web hls.js with versions recorded |
| Compatibility | Compatible diagnostic rollback baseline; old/new relays; old fallback category; planned immutable VOD unchanged |

A healthy forced-rolling device run must cross at least ten hold/publication
cycles without unexpected stalls or deadline retirement. Repeat with the
fetch-stopped and quota conditions expecting a bounded classified failure.
Do not combine those expected-failure tests with the uninterrupted-playback
success count.

Duration acceptance needs a successful exact-identity rebuild and subsequent
immutable route choice. Forced rolling remains a separate run so a repaired
index cannot hide a broken fallback. Record TTFF and scratch peak before/after
the increased initial gate, ceiling-cut count, actual publication gaps,
target constancy and client error logs.

### 12.3 Promotion receipt

Freeze task merges, merge current main into the effort, rerun the pinned
compiler and focused tests on that exact candidate, and run full promotion
qualification. Dispose review findings before merge. A moved base or head
requires current-tree evidence and a new qualification receipt.

The completion record contains task/PR IDs, source SHA, test commands/counts,
fixture tool versions, device versions, review dispositions, pause-default
status, and outstanding production steps. No receipt uses private media paths
or library titles.

## 13. Rollout, rollback, and the production repair preview

The reviewed build is handed over before any unauthorized fleet mutation.
Prepare a concrete release checklist and exact-cohort preview as artifacts:

1. Compatible diagnostic/history behavior is available in the rollback binary.
2. Startup mitigation and the full rolling publication/grace contract pass
   forced-route qualification.
3. Packet fallback is enabled for controlled new analysis.
4. Read-only preview identifies current terminal no-duration rows; operator
   authorizes those exact force requests.
5. Requeue uses the public/established analysis path with deduplication and
   source fencing. Retain per-request outcome and successful route selection.
6. Expand only after cold/warm TTFF, scratch pressure, URI grace and platform
   recovery are accepted.

Rollback preserves existing valid indexes and stops new PacketTimeline work.
Use a rollback reader that understands unknown diagnostic provenance. For
rolling repair, stop admissions and retire/drain with promises intact; never
turn a live session back into indefinitely frozen playlists. Keep the 180 s
pause grace and 30 s startup deadline visibly identified as proposed defaults
until their release decision is recorded.

## 14. Sol's finish condition and handoff response

The user asked for a build from this plan. Sol should finish the implementation
and required reviewable evidence, not stop after reproductions or a partial
plan. If a fixture or hardware environment is unavailable, complete independent
code/test work and report the exact remaining acceptance boundary.

At completion, return the reviewable branch/PRs and qualification state; link
the updated RCA and this plan; summarize actual tests and device observations;
list production actions still awaiting authorization. Do not claim the
incident fixed from unit tests alone.

Guardrails: preserve the selected-video contract, two-second tolerance,
positive artifacts, existing attempt/source/lease fences, bounded recovery,
truthful ENDLIST, hard storage accounting and neutral evidence names.
Do not dispatch another task or deploy merely because this document names Sol.
