# Playback startup failures on October 13

**Status:** fixes built; one adversarial review addressed; promotion, deployment and physical acceptance open.

This record separates the standalone reader contention, native playlist wait,
stale preparation evidence and Apple TV audio failures. Rebooting the Apple TV
cleared its broken audio output; that does not qualify the Cinema fixes below.

## 1. Standalone indexing competed with the viewer

Twilight and Wicked failed to start on lab5. The next title, Tom Segura:
Teacher, started after 20.7 seconds. None had a ready VOD index. A local
whole-file index reader was reading another source concurrently. The local
index loop checked foreground activity before starting, but never cancelled
an already-running read when a viewer arrived. Cluster cancellation did not
cover this standalone path.

A bounded production-FFmpeg copy/audio-conversion experiment consumed 35
seconds of media at adjacent cold offsets:

| Background reader | Wall time | Media speed | Background bytes read |
|---|---:|---:|---:|
| Running | 28.13 s | 1.24x | 307,687,424 |
| Paused | 15.76 s | 2.22x | 0 |
| Resumed | 25.68 s | 1.36x | 331,325,440 |

Offsets differ, so this is not an identical-byte benchmark. The reversible
intervention establishes substantial contention. The reader was resumed.
Both local shared-source and fallback index builds now yield to foreground
activity. Cancellation drops their child and partial PGS staging; the cursor
is not advanced and a later pass may retry. The regression uses real paced
FFmpeg, observes staging, admits a viewer, and verifies cancellation and retry.

## 2. Love, Simon exposed three distinct boundary errors

The October 13 attempt resumed at 2,794.935 seconds. Times below are UTC.

| Time | Observation |
|---|---|
| 16:58:06.191 | Rolling copy producer created |
| 16:58:08.257 | First complete segment, 2.628 seconds |
| 16:58:17.116 | Writer gate opened with 16.017 seconds |
| 16:58:21.422 | Apple preparation timeout at 15 seconds |
| 16:58:26.530 | Native resource failure, URL -1008 / CoreMedia -12884 |
| 16:58:33.250 | First served snapshot, 53.596 seconds produced |
| 16:58:39.219 | Preparation error disagreed with recovery |
| 16:58:57.664 | Rendering with 42.597 seconds of client runway |
| 16:59:10.358 | Original preparation error finally cleared |

**Missing inventory evidence.** At the preparation timeout the server reported
`playlist_ready=false`, `producer_state=running`, recent speed 2.336x and
progress idle 195 ms, but `produced_end_ms=null`. The publication worker saw
writer inventory every 250 ms, while the compatibility inventory used by
status could wait for the unrelated 15-second reaper. The existing
[growing-resume readiness policy](../clients/APPLE-GROWING-RESUME-PREPARATION.md)
correctly refused to extend its deadline without completed-media evidence.
Publication cycles now refresh the fenced inventory before deciding whether
the conservative publication cushion is ready. A regression first failed
with `None` instead of 16,000 ms and now observes 16,000 then 24,000 ms while
the served playlist remains unavailable.

**Native attachment before publication.** A successful session create handed
AVPlayer an unpublished playlist. A loopback synthetic HLS server delaying
its first response by 27 seconds reproduced the exact native -1008/-12884
failure at approximately 20 seconds. This establishes the previously
unclassified resource-error mechanism in the growing-resume record.
The Apple open path now awaits a successful playlist response before creating
its native item. This uses a bounded, cancellable preparation request; the
native decoder still reports its own errors normally. Running the production
helper against that fixture completed preparation in 27.020 seconds, then
rendered 35 sampled frames over 34.57 seconds without a native failure.
Controller tests cover the attachment boundary and stop during preparation;
HTTP tests reject error responses, empty media playlists and non-playlist success bodies.

The single independent adversarial review found two preparation gaps: a
master can precede its media child, and moving the resource request out of
AVPlayer can bypass ingress failover. Preparation now resolves variant media
playlists under one bounded request budget before native attachment. A
transport failure walks the existing advertised ingress list, preserving the
session and recipe, and attaches using the ingress that prepared successfully.
The new regressions failed before these changes and cover an immediate master
with a pending child, cancellation of that child, first-ingress connection
failure, successor attachment, and stopping during ingress recovery.

**Error ownership across failover.** Same-session ingress failover replaced
the native item without publishing its new attachment generation to the
surface model. The previous preparation fault therefore survived over the
successor. Failover now performs the same attachment transition as ordinary
open. The controller regression failed on the old code and passes with this
transition. This clears faults about the retired item, not current failures.

## 3. Avatar's original failure was the audio output

The first Avatar attempts produced and delivered media promptly. Native
Console capture identified `HALS_IOContext_Legacy_Impl::StartIOThread: no
master engine info`, followed by AudioDeviceStart `nope` (1852797029), an
audio-queue start failure and stopped rendering. Navigation sounds failed
with the same code before Avatar started. The audio route reported zero Hz.
After the user rebooted, Avatar played and Love, Simon's failure reported a
48 kHz route. No codec downgrade or server timeout change addresses that
broken output state.

The trigger that lost the system audio engine remains unproven. The user also
reported Avatar stumbling after reboot; the retained app logs include a
paused-item failure and reopen, but do not identify its native cause. Neither
is claimed fixed by these changes.

## 4. Qualification boundary

Pinned Rust 1.97.1 compilation and Clippy, focused real-FFmpeg indexing,
prepublication inventory, publication scheduling, and Apple controller/HTTP
regressions provide local evidence. One existing capacity test initially
failed its exact wall-clock speed assertion (1.192 versus 1.2) before entering
the changed publication function; its isolated rerun passed. This failed run
is retained rather than described as an entirely green batch.

The native delayed-resource check ran on macOS, not the physical Apple TV.
The Apple TV and lab5 must be exercised after the reviewed server and Apple
build are installed. The 48-second server publication cushion and media
recipes are unchanged. No production settings were changed for these fixes.

The full local operations sweep ran 1,018 tests with one failure and two
skips: the unchanged macOS installer test compared a `/var` temporary path
with its resolved `/private/var` form. The same test passed with a canonical
temporary root; neither installer nor test was changed. This full sweep is
not claimed green. iOS and tvOS Release simulator builds passed before review;
the integrated review corrections passed 43 tvOS and 49 iOS focused tests,
plus the current-base Rust publication and indexing regressions. The updated
production helper also waited for a synthetic master's child delayed by 27
seconds and then rendered 35 sampled native frames without an error.
