# Safari playback activation — permission belongs to the media element

**Status:** open · repair reviewed and regression qualified; Safari activation mechanism verified; original HEVC acceptance pending ·
**Written:** 2026-10-20 · **Initial base:** `4e049a4` · **Integrated main:** `2ae6675`

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

## Preparation can outlast the media activation window

The catalog and theater title buttons call
[`play`](../../crates/plurxd/src/web/player/decode-tiers.js) directly from their
inline click handlers. `beginPlayAttempt` and `capturePlayInputs` run
synchronously; wiring the player and preparing the watch layout do not await
or replace its persistent video. Previously `play` awaited the decision before
any explicit media load. Native HLS then awaited master and child playlist
readiness in
[`runNativeHlsReadiness`](../../crates/plurxd/src/web/player/player.js)
before assigning the source and invoking the transport's first Play.
Neither operation synchronously prepared the element in the original click.
The deployed functions `play` (line 732), `notePlaybackPlayRejection` (line 580),
`runNativeHlsReadiness` (line 1764), and `persistentWait` (line 421 in
[`measurements.js`](../../crates/plurxd/src/web/player/measurements.js)) were
compared with pristine `4e049a4` and are identical. The source trace therefore
applies to the incident build as well as this implementation base.

WebKit documents [per-element playback authorization](https://webkit.org/blog/7734/auto-play-policy-changes-for-macos/)
and [gesture-bound media operations](https://webkit.org/blog/6784/new-video-policies-for-ios/).
Current [Document::mediaUserGestureReason](https://github.com/WebKit/WebKit/blob/main/Source/WebCore/dom/Document.cpp)
also accepts the window's transient activation, alongside active gesture tokens
and some grace/quirk paths. An await therefore does not invariably lose media
authority: asynchronous preparation can outlast the transient activation window.
The repair establishes the element's authorization synchronously, without relying
on that window lasting until readiness finishes.

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

**Evidence limit:** the incident logs record browser denial, not its activation
state or policy settings. Preparing-to-rejection spans about 2.4 seconds; those
records cannot establish that a particular activation timeout expired. Missing
synchronous element preparation and incorrect recovery ownership are proven in
the application lifecycle. The Safari comparison below verifies the preparation
mechanism, while the original HEVC title still needs deployment acceptance.

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
A pending seek keeps its destination; an initial start keeps its startup target.
Established playback retries from the current file-relative clock, because the
stall owner's historical startup/seek position can be stale.
Only a new attachment clears the permission refusal. Repeated automatic
Play calls and already-running stall callbacks cannot revive the stopped
owner. Both initial presentation reevaluation and a supply wait that refills
during the control request use the central transport owner. An immediate refill
rejection is processed before the same callback can execute a recovery verdict.

This repair adds no feature switch, watchdog, placeholder media, or muting.
It does not change stream selection or claim to diagnose all decoder failures.

## Qualification — source regressions first, physical Safari after review

A fresh independent Astra review requested two repairs: centralize the refill
Play refusal and retain the current position on an established-playback retry.
Both were fixed and approved on integrated source `ae67fbcc`. The focused
`web-control.test.js` run passed all **108 tests** with zero failures on
2026-10-20. Its native startup cases are included; they were not run separately.
The affected fast lane supplies the remaining checks below:

```bash
scripts/quiet node --test tests/playback/web-control.test.js
scripts/quiet node --test tests/playback/web-policy.test.js
scripts/quiet node --test tests/playback/seek-control.test.js
scripts/quiet node --test tests/web/player-dom.test.js
scripts/quiet node --test tests/web/cinema-remote.test.js
```

`web-control.test.js` includes the native HLS startup cases. Its added
regressions exercise the actual open/decision/attachment and retry functions
with a per-element permission model, including Pause while a retry is still
preparing. The native suite exercises real
readiness, transport rejection and terminal-owner cleanup, including late
attachment, element, intent, source and owned-cancellation races. These
models test lifecycle ownership; they do not impersonate Safari qualification.

On 2026-10-20, real button clicks in Safari **27.0.1 (22625.1.29.11.28)**
verified the mechanism using local native HLS with H.264/AAC, separate initially
empty persistent video elements, and `muted=false`. Under **Never Auto-Play**,
a 1.5-second delay allowed the control to play and was not a decisive comparison.
With an **8-second** delay, the control rejected Play with `NotAllowedError` and
`readyState=0`. The fixed case synchronously loaded its empty element in the
click handler, then attached the source after the same delay: Play resolved and
its clock advanced from 0.000003 to 0.771854 seconds over the next second, with
`paused=false` and `muted=false`. The original local policy was restored after
the check. This agrees with transient activation permitting a short delay and
explicit element preparation surviving a longer one.

This isolated fixture reproduces the preparation sequence, not the complete
production app. Original HEVC acceptance remains: resume the title with sound
at its saved position on the deployed repair and confirm a browser denial
produces one immediate permission prompt without an automatic session reopen.
Record the deployed source revision with that result. Synthetic H.264 mechanism
qualification does not establish HEVC decoder or original-title acceptance.
