# Safari playback activation — permission belongs to the media element

**Status:** implemented, awaiting independent review and qualification ·
**Written:** 2026-10-20 · **Base:** `4e049a4`

Companion to [PLAYBACK.md](../PLAYBACK.md) (transport routing) and the
[playback surface contract](PLAYBACK-SURFACE-CONTRACT.md) (what the player
shows). This note explains the failed native Safari resume, the ownership
repair, and the evidence still needed before calling it physically accepted.

## The incident had ready media and a rejected Play command

The October 20 Safari resume of *Avatar: Fire and Ash* used native HLS with
HEVC copy video at a saved position near 1:03:50. The deployed web build was
`702083541ee6c102d9b256de5105c1be6c70c736`.

| UTC | Observation |
|---|---|
| 11:46:42.535 | The native master and selected media playlist were ready. |
| 11:46:42.743 | `attach_or_resume` rejected with `NotAllowedError`, code 0. |
| Following stall samples | 5.5 seconds buffered; no presentation proof. |
| 11:47:02.968 | Legacy stall recovery reopened the recipe. |
| 11:47:04.954 | The replacement playlists were ready. |
| 11:47:05.119 | The replacement Play rejected with `NotAllowedError` again. |
| 11:47:24.989 | Recovery reached `owner_exhausted`. |

A later read-only inspection of the same failed Safari player showed a
ready server interval of 199.5 seconds, 5.5 seconds buffered on the device,
zero HTTP wait, and failed presentation. These facts distinguish permission
refusal from missing media. They do not prove a decoder can present this
particular HEVC stream after permission is granted.

## The first media operation lost the gesture across asynchronous preparation

The title buttons call [`play`](../../crates/plurxd/src/web/player/decode-tiers.js)
from their click handler. Previously it awaited the decision before any
explicit media load. Native HLS then awaited master and child playlist
readiness in
[`runNativeHlsReadiness`](../../crates/plurxd/src/web/player/player.js)
before assigning the source and invoking the transport's first Play.
Consequently neither operation was bound to the original click. The deployed
functions `play` (line 732), `notePlaybackPlayRejection` (line 580),
`runNativeHlsReadiness` (line 1764), and `persistentWait` (line 421 in
[`measurements.js`](../../crates/plurxd/src/web/player/measurements.js)) were
compared with pristine `4e049a4` and are identical. The source trace therefore
applies to the incident build as well as this implementation base.

WebKit documents both requirements:

- [Playback authorization is per element](https://webkit.org/blog/7734/auto-play-policy-changes-for-macos/).
  Keep that element when changing its source.
- [The media operation must directly follow the gesture](https://webkit.org/blog/6784/new-video-policies-for-ios/).
  A later readiness callback does not retain the click's authority.

The [WebKit implementation](https://github.com/WebKit/WebKit/blob/main/Source/WebCore/html/HTMLMediaElement.cpp)
provides the precise preparation operation: `load()` calls `prepareForLoad`
synchronously, which invokes `removeBehaviorRestrictionsAfterFirstUserGesture`
when processing a media gesture. That removes the audio/video rate-change
restrictions on this element. Calling Play on an unsupported empty source
is unnecessary and can reject before reaching Play's equivalent grant.

The repair loads the empty persistent `#video` during `play`, before the
first await. Source attachment later reuses that same element. An element
with a source or `srcObject` is left alone: a pending replacement must
preserve the incumbent picture and position if preparation fails. Watch
layout uses the existing player host; the incident's native HLS path does
not create a fresh video element at each session reopen. The separate
prepared dual-player path can replace the authoritative element and remains
subject to the same central rejection handler.

**Evidence limit:** the logs record the browser's denial, not its internal
policy settings. The missing gesture-bound element preparation is proven
in the application lifecycle and is consistent with WebKit's policy. A
real Safari check must still demonstrate that this client's version grants
permission through the empty-element load and later source assignment.

## A permission refusal stops the transport owner immediately

[`notePlaybackPlayRejection`](../../crates/plurxd/src/web/player/player.js)
previously stored the error for the startup deadline while the independent
stall owner remained free to retry. Reopening a server session did not
change the browser permission that had refused the original command.

A current `NotAllowedError` now stops the attachment's automatic owners,
loaders and sampling and raises the existing `owner_stopped` surface with
**Try again** and **Close**. It preserves desired playback, the committed
resume destination, and audio/subtitle selections. No transcode alternative
is offered for a browser permission failure. A stale or internally cancelled
invocation can still be logged but cannot stop its successor.

Try again clears and explicitly loads the same element within the new
user gesture, then performs a full preparation with the retained intent.
Only a new attachment clears the permission refusal. Repeated automatic
Play calls and already-running stall callbacks cannot revive the stopped
owner. The presentation reevaluation also uses the central transport owner,
so its rejection cannot be swallowed and converted into a later stall.

This repair adds no feature switch, watchdog, placeholder media, or muting.
It does not change stream selection or claim to diagnose all decoder failures.

## Qualification — source regressions first, physical Safari after review

The following tests are added but **not executed** at this implementation
handoff. The requested workflow runs them after independent review and any
repairs, on the final source:

```bash
scripts/quiet node --test tests/playback/web-control.test.js
scripts/quiet node --test tests/playback/web-policy.test.js
scripts/quiet node --test tests/playback/seek-control.test.js
scripts/quiet node --test tests/web/player-dom.test.js
```

`web-control.test.js` includes the native HLS startup cases. Its added
regressions exercise the actual open/decision/attachment and retry functions
with a per-element permission model. The native suite exercises real
readiness, transport rejection and terminal-owner cleanup, including late
attachment, element, intent, source and owned-cancellation races. These
models test lifecycle ownership; they do not impersonate Safari qualification.

The physical check must show an actual user click, an empty-element explicit
load, asynchronous preparation, source assignment on that same element, an
accepted Play promise, and advancing playback. Compare against the same
flow without the gesture-bound load under a Safari policy requiring a
user gesture. Then verify the original HEVC resume with sound at its saved
position, and confirm a browser denial produces one immediate permission
prompt with no automatic session reopen. Record the browser/build and exact
source revision with the result; a synthetic H.264 pass alone does not
qualify the original HEVC title.
