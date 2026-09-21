# Playback implementation — finish the control loop without another rewrite

> **Next-work reconciliation, 2026-09-12:** The lifecycle implementation landed
> through PR #259 at `5548c3d3`. For remaining work and current execution rules,
> read the [rewrite remainder](PLAYBACK-REWRITE-REMAINDER.md). The test-deferral
> and fast-lane-only instructions below describe the earlier campaign; Paul’s
> replacement AGENTS.md requires focused local tests and current qualification
> for the new effort. Historical receipts below remain unchanged.

**Status:** built — implementation and main promotion completed 2026-09-12.
**Handoff integration baseline:** `30cd51afc` (merged documentation PR #255).
**Incident/source audit baseline:**
`efd54247adddeb3978812e55ebbb6a7f08adc9d9`. No runtime changes are delivered by
this document. **Policy:** Paul's current no-software-gating instruction and
the September 10 CI/CD correction supersede older milestone rollout rules.

Companion to the [lifecycle coverage map](PLAYBACK-LIFECYCLE-COVERAGE.md)
(L01–L33: required behavior) and the
[rewrite remainder](PLAYBACK-REWRITE-REMAINDER.md) (R01–R10: what is unfinished).
This document specifies work order, code boundaries, transition contracts,
focused verification, and stopping point. Use these three documents together;
do not execute the historical milestone backlog as another full rewrite.

## 0. Sol — start here and carry the implementation through

Your assignment is to implement P1–P4, verify the affected behavior, and finish
P5's evidence/status accounting. This is an implementation assignment, not an
invitation to write another plan. Begin with S01 below and work in order. The
numbered technical sections remain the behavioral contract; the S tasks are
the edit/verify sequence. They are not separate features or mandatory PRs.

**Execution instructions from Paul's builder task:** use your own separate
clone, make proper commits, batch them into substantial PRs, maintain a visible
status page, and keep building without waiting for routine decisions. Record
material assumptions for Paul. Ask the source task, `01a09328-12bc-7e71-a788-e3fe4b153bc4`,
for design clarification if needed. Do not stop at another plan or placeholder.

**Test execution is deferred.** Write meaningful regressions alongside product
changes and compile the affected code/test targets. Do not run unit, integration,
simulator, emulator, browser, playback or physical qualification suites during
this implementation campaign. Execute the existing fast lane only after the
single adversarial review and author corrections for each main-bound PR. The
separate sweep owns runtime test execution and batches its results later.
“Verify” and “acceptance” below specify assertions to implement, source/compile
checks to perform, and outcomes to record when that sweep runs; they do not
authorize extra runtime test runs. Mark these tests `written; not run` until
actual evidence exists. Never claim a regression failed before/passed after
without executing it. This instruction supersedes earlier audit suggestions
to run focused tests or obtain physical acceptance during development.

Read this document first, then the companion lifecycle rows and source/test
anchors for the task you are doing. Read older milestone documents only when
following a specific existing interface. Their rollout gates, stale platform
capabilities and obsolete “not implemented” statements do not override this
handoff. Use the current checkout's symbols, not line numbers from an old audit.

**Non-negotiable implementation constraints:**

- Keep both immutable VOD and rolling HLS fallback working. Assert which path
  each regression exercises. A movie title or remux recipe does not identify it.
- Buffering is active play intent. Separate source supply, complete publication,
  successful HTTP transfer, contiguous loaded media, decoding and presentation.
- Reuse current actors/controllers and their deadlines. No additional autonomous
  watchdog, status-driven restart owner, global buffering increase or blanket
  restart-timer reduction is an acceptable substitute for fixing an owning rule.
- No empirical software gates. Enabled features attempt supported operations;
  Developer requirements are advice. Preserve actual authorization, ownership,
  protocol compatibility and resource accounting as operation correctness.
- Preserve v1's local-switch/first-presentation/committed-ack/durable-commit order.
  Never manufacture first-frame evidence or reinterpret `buffer_ready` as commit.
- Apply the current one-review/fast-lane process in §9. Do not add runtime test
  jobs, automatic deployments or additional review rounds to complete this work.

### 0.1 S01 — establish a clean base and a working build loop

**Edit:** nothing yet. Create your own separate clone and inspect current
`main` and active effort/task branches. Reuse an existing matching effort if
work has started. Do not work in Paul's checkout or a worktree attached to it,
reset its state, resurrect the merged documentation branch, or overwrite
concurrent Library channel work. Worktrees attached to your own clone are fine.
Record actual base SHA and inspect changes since `30cd51afc` only in the
playback surfaces this assignment touches.

**Build:** establish Rust 1.97.1 and affected Apple/Android build environments
using §11. Record pre-existing build failures separately. A missing local
compiler calls for the existing source-only compiler loop, not pushing code to
find compiler errors. Do not change the pinned toolchain to match the host.

**Finish:** one clean intended base, known compiler commands, and a maintained
`PLAYBACK-LIFECYCLE-STATUS.md` in the same subject folder, indexed in the same
commit. Use it as the one execution status page: P1–P5 progress, current
task/branch/PR, latest compilation, deferred tests, decisions, and next action.
Keep the remainder as the scope audit, not a second execution tracker. Spend no more than
one working session re-investigating foundations already audited here. A real
access/build blocker is recorded precisely; independent implementation can
continue where it does not depend on that blocker.

### 0.2 S02 — add the wait-episode regression before tuning pacing

**Edit:** the test modules beside `evaluate_flow` in
[transcode.rs](../../crates/plurxd/src/transcode.rs), control decision tests in
[playback_control.rs](../../crates/plurxd/src/playback_control.rs), and existing
client observation/recovery fixtures. Use the
[sanitized incident receipt](../evidence/playback-lifecycle-observation-2026-09-11.json)
to choose states; it is asynchronous observation, not a deterministic replay
with known intervening events.

**Implement:** a controlled-clock scenario with fixed film position, active
intent, producer time hold, and two variants: nearly zero contiguous runway
and approximately 22 seconds of loaded runway. Feed accepted fresh sequences
through real policy; repeat holds, delay media transfer, and advance fake time
without advancing the playhead. Model presentation separately from buffer load.
Use fixture adapters to bridge boundaries rather than invent a new simulator.

**Verify:** distinguish (a) no needed bytes available, (b) available bytes not
transferring, and (c) loaded media not presenting. Trace which current source
condition would violate each assertion; do not execute this fixture now.
If source already satisfies a case, retain its assertion and do not change
producer policy just to match a proposed formula. Name new regressions consistently
with `lifecycle_refill_` in Rust so the separate sweep can select them together.

**Finish:** a compilable behavioral regression for the identified defect, or an
explicit record that only the observed coexistence is modeled and the
initiating cause remains unknown. The latter supports diagnostics work, not a
claim the freeze is fixed. Record execution as deferred in either case.

### 0.3 S03 — carry episode identity and implement P1 at the failing boundary

**Edit:** accepted-control handling and existing delivery/event snapshots;
`FlowInputs` / `evaluate_flow`; the VOD demand/materialization functions in
§4.3; and the client hold-consumption path in §4.4. These are internal state
changes. Do not add fields to strict v1 requests as an incidental refactor.

**Implement:** keep episode state in the existing serialized owner: incarnation,
accepted sequence/intent identity, start/deadline, captured anchor/frontiers,
one refill allowance if justified by S02, and whether a recovery action was
already executed. The precise struct name is an implementation choice. Keep
the state bounded to the current episode; do not create an unbounded history.

Fresh duplicate/replayed requests cannot grant new work. Position drift within
an unresolved episode cannot repeatedly extend its deadline or allowance.
Terminal state and superseding intent cancel the old operation. Actual resumed
presentation closes it. Ordinary pause/resume invalidates stale callbacks but
does not repeatedly mint speculative refill credit without new demand/progress.

If implementing §4.2's refill credit, use known frontiers in one timeline and
checked/saturating arithmetic; unknown publication is not zero. When publication
is unknown, retain normal startup/supply handling and collect its next fact
instead of manufacturing a goal from absent data. Keep the credit in the flow
owner and reuse its existing deadline wakeup. Client and server deadlines use
their own monotonic clocks; neither compares a remote wall clock directly.

**Verify:** T01–T04 plus cancellation at the hold/resume acknowledgement boundary,
unchanged hard limits, finite EOF, non-unit playback rate, nonzero media origin,
and no rearming from `waiting`/`stalled` oscillation. An active producer must
actually receive its existing resume operation; a status-string change is not
progress. In VOD, assert one foreground materializer and pinned contiguous data.

**Finish:** P1's diagnostics explain a wait and its exit, and the demonstrated
defect has a regression. Do not add a restart loop to make the test terminate.

### 0.4 S04 — finish Apple recovery ownership first

**Edit:** `PlayerController`, its snapshot mapper/control session, and existing
[ownership tests](../../clients/apple/Tests/PlayerOperationOwnershipTests.swift),
[control mapping tests](../../clients/apple/Tests/PlaybackControlSnapshotMapperTests.swift),
and [prepared tests](../../clients/apple/Tests/PreparedReplacementTests.swift).

**Implement:** route native wait events, control verdicts, retries and status
observations through one existing controller executor. Capture current intent,
session and attachment before every asynchronous action; recheck before attach,
seek, play or release. A loaded-but-waiting observation gets at most one native
reevaluation per episode, subject to active viewer intent. Repeated `hold`
responses do not postpone the existing absolute recovery bound.

Move each old decision-making call site, then delete its superseded timer or
restart branch. Keep observation and lease maintenance if still needed. Do not
remove a method merely because its name contains “monitor.” Record the exact
deleted executors and remaining observation-only callers at P2 closeout.

**Verify:** T03/T05 on both iOS and tvOS source targets. Test pause during refill,
seek during wait, stop while create is pending, immediate restart before the
old release returns, and recipe change during reopen. Assert final player and
intent, not only callback counts. Include the library-channel controller's
separate active-buffering path without undoing merged PR #254 behavior.

**Finish:** the reported Apple path has one recovery executor, changed targets
compile, and targeted regression definitions are present with execution deferred.
A physical Apple TV run remains a separate sweep observation under §8.1.

### 0.5 S05 — apply the same ownership contract to Android and web

**Edit:** §5's Android/web controllers and reporters; existing
[Android request ownership](../../clients/android/app/src/test/java/tv/plurx/app/player/PlaybackRequestOwnershipTest.kt),
[recipe ownership](../../clients/android/app/src/test/java/tv/plurx/app/player/PlaybackRecipeOwnershipTest.kt),
[control mapping](../../clients/android/app/src/test/java/tv/plurx/app/player/PlaybackControlSnapshotMapperTest.kt),
and [web control tests](../../tests/playback/web-control.test.js).

**Implement:** port the intent/episode/attachment invariants, using native
Media3/browser observations instead of copying AVPlayer heuristics. Ensure
control unavailable/media healthy and media unavailable/control healthy take
distinct paths. Select genuine legacy/direct/progressive adapters once;
transient control failure cannot enable two recovery executors.

**Verify:** T05 and the relevant T10 cases on each client. Trigger simultaneous
native error, control verdict and delayed status observation for the same
episode; assert one recovery and stale-result cleanup. Preserve native text
subtitle retries as track-only work; burn-in follows recipe replacement.

**Finish / first delivery:** P1–P2 are independently reviewable, with all changed
platforms compiled and focused regressions recorded. Promote this coherent
change through §9 before waiting for planned drain or broad codec evidence.
The focused regression receipt here says written/compiled, not runtime-passed.

### 0.6 S06 — re-plan candidate recipes and remove proof-based admission

**Edit:** `candidate_request`, `process_preparation_candidate`,
`review_client_plan`, preparation decisions and settings paths named in §2/§6.

**Implement in this order:** retain ordinary create's full capability/source
facts; resolve latest explicit intent over retained policy; build the candidate
through the ordinary planner; derive the candidate's actual effective recipe;
then remove measured-axis/direction/throughput vetoes. Do not widen the decision
table while leaving stale-transcode construction in place. Native subtitles
that do not change the video recipe stay on their existing fast path.

Keep one speculative successor and existing capacity accounting. Missing
measurements do not refuse it. Actual foreground contention cancels the current
speculative attempt once and yields to refill. A future explicit change can
try again; do not persist a failure as a hidden opt-out.

**Verify:** T07/T08 with an Original request from a transcode, both supported
delivery directions, a grade change expressed through existing full intent,
combined audio/burn/quality change, omitted intent fields, and unknown/low
throughput. Assert planned and actual output, retained policy, and resource
release. A correct required transcode is allowed when source/caps demand it.

**Finish:** enabling the feature actually attempts the planned operation;
Developer advice cannot reject a setting write or silently change preference.

### 0.7 S07 — complete prepared settlement across all three adapters

**Edit:** existing prepared server transaction plus Apple, Android and web
adapters. Use §6.1's v1 order as written, including the locally-switched but
not durably settled window. Preserve existing action ID, origin and intent
fences; do not introduce a second commit protocol.

**Implement:** share/join repeated preparation and settlement for the same
action. Serialize stop/end with any committed acknowledgement. Reconcile a
lost/rejected commit before choosing the surviving incarnation; never assume
the local predecessor is still attached. Disable before switch aborts; disable
during switch settles first. Keep finalization deadlines and physical cleanup
acknowledgements in their existing owners.

**Verify:** T06 with duplicate metadata/buffer/commit messages, expired action,
first frame followed by commit rejection, lost commit response, stop during
local switch, second intent during prime, decoder allocation failure, and a
browser without the strongest frame callback. There is no invented frame time,
leaked decoder, second commit, or unbounded automatic restaging.

**Finish:** P3 can apply a requested change or make one explicit bounded
fallback, with correct current ownership and observed interruption.

### 0.8 S08 — connect planned drain to the prepared transaction

**Edit:** §7's coordinator/routing code and existing store implementations
[SQLite sessions](../../crates/plurx-core/src/store/sqlite/sessions.rs) and
[replicated sessions](../../crates/plurx-core/src/store/hiqlite_sessions.rs)
if the target-owner reservation/CAS requires it. Keep store-interface and both
backend contracts aligned in the same package; do not implement relocation in
only one backend. Reuse source/media authorization and existing relay paths.

**Implement:** a relocation reason bypasses quality-equality classification,
not ownership checks. Reserve/prime on a real eligible target; authorize staged
media before durable control ownership changes. Local first presentation leads
to committed acknowledgement and one atomic owner/route publication. Drain
only the predecessor. For abrupt loss, preserve loaded media and let the
existing takeover protocol choose the winning epoch before one reopen.

**Verify:** T09 on both relevant store contracts and coordinator/relay tests.
Inject crash before local switch, after first frame/before commit, and after
commit/before response. Include an old-owner rejoin, unavailable source,
exhausted target capacity, and different control ingress. Assert one durable
winner and eventual cleanup; do not assume abrupt failover is seamless.

**Finish:** P4's planned and abrupt transitions are implemented and the changed
Rust/store surfaces compile. A new cluster scheduler is outside this task.

### 0.9 S09 — close evidence, remove superseded code, and deliver

Prepare the small physical observation pass in §8.1 for the separate sweep on
explicitly deployed builds. Record it as `not run`; do not wait for every
codec/device combination before delivering source. Preserve the hybrid fallback
and genuinely needed old-client adapters. Delete competing recovery authority
only after its callers use the new owner, with focused race checks in place.

Update the status page, P1–P5 checkboxes and R01–R10 dispositions. Keep a
compact execution record per package: source SHA; changed
symbols; command/result/test count; removed owner or timer; physical build and
outcome if run; unresolved issue and next owner. Do not create a new milestone
document for every command or use “accepted” to mean “not exercised.”

**Finish / second delivery:** P3–P4 and closeout use §9's normal main promotion.
Give Paul PR links, actual behavior changes, validation evidence, and any
remaining observed defect. R07 engine retirement, R09 exhaustive fleet breadth,
and R10 new semantic/prewarm features do not grow this implementation effort.

## 1. Deliver the freeze repair first, then finish the remaining transitions

The control protocol, VOD materialization, rolling flow actor, prepared
successor priming, and client adapters already exist. Extend those owners.
The observed Apple TV waits included both almost-empty and substantial loaded
buffers while the rolling producer was time-held. That establishes an overlap
between pacing and waiting; it does not establish which boundary initiated
each freeze. A loaded range is not proof of decodable or presented frames.

Build in four implementation packages and one closeout. P1 and P2 form the
first useful delivery; do not hold the freeze repair for cluster handoff or a
fleet-wide evidence campaign. P3 and P4 finish the bounded rewrite remainder.

| Package | Deliverable | Remainder covered | Dependency |
|---|---|---|---|
| P1 | Joined wait episodes and a reachable refill path for VOD and rolling HLS | R01, diagnostic portion of R08 | Existing owners and source baseline |
| P2 | One client recovery owner across ordinary lifecycle transitions | R02; seek/subtitle portion of R06; two-engine portion of R07 | P1 episode and progress semantics |
| P3 | Prepared replacement without evidence vetoes; bounded contention handling; Developer controls | R03–R04 | P2 intent and action ownership |
| P4 | Planned drain using the prepared transaction, bounded abrupt-owner-loss recovery | R05; alternate-ingress portion of R08 | P3 transaction and existing cluster fencing |
| P5 | Targeted device observations, removal of superseded paths, current status and handoff | Remaining R06–R09 evidence and reconciliation | Runs alongside packages; closes after P4 |

**Scope boundaries:** keep hybrid VOD with rolling HLS fallback. Keep direct
and progressive delivery as separate adapters. Do not rebuild M1–M7, invent a
new streaming engine, add a generic orchestration framework, expand semantic
detection, or implement new rolling/subtitle prewarm features. Existing VOD
marker prewarm stays subordinate to foreground demand. R10 is explicitly
deferred. Retirement of rolling fallback is a separate product decision.

**Time discipline:** aim for two main promotions, not one PR per transition:
P1–P2 first, then P3–P4 plus closeout. Start with one bounded source-tracing and
regression-construction session, with execution deferred under §0. If it cannot
identify the initiating freeze, finish joined diagnostics and falsifiable
regressions for the observed states;
record the uncertainty instead of spending days tuning speculative timers.
Any new decoder feature, cluster scheduler redesign, or protocol redesign gets
a separate issue with a concrete reason it is required. A defect in a package
must be corrected; an unrelated research milestone must not expand it.

## 2. Enablement is a user choice, not a qualification system

Ship completed behavior enabled by default. Add no rollout cohorts, hidden
environment flags, artifact qualifications, device allowlists, benchmark
receipts, shadow-only prerequisites, or elapsed soak requirements that decide
whether a user may use it. Do not replace one veto with a renamed veto.

Reuse existing switches if a disable path is needed. At most, expose
enable/disable and factual requirements in the Developer tab of Settings.
Requirements are advisory: `met`, `not met`, or `unknown`, with the observed
fact and its age. They never disable the switch, reject its API update, or
silently turn it back off. Missing throughput is `unknown`, not a refusal.
Ordinary viewers need no diagnostics or setup ceremony to play a title.

Actual operations still have concrete outcomes. An absent codec cannot decode,
an old client cannot parse an unimplemented action, an exhausted process slot
cannot start another worker, and a stale owner cannot commit a session. Handle
those as typed runtime outcomes with cleanup and a bounded fallback. Keep
authentication, epoch/intent fencing, allocation limits, and true action
compatibility. They must not contain a “has this device/recipe passed our
qualification?” condition. An observable media-readiness condition before
switching players is transaction correctness, not a feature-enablement rule.

| Current code / policy | Required change |
|---|---|
| `PREPARED_AXIS_SETS`, `AxisNotProven`, `MultipleAxes` in [playback_control.rs](../../crates/plurxd/src/playback_control.rs) | Retain axis classification for recipe handling and diagnostics; remove measured-combination allowlisting from admission. A compound request produces one resolved successor recipe. |
| `PreparationConditions::headroom_refusal` | Remove the 2× measured-throughput prerequisite and missing-measurement refusal. Use real foreground progress and bounded speculative work as in §6. |
| `successor_rate_is_bounded` | Remove the historically measured-direction restriction. Account for real resource use during preparation in either direction. |
| Platform/device proof literals or broad capability withdrawal | Advertise the implemented adapter and actual protocol support. Do not use a historical hardware test result as capability. |
| Web `presentationProof` and `preparedRefused` in [index.html](../../crates/plurxd/src/web/index.html) | Separate observation availability from ability to prepare. Add the bounded presentation fallback in §6; one failed attempt must not disable preparation for the rest of the film. |
| Existing server and client prepared-handoff switches | Retain opt-out behavior and default-on semantics. Display local choice and server choice distinctly in Developer settings; do not add another master switch. |
| Older “qualify, then enable” milestone prose | Mark superseded by this contract. Keep receipts as observations, never as runtime permission data. |

The existing server settings write path in
[http/system.rs](../../crates/plurxd/src/http/system.rs) already persists
`prepared_quality_handoff` without a readiness lookup. Preserve that property.
Use the existing [setting keys](../../crates/plurx-core/src/store/mod.rs),
[Apple settings](../../clients/apple/Sources/SettingsStore.swift),
[Android settings](../../clients/android/app/src/main/java/tv/plurx/app/data/SettingsStore.kt),
and web Developer tab. Do not broaden this package into unrelated feature
settings. Disabling preparation prevents new reservations; abort a successor
that has not switched locally. If the local switch or durable commit is already
in flight, finish its settlement and cleanup before ending preparation. The
active player's demand and media lease remain valid.

## 3. Keep one owner for each decision and one clock for each frontier

### 3.1 State is a tuple, not a growing list of player booleans

Use the existing control vocabulary. Viewer intent is `active`, `hold`, or
`end`; render observation is `starting`, `rendering`, `waiting`, `stalled`,
`seeking`, `ended`, or `failed`. Buffering preserves active intent even when
the playback framework reports rate zero. Supply state and ownership are
separate coordinates. Do not add a parallel state machine beside each existing
controller; consolidate decisions into its existing serialized owner.

```text
 active + rendering ── loss of presentation ──> active + waiting
         ^                                      │
         │               classify next missing boundary
         │                     │                │
         │                supply pending    media already usable
         │                     │                │
         │              refill / fetch     native reevaluation
         │                     └───────┬────────┘
         └──────── actual presentation resumes ┘
                                       │ deadline without presentation
                                       v
                              one owned recovery attempt

 viewer pause / seek / stop supersedes the old episode and its callbacks
```

| Owner | Owns | Must not infer |
|---|---|---|
| Client lifecycle controller | Latest intent, contiguous loaded runway, presentation, one recovery operation, native player attachment | Producer health from loaded bytes; viewer pause from rate zero |
| Control session actor | Accepted sequence, generation/epoch, lease, current demand, wait episode, authoritative action | New demand from repeated identical requests or GET status |
| Rolling flow actor | Physical producer run/hold, publication and resource bounds | Successful rendering from successful SIGCONT |
| VOD manager | Missing-segment materialization, foreground windows, reader/production lifetime | A planned segment is readable or a far-ahead island fills the current gap |
| HTTP delivery owner | Attempt, first byte, committed bytes, complete body or truncation | Successful playback from a 200 response |
| Prepared transaction / cluster coordinator | Reservation, durable commit, successor publication, predecessor retirement | Staged metadata is a playable successor |

All comparisons use film milliseconds. Convert rolling item/segment-relative
positions through the session's `media_origin_ms` before computing gaps. Keep
the original value and origin in diagnostics. Runway is the contiguous loaded
range containing the current playhead, not the sum of every buffered island.
Use intended playback rate to convert media duration to wall-clock reserve.
Unknown published/decoded values remain unknown; never replace them with zero.

### 3.2 Keep the current wire contract and make its use precise

[ControlRequestV1 and DeliveryView](../../crates/plurxd/src/playback_control.rs)
already carry demand, render state, position, buffer endpoints, rate, seek,
observations, selection/intent, supported actions, publication/fetch progress,
hold reason, producer decision, subtitle readiness, and owner information.
Use these before proposing new protocol fields. Requests reject unknown
fields; do not silently add a v1 request field that breaks an older server.

| Event | Communication | Acceptance and effect |
|---|---|---|
| Create / attach | Existing create response, then `POST /api/v1/hls/{session}/control` | Capture session generation, control epoch, client instance and media origin; first exchange sends implemented capabilities/actions. |
| Play, pause, wait, resume, seek or selection change | Prompt exchange through the existing reporter; serialize/coalesce observations | Newest intent wins. Preserve monotonic sequence and current generation. Do not open a second reporter to get a faster response. |
| Periodic maintenance | Existing server-directed cadence; currently 5 seconds | Renew only accepted fresh demand. Exact duplicate replay does not extend ownership. |
| Server progress / decision | Existing control response and delivery view | Apply only to the matching request incarnation and intent. A hold is production information, not evidence of client presentation. |
| Prepared action and acknowledgement | Existing `prepare_replacement` vocabulary and acknowledgement transaction | Deduplicate action ID; retain/replay settlement across response loss. Do not invent an existing `commit_replacement` wire action. |
| Stop / cancel | Demand `end` and existing DELETE release path | Cancel local work immediately; serialize bounded settlement/release, with lease expiry as final server cleanup. |
| Diagnostics | Existing event logging/telemetry, separate from strict control request schema | Add correlation and sample ages without making logging success a playback prerequisite. |

Preserve the existing 4-second exchange deadline and 5-second maintenance
cadence initially. An episode deadline is absolute monotonic time: fresh
heartbeats, repeated holds, changed status text and retry responses cannot
restart it. Retain current lease distinctions (explicit rolling 30 seconds,
legacy rolling 60 seconds, VOD 300 seconds); do not hide the bug by stretching
leases. Direct/progressive paths use their own transport lifecycle where an
HLS session/control exchange does not exist.

## 4. P1 — repair refill at the boundary that is actually waiting

### 4.1 Add a joined episode to existing diagnostics

Extend existing event records at wait entry, boundary progress, action,
and resumed presentation. Do not log a new full snapshot on every video frame.
Internally join by session incarnation, owner epoch, client instance, accepted
sequence, intent revision and a wait episode number. Client and server episode
numbers may differ; join using accepted request identity rather than assuming
their clocks or counters match.

Record: actual engine/presentation; client/server build; intent and native wait
reason; film/item position and origin; contiguous loaded endpoints; decode or
presentation observation if available; published and fetched frontiers;
outstanding requested segment/range; materialization state; physical producer
hold and reason; resource pressure; chosen action; sample age; monotonic wait
duration. Preserve existing private telemetry handling; exported test receipts
use sanitized join IDs and omit capability URLs and user/session identifiers.

**How to read it:** the first boundary that stops advancing while its
downstream needs data is the candidate blocker. A producer hold with a healthy
loaded buffer can be normal. A progressing fetch with no resumed presentation
points downstream. “Server pacing” alone must never be reported as a proven
explanation of a frozen frame. Distinguish planned, available, fetched,
loaded and presented throughout the trace.

### 4.2 Give rolling refill one finite opportunity independent of playhead motion

Edit `explicit_production_target_seconds`, `evaluate_flow`, and the existing
control-to-flow demand path in
[transcode.rs](../../crates/plurxd/src/transcode.rs). Preserve ordinary steady
play pacing: runway plus the existing 30-second intended-rate reserve,
configured time cap, and hysteresis. Preserve byte/global/process bounds and
their physical acknowledgements.

The proposed change is a **single bounded refill credit per wait episode**:

1. On a fresh accepted `active + waiting/stalled` observation, capture the
   current film anchor, contiguous loaded end and published end. In the same
   actor, record a fixed refill goal and deadline. Seek uses the newest target;
   it invalidates the prior wait episode.
2. Let `P` be the film anchor, `E` the published end in film time, `B` the
   contiguous loaded end, `r` the intended rate, and `C` the existing configured
   maximum ahead in media milliseconds. Proposed goal:
   `G = min(P + C, max(P, E, B) + ceil(r * 30_000))`, bounded by finite EOF.
   If time pacing is disabled, keep its existing disabled semantics; do not
   interpret zero as a zero-length buffer or introduce an unbounded credit.
3. While this credit is live and `E < G`, time hysteresis alone must not keep
   the producer stopped. Demand-end, lost lease, terminal failure, actual byte
   or global resource limits still apply. A blocked credit records its real
   resource reason. It does not turn a hard resource limit into a time hold.
4. Stop the exception at `G`, episode deadline, cancellation or terminal state.
   Do not extend `G` on every heartbeat. Do not rearm just because the native
   framework alternates `waiting` and `stalled`; require resumed presentation
   with advancing position, or an explicit new seek/session intent.
5. If `G <= E`, production has no permitted additional supply to offer. Move
   diagnosis to fetch/load/decode; do not promise more production is coming.
   Additional buffered production is not itself a successful recovery.

This formula is a proposed implementation, not a measured cure for the incident.
The coupled regression must establish that it supplies progress with fixed
film position and terminates within the existing bounds. If the fixture shows
the present flow actor already makes that progress, keep the existing formula
and fix the demonstrated downstream condition instead. Do not ship a second
policy that performs the same work merely because it appears in this plan.

### 4.3 VOD refill follows missing contiguous segments, not rolling pacing

Use `control_with_terminal`, `playback_demands`, `reader_window`,
`open_materialized`, and the existing materialization watchdog in
[vodserve.rs](../../crates/plurxd/src/vodserve.rs). For active waiting, the
foreground demand window begins at the current/seek anchor and covers the
next contiguous required segments. Pending requests retain/join the matching
materialization job. Ready distant segments cannot satisfy the near gap.

Ensure current refill outranks marker prewarm and successor speculative work;
preserve deduplication and read-window retention. A bounded wait ends with
servable complete bytes, an explicit retry/resource outcome, or a classified
failure. Repeated polling must not abandon and rebuild the same job. A segment
listed in an immutable playlist is not sufficient readiness. Do not copy the
rolling time-hold algorithm into VOD or add another materialization watchdog.

### 4.4 Separate supply delay from loaded-but-waiting recovery

Update `recovery_outranks_hold` / `resolve_action` in
[playback_control.rs](../../crates/plurxd/src/playback_control.rs) and the
client consumption of holds. The current narrow predicate requires stalled,
starved, runway at most 10 seconds and at least 10 seconds published-but-unfetched.
It does not cover the observed substantial-buffer AVPlayer wait.

| Evidence during active wait | Owning action | Completion |
|---|---|---|
| Required bytes not yet produced/materialized | Existing producer/VOD owner makes bounded refill progress | Complete next required segment available, then fetched |
| Complete bytes published but current request makes no progress | Existing delivery retry policy repairs the exact resource/attempt | Body completes without accepting truncated EOF |
| Contiguous media loaded, native player still waiting | Existing client recovery owner performs one native play/readiness reevaluation, if intent is still active. On Apple, the reevaluation is `playImmediately(atRate:)` only when contiguous runway is strictly greater than `rate * 10s` | Actual presentation and advancing position; AVPlayer status or one displayed frame is insufficient |
| Hard resource pressure | Prioritize foreground, cancel/preempt speculative work through existing scheduler | Admission becomes available or existing bounded failure is surfaced |
| Terminal producer/decoder decision | Existing supported terminal or retry action | Exact decision handled once; no blind wait |
| Evidence insufficient | Keep media, gather the next joined snapshot under the same deadline | Classify or enter existing bounded recovery; never reset the episode |

Production holds may describe server state but cannot indefinitely defer the
client's loaded-media recovery. Ordinary Apple stalls retain the existing
20-second absolute deferral ceiling, while an explicit resume of established
on-demand playback owns one narrower attempt: a one-second buffered fast-path
allowance, one same-delivery repair, and one absolute 15-second deadline shared
across that repair. Resume publishes active intent in control-sequence order
before repair; a pending seek or replacement retains that intent without
starting the predecessor item. Success requires display timestamps newer than
the paused baseline and 250 milliseconds of continuing motion. The same
strictly-greater-than-ten-wall-seconds runway guard governs both buffered
resume and the ordinary native nudge. Keep
`automaticallyWaitsToMinimizeStalling` enabled; do not globally enlarge buffers
or lower unrelated restart timers. The exact Apple implementation contract and
live promotion state are in the
[Apple pause/resume handoff](../clients/APPLE-PAUSE-RESUME-IMPLEMENTATION-HANDOFF.md)
and [status page](../clients/APPLE-PAUSE-RESUME-STATUS.md).

**P1 acceptance:** drive real flow/materialization policy through fixed-position
empty-buffer and substantial-buffer waits; verify reachable supply or a named
downstream action, bounded resources, no duplicate action, and resumed
presentation. Healthy refill must finish on the same session; eventual reopen
alone is not a pass. Preserve the modeled case and state its unexecuted status
if the initiating cause is still unknown; do not label that incident fixed.

## 5. P2 — consolidate the client lifecycle and remove duplicate authority

Use [Apple PlayerController](../../clients/apple/Sources/PlayerController.swift),
[Apple snapshot mapping](../../clients/apple/Sources/PlaybackControlSnapshotMapper.swift),
[Android PlayerScreen](../../clients/android/app/src/main/java/tv/plurx/app/player/PlayerScreen.kt),
[Android control mapping](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackControlSnapshotMapper.kt),
and the [web reporter](../../crates/plurxd/src/web/playback-control.js) with
[web player integration](../../crates/plurxd/src/web/index.html). Keep their
native adapters; align the decision contract and tests rather than introducing
a cross-platform runtime dependency.

The client controller is the sole executor for native retry, reopen, attach,
seek and teardown. Server actions have authoritative production/session meaning;
the client maps them to one operation against current intent. Native events
and diagnostics supply observations, not independent restart authority.

Every asynchronous operation captures session generation, intent revision,
local attachment identity and operation/action ID. Before mutation, compare
all relevant ownership fields. Stale completion performs only cleanup of its
own resources. An old stop cannot end a newly restarted session; an old seek
cannot attach after a newer seek. Replayed actions return the existing result.

| Lifecycle cases | Required implementation behavior |
|---|---|
| L01–L04 open/admission/start | One cancellable start owner; startup demand reaches source/producer before loader waits; metadata-ready and first presentation remain separate. |
| L05–L11 normal/wait/refill/resume | Use P1 episode; event-driven exchange plus existing cadence; only actual presentation closes wait. One recovery executor even if status, native event and server verdict arrive together. |
| L12–L14 pause/resume/background | Pause sends hold and retains bounded buffer/lease under current policy. Apple explicit Resume publishes active intent in sequence order, suppresses predecessor playback while a seek/replacement is pending, and uses the bounded presentation/repair attempt in §4.4. Suspend obeys platform lifecycle; background expiry leads to fresh ownership on wake rather than extending the resume deadline. |
| L15–L17 seek/seek storm | In-range seek reuses current media; cold seek updates foreground window/rolling origin as applicable. Coalesce to latest target, cancel old job ownership, preserve subtitle alignment and explicit play/pause intent. |
| L18–L24 reserve/change/reopen | Latest intent can cancel preparation. Selection changes resolve one coherent recipe. Same-delivery recovery and recipe replacement cannot both attach a successor. P3 owns prepared settlement. |
| L25–L28 stop/restart/end | End cancels work and settles once. Restart gets fresh incarnation. Natural finite EOF requires expected duration/complete delivery; truncation is failure, not successful end. |
| L29–L30 one plane unavailable | Control loss alone preserves playable media; media loss is diagnosed even if control succeeds. Recovery and reattachment reconcile lease/epoch before mutation. |
| L31–L32 owner loss/contention | P4 owns routing/epoch recovery; player retains usable buffer. Foreground current playback has priority over speculative preparation/prewarm. |
| L33 live/channel rollover | Keep Live TV live-edge/tuner semantics and library-channel scheduled-title semantics distinct. Buffering keeps active demand; next scheduled title does not inherit a dead session's identity. |

Audit `startStatusPolling`, `startPlaybackRecoveryMonitor`,
`retrySameDeliveryAfterStall`, `applyStallVerdict`, and `holdMayDecideStall` on
Apple, then corresponding Android/web call sites. For each existing timer
or poll, record whether it supplies an observation, maintains a lease, or
executes recovery. Move decision-making to the lifecycle owner and delete the
superseded executor in the same package. A diagnostic `/status` poll may remain
read-only; it cannot become an alternate lease or veto current control state.

Keep compatibility paths only where an older server/client truly lacks the
control action or where direct/progressive playback has no HLS control session.
Select that adapter once from actual session support; a transient control
timeout must not activate two recovery systems. Do not delete compatibility
solely because all lab devices were upgraded. Its remaining purpose and
callers must be explicit at closeout.

**P2 regression contract:** extend existing lifecycle/intent tests with one injected race for
each competing operation family: pause during refill, stop during start,
restart before old release, seek storm, recipe change during reopen, and
control/media plane failures. Assert final intent, attached session, presented
position, exactly one action, and bounded cleanup—not just a changed enum.
Subtitle warming/retry must not pause otherwise playable video; burn-in is a
recipe change and follows P3. Do not rebuild already implemented M7 windows.

## 6. P3 — finish prepared handoff and remove empirical admission vetoes

### 6.1 Reuse the existing reservation and settlement transaction

In [http/hls.rs](../../crates/plurxd/src/http/hls.rs), extend
`process_preparation_candidate`, `stage_and_prime_prepared_successor`,
`reserve_preparation_settlement`, `settle_preparation_control`,
`settle_committed_preparation`, and `publish_committed_preparation`. Priming
already starts real successor production. Do not replace it with a new session
manager or regard a durable staged row as ready media.

```text
 reserve ──> prime ──> metadata/buffer ready
                          │
                          v
                LOCAL switch to successor
                          │ observe first qualifying presentation
                          v
             committed acknowledgement (frame timestamp + origin)
                          │ durable compare-and-set succeeds
                          v
                successor current on server
                          │ settle/release predecessor drain
                          v
                         done

 before local switch: abort successor and retain predecessor
 after local switch: reconcile settlement; local display is not durable owner
```

| Step | Buffer / ownership / message contract |
|---|---|
| Reserve | One outstanding successor per current intent; reserve actual existing admission resources; predecessor stays current and receives priority. |
| Prime | Produce/serve complete media at intended film boundary, including selected audio and required burned subtitles; client loads successor without consuming predecessor ownership. |
| Ready | Metadata, seek alignment and contiguous media covering the switch boundary are usable; readiness timeout is finite. File availability alone is insufficient. |
| Local switch / presentation | Matching client adapter makes the successor visible and observes its first qualifying presentation near the film boundary. The local predecessor may already be retired; the server pointer has not moved yet. |
| Commit acknowledgement | Client sends `committed` with observed `first_frame_unix_ms` and the offered `committed_media_origin_ms`. This acknowledgement initiates the existing fenced durable compare-and-set; replay returns the settled result. |
| Settle / drain | After durable commit, successor ownership/control becomes current and the existing finalization path releases predecessor drain, including `switched` where used. Keep presentation and durable settlement as separate measurements. |
| Abort / failure | Before local switch, discard successor and retain predecessor. After local switch, reconcile the durable outcome before recovery; never assume the predecessor is still attached or resurrect a fenced incarnation. |

This is the existing v1 order in the
[wire contract](M6-CLIENT-REPLACEMENT-CONTRACT.md) and
[Apple adapter](../../clients/apple/Sources/PreparedReplacement.swift): local
switch and first presentation precede the acknowledgement that starts durable
commit. There is no server “commit now” action. Do not fabricate a frame
timestamp to reverse this order or treat `buffer_ready` as durable commit.

The locally-switched, not-yet-settled window needs explicit handling:

- Lost response: replay/reconcile the same acknowledgement and action ID;
  retain usable successor media while settlement is unknown. Do not create
  another successor or infer success from a timed-out exchange.
- Rejected/expired commit: release the rejected successor and reconcile the
  current durable incarnation. Return to the predecessor only if it is still
  usable and authoritative; otherwise perform one normal replacement for the
  latest intent. Do not claim this fallback is seamless.
- Stop during switch/commit: halt local presentation immediately, settle the
  in-flight acknowledgement, then end the winning incarnation and release both
  local pipelines. A `committed` acknowledgement cannot be combined with
  `demand: end`; serialize through the existing bounded finalization path.
- Successful commit followed by loss: recover the authoritative successor,
  never the now-fenced predecessor. Deadlines still own abandoned cleanup.

### 6.2 Replace proof vetoes with bounded work and real outcomes

Remove admission restrictions listed in §2 **together with candidate
re-planning**. The current `candidate_request` in
[playback_control.rs](../../crates/plurxd/src/playback_control.rs) clones codec
and HDR conversion policy and keeps an existing transcode as a transcode even
for `Original`. Removing allowlists alone cannot implement the reverse-method
or grade transitions promised here.

In `process_preparation_candidate`, replace that construction limitation by
reusing `review_client_plan` and the ordinary capability-aware playback planner
in [http/hls.rs](../../crates/plurxd/src/http/hls.rs). Retain the source facts
and full client planning capabilities needed by ordinary create; the reduced
dynamic preparation capability is not a substitute. Resolve the newest
explicit intent over the retained current policy, preserving omitted fields.
Do not invent a grade/codec request from a selection field that cannot express
it. Use existing full-intent/create ingress for such changes, or explicitly
wire that existing intent to this planner rather than add unknown v1 fields.

Resolve audio, subtitle burn, delivery method, dynamic range and resolution
into one candidate, then derive its effective selection and grade from that
planned request. An `Original` request may return to copy/remux when source,
client and track requirements permit; it is not a promise to bypass a required
transcode. Stage the planned recipe, never a stale clone decorated with a new
quality label. Unsupported output returns a typed runtime failure; it does not
add the device to a persistent blacklist.

Preparation is speculative. Allow one successor, use existing admission limits
and preparation deadline, and schedule its I/O/production behind current
foreground refill. If the current player enters a genuine wait or foreground
resources cannot progress because of preparation, abort that successor once
for the current action. Do not repeatedly restage it on each control heartbeat.
Further automatic retries require a new action/intent; the viewer may retry a
change without a hidden session-wide disable. Low measured throughput can be
shown as advice; real contention is an operation outcome.

If preparation fails and a requested change still needs application, perform
one normal replacement using the existing single-player path. Preserve the
latest film position, pause intent and selected tracks. For an optional
automatic change, retaining the working current recipe is preferable to an
unrequested interruption. Record which happened. A temporary network failure
does not silently change the user's enablement preference.

Apple and Android should reuse native readiness/presentation callbacks.
On web, `requestVideoFrameCallback` provides the stronger observation when
available. When absent, attempt preparation and use an available presented-frame
counter/observation with a finite deadline. Readiness, playing events and
advancing media position help diagnose progress but must not fabricate
`first_frame_unix_ms`. If a qualifying observation cannot be obtained, end that
attempt through the normal fallback; do not disable future attempts or the
user's setting. Record observation precision and never claim frame-accurate
continuity from an inferred progress signal. Audio-only media retains its
appropriate existing presentation adapter.

**P3 acceptance:** exercise both directions of delivery-method change, compound
recipe change, unknown/low throughput, actual decoder/allocation failure,
foreground starvation during prime, rapid new intent, duplicate ready/commit,
lost commit response, stop during commit and preparation disabled mid-flight.
Assert the actual planned kind/codec/grade/tracks and resulting media, including
transcode-to-Original and a capability-permitted grade change; an admitted
decision alone is insufficient. Also assert no proof-based refusal, no double
commit, no leaked player/worker, and correct fallback. Measure first successor
presentation separately from ready and durable settlement. Include explicit
commit rejection after a successful local switch.

## 7. P4 — complete planned drain without promising zero-gap abrupt failover

Use [media_sessions.rs](../../crates/plurxd/src/media_sessions.rs) and its
existing route, epoch, release and drain machinery, together with the P3
transaction. `drain_window_closed` / `lease_tick_drained` are existing drain
foundations; they are not by themselves a prepared cross-node handoff.

For planned drain, add an internal preparation reason/target-owner context to
the existing candidate path. It must trigger a successor even if the effective
selection is unchanged; the current quality comparison's `Unchanged` result
cannot swallow relocation. Keep the public action and client adapter the same.
Stage on a non-draining owner that can access the actual source and acquire
the real production resources. A missing source/resource is a typed runtime
outcome, not an unqualified-device refusal.

Carry target owner and expected current epoch through reservation and commit;
perform routing/current-owner publication through the authoritative durable
transaction. Extend that transaction if target ownership is not represented
today. Do not bolt on a process-local redirect that bypasses its compare-and-set.
The old owner remains authoritative until commit. Afterwards it drains only
the predecessor and cannot overwrite the new route on rejoin.

Relocation follows the same v1 order: prime at the latest valid film boundary,
client readiness, local switch/first presentation, committed acknowledgement,
then durable owner/route publication. Staged media must therefore be reachable
on the target before it becomes authoritative for control. Preserve current
buffered media during preparation. A lost/rejected commit after local switch
uses §6.1 settlement recovery; it must not redirect blindly to the old owner.
If preparation expires before local switch, retain the predecessor for the
existing drain grace period; at its deadline use ordinary bounded recovery
and record the interruption. Never extend drain indefinitely for a vanished
client or treat a not-yet-ready successor as playable.

For abrupt owner loss there may be nothing available to prime. Continue using
already loaded media while existing cluster routing/lease takeover resolves
the owner. Recover once against the winning epoch and latest position/intent.
Reject stale replies, fail closed on conflicting ownership, and expose a
bounded error if source/cluster availability cannot recover. Do not claim
seamless failover after the available buffer is consumed.

**P4 acceptance:** deterministic coordinator tests for planned drain with a
ready/unready successor, crash before/after commit, delayed duplicate commit,
old-owner rejoin, control ingress different from media owner, and unavailable
target source. Assert one current epoch, no premature predecessor retirement,
buffer preserved while usable, and eventual release of every reserved worker.
Add one targeted native-client alternate-ingress observation to P5; a passing
relay unit test is not native playback evidence.

## 8. Verify the changed decisions without rebuilding the full test program

Reuse the C01–C21 source anchors in the
[coverage map](PLAYBACK-LIFECYCLE-COVERAGE.md) and existing fixtures. Add
parameterized cases to owning suites, not one new framework or executable
per lifecycle row. The IDs below name proposed regressions; they are not
claims that tests already exist or pass.

| Case | Smallest useful verification | Required assertion |
|---|---|---|
| T01 rolling coupled refill | Real flow actor + scripted client observations; fixed playhead; empty and 22-second loaded buffer | Credit/hold has a reachable exit; no heartbeat rearm; real bounds preserved; one resumed episode |
| T02 VOD contiguous refill | Materialization + request/read-window fixture; near gap and distant cached island | Near segment completes before prewarm; no duplicate materializer or eviction of active reader |
| T03 loaded native wait | Apple `PlayerResumeTests` plus the existing client adapter tests, with media already loaded and server held | Active intent survives; the strict runway boundary chooses at most one immediate play; a stale paused frame cannot pass; two fresh frames separated by 250ms close the episode; hold/repair cannot reset the 15s deadline |
| T04 delivery fault | Existing body/attempt fixture with delayed chunk and truncated EOF | Retry exact resource; partial body never counted complete; old attempt cannot commit |
| T05 lifecycle races | Existing per-client ownership/intent suites with controlled callback ordering | Stop/restart/seek/pause/recipe changes preserve latest intent and one attachment |
| T06 prepared lifecycle | Existing transaction plus adapter fixtures | Reserve/prime/ready/local switch/first presentation/commit/drain, rejected commit after local switch, and lost reply/stop are bounded and idempotent |
| T07 candidate planning and no software proof veto | Existing planner + server decision and settings/client tests | Unknown measurements and unmeasured axis combinations remain enabled; actual Original/grade/compound output matches the capability-aware plan; omitted intent survives; failure cleans up |
| T08 contention | Foreground refill + successor/prewarm fixture at real allocation limit | Foreground progresses; speculative attempt terminates; no leaked permit or restart loop |
| T09 owner transition | Existing cluster coordinator/relay fixtures | One epoch, correct route, bounded drain/failover and no stale-owner mutation |
| T10 two-engine and special paths | Routing fixture explicitly asserts returned presentation, then lifecycle cases | VOD and rolling both execute; direct/progressive/live/channel cases preserve their distinct semantics |

Write these focused regressions during development and compile their targets.
Record exact commit and compilation result; runtime result stays `not run`
pending the separate sweep. Do not add them to the fast-lane DAG or run them
as a second PR-approval system. Compile each changed platform before promotion.
The one fast-lane run follows the one review and author corrections; rerun it
only to verify changes addressing a failure or a changed head/base.

### 8.1 Keep physical observation small and informative

This is a prepared run card for the separate sweep, not work to execute during
Sol's implementation campaign. After an explicitly deployed build is available,
prioritize the reported Apple
TV and title. Record installed tvOS/client build, server build, actual engine,
and settings; the previous trace did not establish the installed client build.
Use existing lab routing controls to exercise immutable VOD and rolling HLS
deliberately; record which path actually ran. Do not infer it from a `.m3u8`
URL or add a normal-user engine chooser for this test.

Budget one 45–60 minute Apple TV continuous-play observation per engine, with
separate short scripted transitions for pause/resume, cold seek, restart,
quality/track change and stop during preparation. Observe at least one normal
producer hold/resume cycle when that path uses pacing; if none occurs, record
that limit instead of calling the condition covered. Exercise missing/ready
subtitles without turning a subtitle delay into a video stop.

Use one short Android and one web run to cover their changed native adapters,
plus a targeted alternate-ingress/drain run. These observations are a bounded
first pass, not an exhaustive device × codec × engine Cartesian product.
Broader decoder/fleet evidence remains in the manually dispatched sweep.
An unavailable device is an explicit evidence gap, not a hidden enablement or
merge veto. An observed product defect gets a normal corrective change; do
not call failing behavior accepted because compilation passed.

Report freeze count and duration, time to first/resumed presentation, number
of reopens, handoff interruption, and resource cleanup. Healthy normal-play
refill should have zero involuntary reopens. A short test without a freeze
does not establish fleet-wide reliability; an unchanged failing case cannot
be relabeled as success because its watchdog eventually recovered.

## 9. Integrate using the revised CI/CD process

The current user-supplied contributor rules and the September 10 correction in
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) govern execution. This
audit worktree was created from an older source snapshot; an implementer must
use the intended current base and the corrected workflow, not revive old
automatic qualification instructions found below historical status notes.
Do not transplant unrelated uncommitted CI changes from another checkout.

1. Establish the pinned Rust **1.97.1** compiler loop before changing Rust.
   Verify `rustc --version`. Use the documented source-only archive/cloud loop
   when needed; transfer no `.git` directory or repository credential. Keep
   the compiler target warm. Run formatting, check and Clippy locally/in that
   loop rather than using CI to discover compiler errors.
2. Integrate related package work on one temporary `effort/playback-lifecycle`
   branch, with `codex/` task branches based on its current head. Task PRs into
   the effort have no required adversarial review or fast lane. Keep P1–P2
   independently promotable; freeze task merges for its main promotion, then
   resume remaining packages from the promoted current base.
3. Before each main promotion, merge current `main` into the effort and compile
   that exact resulting source. Complete docs/index/ownership, corrective
   evidence and applicable Apple/Android build-counter paperwork in the change
   that owes it. Workspace releases align both counters and marketing versions.
4. Open the main-bound PR as **draft**. Request **exactly one adversarial agent
   review**. Address every finding and verify it as author. Do not request a
   second review, re-review, panel, or follow-up approval.
5. Mark ready after findings are addressed; that is what starts the lane, and
   since 2026-09-13 there is no `fast-lane` label to apply. Merge only after
   the **current head** has a green `Main promotion gate`. Returning a ready
   PR to draft cancels the run in flight.
6. Record merge and deployment separately. Main push/merge does not start the
   full suite or automatically publish a fleet image. Use the existing explicit
   deployment/release path; explicit release tags retain release processing.

The fast lane remains policy bookkeeping, deterministic static contracts, and
affected Rust/web/Apple/Android compilation. Do **not** attach unit, integration,
browser, simulator, emulator, recovery, playback, package or smoke suites to it.
Do **not** install pre-commit hooks. Runtime suites stay in the separate manual
sweep at Paul's chosen cadence. A sweep may repair tests/fixtures/timing/infra
without changing behavior; product changes become separate issues and follow
the ordinary main-bound process. This plan creates no new recurring schedule.

## 10. Close out with an honest result and a finite remainder

Use this checklist as project accounting, not as code that switches features
on. P5 updates the companion remainder/lifecycle map in place and the one
execution status page created in S01. Do not add competing milestone trackers.

- [x] P1: both engine mechanisms have bounded refill behavior; loaded-player
  waiting has an owned exit; joined traces distinguish each buffer boundary.
- [x] P2: one executor owns recovery and latest intent; removed polling/timers
  are listed by actual deleted symbols; any surviving adapter has a named need.
- [x] P3: prepared lifecycle handles compound changes and resource failure;
  empirical proof vetoes are removed; Developer advice never overrides choice.
- [x] P4: planned relocation and abrupt owner loss have compiled epoch/cleanup
  regressions; execution and continuity observations are explicitly deferred.
- [x] P5: exact source/build receipts, focused results and physical observations
  are recorded as pass/fail/not run; unresolved failures have specific owners
  and issues. R09 fleet breadth is evidence debt, not a new decoder rewrite.
- [x] Current references describe shipped hybrid behavior, settings and
  control ownership. Mark contradictory M6/M9 gating prose superseded. Keep
  the existing docs index and functionality-point ownership current.
- [x] R07 fallback retirement and R10 semantic/prewarm expansion remain explicit
  separate decisions. They do not keep this implementation effort open.

The work is complete when the four packages' behavior is implemented and
compiled, their boundary regressions are written, obsolete competing recovery
paths are removed, and the deferred runtime/device evidence is recorded honestly.
Main promotion still requires the reviewed current head's green fast lane. No
additional milestone is earned by adding another watchdog, multiplying
qualification documents, or requiring an exhaustive fleet campaign before
users can use finished behavior.

## 11. Sol's compiler commands and delivery receipt

Run commands from the root of your own clone/worktree. First verify the pin;
then establish a baseline before editing Rust. These compile commands do not
execute unit or playback tests:

```bash
git status --short                         # inspect only your clone
git rev-parse HEAD                         # record the source being compiled
rustup run 1.97.1 rustc --version           # verify the actual compiler
rustup run 1.97.1 cargo fmt --all -- --check
rustup run 1.97.1 cargo check -p plurxd --locked --all-targets
rustup run 1.97.1 cargo clippy -p plurxd --locked --all-targets -- -D warnings
```

After edits, format and repeat check/Clippy on affected crates. Include
`-p plurx-core` when its planner/store code changes; use the repository's wider
workspace compile requirements before main promotion. `--all-targets` checks
test code enabled by the selected features without running it; it does not
activate feature-gated fixtures. P4 changes to replicated store contracts or
separate-process cluster fixtures additionally require these compile-only checks:

```bash
rustup run 1.97.1 cargo check -p plurx-core --locked --all-targets \
  --features hiqlite-contract-tests
rustup run 1.97.1 cargo check -p plurxd --locked --all-targets \
  --features cluster-integration-tests
```

Use the corresponding feature selection in Clippy when those fixtures change.
The features enable compilation here, not runtime execution or feature rollout.
Record which feature-specific command compiled each changed fixture; do not
credit a default-feature build as coverage for a skipped target.

If the clone host cannot use the pin, follow the
[source-only compile loop](../ci/AGENT-COMPILE-LOOP.md). Archive committed
source, retain a warm target directory, and verify again on the final merged
base. Do not copy credentials or `.git` to the compiler host.

Compile affected mobile clients through existing targets:

```bash
make apple-build                          # iOS + tvOS compile, no tests
make android                              # pinned Android image, debug compile
```

Use the [Makefile](../../Makefile) and current CI definitions for environment
setup instead of inventing SDK/toolchain versions. Apple app compilation alone
does not type-check new XCTest code: for changed tests use the existing
scheme's `build-for-testing` action without executing tests, following the
`apple-test` target's build step only. For Android, invoke the pinned build
environment's `:app:compileDebugUnitTestKotlin` when test sources change;
do not run `testDebugUnitTest`, `lintDebug`, or connected tests in this campaign.
Do not call broad `make check`, `make apple-test`, `make android-test`, or
`make web-check` as a convenience compilation command.

For web, check syntax using the existing fast web syntax gate's command and
write the control/ownership regressions without executing their runtime suite.
The main-bound fast lane itself owns catalog/history/docs/static checks and
affected compilation; its command list is not permission to run the full graph.
Build counters and corrective evidence belong in the implementation commits,
as §9 specifies. Do not enable pre-commit hooks.

At each main promotion, provide a compact receipt on the execution status page:

| Field | Required content |
|---|---|
| Scope | Completed P/S task IDs and behavior changed; remaining task |
| Source | Exact head/base SHA, branch and PR link |
| Ownership | Existing executor extended; obsolete timers/branches actually deleted |
| Compilation | Compiler/platform version, exact command, result, source SHA |
| Regressions | Tests added/changed and asserted behavior; `written; not run` until sweep |
| Review | One adversarial review for this main-bound PR; findings and author's dispositions |
| Fast lane | Current-head `Main promotion gate` result and run link |
| Runtime evidence | Separate sweep/device results if supplied; otherwise `not run` |
| Decisions | Assumptions taken without waiting, unresolved defect/issue, next owner |
| Cleanup | Temporary outputs/processes removed; reusable caches retained deliberately |

Continue from the first promotion into the remaining packages without waiting
for Paul to restate the assignment. Stop only for an actual authority/access
blocker that cannot be worked around within the requested scope; state the
blocker precisely and continue independent work. This handoff gives no reason
to deploy without the builder task's deployment authority, spend an unbounded
session benchmarking, or leave a feature inaccessible pending measurements.
