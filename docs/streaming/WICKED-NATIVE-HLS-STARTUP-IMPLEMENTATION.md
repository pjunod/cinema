# Wicked startup repair — an implementation contract for native HLS and source preparation

**Status:** ready for implementation; no product patch or deployment claimed.
**Written:** 2026-09-17. **Executes:** Fable's “approve with changes” review
of the [Wicked RCA](WICKED-NATIVE-HLS-STARTUP-RCA.md).
**Verified source base:** `363a22e28aa53094d899a9ad3c812241a6243548`.
**Audience:** the Sol or Opus agent implementing and validating the repair.

Build the work orders below, reconcile them against the current intended
base, and deliver reviewable changes with actual verification evidence.
This is an execution contract, not a request for another plan. Read the
[startup recovery status](WEB-HLS-STARTUP-RECOVERY-STATUS.md),
[playback contract](../PLAYBACK.md),
[surface contract](../clients/PLAYBACK-SURFACE-CONTRACT.md), and
[development pipeline](../DEVELOPMENT_PIPELINE.md) first. Preserve the
existing preparation, startup, control and fallback owners. If a required
behavior cannot fit those owners, identify the concrete conflict before
inventing another controller.

## 1. Outcome — repair two independent failures

**Track N restores the incident's intended playback.** Native Safari HLS
must wait for the current master and selected child media playlist to become
ready before setting `video.src`. A retryable publication delay retains the
same copy/Dolby Vision session. It must not become a codec rejection or an
automatic downgrade. Once attached, one bounded native reload and the
existing compatible fallback handle a persistent native rejection with
truthful attribution.

**Track S fixes independent encoded-source preparation.** Transcode and
subtitle-burn preparation must distinguish inability to inspect a source
from proof that it changed. Slow preparation is one bounded, joinable job;
an HTTP retry under the same request identity waits for that job instead of
opening the source and launching FFprobe again. Web, Apple and Android must
agree on the new response and retry contract.

`prepare_vod_encoding` returns `Ok(None)` for copy without subtitle burn.
Wicked's intended recipe takes that early return. Track N alone would have
kept the successful recipe; Track S is not a prerequisite for that rescue.
It was exposed only after the wrong transcode fallback and also affects
other clients starting encoded or burned presentations.

**Non-goals:** change media or NAS settings; force transcode or lower quality;
disable Dolby Vision; weaken source identity, authorization, serving fences
or lease checks; enlarge the server's five-second publication deadline;
change established-playback recovery policy; create a second request registry;
or claim that disk spin-up caused this incident. The storage trigger remains
unproved. Do not restart or deploy to nynuc while implementing this contract.

## 2. Verified starting points — inspect these seams before editing

The five incident files in the RCA are byte-identical between deployed
`c9e4edf451e12247a7aa4188903e5ba36888e7e9` and the verified main base above.
Recheck symbols and current changes before implementation; line numbers in
the RCA are historical evidence locators.

| Seam | Source | Existing behavior that matters |
|---|---|---|
| Native attachment | [index.html](../../crates/plurxd/src/web/index.html), `attachHls` | Native branch assigns the source immediately; hls.js alone creates a startup episode |
| Episode identity | Same file, `hlsStartupCurrent` | `player.hls===episode.hls` becomes the meaningless `null===null` on native |
| Native error | Same file, `wirePlayerMedia` | Reports `failed` early and immediately transcodes code 4 |
| Sticky render override | Same file, `notifyPlaybackControl`, `attachSession` | `failed` persists until `attachSession` clears it; same-session reload does not currently do so |
| Native timeline | Same file, `refreshSegTimes`, `parseSegTimes` | Master has no `EXTINF`; subtitle-bearing master sessions never populate `segTimes` |
| Create retry | Same file, `beginPlaybackPreparation`, `openSessionRetryingNotYet` | Web create owner is 20 s; one request ID spans the sequence; exhaustion stops unconditionally |
| Web policy | [playback-policy.js](../../crates/plurxd/src/web/playback-policy.js) | 40 s cold / 20 s seek attachment, 16 application manifest sends; create row is start-only |
| Source fence | [fragment_index_cluster.rs](../../crates/plurxd/src/fragment_index_cluster.rs), `open_source_fence` | Open/stat and identity failures currently share a string error channel |
| Source inspection | [ffmpeg.rs](../../crates/plurxd/src/ffmpeg.rs), `held_source_probe_json` | Uses the five-second local engine-probe timeout |
| Encoded recipe | [transcode.rs](../../crates/plurxd/src/transcode.rs), `prepare_vod_encoding` | Both source-fence errors and probe errors become rescan refusals |
| Idempotency | Same file, `RequestState`, `RequestClaim`, `claim_request` | InFlight waits; Ready recovers; dropping a failed claim removes the reservation |
| HTTP mapping | [http/hls.rs](../../crates/plurxd/src/http/hls.rs), `session_start_error` | Unknown refusal codes become internal errors |

Native contract restatements are part of Track S, not optional follow-up:

- [Shared fixture](../../tests/playback/playback-surface-contract.json).
- [Android policy](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackPolicy.kt),
  [Android surface](../../clients/android/app/src/main/java/tv/plurx/app/player/PlaybackSurface.kt),
  and [CreateRetryTest](../../clients/android/app/src/test/java/tv/plurx/app/player/CreateRetryTest.kt).
- [Apple surface model](../../clients/apple/Sources/PlaybackSurfaceModel.swift)
  and [Apple controller](../../clients/apple/Sources/PlayerController.swift).

At the verified base the server has no behavior matching `RenderState::Failed`
that would terminate this producer. The early report did not cause its
retirement. The sticky client override still needs correction. Also,
`rememberDecodeLimit` is called from margin rescue; code 4 does not currently
persist a decode limit. Its actual exposure is a false `causeIsDecode=true`
report. Preserve that non-learning behavior while fixing attribution.

## 3. Decisions — implement these rather than reopen the review

1. **Readiness precedes native source assignment.** Fetch the master and
   selected child media playlist successfully before assigning the source.
   Pre-source readiness failures never enter the video-element error ladder.
2. **Use explicit native episode identity.** Carry transport, player,
   attachment, intent generation and media-execution generation. HLS-object
   equality alone is never native identity.
3. **One same-session native reload precedes compatible fallback.** Reload
   does not create a session, renew a deadline, reset a fallback credit or
   leave `controlRenderOverride='failed'` behind.
4. **Ambiguous native rejection may still use the one existing fallback.**
   After ready resources and the one reload, persistent code 4 may trigger
   compatible fallback even without decoder-progress signals. Attribute it
   as “unverified, refused before decode”; do not learn a decoder limit.
5. **Keep finite phase budgets.** Web creation remains 20 s, followed by a
   40 s cold or 20 s seek attachment budget. Worst case is 60 s cold / 40 s
   seek for one create-plus-attachment cycle. Pause and reload renew neither
   clock. Existing compatible fallback is a separately bounded recovery
   action; report its elapsed time rather than disguising it as this cycle.
6. **Choose review option 2(b): joinable preparation.** A five-second HTTP
   wait may return “not yet” while the same preparation job continues up to
   its absolute server deadline. Discard the old proposal to kill a probe
   after 15 s and start it again with four seconds left in the web owner.
7. **Keep automatic create retry start-only.** Do not widen the existing
   `create_503_not_yet` row to change context. A failed replacement preserves
   a valid predecessor and offers Try again. This avoids exposing the
   unconditional cold-start `exhaust` closure to a visible predecessor.
8. **Ship response semantics across all three clients together.** The server
   must not emit a new retry code while either native client only knows how
   to classify it as a generic stopped refusal.

## 4. Track N — native readiness under the existing startup owner

### 4.1 Allocate the episode before starting native network work

Refactor transport-independent startup allocation out of the hls.js branch.
The preparation owner ends at the existing `attachSession` handoff; the
startup episode then owns readiness through first presentation. Keep that
boundary explicit even though the native element has no source yet.

Minimum logical fields; these names are illustrative except existing fields:

```text
transport: hlsjs | native
player / attachment / mediaAttachment / intentGeneration
executionGeneration: changes on a native source reset
state: active | paused | presenting | exhausted | cancelled
phase: master | media_playlist | attached | classify_error | reload
startedAt / deadlineMs: never refreshed by retry, pause or reload
dispatches / requestOrdinal / latestFailure
nativeReloadUsed / errorClassificationInFlight
owned abort controllers and timer handles
```

Do not create a parallel authoritative state machine in the presenter.
Refactor `hlsStartupCurrent`, completion, exhaustion, abort, pause/resume,
failure clearing and teardown through transport-specific operations. Keep
the hls.js instance check on its own branch. Native currentness must check
the episode object and generations even when both HLS fields are null.

### 4.2 Fetch master and media playlist before `src`

The native readiness adapter performs these operations sequentially:

1. Fetch the exact authorized session playlist URL with a bounded body and
   the episode's abort signal. Classify status and bounded typed JSON.
2. If it is a master, select the same video rendition the native attachment
   will use and resolve its child URI against the master's actual response
   URL. Fetch the child and require a usable media playlist. Direct media
   playlist inputs need no artificial second master request.
3. Feed that media playlist's `EXTINF` entries into existing `parseSegTimes`.
   A successful master alone is not segment-time evidence.
4. Only while the episode remains current, active and within budget, call
   the existing source setter/position/transport-intent helpers with the
   original native playback URL. Preserve tracks and source-timeline offset.

Preserve credentials in both playlist URLs and child/init URLs. Relative
resolution does not automatically preserve a parent's query: apply the
existing session authorization contract explicitly, including the embedded
session capability and any required token query. Do not strip, re-log or
forward credentials to an unrelated origin. Keep AirPlay's existing ability
to load with the session URL; do not replace it with a browser-only blob URL.

HTTP 200 is necessary but not sufficient: reject malformed playlists and
unusable child references accurately. Do not classify their parse failure
as decoder rejection. Reuse a bounded parser/helper where available; this
is not a new general HLS implementation. Verify the current master's video
variant assumptions; never select an audio or subtitle rendition by taking
the first arbitrary non-comment line.

There is at most one application readiness request in flight per episode.
Coalesce or suppress `refreshSegTimes` while readiness owns those fetches;
after readiness, point its timeline refresh at the child media playlist.
This restores missing boundary timing for native sessions with subtitle
renditions. It does not replace the video's actual buffered-range accounting.

### 4.3 Retry temporary publication failures within the existing limits

Keep `response_publication_timeout` and other existing retryable readiness
codes retryable. Typed terminal responses and authentication errors go to
their existing terminal/handoff owner even if transported on HTTP 503.

Use 1 s, 2 s, then at most 4 s backoff under the fixed attachment deadline
and existing 16 application manifest-dispatch ceiling. Count master, child
and diagnostic playlist dispatches together. A native diagnostic init fetch
is also bounded and must not introduce an unbounded secondary retry loop.
If it uses a separate counter, cap it at one per error classification and
share the same deadline. Safari's own opaque HTTP sends cannot be counted
by this application limit; report the distinction honestly.

Each dispatch is bounded by the smaller of its normal request bound and
remaining episode time; bound body reads and abort-ignoring transports too.
No dispatch while paused. Resume checks the original deadline immediately.
Seek, Close, replacement and terminal authority loss abort owned work.
Every completion rechecks currentness and request ordinal before mutating
source, failure evidence, surface or control state.

Before `src`, do not report a video decode error or consume native reload /
compatible fallback credits. Maintain valid existing preparation demand.
The startup owner alone exhausts readiness; verify that the existing stall
watchdog cannot start a competing diagnosis before readiness completes.

### 4.4 Classify post-readiness code 4 exactly once

Ignore errors from stale media executions and intentional resets. For a
current native error before first presentation, allow one classification
operation in flight. Capture the episode and execution generation before
awaiting network work; repeated element events cannot spend extra credits.

Obtain bounded current evidence for the master, child playlist and its init
resource (`EXT-X-MAP`). Use a ranged init GET under the same session
credential. Accept a valid 206 or a bounded full 200 response; enforce byte
and time bounds and validate that media bytes, not JSON/HTML, were returned.
Handle an absent init declaration as an unsupported diagnostic shape, not
as silent successful proof. Actual sample decode is not proved by init alone.

| Evidence / state | Required action |
|---|---|
| Typed temporary delivery failure | Return to readiness within the same episode; no transcode claim |
| Terminal failure or lost authority | Existing terminal/handoff path; no native retry behind it |
| Ready resources; native reload unused | Clear transient execution failure, spend one reload credit, reload same session |
| Ready resources; persistent code 4 after reload; decoder-facing signal exists | One existing compatible fallback; label native rejection after media readiness |
| Ready resources; persistent code 4 after reload; no decoder-facing signal | One existing compatible fallback; label unverified refusal before decode |
| Deadline / dispatch limit spent without ready delivery | Stop with readiness/delivery explanation and Retry/Close |
| Compatible fallback already spent | Existing stopped outcome and manual actions; no new rescue ladder |

Decoder-facing signals are `loadedmetadata` in this execution,
`readyState >= 1`, `videoWidth > 0`, or
`getVideoPlaybackQuality().totalVideoFrames > 0` when available. Snapshot
and reset execution-local evidence on reload so the old source cannot
supply the new execution's evidence. These signals strengthen attribution;
they do not independently identify a codec limit. Keep automatic code-4
fallback non-learning in both branches.

Move `notifyPlaybackControl('failed')` into actual stop / compatible-fallback
branches, after classification. Before same-session reload explicitly clear
the stale `controlRenderOverride` through the existing state owner and emit
a fresh valid control observation. Do not call `attachSession` just to clear
one field or allocate a new server session as a side effect. Assert subsequent
control reports no longer carry the old `failed` override.

## 5. Track S — one source-preparation job survives response retries

### 5.1 Typed source errors cover open, stat, identity and probe

Introduce typed errors at both `open_source_fence` and held-source probe
boundaries, or add a typed internal variant with compatibility wrappers for
unaffected callers. Preserve errno/category and phase internally. Do not
parse English strings to distinguish mismatch from I/O.

| Condition | Required semantic result |
|---|---|
| Identity/version or normalized completed probe demonstrably differs | Existing HTTP 409 `vod_source_rescan_required` |
| HTTP waiter reaches 5 s while its bounded source job is still running | HTTP 503 `source_probe_timeout`, explicitly “still preparing” |
| Source job reaches its absolute deadline | Terminal typed `source_preparation_expired`; no same-ID fresh job |
| Open/stat/read I/O error, including EIO | Accurate typed `source_unavailable`; never a rescan claim |
| Missing or inaccessible source | Accurate unavailable/permission category; no automatic not-yet retry |
| FFprobe execution failure or invalid output | Accurate preparation/probe failure; never evidence of mismatch |
| Cancellation / superseded intent / lost authority | Existing cancellation or authority path; no publication |

Use the existing `{code, message}` envelope and explicit
`session_start_error` branches for every emitted code. Proposed non-mismatch
terminal categories may share a bounded `source_preparation_failed` code
with safe phase detail; the implementation PR must list the final code map
and pin it in all affected client classifications. The required stable
retry code for this work is `source_probe_timeout`. For terminal expiry use
HTTP 503 `source_preparation_expired`, listed as terminal even on 503.

The timeout response means the response waiter expired, not that FFprobe
was killed. Example:

```json
{
  "code": "source_probe_timeout",
  "message": "The source is still being prepared; retry this request shortly."
}
```

Never emit that response after the job has actually expired or failed.
Keep engine-introspection probes at five seconds. Source open/probe work
shares the job's remaining absolute budget; do not chain independent
20-second caps for each phase or keep the source FFprobe's old five-second
kill timer while claiming it continues behind the response.

### 5.2 Separate waiter lifetime from job lifetime in the existing registry

Use the existing authenticated request key and intent fingerprint. One
manager-owned preparation job holds the claim across HTTP responses.
Identical requests join it; a different fingerprint under the key still
conflicts. Do not create a second keyed registry that can disagree with
`RequestState`.

The current borrowed `RequestClaim<'a>` belongs to the request future and
its Drop removes `InFlight`. Returning 503 from that owner cannot provide
joinability. Give the claim a real manager-owned lifetime, with generation
checking on complete/remove and a supervised task handle. Do not use a
leaked reference or `mem::forget` to defeat Drop.

Required logical states; extend the existing representation as needed:

```text
Reserved/InFlight(source preparation, generation, fixed deadline)
  ├─ 5 s response wait ends ─▶ waiter returns 503; SAME job remains InFlight
  ├─ source verified ────────▶ Prepared(held source + frozen recipe)
  ├─ actual failure ────────▶ Failed(typed result, generation)
  └─ deadline/cancel ───────▶ Expired/Cancelled; cleanup owner retains resources

Prepared + current authorized create waiter
  └─ claim finishing exactly once ─▶ existing fenced session creation ─▶ Ready(session)

same request ID:
  InFlight → wait/join; Prepared → finish/join; Ready → recover same session
  Failed/Expired → same terminal result, never spawn a replacement job
```

`Prepared` may be an owned result in an extended `InFlight` entry rather
than a new enum variant. Its invariant is mandatory: the preparation task
must not supersede a predecessor or publish a session after the final
request waiter has disappeared. Only an active, authorized, current waiter
may cross the existing session-creation/supersession boundary. Exactly one
waiter takes that step; others await its result. Reuse existing admission,
normalization, target-height and serving-authority checks there.

If session creation has already started, preserve its existing lifecycle
and cleanup owner; do not move publication into an unsupervised background
task. A late prepared result after cancellation is disposed, never applied.

### 5.3 Fixed clocks, cleanup and capacity are part of correctness

Set the source job's proposed absolute lifetime to **20 seconds from its
first reservation**; join/retry does not renew it. Each response wait is at
most **5 seconds**, capped by remaining job time. Web additionally aborts
its create sequence at its existing 20-second preparation deadline. Native
clients retain their existing finite create deadlines; the server job's
20-second lifetime still applies and expiry remains terminal for the same ID.

Example of actual recovery, unlike the rejected 15-second retry design:

| Time from first reservation | Event |
|---|---|
| 0 s | One open/probe job starts |
| 5 s | First HTTP wait returns not-yet; job keeps running |
| 6 s | Same-ID retry joins the original job |
| 8 s | Probe succeeds; waiting create can finish using its retained result |
| 20 s | Any still-pending job expires regardless of retry count |

HTTP completion or one disconnected waiter does not cancel a shared source
job: the gaps between retry rungs are expected. Close aborts client waits;
server work may remain bounded until the original 20-second expiry if no
existing explicit cancellation signal reaches it. Say that in telemetry
and tests rather than promising immediate server cancellation from an HTTP
disconnect. Shutdown, explicit owner cancellation, confirmed supersession
and authority loss cancel through the supervised job owner.

Bound pending jobs and retained results with the existing creation admission
limits, or add a bounded preparation permit if those limits are acquired
only later. The implementation must report the chosen numeric limit and
prove it is acquired **before** open/stat/probe work. No unbounded waiting
task or queue is permitted. Reuse an existing capacity refusal for overflow.
Hold the permit through cancellation cleanup, not just until a 503 is sent.

Keep failed/expired result tombstones for the maximum existing client
create-retry horizon (at least the current 60 seconds from reservation),
under bounded admission. A stale completion cannot remove a newer
generation. Explicit Try again gets a new request identity; automatic
same-ID retries cannot resurrect an expired job or bypass capacity.

Tokio filesystem work may run on a blocking thread; cancelling its await
does not stop the underlying open. Linux hard-NFS I/O may also be
uninterruptible even after a process is signalled. Bound the logical deadline
and admission separately from physical reaping: retain a cleanup owner and
permit until the OS operation ends, report overdue cleanup, and prevent
success publication. Do not falsely assert immediate reaping under a hung
hard mount or release capacity while hidden work still runs. Normal and
cooperatively blocked fixture children must be killed and reaped promptly.

### 5.4 Three-client retry contract stays start-only

Add `source_probe_timeout` to the shared fixture's
`create_503_not_yet.codes` and all web/Android/Apple restatements. Keep that
row `context: start`. Ensure native create coordinators also preserve the
same request ID and distinguish a wait timeout from terminal expiry.

Update the terminal code sets and overlays for `source_preparation_expired`
and final non-mismatch failure codes. Retrying every 503 is incorrect.
Keep `vod_source_rescan_required` terminal. Add fixture/drift tests rather
than making one client accept an unspecified wildcard error family.

In a change context, a source timeout must produce `change_failed` and
preserve any valid predecessor; it does not enter the start-only automatic
create ladder. Audit all three clients' exhaustion/cancellation paths.
Pin that policy with a test so a later row widening cannot call web's
unconditional `stopPlayerForExhaustion` on a visible predecessor. The
incidental bad fallback in the original incident is removed by Track N;
Track S need not invent automatic retries for that replacement context.

## 6. Work orders — concrete outputs and exit checks

Use one temporary effort branch for the combined multi-task delivery, per
AGENTS.md. Suggested name: `effort/native-hls-startup`. Build each reviewable
task on the current effort and target its PR there. Track N can be completed
and assessed without Track S; do not delay the incident repair merely to
make the independent server change look like one causal fix.

| Order | Scope and output | Exit check |
|---|---|---|
| S01 | Establish intended base, compiler loop and failing regressions | Record base/toolchain; inverse native test fails because old code sets src/transcodes before readiness; source test reproduces false rescan |
| S02 | Native episode identity and pre-src master/child readiness | Delayed origin yields no native src until both playlists are ready; single in-flight fetch; same session/recipe starts |
| S03 | Post-readiness classification, reload and override clearing | One reload then one fallback; ambiguous attribution truthful; subsequent control does not retain failed; stale events inert |
| S04 | Source error taxonomy including open_source_fence | EIO/timeout/invalid probe are distinct from actual mismatch; actual mismatch stays 409 |
| S05 | Manager-owned joinable source preparation and cleanup | Same-ID waiters launch one source job; five-second response does not kill it; fixed expiry, capacity, tombstones and no-waiter publication tests pass |
| S06 | Shared surface fixture and all three client restatements | Start retries same ID; change refuses without stopping predecessor; terminal-expiry and drift tests pass; native compile checks pass |
| S07 | Real native browser evidence and hls.js regressions | Actual Safari delayed-readiness case presents without transcode; post-readiness failure respects ladder; existing hls.js startup remains green |
| S08 | Diagnostics, final review, integration and qualification | Phase/attempt evidence retained, current-main integration revalidated, normal gates satisfied; no unsupported claim of NAS cause |

S04–S06 are one coherent response-contract delivery: do not deploy the new
server response with old client code lists. Where independent PRs are used,
integration happens on the effort before promotion. No tests are deferred
merely because an older status document describes a past defer-until-review
workflow; current repository instructions govern this change.

## 7. Required regression matrix — test the boundaries, not call counts

### 7.1 Native startup and control

| Fixture | Required assertions |
|---|---|
| Master returns typed 503 twice, then master + child become ready | No source assignment or transcode before readiness; same session and DV recipe; ready child fills segTimes |
| Master ready, child still 503 | Source remains unset; master alone cannot consume a reload credit |
| Permanent readiness delay | Fixed deadline/dispatch exhaustion; accurate delivery message; Retry/Close work |
| Pause at 3 s, fake clock advances beyond deadline, resume | No paused dispatch; resume does not renew deadline or attach late result |
| Seek/Close/replacement during master or child body read | Abort; stale response changes no source, surface, credit, timeline or control state |
| Two native episodes both have null HLS references | Only current episode can attach or consume evidence |
| Ready resources then code 4, reload then code 4 | One same-session reload, one existing fallback, no repeated classification races |
| Same case without metadata/frame signals | Fallback labelled unverified refusal before decode; no persistent decode-limit write |
| Old execution emits delayed error after reload | Error ignored; does not consume fallback credit |
| Failed override set before reload | Next valid control report no longer reports failed |
| Typed terminal/auth/authority response on 503 | Correct terminal/handoff owner; no polling behind its decision |
| Child URI/query, redirected master, init 206 or ignored Range | Correct authorized URL resolution; bounded body; no leaked token |
| Pre-existing hls.js manifest and pause/resume cases | Existing transport behavior and owner remain passing |

Add the repaired-behavior tests to the shipped-function harness, not just
the pure policy. The RCA's hash-pinned replay is intentionally a defect
demonstrator: its fetch stub throws and its expected behavior is wrong for
the repaired player. Keep it as historical evidence. Write an inverse
fixture with controllable fetches and a fake clock that fails on the old
implementation and passes on the repair. Record the failing reason to
exclude incidental hash/parser failures.

### 7.2 Source jobs and all client contracts

| Fixture | Required assertions |
|---|---|
| Open or probe completes at 8 s | First response times out at 5 s; retry joins; exactly one open/probe; success by owner deadline |
| Two concurrent same-ID calls | One job and one finishing session creation; same returned session |
| Same ID with different fingerprint/user scope | Existing conflict/auth behavior; cannot join another user's source |
| Job exceeds 20 s; repeated same-ID retries | One expiration result, no fresh job; no renewal; tombstone bounded |
| Worker completes after expiry or generation replacement | No source publication, session creation or claim deletion for newer generation |
| HTTP waiter disappears during retry gap | Job stays supervised and bounded; no orphan; no headless supersession |
| Shutdown/cancel with child running | Prompt termination/reaping for controllable child; capacity released only after cleanup |
| Blocked filesystem operation ignores cancellation | Logical expiry, retained cleanup owner/permit, no growing hidden tasks or late publication |
| Open/stat EIO, missing file, permissions, invalid JSON, child failure | Accurate non-mismatch categories; no false rescan or blind not-yet retry |
| Genuine identity or normalized probe mismatch | Existing rescan code and HTTP 409 unchanged |
| Copy with no burn | No held-source preparation job introduced; incident fix independent |
| Admission exhausted | Existing capacity refusal; no unadmitted source operations |
| Web/Apple/Android start | Same new retry code, finite existing client deadline, same request identity |
| Web/Apple/Android change with healthy predecessor | No start-only retry; predecessor stays visible; Try again belongs to failed change |
| Terminal expiry carried on 503 | Every client stops retrying; differs from source_probe_timeout |

Use injected source open/probe functions and process fixtures to make time,
spawn counts, cancellation, identity and stale completion deterministic.
Then run a bounded real FFprobe integration case. Do not reproduce a cold
NAS by dropping production caches or spinning disks down.

### 7.3 Actual Safari evidence is required

Use a controlled local origin and disposable player session. The test must
load the shipped page in actual Safari's native HLS path, inject the delayed
master/child behavior, and observe real presentation. Record Safari version,
OS, source build, URLs with credentials removed, dispatch/phase timestamps,
chosen recipe, session identity, time to first frame and fallback count.
Chromium plus mocked `canPlayType`, or a generic WebKit harness alone, does
not establish Safari's production error timing. Report those as supporting
tests if used, not the required evidence.

Separately exercise code 4 after readiness/reload with a controlled invalid
native media resource. Do not confuse a test that proves Safari's original
503-to-code-4 behavior with a test proving the repaired preflight prevents
the source from being assigned during 503s.

## 8. Validation and delivery — prove the exact candidate

Before Rust edits, verify `rustc --version` is the repository pin (1.97.1 at
this base). Use the [source-only compile loop](../ci/AGENT-COMPILE-LOOP.md)
if necessary. Transfer committed source with `git archive`, never `.git`
or repository credentials. Keep a warm target directory and run check,
Clippy, formatting and relevant focused tests before pushing.

Existing starting commands; inspect the intended base's test entry points
and build documentation before adding platform-specific commands:

```bash
node tests/playback/web-policy.test.js
node tests/playback/web-control.test.js
node tests/playback/playback-surface-contract.test.js
python3 -m unittest discover -s tests/operations -p test_docs_index.py
cargo fmt --all -- --check
cargo check -p plurxd --all-targets
cargo clippy -p plurxd --all-targets -- -D warnings
```

Run focused Rust regressions for source fences, held probes, request claims,
HTTP mapping and cancellation. Run Android `CreateRetryTest` and affected
Android compilation through the repository's documented runner. Run Apple
policy/contract tests and affected Apple compilation. Record exact commands,
toolchain versions and results; do not substitute JavaScript drift checks
for native compilation. If a platform toolchain is unavailable, identify
that remaining evidence rather than marking S06 complete.

Provide one implementation ledger containing completed work orders, exact
SHAs, focused regressions, native browser evidence, review findings and any
remaining qualification. Follow the project documentation style and index
any added document in the same commit. Rebase/integrate current main and
rerun checks against that exact candidate before claiming merge readiness.
Follow the effort and promotion gates in AGENTS.md; a fast gate alone is
not release qualification. Deliver a reviewable patch/PR and results; do not
silently restart or deploy to nynuc as a side effect of finishing the code.

## 9. Review disposition and evidence limits

| Fable finding | Binding disposition |
|---|---|
| 1: encoded preparation independent; source open also misclassified | Separate Tracks N/S; typed open/stat and probe errors in §5.1 |
| 2: 15 s cap cannot provide a useful retry under 20 s owner | Choose joinable option 2(b); response and job clocks separated in §5.2–§5.3 |
| 3: three-client contract | Shared fixture, both native restatements, drift tests and compilation in §5.4 / S06 |
| 4: readiness before src; null identity; sticky failed override | Required preflight and explicit identity; post-readiness-only classification and override clearing in §4 |
| 5: docs relied on untracked companion and missing index row | Link committed STATUS; ship only this handoff, revised RCA, replay and their index rows; clean-main docs gate |
| 6: master lacks EXTINF | Child media playlist supplies native timeline data in §4.2 |
| 7: second master GET unattributed | Retain uncertainty; correlate request/connection and cancellation timings if evidence becomes available |
| 8: early failed report server-inert | No claim it killed the producer; move report for correct state, fix client override |
| 9: attribution, not existing code-4 learning | Truthful fallback report; preserve current non-learning behavior |

The second incident master GET cannot be assigned to `refreshSegTimes` or
Safari from the retained logs. Do not label it a proven duplicate request
or a proven server cancellation leak. Add bounded request-origin/cancellation
timing where appropriate, keeping credential-bearing paths out of logs.

This handoff selects contracts for implementation; it is not evidence that
they have been built. The original source delay and NAS mechanism remain
unproved. The independent source-job design requires implementation review
of ownership, cleanup and resource bounds in addition to the native repair.
