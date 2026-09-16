# Web playback freeze recovery — preserve delivery and explain quality changes

**Status:** implemented and locally validated, 2026-09-16. Draft review pending; no fix deployed.
**Executes:** the Free Fall (2013) investigation on nynuc, 2026-09-16.
**Owner:** the separate Sol implementation task requested by Paul.

Build the four repairs in §4–§7, prove their interaction with §8, and leave
a reviewable implementation with validation evidence. This is an execution
contract, not a request for another plan. Reconcile it against current main
before editing; the documentation checkout predates the incident build.

Read [PLAYBACK.md](../PLAYBACK.md) for delivery behavior,
[PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md) for playback evidence,
[the startup recovery status](WEB-HLS-STARTUP-RECOVERY-STATUS.md)
for the startup controller's existing obligations, and
[DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) plus the repository's
AGENTS.md for validation and review. This work repairs established playback
without undoing the earlier unloaded-manifest startup repair.

## 1. Outcome and boundaries

An established HLS presentation must resume segment loading after pause.
Stopping and restarting an immutable encoded VOD producer must retain a
byte-identical initialization segment when the codec recipe is unchanged.
The browser must receive typed server failures even when media requests use
binary XHR responses. Auto quality must require evidence that a lower rung
addresses the problem; an empty buffer alone is insufficient.

The user observed repeated freezes and a downgrade to 360p. Investigation
found multiple mechanisms, not one explanation for every interruption.
The confirmed failures and the remaining uncertainty are separated below.

| Scope | Required result |
|---|---|
| Established pause/resume | Loader resumes under the current attachment and play intent |
| Encoded VOD generations | Source chapters cannot change immutable init identity |
| Stream failure observation | Binary responses are safely decoded and attributed |
| Auto-quality attribution | Server failure or a stopped loader cannot masquerade as a bandwidth cliff |
| Recovery ownership | One existing owner, bounded attempts, no competing rescue paths |
| Diagnostics | Explain both a quality change and a suppressed change with bounded evidence |

Non-goals:

- Do not weaken serving authority, quorum, lease, or fencing checks. The
  initial authority outage has a separate, unresolved underlying cause.
- Do not remove init identity checks, rewrite published init bytes, or
  purge active viewers' caches. Those checks caught an actual byte change.
- Do not disable Auto quality, pin every viewer to a resolution, or remove
  legitimate bandwidth-cliff handling.
- Do not create another retry controller or change decoder capability
  learning to compensate for delivery failures.
- Do not restart nynuc, deploy a build, or run disruptive live playback
  experiments as part of implementation. Paul may still be watching.

## 2. Evidence and provenance

### 2.1 Source versions

The incident ran **`v0.3.0-2606-gc9937db1`**. Exact deployed source was read
using `git archive c9937db1`, not by assuming the deployment checkout matched
the running binary. That checkout had subsequently reached
`74bd6631f22956da4c7f421e049f996e3124f2fb`.

The documentation workspace HEAD was
`10f2afe60b3d177866fdcc5741acd9f494525d73` and contains unrelated edits.
Its player predates some deployed startup functions. Use a fresh worktree
of current intended main; copy this document and add only its index row.
Do not copy the original working tree, its entire docs index, or the archived
deployed player over current code.

Symbol names below describe the deployed version. Line numbers are diagnostic
locators, not patch instructions. Re-find the symbols and record the actual
implementation base SHA in the completion evidence.

### 2.2 Incident sequence

Times are UTC on 2026-09-16. Eastern local time was UTC minus four hours.

| Time | Observation | Meaning |
|---|---|---|
| 16:00:54.737 | A scheduled library scan started | Temporal correlation only |
| 16:00:55.505–16:01:00.303 | Serving authority expired, the original copy session self-fenced, replicated reads timed out, then authority recovered | A real earlier control-plane interruption; underlying cause unproved |
| 16:04:30 | Original presentation stalled around 80.4 s | Do not attribute this solely to the later encoded-init bug |
| 16:04:44.596 | Encoded 1040p generation started at entry 40, around 80 s | Established the rendition's original init identity |
| 16:05:01.884–16:05:02.354 | Generation restarted at entry 138, around 276 s, and rejected its changed init | Confirmed producer failure before the viewer reached that boundary |
| From 16:07:42 | Segment 138 requests returned 502 | Delivery failed at the generation boundary |
| Around 16:08:00 | Playback stalled at 276 s | Consistent with the failed next generation |
| 16:08:24 | Another rendition encountered an init mismatch | Lower quality did not cure the recipe defect |
| 16:10:55 | A 720p attachment began around entry 220 | Later, independently observed presentation |
| 16:11:44.074 and .567 | Client demand accepted as hold at 487.516 s, buffered through 516 s | Telemetry shows a pause/hold edge; the user did not recall whether they had paused |
| 16:12:51.013 | Empty-buffer stall at exactly 516 s; estimated throughput about 149,180 kbps | The previous buffer boundary was exhausted |
| 16:12:53–16:12:55.633 | Replacement began and quality changed from 720p to 360p | The policy chose the floor despite high measured throughput |

For the 516 s stall, server segments 254–260 already existed by about
16:11:00. The retained fragment timing was contiguous across the boundary:

| Track | Segment | Start ticks | Duration ticks | Timescale |
|---|---:|---:|---:|---:|
| Video | 257 | 12,336 | 48 | 24 |
| Video | 258 | 12,384 | 48 | 24 |
| Video | 259 | 12,432 | 48 | 24 |
| Audio | 257 | 24,672,256 | 96,256 | 48,000 |
| Audio | 258 | 24,768,512 | 95,232 | 48,000 |
| Audio | 259 | 24,863,744 | 96,256 | 48,000 |

This rules out a missing producer segment or a timestamp gap at that examined
boundary. It does not prove the absence of every possible browser fault.

### 2.3 Reproductions already completed

**Established loader suspension.** A Node VM replay used the exact deployed
`pauseHlsStartup` and `resumeHlsStartup` functions with a presenting episode
and an instrumented HLS object. Pause returned true and called `stopLoad()`;
the episode remained `presenting`. Resume returned false and never called
`startLoad()`. This reproduces the code defect without relying on the user's
memory. The telemetry and buffer-boundary stall support its incident role.

**Chapter-dependent init drift.** Four bounded header-only probes used the
installed FFmpeg/QSV pipeline and captured production arguments, with a
1920×1040, 24 fps rendition and the same audio alignment. The probes varied
only start position and chapter mapping. They consumed `ftyp` and `moov`,
then stopped; no production rendition was overwritten.

| Start | Chapters mapped | Init bytes | SHA-256 |
|---|---|---:|---|
| 80 s | yes | 2,277 | `c82eaada595b1c3073b9110002f474a1489aeea8cae6ff7deef2b7d49d2b78b6` |
| 276 s | yes | 2,256 | `185bff598c443d64d8233e7ef8da622a3defb6f5a78e5db8867e6977bb4f2176` |
| 80 s | no | 1,288 | `b4964c331b07d081aafe9279b3f8986b45f6b649cd7d51fa6457e765fe4937e9` |
| 276 s | no | 1,288 | `b4964c331b07d081aafe9279b3f8986b45f6b649cd7d51fa6457e765fe4937e9` |

The two chapter-enabled hashes exactly match the incident mismatch. The
changed leaf was `/moov[1]/udta[1]/chpl[1]`, shrinking from 458 to 437 bytes;
codec descriptions were unchanged. Chapter mapping also introduced an
unwanted text track/reference. Disabling chapters removed that material and
made both offsets byte-identical. The generic error description about a
changed video pipeline did not identify the actual changed field.

**Floor selection.** A replay of deployed `decideRung` used a ladder of
360/480/720/1040 with costs 1,360/2,160/4,160/8,160 kbps, current 720p,
and measured bandwidth 149,180 kbps. With a healthy 30 s runway it retained
720p. With zero runway and `activeSupplyStall=true` it chose 360p with
`reason="supply stalls"` and `emergency=true`. The floor choice was a policy
branch, not evidence that 360p fit a measured bandwidth limit.

**Hidden server refusal.** Safari logged `InvalidStateError` at deployed
`index.html:7362:42` during the segment-138 502s. The load listener read
`xhr.responseText` on a media request with `responseType="arraybuffer"`.
That getter throws, preventing the typed failure observer from running.

### 2.4 Local corroborating artifacts

These temporary files are available on the original host while retained.
The plan is self-contained; do not make tests depend on these paths.

- `/private/tmp/freefall-deployed-source/`: source archived from `c9937db1`.
- `/private/tmp/plurx-freefall-20260916.log`: early stripped Docker log.
- `/private/tmp/freefall-third-freeze.log`: later stripped Docker log.
- `/private/tmp/freefall-init-reproduction.jsonl`: four probe results.
- `/private/tmp/freefall-reproduce-init.py`: diagnostic probe implementation.
- `/private/tmp/freefall-encoder-args.json`: captured pipeline arguments.
- `/private/tmp/freefall-inits/`: small init samples and probe outputs.

Inspect only relevant evidence. Do not commit private film data, raw browser
URLs, credentials, session tokens, or unfiltered logs. Build synthetic media
fixtures for repeatable validation.

### 2.5 Follow-up: misleading restart requirement at 12:53 Eastern

Paul supplied a screenshot taken at 12:53:14 Eastern showing: “this server
must restart before it can serve this title again.” The new log capture is
`/private/tmp/freefall-restart-required.log`. It establishes the exact chain:

| UTC | Observation |
|---|---|
| 16:52:50.931 | Auto changed 360p to 480p with about 53,047 kbps estimated bandwidth |
| 16:52:51.174 | Generation began at entry 1162 |
| 16:52:51.663 | Init mismatch failed the producer with decision `engine_changed`; segment 1162 returned 502 |
| 16:52:59.268 | Client recorded `terminal:engine_changed` and displayed the restart sentence |
| Through 16:53:31.442 | Segment retries continued after the terminal stopped surface |
| 16:53:31.500–16:53:33.554 | Late `fragLoadError` led to a recovering surface, then startup exhaustion |
| 16:53:36.446 | Stopped surface cleared with reason `user` |
| 16:53:38.119–16:53:39.928 | Failed rendition was replaced; a new attachment presented at 480p |

The rejected generation hash was
`a125a568a6da1563c32e4fd91e3fb9c51d96d9e5df7b68f4420f4c5d5a16d2d1`;
the stored hash was
`0b762616a49528bc18c4c53c61695019a4780b41a04e550091292d6a65814ad4`.
This is another confirmed init-identity failure. Its individual changed MP4
leaf was not reprobed; chapter drift is strongly supported by the earlier
exact reproduction, not separately proved for this particular hash pair.

Container inspection after this event reported start time
`2026-09-16T14:57:41.55302531Z` and restart count zero. Thus playback resumed
without the server restart the message claimed was necessary.

The deployed `vodserve.rs::classify_failure` maps both `Failure::InitDrift`
and `Failure::EngineChanged` to `ProducerDecisionReason::EngineChanged`;
the specialized init-drift path records the same class. The permanent
`EngineChanged` contract describes a process-wide fragment-index engine
baseline held in a `OnceCell`. A rendition init byte mismatch does not prove
that process-wide condition. `playback_control.rs::terminal_message` then
turns this overbroad classification into the restart instruction.

This adds two required regressions to the existing repair: distinguish
rendition init drift from actual process engine-baseline failure (§5), and
prevent late loader errors from reopening recovery after an owned terminal
stop (§6). User-initiated retry remains a new owned attempt.

## 3. Execution order and shared contracts

First establish the pinned compiler loop (§9) and inspect current ownership
and error types. Then implement and test these milestones:

1. Repair established loader pause/resume (§4).
2. Remove chapter mapping from encoded immutable VOD recipes (§5).
3. Safely observe typed failure bodies (§6).
4. Gate all automatic quality/rescue decisions on causal evidence (§7).
5. Exercise their interaction and publish validation evidence (§8–§10).

Use existing attachment identities, intent epochs, retry ownership, and
failure types where available. Names suggested here describe semantics, not
a mandate for parallel abstractions. If current main already fixes a part,
prove it with a regression and implement only the remaining gap.

Every asynchronous operation must verify both attachment ownership and
current intent before committing state. A successful response for one
resource must not silently clear a terminal failure for another. Unknown
health is unknown; it is neither healthy delivery nor a bandwidth failure.

## 4. Established HLS loading survives pause/resume

### 4.1 Defect and source map

In deployed `crates/plurxd/src/web/index.html`, `pauseHlsStartup` around
line 7144 handles both `active` and `presenting` startup episodes. The
presenting branch cancels a reserved retry and calls `hls.stopLoad()` but
keeps state `presenting`. `resumeHlsStartup` around line 7160 accepts only
state `paused`. Both native transport events and the manual play/pause
toggle use these helpers.

### 4.2 Implementation contract

Represent established loader suspension separately from startup suspension.
Use existing transport state if it can represent the distinction without
ambiguity; otherwise add a small attachment-owned suspension record.

| Event | Required behavior |
|---|---|
| Pause before first presentation | Preserve existing startup pause/deadline/retry semantics |
| Pause while presenting | Stop fetching if current policy calls for it and record that this attachment's established loader was suspended |
| Resume same attachment with play intent | Resume the parsed-manifest loader at the correct current media-local position, exactly once |
| Repeated pause/play events | Idempotent; no duplicated loader start or retry reservation |
| Seek, replacement, teardown, terminal stop | Invalidate the old suspension and its callbacks |
| Pause during a reserved retry | Cancel/suspend through the existing owner; a later stale callback cannot restart loading |

Do not reset startup attempt counts, original deadlines, TTFF, session
identity, quality, seek intent, or recovery budgets merely to resume an
established loader. Do not call `loadSource` for a parsed manifest simply to
repair this state. Preserve the unloaded-manifest startup path, where
`startLoad` alone cannot fetch the first manifest.

Check timebase conversion explicitly: video element time, content timeline,
and session origin need not match. Resume from the current owned position,
not the original startup position or an old captured seek target. Do not
call `video.play()` from loader recovery when viewer intent is paused.

### 4.3 Acceptance

Run actual production helper code in the regression harness. Verify the
sequence `presenting → pause → play` emits stop then start and subsequently
fetches beyond the prior buffer boundary. Cover manual and native edges,
duplicate events, pause during retry, pause/seek/resume, replacement while
paused, and teardown followed by a stale resume callback. Assert that the
server session and selected rung do not change during ordinary resume.

## 5. Encoded VOD init identity excludes source chapters

### 5.1 Defect and source map

`crates/plurx-core/src/transcode/vod.rs::vod_pipe_args` constructs encoded
fragmented MP4 pipe arguments without disabling chapter mapping. FFmpeg's
default chapter mapping survives explicit video/audio stream maps; changing
the remaining output duration at a generation restart changes chapter data.

The copy path in `transcode/mod.rs` already has a documented
`-map_chapters -1` exclusion. Inspect it for consistency. Relevant identity
consumers are `renditiondir.rs::InitIdentity`, `vodgen.rs::GenerationRun::on_init`,
`vodserve.rs::run_generation`, and `vodencode.rs::Encoding::identity`.

### 5.2 Implementation contract

Add explicit `-map_chapters -1` at the appropriate output scope in the shared
encoded VOD pipe builder. Ensure ordinary generation and regeneration use
the same corrected recipe. Keep audio mapping, subtitles handled outside
this media pipe, color/HDR signaling, keyframe placement, and alignment
unchanged. Library chapter metadata and chapter navigation remain available
through their existing metadata path.

Keep full init identity verification. A true codec/configuration mismatch
must still fail before publishing incompatible media. Do not normalize away
arbitrary `moov` changes or merely compare codec names.

Verify that the effective argument change changes the immutable rendition
recipe/cache identity. The deployed implementation fingerprints arguments
at offset zero; establish the behavior on current main. New recipes must
use their new namespace. Existing live sessions retain their own published
identity; no in-place cache migration or deletion is required.

If improving the error message, distinguish byte identity mismatch from a
proved codec change. Keep diagnostics bounded and avoid embedding raw init
bytes or source paths in user-facing failures.

The follow-up in §2.5 makes failure classification a required part of this
milestone. Do not classify arbitrary `InitDrift` as proof of process-wide
`EngineChanged`. Trace both `classify_failure` and the specialized
`on_init_drift` path. Preserve the real engine-baseline restart requirement,
but give rendition identity refusal an accurate scope and explanation.
Reuse a suitable typed reason or extend the contract consistently across
server serialization, clients, telemetry, and exhaustive tests. Inspect all
consumers before adding a public reason.

Do not make every init mismatch endlessly retryable. The current immutable
presentation must still refuse incompatible bytes. Any fresh-rendition
recovery must respect existing publication/cache safety and bounded owner
budgets. A truthful stopped surface is preferable to inventing either a
required server restart or a guaranteed successful retry.

### 5.3 Acceptance

Add a unit regression asserting explicit chapter exclusion in the shared
recipe and relevant backend variants. Add a real FFmpeg regression using a
small synthetic chaptered video/audio source, with generation starts on
opposite sides of a chapter boundary and different remaining durations.

The generated init segments must match byte-for-byte and contain only the
intended video/audio tracks, without chapter `chpl` or chapter text-track
references. Prove the fixture actually contains chapters; an unchaptered
fixture would not test this failure. Check init identity acceptance across
both generations and continued segment delivery. Retain a negative test
that genuine incompatible init bytes are rejected.

Add separate controls for actual process engine-baseline change and local
rendition init drift. Only evidence of the former may produce the process
restart instruction. Test the specialized drift handler as well as the
generic classification function so their verdicts cannot diverge.

Use software encoding for portable coverage where suitable, and record
hardware-backed evidence separately if available without disrupting nynuc.
The existing production QSV probe proves the diagnosis; do not label a
software-only new test as hardware qualification. Record any unexecuted
hardware test as a limitation.

## 6. Observe typed failures from binary media requests

### 6.1 Defect and source map

The deployed `xhrSetup` load listener around `index.html:7358` calls
`noteStreamFailure(status, xhr.responseText, context)` for HTTP errors.
This throws for binary response types. Also inspect the startup-specific
loader's error observation and `_plurxStartupObserved` deduplication.

### 6.2 Implementation contract

Centralize safe, bounded error-body extraction for the response types the
application supports: empty/text, JSON, ArrayBuffer, and Blob where used.
Read `responseText` only for compatible types. Decode binary textual errors
without throwing into hls.js callbacks. Apply a documented byte cap before
decoding or parsing; avoid serializing arbitrary response objects.

For asynchronous Blob extraction, capture attachment, intent, resource, and
request ordinal before awaiting. Revalidate them after decoding and before
recording the result. Reserve request ordering at observation time, not
completion time, so a slow old failure cannot overwrite a newer result.
Preserve the existing observer's freshness rules and deduplicate startup
and general loader observation.

Malformed, oversized, absent, or unreadable bodies produce a bounded unknown
delivery failure, not a decoder or bandwidth verdict. Network failures and
timeouts without HTTP bodies remain distinct. Do not issue a second fetch
solely to inspect the error and do not add retries in the extractor.

Connect typed `producer_failed`, authority refusal, and publication failures
to existing recovery/surface state before Auto can act on the resulting
starvation. Observe nonfatal failed segment requests; waiting for hls.js to
exhaust its internal retries leaves the classification absent too long.

Audit `clearStreamFailureFor` and successful playlist responses. A healthy
playlist must not clear an attachment's terminal failed-producer evidence.
Clear by the appropriate owned recovery/replacement or stronger relevant
success, following the existing failure contract.

After the owner accepts a terminal stop, cancel or retire current loader
activity through the existing lifecycle. Late nonfatal/fatal events and
reserved retry timers must not start another recovery or replace the
terminal explanation with startup exhaustion. Fence by the stopped attempt,
while allowing a deliberate user retry to create a new owned attempt. The
16:52:59–16:53:33 sequence in §2.5 is the regression case.

### 6.3 Acceptance

Exercise text, JSON, ArrayBuffer and asynchronous Blob errors, malformed and
oversized bodies, getter/decode exceptions, no-body network failure, and
timeout. Include the exact typed 502 producer-failure shape supported by the
server. Assert no exception escapes, no extra request is made, and only one
failure is recorded. Test replacement and intent changes during decode,
out-of-order responses, duplicate startup observation, and successful
playlist refresh following a terminal segment failure.

## 7. Auto quality requires a cause a lower rung can address

### 7.1 Defect and source map

In `crates/plurxd/src/web/playback-policy.js::decideRung`, the deployed severe
branch calculates a safe bandwidth index but then uses:

```javascript
const starvation = activeSupplyStall || nearEmpty || supplyBurst;
const target = starvation ? available[0] : /* bandwidth-limited step */;
```

This maps starvation directly to the lowest rung. `autoControllerTick` does
not gate that decision on typed stream failures or `health.producer_state`.
`noteAutoStall` uses runway to classify supply versus decode; supply does not
mean the network is slow. `rescueAutoSupply` also converts remux to transcode
after repeated supply events without enough cause attribution.

### 7.2 Evidence and decision contract

Pass normalized, fresh, attachment-scoped cause evidence into the pure
policy. Reuse existing vocabulary where possible. Explicitly distinguish:

| Evidence | Quality decision | Recovery responsibility |
|---|---|---|
| Confirmed insufficient sustained media throughput | Existing bandwidth-limited downshift, with hysteresis | Auto quality owner |
| Demonstrated encoding capacity shortfall, if supported by current policy | Capacity-based change only with its documented threshold and healthy producer evidence | Existing capacity/quality owner |
| Typed producer failure or serving-authority refusal | Suppress quality change; report the actual cause | Existing bounded delivery recovery or terminal surface |
| Established loader known suspended/stopped unexpectedly | Suppress quality change; repair loader under current intent | Transport/recovery owner |
| Empty runway with unknown, missing, or stale health | No quality verdict from emptiness alone | Existing bounded diagnosis/recovery |
| Positive decoder-specific evidence | Preserve existing decoder policy and capability rules | Existing decoder recovery owner |

Document the selected freshness window and bandwidth confidence threshold
in code/tests using existing constants where appropriate. High historical
bandwidth alone cannot prove current health; empty runway alone cannot prove
low bandwidth. In-flight and failed-request durations must not contaminate
successful media-throughput samples as if they measured link capacity.

Apply attribution before every automatic rung change and remux-to-transcode
rescue, not only `decideRung`. Audit watchdogs, nonfatal error handling,
fallback routes, and `rescueAutoSupply` for bypasses. Preserve the single
existing recovery claim across concurrent controllers. Manual quality and
Original remain governed by their existing user-intent contracts.

Unknown or terminal server evidence must not teach a lower network ceiling,
mark a decoder unsupported, or persist a worst-rung preference. A retryable
delivery error may recover at the same quality under the existing bounded
budget. A terminal error must reach its truthful surface rather than loop
through progressively smaller renditions. Do not increase retry budgets to
make these tests pass.

### 7.3 Explainability and acceptance

Return a bounded decision reason and evidence summary for both changes and
suppression. Suggested semantics include bandwidth-limited, producer-failed,
authority-refused, loader-suspended, and insufficient-evidence; use the
repository's existing event vocabulary if equivalent. Include current and
target rung, decision action, evidence age, and relevant numeric runway and
throughput measurements. No URLs, tokens, arbitrary server text, or unbounded
event spam. Emit on a decision/state transition or existing bounded cadence.

Regress the 149,180 kbps, 720p, zero-runway case with failed producer,
suspended loader, and unknown health separately: none may choose 360p solely
because of starvation. Add a positive bandwidth-cliff control showing that
a genuinely inadequate, fresh throughput estimate still downshifts. Retain
upgrade hysteresis, cooldowns, ladder bounds, manual mode, and legitimate
decoder recovery coverage. Assert actual recovery/quality side effects as
well as the pure function's return value.

## 8. Integrated validation matrix

Use the actual vendored HLS version and production helper paths where
possible. Test doubles should expose calls and ordering, not reproduce the
implementation in the test. Source-string assertions alone are insufficient.

| Scenario | Evidence required |
|---|---|
| Presenting → pause → resume, buffered data then exhausted | New segment requests continue past the old boundary; same session and rung |
| Startup with no parsed manifest | Existing bounded manifest reload still works |
| Pause while startup retry reserved | No stale timer restarts paused or replaced media |
| Two encoded generations across chapters | Identical init; next generation delivers segments; genuine drift still refused |
| Failed segment returns binary typed 502 | No InvalidStateError; producer failure observed before automatic quality evaluation |
| 502 followed by successful playlist refresh | Terminal segment/producer evidence survives unrelated success |
| High throughput plus producer failure | No quality floor, no capability poisoning, truthful bounded recovery/surface |
| High throughput plus stopped loader | Loader recovery, no unnecessary replacement/transcode |
| Empty buffer plus unknown/stale health | Bounded diagnosis; no invented bandwidth verdict or endless retry |
| Real bandwidth cliff | Legitimate downshift remains functional, followed by existing recovery hysteresis |
| Replacement during body decode or health request | Old response cannot mutate the new attachment |
| Simultaneous watchdog, Auto tick, and error callback | At most one recovery/quality owner commits an action |
| Manual quality or Original | User intent preserved across delivery errors |

A deterministic transport/loader integration test plus a real FFmpeg fixture
are required. If a browser playback harness is available, exercise an actual
buffer-drain/resume sequence and binary server refusal through it. Record
browser execution separately from VM helper tests; neither implies the other.
Do not use the live viewing session as the regression harness.

## 9. Compiler loop and focused commands

Before editing Rust, confirm `rustc --version` matches the repository-pinned
**1.97.1** toolchain. The documentation host had Homebrew Rust 1.98, which is
not equivalent evidence. Follow the
[source-only compiler loop](../ci/AGENT-COMPILE-LOOP.md) if the checkout host
cannot run the pin. Transfer committed source with `git archive`, never
`.git` or credentials, and keep the compiler target directory warm.

Existing focused entry points in the documentation checkout:

```bash
node --test tests/playback/web-policy.test.js tests/playback/web-control.test.js
python3 tests/operations/test_docs_index.py
cargo fmt --all -- --check
cargo check -p plurx-core -p plurxd --all-targets
cargo clippy -p plurx-core -p plurxd --all-targets -- -D warnings
```

Confirm the current harness and extend the relevant tests. Run the new
transport/error/Auto regressions and the smallest Rust tests covering
`vod_pipe_args`, encoding identity, and generation init validation; record
their exact commands and test names. Run the synthetic FFmpeg regression
explicitly rather than silently skipping it behind an unavailable tool.
If a required environment is unavailable, report that validation gap and
complete independent work without pretending it passed.

Follow current AGENTS.md and pipeline requirements for additional checks,
review, and gates. If the base moves or the patch is ported, archive and
validate the exact final candidate again. CI is a gate, not the compiler
loop. Do not push merely to discover compilation errors.

## 10. Completion and review contract

Keep this document and its docs/README.md index entry in the same commit.
Update its status and record the implementation branch/base, changed
contracts, exact validation commands/results, and remaining limitations.
Preserve unrelated documentation and source work in the original checkout.

The implementation is ready for review when all four repairs are connected,
the positive and negative controls pass, and no extra recovery owner or
unbounded retry has been introduced. Commit the scoped changes and prepare
a draft PR under the current contributor workflow after local validation.
Do not merge or deploy as part of this handoff.

The PR should explain the concrete before/after behavior: resuming no longer
leaves the established loader stopped; generation restarts do not inherit
offset-dependent chapters; binary server refusals remain visible; and Auto
quality distinguishes capacity problems from other delivery failures. Include
the retained bandwidth-cliff control and the init-identity negative control.

Report the initial authority outage as unresolved. The scan began nearby in
time, and synchronous storage work is a possible follow-up hypothesis, but
the collected evidence does not establish CPU pressure, CI load, NFS, or a
scan as its root cause. Do not present this patch as proving or repairing
that separate control-plane mechanism.

## 11. Completion evidence

Implementation branch: `codex/free-fall-playback-repair`.
Implementation base: `1731ea26ecaaff4a43daaa4f40e704fa7061493b`
(`origin/main`, reconciled before editing on 2026-09-16).

The completed change preserves the existing owners and budgets:

- established Pause records one attachment- and intent-owned loader
  suspension; the immediately following Play resumes that same hls.js
  instance at the current media-local time, while replacement, seek, teardown,
  and duplicate Play cannot revive it;
- encoded VOD output explicitly passes `-map_chapters -1`; the argument is
  part of the existing argv-based encoding identity, and strict init-byte
  validation remains unchanged;
- local init drift now records `rendition_init_changed` and tells the viewer
  to reopen that rendition; only an attested process engine-baseline change
  retains `engine_changed` and the server-restart instruction;
- text, JSON, ArrayBuffer, and Blob failure bodies use response-type-safe,
  size-bounded decoding, with attachment, intent, and request-order checks
  around asynchronous completion;
- an owned terminal stop retires the current HLS attempt, aborts startup
  loaders, cancels reserved retries, and fences late events; deliberate Retry
  becomes eligible only after a fresh attachment is minted;
- Auto suppresses changes for fresh producer, authority, delivery, and loader
  causes. Starvation without a fresh completed slow transfer is insufficient;
  a demonstrated capacity shortfall and a fresh bandwidth cliff retain their
  existing bounded paths. Suppression emits only on a decision transition.

Local validation used the repository-pinned compiler and FFmpeg 9.0.1:

| Command | Result |
|---|---|
| `rustup run 1.97.1 rustc --version` | `rustc 1.97.1 (8bab26f4f 2026-07-14)` |
| `node --test tests/playback/web-policy.test.js` | passed |
| `node tests/playback/web-control.test.js --free-fall` | passed; manual and native resume, terminal loader retirement and fresh-retry ownership, safe binary refusal decoding, stale decode ownership, and the actual vendored loader |
| `rustup run 1.97.1 cargo test -p plurx-core transcode::vod::tests::encoded_vod_recipe_explicitly_excludes_source_chapters -- --exact` | passed |
| `rustup run 1.97.1 cargo test -p plurx-core fmp4::tests::encoded_vod_restarts_keep_init_identity_across_chapters -- --exact --nocapture` | passed with a real synthetic chaptered FFmpeg fixture; both generations emitted fragments and identical init bytes |
| `rustup run 1.97.1 cargo test -p plurxd renditiondir::tests::a_changed_video_pipeline_is_refused_before_a_segment_is_written -- --exact` | passed; genuine pipeline drift is still refused before publication |
| `rustup run 1.97.1 cargo test -p plurxd playback_control::tests::local_init_drift_and_process_engine_change_have_distinct_verdicts -- --exact` | passed; only the process verdict instructs a server restart |
| `rustup run 1.97.1 cargo test -p plurxd vodserve::tests::every_generation_failure_names_what_actually_happened -- --exact` | passed; generic init drift and engine change have distinct classes |
| `rustup run 1.97.1 cargo test -p plurxd vodserve::tests::specialized_init_drift_records_only_the_rendition_scope -- --exact` | passed; the specialized handler matches the generic classifier |
| `rustup run 1.97.1 cargo test -p plurxd ffmpeg::tests::process_fragment_engine_baseline_detects_an_actual_object_change -- --exact` | passed; an actual attested object replacement invalidates the process baseline |
| `rustup run 1.97.1 cargo fmt --all -- --check` | passed |
| `rustup run 1.97.1 cargo check -p plurx-core -p plurxd --all-targets` | passed |
| `rustup run 1.97.1 cargo clippy -p plurx-core -p plurxd --all-targets -- -D warnings` | passed |
| `python3 tests/operations/test_docs_index.py` | passed, four tests |
| `git diff --check` | passed |

Remaining limitations are explicit. The broad legacy
`node tests/playback/web-control.test.js` full-open fixture on this base exits
before the new cases because it does not define the current production
`currentCapsDocument` dependency; the scoped production-helper path above is
green. No live browser or live-viewer buffer-drain experiment was run, and the
synthetic FFmpeg regression used the software encoder rather than QSV. No
service was restarted, no cache was purged, and no build was deployed.

The initial serving-authority outage remains unresolved. This implementation
does not claim that the nearby scan caused it or that these playback repairs
change quorum, lease, or fencing behavior.
