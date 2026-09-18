# Wicked playback — root cause evidence and proposed callback lifecycle fix

**Status:** open, Fable findings addressed; awaiting CI qualification · **Written:** 2026-09-17 ·
**Candidate:** `codex/wicked-native-progress`, including the Fable follow-up ·
**Previously reviewed code:** `c8abdf7fa` · **Base:** `2b9146233`

Companion to [PLAYBACK.md](../PLAYBACK.md) (delivery architecture) and
[PLAYBACK-TESTING.md](../PLAYBACK-TESTING.md) (playback validation). This is the
review packet for [PR #358](http://192.168.4.7:3000/noirr/plurx/pulls/358),
branch `codex/wicked-native-progress`. It describes the final net change,
including the buffering and Fable follow-ups. The PR remains unmerged; nothing has been deployed. Review the diagnosis independently of the proposed code.

## 1. The failure is missing browser frame delivery

Cold-resuming Wicked in Safari loaded the native HLS stream and sought to
the saved position. Media time and browser frame counters advanced, but the
browser never invoked the registered `requestVideoFrameCallback` callback.
Plurx therefore received no callback-based evidence of presentation, and its
existing progress detector initiated stall recovery after its observation
window expired.

**These callbacks are local browser API callbacks:** the browser invokes
JavaScript registered on the video element. The reproduced defect is at
that boundary. It is not evidence that an HTTP callback from the browser to
the server was lost. Server telemetry and control reporting are downstream
concerns; this change does not modify their transport.

The application-level diagnosis is that Plurx registered a frame request
before initial media readiness/seek completion and assumed that request
would remain deliverable across those transitions. In the reproduced Safari
pipeline it remained pending indefinitely. Moving registration to after
readiness and seek completion restored actual callback delivery.

**Confidence boundary:** controlled tests establish this registration timing
failure and an effective lifecycle repair. They do not identify the precise
internal WebKit/AVFoundation defect. Registering before a seek is not claimed
to violate the browser API, nor is re-registration claimed to be mandatory
for every browser or every ordinary seek.

## 2. Preserve the existing presentation contract

The relevant functions are in the
[web player](../../crates/plurxd/src/web/index.html):

| Function | Responsibility |
|---|---|
| `queuePlaybackFrame(v,p,callback)` | Register the next browser frame observation for a player and element. |
| `armHitchDetector(v)` | Consume delivered callbacks, update presentation counters, and queue the next observation. |
| `playbackProgressTick(v,p)` | Evaluate clock and presentation progress on the existing player tick. |
| `markPlaybackControlSeekExecuted(...)` | Advance the presentation epoch for an executed seek. |
| `closePlayer()` / `adoptPlaybackMediaElement(...)` | End or transfer player/element ownership. |

When the browser supports frame callbacks, `controlPresentedFrames` is the
frame evidence used by the progress detector. An advancing media clock
alone is insufficient: with video frame evidence available, both clock and
frames must advance. Zero delivered callbacks must not silently become
successful presentation. The existing persistent-stall window is 8,000 ms,
with existing ownership, active-playback, pause/background, and pending-seek
rules still applying.

The proposal preserves `playbackProgressTick` byte for byte relative to the
base. It changes how the observation is subscribed, not what counts as
progress. Neither `loadeddata`, `seeked`, nor `canplay` counts as a presented
frame. Readiness permits registration; only a delivered callback supplies
callback-based presentation evidence.

An earlier proposal on this branch substituted browser frame counters when
callbacks were silent. That approach was withdrawn and is absent from the
net diff. It could conceal the broken observation path instead of repairing
it. Historical commits remain in the branch, so review the net diff against
`2b9146233`, not an intermediate counter-fallback commit.

## 3. Controlled playback tests isolate registration timing

The reproduced environment was Safari 27.0, build `22625.1.29.11.27`, using
the web UI on `nynuc:32400`. Wicked is library file 120, using native copy
HLS with 4K Dolby Vision P7-to-P8 delivery. The controlled resume position
was 566.055 seconds. The server remained at build
`1e530d32c87df15a2d2ded10b46c185c07cdb832` throughout these trials.

Instrumentation wrapped native frame registration and cancellation, then
recorded callback invocation, element identity/ownership, media events,
readiness, seek state, clock, frame counters, and detector state. This
observed the native callback boundary independently of Plurx's frame counter.
The detector retained its original behavior in all trials below.

| Trial | Intervention | Observation | What it establishes |
|---|---|---|---|
| Fresh-page cold resume | Original registration behavior | Handle 1 registered at media time 0 with `readyState=0`; it never fired. A separate raw request added at `playing` also never fired. The detector fired while the clock advanced. | Failure exists below Plurx's callback handler and is reproducible without an older player instance. Adding another request without clearing the pending one did not restore delivery in this trial. |
| Clean-page rearm | Cancel the one pending handle and register on `seeked` | New handle registered at `readyState=2`, then delivered about 1.45 seconds later; hundreds of callbacks followed without recovery. | Cancellation and re-registration after the initial seek restore the observation stream. |
| Clean-page deferral | Make no native request until current data exists and seeking has ended | First registration delivered about 1.45 seconds later; hundreds of callbacks followed without recovery. | Avoiding the premature registration is sufficient; a periodic repair mechanism is unnecessary in the reproduced case. |
| Proposed lifecycle function | Inject the exact replacement function, resume at 566.055 seconds, then seek to 1,200 seconds | More than 11 minutes of playback; 16,155 callbacks at export, no pending seek, zero dropped frames in the captured samples, no observed recovery. | The replacement registration lifecycle works across cold resume and a subsequent seek in this Safari session. |

The long trial preceded the review follow-up that adds `canplay` as another
readiness event. Final Safari cold-resume, paused-seek, and buffer-recovery
checks are recorded in §5; the final revision has not had another 11-minute
soak test.

The long trace uses bounded buffers: its export retains the last 3,000
request records and roughly five minutes of periodic samples, plus media
events and the cumulative callback count. It is not a complete per-frame
record of all 11 minutes. Temporary page instrumentation was removed and the
page reloaded after testing; the server was never patched for the trial.

Earlier tests of Start Over at zero and an ordinary seek during established
playback were healthy. The demonstrated trigger is the initial load/resume
seek sequence, not a universal pause/resume or seek failure.

**Local evidence provenance.** Raw traces remain on the investigation Mac
under `/private/tmp/wicked-native-fixture/`:
`fresh-page-cold-resume.json`, `clean-rearm-on-seeked.json`,
`defer-first-registration.json`, `exact-callback-lifecycle.json`, and
`exact-callback-lifecycle-long.json`. These are local diagnostic artifacts,
not committed fixtures or remotely accessible PR attachments. The table
above provides the reviewable observations without requiring those files.

## 4. The proposed fix owns one subscription through media transitions

The candidate changes three files: the
[player](../../crates/plurxd/src/web/index.html),
[playback regression tests](../../tests/playback/web-control.test.js), and
[corrective-history mappings](../../tests/client-fixes.toml).

The subscription retains its player, element, original presentation epoch,
native request handle, and cancellation generation. Its transitions are:

| Trigger | Action |
|---|---|
| Queue an observation | Retire the player's and element's prior subscription; capture the original presentation epoch. |
| Element has current data (`readyState >= 2`) and is not seeking | Register exactly one native frame callback. |
| Element is not ready or is seeking | Wait on media lifecycle events; do not claim presentation. |
| `emptied` | Cancel the pending native request and invalidate deliveries from that registration. |
| `seeking` with an already registered request | Preserve the request so it can observe the landing frame, including during a paused seek. New registration remains deferred while `v.seeking` is true. |
| `loadeddata`, `seeked`, or `canplay` | Re-evaluate readiness; register only if no request is pending. |
| Native callback arrives | Reject retired/cancelled generations; clean up the subscription; deliver the callback with its original epoch if the player still owns it. The existing detector queues the next observation. |
| Player close or subscription replacement | Cancel the request and remove all lifecycle listeners. |

The epoch remains the epoch of the original logical observation, including
while registration is deferred. This preserves the existing fence against
letting a predecessor observation settle a new seek. A subsequent observation
captures the current epoch. Fable should assess this choice explicitly,
especially for seeks that land while paused.

**Architectural decisions and trade-offs:**

1. **Repair the registration boundary.** Keep the detector's evidence contract
   intact. No timer, polling loop, movie identifier, codec-specific branch,
   or new watchdog is introduced.
2. **Apply lifecycle ownership to the shared frame subscriber.** Both native
   HLS and other browser playback routes use it. This avoids a Safari-only
   alternate progress model, but requires checking other routes for regressions.
3. **Fence cancellation as well as calling the browser cancel API.** A
   callback already queued for delivery must not contribute evidence after
   its subscription was retired.
4. **Remove listeners on delivery/retirement.** This keeps lifetime bounded.
   The final implementation adds/removes four event listeners per observation.
   Fable measured the preceding five-listener version at 7.6 microseconds
   per observation in Chromium versus 2.5 for base; this is reviewer-provided
   evidence, not a Safari performance measurement.

## 5. Review findings and their disposition

The fresh review agent identified that a deferred registration could wait
forever after buffering if it listened only for `loadeddata` and `seeked`.
`loadeddata` is emitted for initial current-data readiness; replenishing a
buffer does not require emitting it again. `canplay` covers renewed playback
readiness under the
[HTML media readiness contract](https://html.spec.whatwg.org/multipage/media.html#ready-states).

A regression modeled registration being deferred at `readyState=1`, then
buffer recovery to state 3 with `canplay` and no second `loadeddata`. It
failed against `b217c615e`: no new request was registered. Commit `85b177a1b`
adds the corresponding `canplay` listener and cleanup; the test passes. It
also checks that repeated readiness events cannot create duplicate requests
or count as presentation, and that the listener is removed after delivery.

The first agent hit its usage limit before finishing its review. Fable then
reviewed `c8abdf7fa` independently in a private clone and returned **APPROVE
WITH CHANGES**. Fable ran the focused suites, 20 source mutations, and real
Chromium file-playback trials. Those results belong to the preceding
candidate, not an approval of the follow-up implementation.

**Cancel-on-seek removed.** Fable observed that cancelling on every `seeking`
lost the sole paused landing frame in Chromium. The Safari evidence only
justified deferring an initial request, not cancelling an established one.
The final subscriber therefore preserves an existing request through an
ordinary seek. The regression now fails if this cancellation is restored;
initial-seek deferral and source-reset cancellation remain intact.

**Ownership and cleanup fences pinned.** New tests independently exercise
callback delivery after `PLAYER` replacement on the same video element,
source reset after owner replacement, and element reuse by a new player with
no prior player-side cancel handle. A close-path assertion pins explicit
retirement because `PLAYER` survives close. Removing each respective owner
check, element retirement, or close cancellation fails its test. Restoring
both the old seeking listener and its cleanup also fails the paused-landing
regression. The redundant `retired` half of the native delivery fence was
removed: cancellation already advances the generation. The readiness path
still checks retirement.

**Final Safari checks completed.** The exact final subscriber, including
`canplay` and removal of cancel-on-seek, was injected into the production
page for these trials:

| Check | Result |
|---|---|
| Wicked native-HLS cold resume at 566.055 seconds | 1,176 callbacks at 615.242 seconds, zero captured errors and zero fired detector samples. |
| Paused seek to 650 seconds | One landing callback: counter advanced from the seek's frame floor of 1,665 to 1,666. Video stayed paused at the destination. The existing pending-seek settlement limitation remains, as Fable predicted; no epoch or settlement rule changed. |
| Native-HLS buffer recovery | Passed in an isolated local native-HLS fixture using the exact final subscriber. Buffer depletion produced `waiting` at media time 26.357 seconds, ready state 2, and 631 callbacks. The fixture released withheld segments after 2 seconds; `canplay` and `playing` followed 2.064 seconds after `waiting`, at ready state 3. Callback delivery resumed to 749 at export; `loadeddata` fired only once, at startup. |

The buffer test used a minimal callback consumer around the exact production
subscriber, not the full production progress detector. It proves real Safari
callback delivery survives buffer depletion/replenishment; the regression
test separately pins the deferred-registration path at ready state 1. Its
first run released the gate on a startup event and was discarded. The
corrected run excluded startup waiting and used a fresh local origin.

The production page was restored, the temporary test tab closed, and the
local buffer server stopped.
Final cold-resume and paused-seek traces are local files
`fable-final-cold-resume.json`, `fable-final-paused-seek.json`, and
`fable-final-buffer-recovery.json` in the same fixture directory as the earlier
evidence.

**Separate follow-ups.** Fable identified a raw frame registration in
`preparedFirstFrame` that bypasses the shared subscriber. Interference with
the adopted element's subscription is a hypothesis, not a reproduced
failure, and is outside this patch. Paused-seek settlement also remains an
existing limitation. Neither is silently claimed repaired here.

## 6. Validation and remaining review work

These commands passed on the candidate, including the buffering follow-up:

```bash
node tests/playback/web-control.test.js       # Callback lifecycle and control regressions
node tests/playback/web-policy.test.js        # Playback policy regressions
node --test tests/playback/seek-control.test.js # Seek control regressions
make history-check                           # Corrective-history evidence mapping
CARGO_TARGET_DIR=/Users/pjunod/code/plurx/target CARGO_NET_OFFLINE=true \
  make precommit-check CARGO='rustup run 1.97.1 cargo'
git diff 2b9146233 --check                     # Patch whitespace validation
```

The normal commit hook also passed catalog checks, formatting, Clippy, and
embedded JavaScript syntax checks. Rust 1.97.1 was verified; there are no
Rust source changes. The new initial-registration regression rejects the
old implementation. Tests also exercise stale callback delivery, source
reset, replacement, element adoption, cleanup, and epoch ownership.

These are focused results, not a claim that the full web suite or CI passed.
The PR has not been qualified for merge. Fable supplied Chromium file-playback evidence for the preceding candidate;
MSE/HLS.js and Live TV were not run. The final Safari cold-resume and paused
seek and buffer-recovery results are in §5. All three Safari checks requested
by Fable are complete; CI qualification and merge are separate steps.

**Requested Fable review:**

- Does the controlled evidence justify the application-level diagnosis, or
  is another experiment needed to distinguish initial load from initial seek?
- Does the lifecycle belong in `queuePlaybackFrame`, and do cancellation,
  owner replacement, and element adoption cover every production caller?
- Can any valid readiness/event ordering leave the subscription dormant,
  duplicate it, or permit an obsolete callback to count as presentation?
- Is retaining the original epoch across deferred/rearmed registration
  correct for both playing and paused seeks? Inspect the real settlement
  callers, not only the isolated test harness.
- Is cancelling on every seek appropriate across native HLS and MSE, given
  that ordinary seeks already worked in the baseline?
- Are the tests sufficiently independent of the implementation, and which
  browser acceptance cases should block merge?
- Is the per-frame listener setup/teardown acceptable, or should subscription
  ownership have a longer lifetime without weakening its cancellation fences?

Review the net implementation with:

```bash
git diff 2b9146233 HEAD -- crates/plurxd/src/web/index.html \
  tests/playback/web-control.test.js tests/client-fixes.toml
```

## 7. The separate HTTP 503 remains unresolved

The investigation also observed an earlier server failure with the message
`media-session response publication lost its exact activation`. That is a
separate server publication/activation problem. The captured historical
state did not establish which exact publication predicate failed, and later
bounded startup tests did not reproduce that failure. A later playlist GET
also returned 503; a different failing endpoint is not proof of the same cause.

This PR contains no server-side fix and does not establish that every original
Wicked error is resolved. Callback lifecycle repair should be evaluated on
its own evidence. Keep the separate publication failure open rather than
expanding the callback watchdog or claiming the 503 was fixed by this patch.
