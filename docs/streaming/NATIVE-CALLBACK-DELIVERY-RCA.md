# Native callbacks — a pending subscription survives a completed seek without delivering

**Status:** open; subscription-lifecycle root cause established, fix proposed ·
**Written:** 2026-11-02 EST · **Runtime:** `8a212be7021f` ·
**Browser:** Safari 27.2 / WebKit `22625.2.8.11.9`, macOS 27.2.

Companion to [the consolidated retest RCA](COLD-PLAYBACK-AC3-RETEST-RCA.md)
and [the canonical startup proposal](COLD-PLAYBACK-STARTUP-RCA-AND-PROPOSAL.md).
This resolves the native callback silence left open in the earlier packet.
The proposed lifecycle change has not been implemented in production.

The native load/seek left an outstanding frame-callback subscription
nonoperational. The app kept its returned request ID across `seeked` and
`canplay`, so it never renewed that subscription after the native position
became ready. The browser clock and decoded frames progressed, but the app's
callback receipt counter did not. All subsequent presentation and seek proof
waited on that same subscription, eventually producing the buffering/error
behavior documented in the Resume RCA.

Canceling the pending subscription and registering anew after the transition
restored callbacks on the running stream. One complete raw trace recovered
its first callback in **32 ms**. An independent **rearm-only** repeat, with
no CSS or diagnostic-probe changes, recovered **36 app callbacks in 1.504 s**,
settled the seek, and continued to at least 53.9 s. This is a causal
subscription-recovery result, rather than another timeout classification.

## The pending request is retained across the native seek boundary

The raw trace records native request/cancel/delivery calls before the app's
callback handler, together with video/player/attachment identity, source
identity, media readiness, seeking state and app receipt counters. It also
records native media events and actual `currentTime` assignments. The wrappers
forward arguments, results and callback metadata unchanged; callback/request
exceptions are recorded and rethrown. Request and delivery detail is bounded,
with per-generation counts retained. No media payload or authentication was
read by this instrumentation.

For native attempt `a5`, the ordered events are (UTC, November 2; EST is five
hours earlier):

| UTC | Observed event |
|---|---|
| 22:32:24.218 | `loadedmetadata` |
| 22:32:24.219 | App assigns element clock 0.059 s; native seeking begins |
| 22:32:24.774 | `loadeddata`: app request **4595** registers at readyState **2**, **seeking=true**, decoded count 0 |
| 22:32:24.817 | `seeked`, 43 ms after registration; 4595 remains pending |
| 22:32:26.150 | `canplay`; 4595 is still not renewed |
| 22:32:27.707 | Clock 1.396 s, decoded count 42, app receipts frozen at **2282**; independent request 4596 added |
| 22:32:28.711 | Probe has no delivery; cancel 4596. App request 4595 remains pending |
| 22:32:29.715 | Cancel 4595; known pending list becomes empty. Existing app rearm registers **4597**, readyState **4**, **seeking=false** |
| 22:32:29.747 | First raw delivery for 4597, **32 ms** after registration |
| 22:32:31.219 | App receipts **2318** (+36), seek settled, playback active |

Request 4595 remained undelivered for **4.941 s**, including after native
seeking had completed. The same current video, player, attachment, source and
presentation epoch span the successful renewal. The trace contains no request,
cancel or callback exception for this failure/recovery interval. The app's
observer and the current video agree on the subscription owner.

`queuePlaybackFrame` in
[decode-tiers.js](../../crates/plurxd/src/web/player/decode-tiers.js) explains
the retained request. `markPlaybackControlSeekExecuted` rearms observation
before native loading/positioning finishes. That renewal passes
`renewingSeek=true`, allowing registration while `video.seeking` is true.
The `ready` handler begins:

```javascript
if (id != null || v.readyState < 2 || (v.seeking && !renewingSeek)) return;
```

Once 4595 exists, the completed seek and later readiness event call this
handler but cannot renew it. The app treats an outstanding request handle as
a usable observation subscription. Its frame-receipt counter only advances
inside the callback, so a nonoperational request leaves the app's evidence
permanently behind actual media progress.

## The rearm-only control removes the overlay-test confound

The first raw trace also included a one-second CSS-only hide of `#ploading`.
No callback arrived during the hide. Restoring that diagnostic rule coincided
with the renewal, so the first experiment alone cannot separate those two
interventions. It is retained as supporting evidence, not the sole causal test.

The independent `a6` comparison made **no overlay change and registered no
independent probe**. It used the same existing app rearm method, after a short
negative observation interval:

| Phase | Element clock | App receipts | Decoded frames | Seek pending |
|---|---:|---:|---:|---|
| Silence detected | 1.644 s | 21,110 | 46 | yes |
| 750 ms later, before renewal | 2.396 s | 21,110 | 46 | yes |
| 1.504 s after renewal | 3.900 s | **21,146** | 91 | **no** |
| Later observation | 53.872 s | 22,345 | — | no |

The intervention issued an observer renewal; it issued no media restart,
source replacement, seek, quality change or CSS operation. Existing callback
metadata checks supplied the target-frame proof and settled the seek; the
experiment did not fabricate receipt counts or mark playback successful.
The second capture is lighter than the first: the raw request-ID sequence is
from `a5`, while `a6` records the independent state/intervention comparison.

The `a6` test temporarily held legacy recovery for 45 s and normalized
pending-native position reporting to the explicit seek target to isolate the
already-confirmed stale-predecessor-clock bug. Native startup's own deadline
was retained. Those controls are disclosed; this is not a claim that the
unmodified complete recovery lifecycle passed. The measured effect is renewal
restoring delivered callbacks within that attempt.

A separate attempt to compare registration deferred until after seeking was
refused during session creation for rolling scratch capacity. It never
attached a new native source and supplies **no acceptance evidence** for the
proposed boundary fix. Earlier manual probes that ran after playback had
paused are also excluded from the active-delivery conclusion.

## WebKit's request-list transition explains why an extra probe did not restart gathering

The installed browser version was read from the Safari and WebKit bundle
metadata. The public `WebKit-7625.2.8.11.9` tag has the matching
`625.2.8.11.9` version suffix; platform prefixes differ. It provides a
version-corresponding source explanation, rather than a binary comparison of
the installed browser.

In that source, registering a callback starts native frame-metadata gathering
when the request list was **empty**. Canceling the final **pending** request
stops gathering. Adding another request while the stranded request remains
pending does not start it again. This is why the diagnostic probe had to be
canceled before the app request for the renewal test. Canceling the app request
then registering anew crosses the stop/start boundary. [WebKit request and
cancellation implementation](https://github.com/WebKit/WebKit/blob/WebKit-7625.2.8.11.9/Source/WebCore/html/HTMLVideoElement.cpp)

The AVFoundation implementation creates an image-change observer when gathering
starts. Its callback clears the one-shot observer before checking current
pixel-buffer metadata. That check has early returns, and does not itself
rearm the observer. This supplies a source-backed explanation for a native
metadata-gathering cycle becoming nonoperational during initial positioning.
The raw JavaScript trace does not identify which internal pixel-buffer or
notification branch ran. The directly established cause and repair boundary
are the retained, undelivered subscription and successful cancel-to-empty /
resubscribe operation. [AVFoundation metadata gathering](https://github.com/WebKit/WebKit/blob/WebKit-7625.2.8.11.9/Source/WebCore/platform/graphics/avfoundation/objc/MediaPlayerPrivateAVFoundationObjC.mm)

The trace excludes app callback metadata rejection as the explanation for the
silent interval: raw delivery never reached the handler. Request ID zero,
stale callback rejection and callback exceptions have separate source fixtures;
they do not explain this observed pending request. The installed browser can
deliver native callbacks after renewal, so the fix should preserve real
callback-based presentation proof.

## Revised fix — one multiplexer per element, renew at native discontinuities

The repair follows the native request-list boundary that the trace established.
Every repository caller uses one per-element multiplexer. It holds at most one
native `requestVideoFrameCallback` request and fans each delivery out to its
subscribers. Each subscriber keeps its own generation fence, proof floor,
callback lifetime and cancellation handle. The incumbent and spare elements
have separate multiplexers. No owner-cohort negotiation is needed: a renewal
cancels the single native request and registers a fresh one on that element.

For native-src playback, renew across source attachment, `emptied` and
`loadeddata`, and across active-playback seek start. A paused same-resource
seek preserves an already armed request from the current generation so its
single landing can arrive before `seeked`; if none is pending, current data
can arm one. A source change or stale generation retains cancellation and
late-delivery fences. At the first qualified readiness after an invalidating
discontinuity, cancel the request and register a new one exactly once:

- Active playback: `!seeking && readyState >= 3`.
- Paused playback: retain the separate paused-seek path in the consolidated
  retest RCA. Current data at `readyState >= 2` can observe the single
  landing before `seeked`; subscriber target and floor checks remain intact.
  Renew a request still pending from seeking once at `seeked`. Only a
  delivered callback can prove the displayed target frame.

This is event-driven and uses no timer. Clock and decoded-frame observations
remain available for the bounded `observation unavailable` diagnosis; they do
not gate active-playback renewal. Preserve the original startup deadline,
target, attachment, presentation epoch and subscriber proof floor. Fence late
callbacks by both multiplexer request generation and subscriber generation, so
canceled deliveries cannot credit or retire replacement work.

The callback-delivery evidence is from native-src Safari/AVFoundation. The
multiplexer covers every repository callback caller, including the MSE
continuous-quality subscriber, but the renewal contract is native-only until
an MSE seek acceptance cell establishes its behavior. MSE's callback player
uses a different WebKit media path; this RCA does not claim parity there.

## Acceptance — preserve evidence while making renewal local

- Exercise the real shipped queue and native load/seek path. A request pending
  before a discontinuity must be canceled and renewed once at the first
  qualified readiness, with `readyState >= 3` and `!seeking` for active native
  playback. Confirm actual target callback delivery, seek settlement and
  startup-surface clearance without extending the original deadline.
- Cover `seeking`, `emptied`/source attach and `loadeddata`, plus delayed
  readiness, duplicate readiness events, paused single-frame delivery and
  play/resume. Keep the paused path separate from active-playback qualification.
- Keep at most one native request per element while multiple subscribers are
  active. Verify delivery fan-out, independent subscriber proof floors and
  generations, exact cancellation, subscriber retirement and both spare and
  incumbent elements. A canceled late callback, newer seek, source replacement
  or attachment change must not credit a successor.
- Preserve healthy continuous delivery and valid landing evidence observed
  before `seeked`. Include request ID zero and request/cancel exceptions.
- Keep silent callbacks with independent progress and a real presentation
  freeze distinguishable. Clock/decoded evidence may diagnose bounded
  observation unavailability but may not prevent the first qualified renewal.
- Repeat the real Safari/native movie case with request registration,
  cancellation, delivery and element/attachment identity recorded. Verify first
  frame and continued playback. MSE seek behavior remains explicitly unknown
  unless a separate MSE acceptance cell is added.
- Retain the Resume handoff and native clock/buffer ownership requirements in
  the consolidated retest RCA as separate acceptance work.

## Evidence and review

Private evidence remains `native-callback-causal-2228.json`, its
credential-free ordered derivative `native-callback-causal-2228-derived.json`,
`native-callback-rearm-only.json`, and `native-callback-browser-version.json`.
The packet includes safe measurements, version metadata, source excerpts and
portable source fixtures. It excludes raw HAR/authentication and does not
label manual incident measurements as qualification receipts.

The traces, source inspection and a5/a6 controls establish the root cause and
successful renewal mechanism. The revised design supersedes the earlier
cohort-reset proposal. Fable's review and dispositions are recorded in
[NATIVE-CALLBACK-FABLE-REVIEW.md](NATIVE-CALLBACK-FABLE-REVIEW.md). The
multiplexer, discontinuity renewal and all runtime acceptance remain pending;
no implementation or production qualification is claimed here.
