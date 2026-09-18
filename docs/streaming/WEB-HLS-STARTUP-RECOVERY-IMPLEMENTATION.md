# Web HLS startup recovery — recover an unloaded manifest and report the cause

**Status:** ready for implementation after one adversarial agent review and
author corrections, 2026-09-14; implementation not started.
**Written:** 2026-09-14. **Executes:** the Child's Play startup diagnosis on
nynuc from 2026-09-14. This document delivers an implementation contract;
it does not claim a product fix or authorize a production deployment.

Companion to [PLAYBACK.md](../PLAYBACK.md) (delivery behavior),
[PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md) (playback evidence), and the
[lifecycle coverage map](../playback-control/PLAYBACK-LIFECYCLE-COVERAGE.md)
(ownership across preparation, transfer and presentation). Work through §7
in order. Keep the current recovery owner and publication authority checks.
If implementation needs a new recovery controller, an ownership bypass, or
a different retry budget, record and resolve that design change here first.

## 1. Incident — a retryable startup became a stopped player

### 1.1 Evidence belongs to the deployed tree

The incident ran build `v0.3.0-2411-gfd70676b`, commit
`fd70676bfbcdf18ecdc487a0438f900d02fbc111`, with vendored hls.js **1.6.16**.
The source was read from nynuc's deployment checkout using `git archive`.
The documentation workspace's HEAD was
`10f2afe60b3d177866fdcc5741acd9f494525d73`; its web player predates the
deployed retry and surface code and also contains unrelated local edits.
Do not implement by replacing that workspace's player with the archived one.
Use an isolated checkout of current intended main and reconcile symbols there.

Source links below resolve in the documentation checkout. Symbol names and
deployed line references identify the incident version; re-verify them at
implementation time. In particular, `scheduleHlsNetworkRetry` and the deployed
surface presenter are absent from the older workspace player.

**Observed item:** Child's Play (2019), file ID `5976`, 2160p HEVC remux,
resume position `3827.549` seconds (1:03:47.549). Both attempts copied video,
converted DTS audio to AAC, and stripped Dolby Vision metadata while keeping
the compatible HDR10 base. Both used rolling HLS because `vod_index_pending`
prevented immutable VOD. Index completion did not explain the successful retry.

| UTC on 2026-09-14 | Observation | What it establishes |
|---|---|---|
| 12:52:50.295 | Copy-HLS command logged | Preparation of the first attempt |
| 12:52:55.296 | Media-origin probe exceeded 5 s | Source probing was slow; requested start used as fallback |
| 12:52:55.297 | Actor-owned prepublication session started | Worker session existed before playlist delivery |
| 12:53:00.853 | Master playlist returned 503 after 5,000 ms | First publication deadline exhausted |
| 12:53:06.924 | Master playlist returned 503 after 5,059 ms | hls.js's built-in error retry also exhausted |
| 12:53:06.985 | `hls_fatal: manifestLoadError` | No usable manifest had loaded |
| 12:53:06.987 | `stream_refused: response_publication_timeout` | Server explicitly described a retryable publication timeout |
| 12:53:08.988 | `hls_retry` logged a reload at 0.0 s | App called `startLoad(0)`; logging the call did not prove a request |
| 12:53:09.590 | HDR10 metadata promoted into init | Init appeared about 19.3 s after command logging |
| 12:53:18.817 | Producer held with 34 s ahead | Production had supplied media while the player remained stuck |
| 12:53:36.000 | Diagnostic master request validated copy init | First successful handoff coincided with the diagnostic probe |
| 12:53:36.068 | “The stream reaches the browser but won't play”; `decoder_failed` | A reachable playlist was misclassified as decoder evidence |
| 12:54:08.594 | Idle reaper ended the session | Session cleanup followed the visible failure |
| 12:55:10.641 | User cleared the stopped surface | Manual Try again |
| 12:55:12.519–12.540 | Same command; source probe completed in about 20 ms | Second startup was much faster |
| 12:55:12.628–12.990 | Init promoted and validated | Same recipe became deliverable |
| 12:55:14.060 | First frame | About 3.4 s after manual retry |

Eastern local time was UTC minus four hours. The logged second-attempt TTFF
of 146.2 s spans the original open and the time the user spent at the error;
it is not the duration of the manual retry. Preserve both item-open elapsed
time and attachment elapsed time when adding diagnostic timing (§6).

### 1.2 Confirmed mechanism and the limit of the storage finding

The master handler wraps `exact_hls_context` in the five-second publication
deadline. For HEVC this helper waits for the actual init to derive codec
information. Missing init can therefore exhaust that deadline. The server
answered twice before the first attempt's init appeared.

The deployed application then called `hls.startLoad(0)`. With no parsed
manifest, hls.js has no levels to load; `StreamController.startLoad` leaves
the controller `STOPPED`. `loadSource(url)` is the operation that emits
`MANIFEST_LOADING`. A synthetic 503 loader against the exact deployed library
confirmed `manifestLoadError`, one request before and after `startLoad(0)`,
and controller state `STOPPED`. This isolates the library behavior; it is
not a browser playback test or a reproduction of the QNAP delay.

The watchdog later fetched the playlist successfully. For `method=remux`,
the deployed `probePlaybackSource` did not inspect a sample segment at all;
`stallDiagnose` treated playlist HTTP success as media reaching the decoder.
It stopped the player with `decoder_failed`, despite no decoder rejection.
The loaded-playback `persistentWait` path requires prior playback progress
and does not reliably rescue this never-started state.

The media file is on QNAP over NFS. A cold read, disk wake-up or temporary
storage delay is plausible, not proved. No relevant kernel error was retained;
ten-minute statistics showed no NFS retransmissions or sustained CPU pressure.
Those coarse samples cannot exclude a short stall. Do not claim an NFS
outage, bad media, unsupported HEVC or an extension failure from this evidence.
The later final-segment ENOENT followed idle retirement; it did not initiate
this playback failure and is outside this repair.

## 2. Boundaries — repair startup within existing ownership

**Required outcome:** a delayed but valid manifest can recover without a
manual click; a permanently unavailable stream ends within a fixed startup
budget with an accurate explanation and working Try again/Close actions.

| In scope | Why |
|---|---|
| Manifest-aware automatic retry | `startLoad` cannot recover an unloaded manifest |
| Explicit startup request budgets and cancellation | More retries must not create an endless spinner |
| Evidence-based startup diagnosis | HTTP reachability is not successful decode |
| Publication-phase diagnostics and typed readiness | Separate waiting for bytes from losing control authority |
| Deployed-library, application and browser regressions | A mocked `startLoad` call count would miss the defect |

**Non-goals:** changing QNAP mounts or waking disks, rebuilding all fragment
indexes, changing source files, increasing buffers, changing encoder/quality
policy, adding a recovery controller, altering native-client policy, changing
the strict playback-control wire contract, or repairing post-retirement
scratch cleanup. These do not correct the demonstrated manifest retry.
Keep immutable VOD and rolling fallback covered separately.

## 3. Web contract — one startup episode, bounded work

### 3.1 Extend the existing attachment state

Re-verify these interfaces in the
[web player](../../crates/plurxd/src/web/index.html):

```javascript
function attachHls(video, playlistUrl, startAt)
function scheduleHlsNetworkRetry(video, player, detail) // deployed tree
function armStall(fromSec, graceMs)
async function stallDiagnose()
async function probePlaybackSource(url, headers, isTranscode)
function retryPlayback()
```

Keep policy decisions in
[playback-policy.js](../../crates/plurxd/src/web/playback-policy.js) and side
effects in the current player executor. Add attachment-scoped state with
these semantics; exact field names are implementation choices:

| State | Contract |
|---|---|
| Ownership capture | Player object, attachment token, hls instance, attempt, session, seek token and control-intent generation |
| Source identity | Actual attached playlist URL and initial media-coordinate start position; never reconstruct from the title or current route |
| Manifest state | Unknown/loading, loaded, parsed; only this instance's events advance it |
| Startup state | `active`, `paused`, `presenting`, `exhausted` or `cancelled`; explicit transitions below |
| Startup deadline | Monotonic absolute deadline set once at attach; 40,000 ms cold open, existing 20,000 ms seek grace where applicable |
| Application retry budget | One credit per attachment, shared with segment recovery; `unused`, `reserved` or `dispatched`, with pause behavior in §3.4 |
| Pending work | One retry timer and its cancellation handle; in-flight requests belong to hls.js |
| Evidence | Latest typed refusal and its resource class, timestamp and ownership capture; real media/decoder/presentation facts recorded separately |

Successful manifest reload does not allocate a new server session, new
attempt, new retry credit or fresh startup deadline. A manual Try again is
a new user-authorized attempt and retains the original film resume position.
Do not convert local `video.currentTime=0` into film time zero: rolling HLS
uses a session-relative clock, while immutable VOD uses the film timeline.
Reuse the existing attach/seek coordinate conversion and selected tracks.

### 3.2 Reload the missing manifest, retain loaded-stream behavior

```text
 fatal event from current HLS attachment
        │
        ├─ terminal/auth/media failure ──▶ existing classified owner action
        │
        └─ retryable network failure
                  │
                  ├─ manifest never parsed ──▶ loadSource(captured URL)
                  │                              then honor original start/intent
                  └─ manifest already parsed ──▶ existing startLoad(position)
```

Pass the structured hls error details and manifest state to the retry
decision; a string used for a log is insufficient. Handle both
`manifestLoadError` and `manifestLoadTimeOut`. For this initial-manifest
case, call `loadSource` on the same current hls instance, and arrange for
loading from the captured start coordinate when that manifest parses. Use
the library's actual auto-start behavior, tested against the vendored code;
do not both auto-start and issue a second independent start path.

Keep `startLoad` for the established stream's level/fragment network
recovery. Do not call `loadSource` on every network error: it can reset an
active MediaSource and discard useful buffered media. Do not call
`recoverMediaError` for HTTP 503 or choose transcode without actual media
failure evidence.

401/403 do not enter either the library or application startup retry loop.
Structured terminal, ended or
owner-lost responses take precedence over their HTTP status; preserve the
existing reopen/owner-transition path for cases it owns. Unstructured network
errors remain generic network failures. A generic 503 is not proof that
FFmpeg is still starting, even when another bounded request is reasonable.

### 3.3 Budget manifest retries explicitly

Keep the existing fragment policy unchanged. Give the manifest policy its
own explicit settings, derived from one web startup policy definition:

| Setting | Proposed value | Meaning |
|---|---|---|
| `maxTimeToFirstByteMs` | 10,000 ms | Longer than the server's 5 s publication deadline |
| `maxLoadTimeMs` | 12,000 ms | A response that starts and stalls also terminates |
| `errorRetry.maxNumRetry` | 7 | At most eight HTTP-error attempts per source-load cycle |
| `errorRetry.retryDelayMs` | 1,000 ms | 1/2/4 s backoff, capped below |
| `errorRetry.maxRetryDelayMs` | 4,000 ms | Bounded backoff |
| `timeoutRetry.maxNumRetry` | 1 | At most one transport-timeout retry per cycle |
| `timeoutRetry.retryDelayMs` | 1,000 ms | Prevent an immediate timeout loop |
| `timeoutRetry.maxRetryDelayMs` | 1,000 ms | No added exponential timeout tail |
| Application recovery delay | Existing 2,000 ms | One shared corrective retry, not a second periodic poller |
| Manifest dispatch ceiling | 16 per startup attachment | Both source-load cycles together, including interrupted requests and mixed failures |

These are new contract values, not measurements of existing configuration.
The deployed manifest policy allows one HTTP-error retry; only fragment
policy is overridden by `vodClientContract` today. Test the actual library's
mixed error/timeout behavior; do not assume its counters are independent.
Enforce the 16-dispatch ceiling at the transport boundary as well as the
elapsed-time limit.

**Timing acceptance:** for immediate retryable 503s and negligible event-loop
delay, the first cycle dispatches at approximately 0/1/3/7/11/15/19/23 s.
The one application reload starts at 25 s; its requests are 25/26/28/32/36 s
before the 40 s limit rejects the next dispatch. Thus readiness at 19 s or
30 s is sampled, unlike the reviewed draft's last request at 16 s. Requests
held for five seconds have a different schedule; test them separately.
These are elapsed times from the first manifest request, which must begin
promptly after attach; actual remaining attachment time always wins.
Readiness just before expiry does not guarantee first presentation: there
must be time for the next request, media and a frame. Boundary tests assert
truthful exhaustion, not an impossible guarantee at 39.999 s.

All built-in retries and the single application retry fit inside the same
absolute 40 s/20 s deadline while startup is active. A corrective reload receives only the
remaining time. At expiry, cancel the retry timer and abort hls loading through
the existing owner before raising the exhausted-startup surface. Browser
timer throttling can delay callback execution: the first resumed callback
must check the absolute deadline before sending or attaching anything.

Do not mint another 40 s by rearming `armStall`, receiving a 503, polling
status, reloading the manifest, or observing a control `hold`. A manual
Try again or a seek/track/quality action that creates a new attachment can
start a new episode. Ordinary pause/resume follows §3.4 and does not do so.

**Transport enforcement:** a guarded adapter around the pinned stock XHR
loader owns the startup admission checks. It must check immediately before
every actual send, including the library's internal backoff callbacks;
`scheduleHlsNetworkRetry` guards alone cannot do this. Keep stock loading,
parsing, progress and retry behavior behind the adapter. Do not fork the
vendored library. The current library's `loadInternal` creates each retry's
XHR, asynchronously runs `xhrSetup`, then calls `openAndSendXhr`. A narrow
adapter must fence the final send, after that asynchronous setup, not only
entry to `loadInternal`. Declare and test the internal hooks used against
the vendored version so a later library upgrade cannot silently bypass them.

The adapter maintains a bounded registry of live loaders for the current
attachment. A request sends only when ownership/intent are current, startup
is active, time remains and the manifest dispatch ceiling is unspent. The
counter advances at send, not timer reservation. Startup cancellation or
exhaustion aborts registered XHRs and destroys their loaders, including
internal retry timers. `hls.stopLoad()` alone is insufficient: the deployed
playlist loader is a core component outside its network-controller loop.
Use the existing teardown owner to settle these resources. No stale abort
or callback may render a new error or trigger fallback.

Before the stock loader decides to retry an HTTP failure, inspect the bounded
owned response body and status. Terminal/auth responses bypass stock retry
and are delivered once to the application's typed owner path; a terminal
503 must not wait for `hls_fatal` before retries stop. Preserve the default
status policy for unstructured failures. This interception must occur before
the stock `readystatechange` schedules a retry; the later XHR `load` listener
used for diagnostics is too late. Keep manifests as text and retain existing
auth/header setup. Non-startup traffic delegates to existing semantics.

The adapter is transport plumbing for the existing executor, not another
recovery owner: it admits/aborts requests and reports evidence; it does not
schedule a session reopen, choose quality or show a surface. Tests must run
stock retry scheduling beneath it, rather than simulate retry counters in a
fake loader that never exercises this boundary.

**Startup completion:** use the existing attachment-scoped presentation
evidence to transition `active` to `presenting`: a video frame callback for
the current attachment, or the existing frame-counter/advancing-media-clock
fallback when frame callbacks are unavailable, with the target timeline
accepted. For audio-only media use the existing active `playing` plus media
clock advance evidence; do not wait for a nonexistent video frame. Manifest
parsing, segment transfer, readyState or `p.started` alone do not complete
startup. In the deployed player, `playing` can set `p.started` without a
presented video frame.

On completion, retire the startup expiry and diagnostic timers, cancel any
unexecuted startup retry reservation, and hand recovery to the existing
established-playback path. Cancellation here does not mint another credit;
an unused credit stays unused, while a reserved/dispatched one stays spent.
The transport adapter stops applying the startup deadline/dispatch ceiling
after this handoff but retains normal ownership checks. Do not abort healthy
media requests at 40 s or apply that old deadline to a later fragment error.
Replace startup watchdog short-circuits based solely on `p.started` with this
predicate; a `playing` event without presentation must still reach truthful
exhaustion. Do not create a second frame observer if existing evidence suffices.

### 3.4 Cancel old work before it can change the picture

Retry scheduling and execution both require current ownership. Recheck after
every await and before `loadSource`, `startLoad`, attach, seek or play. Pause,
seek, close, quality/audio/subtitle change, replacement, terminal state and
manual retry invalidate pending actions. Teardown clears the timer, even if
the callback also guards itself.

Pause preserves the resume target and prevents play. The absolute deadline
continues to age while paused; no startup exhaustion surface interrupts the
pause. The resume transition below makes the outcome explicit. A reserved
credit identifies an action that has not dispatched; once dispatched it is
spent even if pause interrupts the request. Retain this distinction without
replenishing credits on repeated toggles.

| Transition | Required behavior |
|---|---|
| Pause before a reserved application retry sends | Cancel timer and internal loader work; retain reservation and original due time, target, counters and deadline; enter `paused` |
| Resume that reservation before deadline | Rebind to the new intent generation; schedule the same action after `max(0, original due time - now)`, recheck ownership and dispatch ceiling, then mark `dispatched` |
| Pause during an initial load while application credit is unused | Abort work; on resume before deadline reserve the one corrective source reload using §3.2; do not call ineffective `startLoad` with no manifest |
| Pause after the corrective reload has dispatched, with no presentation | Abort work; the credit remains spent. On resume show truthful interrupted-startup exhaustion with Try again/Close; no third source-load cycle |
| Resume after original startup deadline | Show startup timeout with Try again/Close without sending a request or forcing play; only Try again starts a new attempt |
| Seek/track/quality replacement or Close | Cancel the old episode and its loaders completely; any new attachment owns its own state |
| Pause after startup completed | Established-playback intent semantics apply; the startup timer does not return |

If manifest/media loading was aborted while paused, do not pretend a
`playing` call will restart an unloaded manifest. Resume must execute the
specific transition above. The conservative exhausted action after an
already-dispatched corrective reload is intentional: it is usable and
bounded without introducing hidden automatic credit. A successful response
from an old hls instance must not clear a current refusal or lower a stopped
surface. Test repeated toggles on both sides of the dispatch boundary.

## 4. Diagnosis — say what failed, not what a probe guessed

Replace the method-based `isTranscode` diagnostic argument with evidence
about actual transport and resource phase. A remux is HLS in this incident;
the method name cannot decide whether a playlist or segment was transferred.

Use the current loader's observations as the primary startup diagnosis.
A diagnostic fetch is optional supplementary reachability evidence; it must
not race a player reload or silently become the only operation that fetches
the manifest. Prefer removing that fetch from the HLS startup verdict.
If retained for progressive/native paths, preserve its one eight-second
abortable deadline, cancel its body, and fence its result by attachment.
Do not extend the startup deadline to wait for it.

| Evidence at exhaustion | Viewer text / action | Forbidden inference |
|---|---|---|
| Current typed startup/publication timeout | “The stream did not become ready in time.” Show server detail; Try again/Close | Unsupported codec |
| Manifest failed, generic HTTP/network error | “The stream playlist could not be loaded.” Preserve status/code if safe | Extension or FFmpeg failure without evidence |
| Manifest parsed, media unavailable | “The video data did not arrive in time.” | Manifest HTTP 200 means media arrived |
| Media transferred, no frame, no decoder error | “Playback did not start.” Report uncertainty and offer retry | Guaranteed decoder failure |
| Actual decoder/media rejection | Existing decoder failure/fallback path | HTTP failures are decoder evidence |
| Explicit authentication, terminal or owner verdict | Existing typed action and explanation | Retryable startup overrides authoritative terminal state |

In the deployed surface vocabulary, `decoder_failed` must not be the generic
startup-exhaustion event. Add or reuse a correctly typed startup exhaustion
row in the surface policy and its shared fixture; audit consumers before
changing that fixture. Keep the established rule: the recovery owner stops
the player, then the presenter renders the stopped surface. Presentation
code does not acquire restart authority. The deployment has a
`playback-surface-contract.json` fixture and corresponding test under the
playback test directory; verify their current paths on the implementation
base, as they are absent from this documentation checkout.

Bind refusal capture to the request and current attachment. Manifest success
can clear that manifest's retryable failure; it cannot clear a terminal
segment refusal, an unrelated request's failure or another generation's
error. A subsequent older XHR completion cannot overwrite a newer terminal
diagnosis. Preserve safe text escaping and existing client-log size/rate limits.

## 5. Server contract — distinguish readiness from authority timeouts

Re-verify the following in
[http/hls.rs](../../crates/plurxd/src/http/hls.rs) and
[transcode.rs](../../crates/plurxd/src/transcode.rs):

```rust
const RESPONSE_PUBLICATION_LIFECYCLE_BUDGET: Duration = Duration::from_secs(5);
async fn exact_hls_context(
    state: &AppState,
    session: &str,
    context: crate::transcode::HlsContext,
) -> crate::transcode::HlsContext;
fn response_publication_timeout() -> ApiError;
```

The five-second cap remains a control/publication bound. Do not increase it
globally to make a cold file pass. Do not return an unvalidated master with
guessed HEVC/Dolby Vision metadata, bypass the first-media handoff, or remove
durable route/actor/producer-attempt checks.

Refactor init inspection to distinguish **not ready under the admitted
owner**, **ready and inspected**, **invalid or unsupported ready bytes**,
**read failure**, **producer/session terminal**, and **authority unavailable**.
Thread the existing absolute request
deadline into the helper; no nested operation may obtain a fresh full budget.
Reuse the existing typed `startup_timeout` body only for confirmed pending
init/media readiness, with a message naming initialization readiness.
Keep `response_publication_timeout` for a deadline whose authority phase or
cause cannot be established. Both remain retryable to the web startup policy.

Do not infer a healthy producer from an expired timeout alone. If deciding
between readiness and terminal needs another authoritative read, it must use
the remaining request budget; when there is none, return the honest generic
publication timeout. No extra query after expiry is required for nicer text.
Retain current terminal/replacement/owner-transition classification and final
publication fencing on success. Inspect rolling and VOD retrieval separately;
the helper must not erase a VOD terminal into a scanner-codec fallback.

Use this explicit result mapping. The new response codes below are additions
to the existing HTTP `{code, message}` error shape, not new control-protocol
fields. Document them in [API.md](../API.md) with the implementation.

| Inspection result | HTTP outcome | Authority and recovery rule |
|---|---|---|
| Codec outside the helper's HEVC/Dolby Vision inspection scope | Existing context; normal success path | Preserve current codec behavior and final publication authorization |
| Complete, bounded init with valid required codec records | Derived exact context; normal success path | Final route/attempt/handoff checks still precede bytes |
| Init or required first complete segment absent/incomplete, exact owner known active | 503 `startup_timeout` | Retryable readiness, no claim of decoder or producer failure |
| Ready/published bytes are truncated, malformed, missing required codec records or exceed the inspection size limit | 502 `hls_init_invalid` (new) | Terminal for this request/recipe; never return a guessed master or retry it as pending |
| Complete init is valid but its codec-record form cannot be interpreted by the supported inspector | 502 `hls_init_unsupported` (new) | Explain unsupported inspection, not corruption or client decoder failure; no guessed master |
| Storage read fails while inspecting an admitted object, without an authoritative terminal verdict | 503 `init_inspection_unavailable` (new) | Retryable transport/storage uncertainty within existing budgets; sanitized detail, no source path |
| Actor has accepted producer/session failure or session ended | Existing admitted terminal error (`producer_failed`, `session_failed`, `media_session_ended`, etc.) | Preserve exact actor verdict and existing HTTP status/action; do not re-label as a pending init |
| Owner/attempt changed during inspection | Existing fenced reclassification or owner-transition error | No bytes or init verdict from the predecessor escape |
| Deadline expires without enough evidence to classify | 503 `response_publication_timeout` | No fresh post-deadline read to manufacture a more specific verdict |

Distinguish an object not yet fully published from a fully published object
with invalid bytes; file existence or nonzero length alone is not readiness.
Use the existing complete-publication evidence. A request-specific
`hls_init_invalid`/`hls_init_unsupported` does not directly mutate the durable
session or manufacture `session_failed`. If the existing actor validation
path accepts a terminal failure, surface that authoritative verdict instead.
All typed error publication must use existing owner/attempt error fencing,
including VOD's error path; a local parse result cannot outrank a concurrent
owner change. Add both new terminal codes to the web refusal classifier so
they do not enter the startup retry loop or masquerade as browser decoding.

This server work improves cause attribution; the client must recover with
the old `response_publication_timeout` response as well, so deployment order
does not become a new failure mode. The five-second request cap and every
existing response-authorization check remain regression requirements.

## 6. Diagnostics — retain enough to explain the next cold startup

Use existing tracing and client-log/playback-event plumbing. Record bounded
events at phase transitions, not one event on every poll or packet.

| Layer | Required facts |
|---|---|
| Server | Hashed session identity, file ID, producer attempt, build, rolling/VOD, start coordinate, source-probe duration/outcome, init-ready elapsed time, first-complete-media elapsed time, request phase and deadline exhaustion |
| Client retry | Attachment/attempt correlation, resource class, typed status/code, manifest parsed flag, chosen action (`reload_manifest` or `resume_loading`), ordinal, elapsed/remaining startup budget, executed or cancelled reason |
| Client exhaustion | Manifest/media/decoder/presentation evidence separately; latest owned refusal; total requests and application retries; final surface cause |
| First frame | Attachment TTFF and item-open elapsed time separately; original target and observed film position |

Log a reload as requested when the executor calls it, and log the actual
manifest request/load event separately. Never label an API invocation a
successful recovery. Keep `bw=500kbps` before transfer recognizable as the
library's initial estimate, not a measurement of a slow link.

Reuse `session_log_id` on the server. Do not log raw capability URLs, query
tokens, bearer tokens, cookies or full media paths in new evidence records.
Do not copy full production logs into the repository. The sanitized timeline
in §1 is the retained incident receipt; preserve the local synthetic repro
as a proper regression in §7 rather than depending on temporary files.

## 7. Work orders — each ends with observable acceptance

### 7.1 S01 — establish the base and reproduce both defects

**Edit:** use an isolated current-main checkout; record its SHA and compare
the named deployed functions. Read repository instructions and establish the
pinned Rust **1.97.1** compile loop before touching Rust, following
[AGENT-COMPILE-LOOP.md](../ci/AGENT-COMPILE-LOOP.md). No compiler is required
for this documentation-only task.

**Implement:** retain a vendored-library loader regression, then run the
actual application's retry helper against that library with synthetic 503s.
Extend the existing shipped-source harness in
[web-policy.test.js](../../tests/playback/web-policy.test.js). Add a small
dedicated manifest-startup test file if library/timer setup would obscure
the existing suite. The test must load the repository's vendored library,
never an npm download or a hand-written imitation of `startLoad`.

**Acceptance:** the old application records a retry without making a new
manifest request; the old diagnostic labels a successful playlist probe as
decoder failure. Tests assert externally visible requests/state/cause, not
function names or the number of calls to a mocked `startLoad`.

### 7.2 S02 — implement owned, bounded manifest recovery

**Edit:** web attachment executor, policy and existing teardown/intent paths.
Implement §3, integrate the explicit manifest policy and preserve the
existing fragment policy. Cover both cold-open and seek-open deadlines.

**Acceptance:** controlled startup delays of 0 s, 6 s, 19 s and 30 s can
recover within a cold attachment's 40 s budget when responses and first
media are otherwise prompt, for both immediate-503 and five-second-held
responses. Assert dispatch timestamps, not just eventual success. A seek
whose media is not ready within 20 s
ends truthfully. Always-unavailable transport terminates; close/pause/seek
prevents stale retries; one session and original resume/track selection stay
intact on same-source recovery. One application retry remains shared with
post-manifest network recovery.

### 7.3 S03 — correct diagnosis and surface ownership

**Edit:** `stallDiagnose`, probe callers, refusal ownership, surface policy
and its contract tests. Implement §4, including remux HLS and progressive
paths. Do not edit unrelated player UI work in the documentation checkout.

**Acceptance:** the incident's typed 503 survives into a truthful startup
exhaustion explanation if recovery fails. Playlist 200 with zero media
does not emit `decoder_failed`; actual decoder failures retain their normal
fallback. The owner stops once, and Try again/Close remain usable.

### 7.4 S04 — attribute server phases without weakening fencing

**Edit:** init/context inspection and tracing at existing publication phases;
client timing/retry evidence. Implement §§5–6 and focused Rust regressions.
If existing APIs already carry a terminal result, preserve it instead of
adding an alternate source of truth.

**Acceptance:** missing init within the budget yields pending or honest
generic timeout; terminal/owner change never becomes a successful master.
Concurrent status/other-session requests remain responsive. Internal codec
inspection is excluded from client throughput accounting. No sensitive URL
is introduced into logs. Old-server/new-client interoperability passes.

### 7.5 S05 — run the regression matrix and retain qualification evidence

| ID | Fault injection / transition | Assertion |
|---|---|---|
| T01 | Vendored library, initial manifest 503 then valid manifest | Actual new request and parsed manifest after application recovery |
| T02 | Real application helper, default built-in retries exhaust | Corrective reload occurs at most once; budget survives reload |
| T03 | Immediate 503, five-second-held 503, dropped connection, first-byte timeout, stalled response body | Assert each dispatch timestamp; no more than 16 manifest sends; active-startup work stops at the absolute deadline |
| T04 | Delays 0/6/19/30 s, permanent 503, ready just before/at/after deadline | Named prompt-media cases recover; boundary allows truthful exhaustion; overdue internal retry run before the expiry callback still sends nothing |
| T05 | Close/pause/seek/track change/manual retry during pending delay or response | No old attach, play, release, error overwrite or extra request; verify every §3.4 pause/resume row and repeated toggles |
| T06 | Start at 3827.549 s in rolling and immutable VOD | First presented film position is within 1 s of intended start; no jump to film zero |
| T07 | Real presentation, playback continues beyond 40 s, then fragment/level failure with buffered video | Startup deadline retired; existing loaded-stream recovery and shared remaining credit; no destructive manifest reload |
| T08 | 401/403, terminal JSON on the first 503, owner transition/loss, malformed 503 body | No built-in or app retry for auth/terminal response; correct owner action; generic network wording for unstructured body |
| T09 | Manifest 200, zero media; media loaded without frame; actual decode error | Three distinct evidence states and truthful surfaces |
| T10 | Old successful XHR arrives after a new failure/attachment | Current typed refusal and surface remain intact |
| T11 | HEVC/DV init pending, malformed/missing codec record, oversized/truncated ready object, unsupported valid record, read error, producer exit, owner change mid-read | Assert every §5 HTTP/code result for rolling and VOD; five-second cap and exact authority; no guessed successful master for invalid bytes |
| T12 | VOD-ready versus index-pending rolling fallback | Both paths pass; no dependence on index rebuilding |
| T13 | Startup recoveries repeated across many attachments | Timers/listeners/loaders settle; bounded current state and telemetry |
| T14 | Native-HLS and progressive browser paths | No hls.js calls on native instances; existing intent/actions still work |
| T15 | Old server response plus new web; updated server plus existing clients | Typed timeout compatibility; unchanged strict control wire shape |
| T16 | `playing` without video presentation; valid frame/clock fallback; audio-only playback | No premature startup completion; actual presentation retires the deadline; audio does not wait for video frames |
| T17 | Stock internal retry admitted, then pause/close before asynchronous XHR setup settles | Final send gate prevents dispatch; actual stock retry timers and loaders are destroyed, not merely the app timer |

Use fake monotonic time for episode/boundary tests and real vendored-library
behavior for loader tests. A browser case must run the shipped page with a
small playable fixture and a controlled delayed-manifest endpoint, proving
first-frame/film-position behavior rather than merely `MANIFEST_PARSED`.
Keep the production QNAP path out of deterministic tests. Do not flush caches
or disturb someone else's stream to simulate a cold disk.

Suggested commands below name existing runners. Any new test file must be
wired into `web-check` and the applicable validation catalog. Rust test names
with prefix `web_hls_startup_` are **new tests to add**, not existing evidence.

```bash
node tests/playback/web-policy.test.js       # shipped policy/executor harness
node tests/playback/web-control.test.js      # ownership/control interactions
scripts/js-check                            # embedded JavaScript syntax
make web-check                              # affected web contracts
python3 -m unittest discover -s tests/operations -p test_docs_index.py
rustc --version                             # must report Rust 1.97.1
cargo check -p plurxd --all-targets --locked
cargo clippy -p plurxd --all-targets --locked -- -D warnings
cargo test -p plurxd --bin plurxd web_hls_startup_ --locked
cargo fmt --all -- --check
```

Record actual browser command, test names, fixture hash, base/head SHA,
server build, vendored hls version, browser version and pass/fail/blocked for
each T row. A filtered test invocation selecting zero tests is not a pass.
Re-run affected evidence on the exact intended branch after base changes.

## 8. Delivery — prove the candidate before publishing it

The documentation task ends with this reviewed plan and an index row.
Implementation uses normal commits and the current contributor workflow;
follow the explicit session/AGENTS instructions if older pipeline prose
disagrees. Read [DEVELOPMENT_PIPELINE.md](../DEVELOPMENT_PIPELINE.md) for the
compiler and PR conventions; do not use CI as a compiler or assume merge
deploys an image. The historical lifecycle campaign's deferred-test policy
does not silently defer this new repair's focused evidence.

Implementation may be one coherent PR with the S work orders as commits.
The later product diff still needs its own required adversarial review; this
plan review is not a review of code that does not yet exist. Update
[PLAYBACK.md](../PLAYBACK.md), [OPERATIONS.md](../OPERATIONS.md), and
[PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md) where their behavior or commands
change, plus the docs index in the same commit as any added document.

After authorized deployment, verify one normal warm start and one controlled
delayed start on the stamped candidate, then a real Child's Play resume on
nynuc when it will not interrupt a viewer. Record actual rolling/VOD and
first-frame position. Deterministic replay establishes the software repair;
a warm production success alone does not prove cold-start recovery.

Rollback the release through the established deployment procedure if retries
continue after cancellation/deadline, paused viewers are forced to play,
resume positions change, terminal/auth errors loop, or publication fencing
regresses. Preserve sanitized evidence before rollback. Do not roll back by
loosening server authority checks or increasing retry/buffer limits.

## 9. Completion ledger — implementation remains separate from this plan

| Deliverable | State on 2026-09-14 |
|---|---|
| Incident log and deployed-source investigation | Complete; sanitized facts in §1 |
| Synthetic deployed hls.js `startLoad` reproduction | Passed during diagnosis; proper repository regression still required |
| Detailed implementation contract | Ready for implementation; corrected after review |
| Independent adversarial review and author dispositions | One review completed; all five P2 findings addressed in §10 |
| Product code, Rust compile evidence, application/browser regressions | Not implemented / not run for this repair |
| Production deployment and cold-start acceptance | Not performed |

## 10. Adversarial review — findings and their disposition

**Review performed:** 2026-09-14 by independent agent
`adversarial_startup_plan`, against the first draft and the exact deployed
source/library plus the local source. The reviewer returned five P2 findings;
no P1/P3 findings were reported. All were accepted. There was one review
round; the author checked and incorporated the corrections below. This is
closure of design findings, not independent verification of future code.

| Finding | Concrete failure in the draft | Author correction and required proof | Disposition |
|---|---|---|---|
| R1 · P2 · Retry coverage | Immediate 503 requests ended at 16 s; readiness at 19/30 s was never sampled | §3.3 now gives explicit 0/1/3/7/11/15/19/23 s timing, one corrective cycle, a 16-send ceiling and deadline-boundary caveat; S02/T03/T04 require both fast and held-response tests | Addressed in plan |
| R2 · P2 · Built-in retry authority | hls.js could send an overdue internal retry before the app's timer, or retry terminal JSON before a fatal callback | §3.3 names a stock-loader adapter with final-send gating, pre-retry typed-body interception, and loader/timer destruction; T04/T08/T17 exercise those races with stock scheduling | Addressed in plan |
| R3 · P2 · Startup completion | No completion transition could kill healthy play at 40 s; `playing`/`p.started` could also hide a never-presented frame | §3.3 defines current-attachment presentation, audio fallback, retirement of startup work and handoff to established recovery without fresh credit; T07/T16 cover both errors | Addressed in plan |
| R4 · P2 · Pause/resume credit | A cancelled reservation could strand an unloaded manifest; long pause had no deadline rule | §§3.1/3.4 distinguish unused/reserved/dispatched credit and define resume before/after expiry, interrupted reload and repeated toggles; T05 checks all rows | Addressed in plan |
| R5 · P2 · Invalid init result | Arrived but malformed/unsupported bytes had no result, allowing guessed masters or permanent retryable readiness | §5 maps every inspection result to HTTP/code and preserves actor/error fencing; T11 asserts rolling and VOD behavior for every category | Addressed in plan |

**Author validation:** the existing four documentation-index tests passed
after adding this document, and explicit relative-link checks cover this
untracked file as well. Repeat those checks after any revision. Application
and browser tests in §7 remain work to implement and execute; none are
claimed to have passed merely because this plan names them.
