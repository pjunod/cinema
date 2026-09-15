# Web recovery — Sol implementation handoff

**Status:** Wave 1 implemented · **Written:** 2026-09-15
· **Executes:** W1/W2 in [the effort plan](STREAMING-RELIABILITY-IMPLEMENTATION.md)
· **Tested base:** fea5d245131095f26f60d67a69e6570aa3626125

Read the effort plan and repository AGENTS.md first. Work in a separate
worktree and codex branch from current effort/streaming-reliability. The
coordinator owns server control and rolling playlist behavior. You own web
stall evidence, action selection and completion. Keep client transport and
server producer ownership distinct; no new autonomous watchdog or ABR loop.

## 1. Deliverable — a presentation stall does not silently reduce quality

At the incident baseline, persistentWait infers decoder failed from a loaded
buffer and stationary playback. stallRecoveryAction then sends every Auto
remux through transcode. Remove that inference and preserve the selected
recipe when there is no independent reason to change it. Keep recovery of
real decoder errors and existing evidence-based adaptation working.

## 2. Owned interfaces — current code anchors

| File | Symbols / responsibility |
|---|---|
| [index.html](../../crates/plurxd/src/web/index.html) | beginWait, persistentWait, playbackControlSnapshot, askPlaybackControl, finishStallRecovery, seekTo; attachment owner |
| [playback-policy.js](../../crates/plurxd/src/web/playback-policy.js) | stallRecoveryAction, stallRecoveryTargetHeight; existing quality and decode policy |
| [playback-control.js](../../crates/plurxd/src/web/playback-control.js) | Reporter; bounded current snapshots and sequence/epoch fencing |
| [web-policy.test.js](../../tests/playback/web-policy.test.js) | Routing and recovery policy tests |
| [web-control.test.js](../../tests/playback/web-control.test.js) | Protocol and shipped-player integration regressions |

Existing policy signatures, to re-verify before editing:

~~~javascript
stallRecoveryAction({ method, quality = "auto", alreadyRecovered = false })
stallRecoveryTargetHeight({ method, quality, kind, ladder, currentHeight,
  estimateKbps, recentEstimateKbps, recentEstimateAtMs, nowMs, defaults })
~~~

Make missing attribution conservative in the policy API: unknown evidence
cannot authorize a quality reduction. If adding a local argument, update
all call sites and test fixtures; do not add an unnegotiated wire field.
Current wire decoder states remain unknown/ready/starved/failed. The coordinator
changes the server loaded-stall predicate so it does not require a false
failed state. Your client must also remain bounded against the old server.

## 3. Steps — one complete Wave 1 PR

### 3.1 Separate observation from classification

Use actual media error information and existing qualifying lost-frame policy
for decoder attribution. A timer plus loaded runway means presentation wait,
with unknown cause. Empty loaded runway establishes supply pressure, not
whether the disk, producer or network is responsible. Keep unsupported frame
observation explicit. Reuse the existing progress observer and intent fences.

Avoid latched rendering overrides hiding a stationary player. Source each
state transition from the current attachment and actual observation; an old
playing event cannot complete a newer episode. Preserve the first incident
snapshot separately from later buffer updates so logs do not rewrite the
cause using post-recovery state.

### 3.2 Preserve the selected recipe through bounded recovery

Use the existing attachment recovery owner and budget. Snapshot movie position,
copy/encode mode, resolution, HDR/DV output, audio/subtitle choices and offset.
For an unattributed presentation/delivery fault, make the existing native
reevaluation then a bounded same-recipe repair as appropriate. Do not call
startTranscodeFallback solely because the old method is remux and Auto.

Trace the outgoing session request: a legacy reopen_reason of stall can cause
the server to lower Auto one rung even if the web label says reconnect.
Preserve lifecycle/supersession identity while avoiding that downgrade ticket
for same-recipe presentation repair. Keep explicit user quality intent and
selected tracks. If the existing client API cannot express the exact repair,
send the coordinator a precise request example and required server seam;
continue policy tests while that contract is resolved.

No new switch or Developer control is needed for this correction. Do not write
learned decoder limitations or bandwidth priors from an unknown stall. Keep
genuine measured network/decode adaptation; add positive tests proving it.

### 3.3 Settle one episode using presentation progress

Retain generation/epoch/intent fences and the existing absolute episode bound.
The first patch keeps the 20-second outer bound; do not repeatedly defer to
passive control responses without a pending action that could make progress.
Do not reduce delay by sprinkling shorter timers through multiple callbacks.

A fulfilled play promise, accepted control reply, readyState or increased
buffer does not by itself establish recovery. Finish on new presentation
progress at the intended position, using the established fallback when frame
callbacks are unavailable. Pause/seek/close cancels the episode. Failed or
cancelled preparations leave any usable predecessor intact. One episode may
emit multiple observations but only one replacement action.

Recovery messages must describe the observed fault accurately. In particular,
do not show buffer ran dry for an episode with substantial loaded media.
Keep production implementation details out of viewer-facing text. Joined
telemetry can use existing bounded log fields; never log bearer URLs.

### 3.4 Focused proof, commit and PR

Use the shipped functions through existing test extraction/harness mechanisms,
not a parallel reimplementation of their policy. Minimum missing cases:

| Case | Required result |
|---|---|
| 18310 ms loaded, no frame progress, no media error | Unknown presentation fault; no synthesized decoder failure |
| Same case, Auto remux | One same-recipe repair; no transcode/rung-drop ticket |
| Actual decoder error / qualifying lost-frame pressure | Compatible recovery remains reachable |
| Control none/hold/timeout/429 | Bounded one-owner recovery; no duplicate mutation |
| Play promise resolves, frames stationary | Episode remains unresolved |
| Frames resume at intended position | Episode completes once |
| Pause, seek, close or old callback after replacement | Stale work cannot act |
| Supersession request and selected tracks | Ownership and quality snapshot preserved |
| Frame callbacks unavailable | Explicit supported progress fallback; no fabricated decode facts |

~~~bash
node --test tests/playback/web-policy.test.js tests/playback/web-control.test.js
~~~

Run the affected embedded-JavaScript static check from the current Makefile.
Do not repeatedly run the full browser suite or unrelated Rust tests. Record
commands and results against the candidate commit. Do not claim native Safari
acceptance from fake video objects. A physical regression run is coordinated
later; do not manipulate the viewer's active movie.

Commit normally and update this handoff's result section in the same commit.
Push codex/streaming-web-recovery (or a collision-free codex branch) and open a
PR into effort/streaming-reliability. Do not merge yourself or request an
adversarial agent review. The coordinator owns the single final main-PR review.
Send needed validation catalog and live-reference edits to the coordinator;
complete them before task merge without independently editing docs/README.md.

## 4. Wave 2 — port behavior through native owners

Begin only after C1/W1 are integrated and the coordinator transfers the native
files to you. Discover the existing Apple and Android recovery owners from
[the lifecycle implementation](../playback-control/PLAYBACK-LIFECYCLE-IMPLEMENTATION.md)
and current source. Audit actual callbacks before changing them: a local
observer is not automatically a second action owner.

Port the same invariants rather than the JavaScript algorithm. Preserve
native decoder errors, film-time mapping, pause intent, audio/subtitles,
prepared successor acknowledgement, cancellation and release. A same-delivery
repair must not carry a reason that normalizes the requested quality down.
Reuse existing native recovery budgets and progress evidence; unsupported
observations stay unknown.

Compile affected Apple/Android targets, run the smallest native tests for
these transitions, and follow existing build-counter/release-note conventions.
The coordinator handles shared documentation/index merge ordering. One PR
for native parity is preferred if both platforms are ready; split only if a
specific platform dependency prevents reviewable integration.

## 5. Non-goals and completion

No player rewrite, new protocol version, decoder certification registry,
empirical feature veto, new CI workflow, general UI redesign or timer sweep.
Do not remove existing bounds merely to reduce the number of timers. Do not
turn capability or hardware-health measurements into an activation gate.

Return branch/commit/PR, actual test commands/results, changed files, known
interop limits, and the fixture proving no quality loss. Stop after Wave 1
is ready and report; the coordinator dispatches Wave 2 after integration.

## 6. Result — implementation task fills this in

- Tested base: `fea5d245131095f26f60d67a69e6570aa3626125`. Implementation:
  `c29b0ac21931ad6ca1026af4190e5816934c9177`; focused integration amendment:
  `019f17586`.
- Observation/action policy change: loaded runway with no frame progress and no
  media error is a presentation stall with unknown cause. Its one bounded repair
  preserves the current delivery recipe. A generic network disconnect also
  reconnects the same recipe: failed transfer is not proof of inadequate
  capacity. Typed decoder errors and the existing measured supply controller
  retain their compatible adaptation paths.
- Identity and completion: the first episode freezes film position, delivery,
  resolution, dynamic range, tracks, offset, attachment and control generations.
  A play event alone does not settle it; new presented frames, or film-clock
  progress when frame counters are unavailable, settle it once. Viewer intent
  supersedes stale recovery.
- Control and lifecycle binding: one completed native reevaluation plus a
  passive `none`, no verdict, or an old-server hold leaves no work pending and
  reaches the one local repair without another deferral timer. Same-recipe
  repair omits the legacy `previous_session_id` / `reopen_reason` pair because
  that pair requests a server rung reduction. It remains bound by the stable
  per-page `playback_id` predecessor CAS, the accepted `control_sequence`, and
  one `request_id` across any create retry; no new wire field is required.
- Focused proof: `node --test tests/playback/web-policy.test.js
  tests/playback/web-control.test.js` passed. `make web-check` reached and passed
  both changed playback suites, then stopped at the unchanged
  `tests/web/library-channels.test.js` harness because its baseline `document`
  mock lacks `addEventListener`; the failing extracted source is outside this
  task's diff. The settings-section test for the removed HLS switch passes.
- PR: [Forgejo #324](http://192.168.4.7:3000/noirr/plurx/pulls/324),
  `codex/streaming-web-recovery` into `effort/streaming-reliability`.
- Wave 2 native parity: Apple and Android timer-only stalls now publish unknown
  decoder cause and issue one bounded same-recipe replacement without the
  legacy quality-reduction ticket. Apple no longer enters the HDR/codec ladder
  from a silent clock alone; Android no longer turns its one native
  reevaluation into a second passive deadline. Actual AVPlayer item failures
  and Media3 player errors retain their established error-specific ladders.
- Native proof on Xcode 27.0 and the installed Android SDK: the two selected
  Apple regression methods passed; the four selected Android recovery classes
  passed 89 tests; generic iOS/tvOS simulator builds and Android
  `:app:assembleDebug` passed. Android test compilation also needed the
  coordinator-approved missing `decodeFromJsonElement` import in the existing
  DVR fixture; no DVR behavior changed.
- Wave 2 PR: [Forgejo #328](http://192.168.4.7:3000/noirr/plurx/pulls/328),
  `codex/streaming-native-parity` into `effort/streaming-reliability`.
- Remaining physical observation: Safari playback has not been claimed from
  fake video objects. The coordinator owns the later physical regression run.

## CI and review rule — current pipeline correction

Follow the September 13 correction at the top of DEVELOPMENT_PIPELINE.md.
Task PRs into the effort allocate no automatic jobs. Run affected compilation,
lint and focused proof locally; record them in the PR. Do not dispatch full CI
or repeat successful suites. Only the final main-bound PR receives the one
adversarial agent review, then the current-candidate main fast lane after it
is marked ready. The coordinator owns promotion. Older automatic effort-gate
and full-qualification instructions are superseded by that correction.
