# Prepared-stream seek incident — why backward skip stopped Esteros

**Status:** open · **Observed:** 2026-10-20 · **Runtime:** `c1c344132`

Companion to [WEB-SEEK-LATENCY-RCA-AND-FIX.md](WEB-SEEK-LATENCY-RCA-AND-FIX.md)
(the earlier seek mechanisms). This records the ROG browser incident, the
underlying subtitle/quality intent defect and the evidence still needed to
qualify the repair on the laptop.

## Observed failure

Chrome was playing Esteros from the laptop's local Compose server. Container
logs and read-only queries of its durable session and telemetry databases
establish this sequence. Times below are UTC; the incident was at 14:24 EDT.

| Time | Evidence |
|---|---|
| 18:24:38.932 | Auto stages a 480p HLS VOD replacement while 720p keeps playing. |
| 18:24:40.607 | Chrome proves successor frames and exposes it at about film second 3397. |
| 18:24:40.634 | A playback-control exchange reports `control_rate_limited`. |
| 18:24:40.887 | The durable successor becomes `ended`, with reason `replaced`. No successful terminal commit receipt remains for it. |
| 18:24:40.890 | A segment request fails with HTTP 503 as that successor retires. |
| 18:24:43.696 | Backward skip targets 3369.076 seconds. The attached VOD buffer covers only 3396–3404 seconds, so the seek needs more media. |
| 18:24:46.707 | An explicit transport command occurs; the later accepted recovery snapshot has demand `hold`. |
| 18:25:03.697 | After 20 seconds, local seek recovery reopens at the same target. |
| 18:25:04.077 | The server finishes the new create in 276 ms. |
| 18:25:24.113 | HLS startup reports that its playlist did not load before the deadline. |
| 18:25:24.131 | Another watchdog overwrites that verdict with “The stream reaches the browser but playback did not start.” |

The session correlation was checked using the server's SHA-256 log identity.
Bearer session handles and machine-specific paths are deliberately omitted.
Every preparation-cancellation counter was zero at inspection; this gives no
support to a background capacity-cancellation explanation for this incident.
The rate-limit branch itself does not retire a session.

## Underlying cause — subtitle reattachment cancels the quality request

The incident had native subtitle track 1 selected. At a prepared switch,
`adoptPlaybackMediaElement` called `setSub(p.curSub)` to attach that subtitle
to the newly visible element. `setSub` is a viewer command: even for the same
index, it calls `supersedePlaybackControlIntent`. That cancels an unsettled
automatic quality change and restores its previous requested candidate.

Exposure had already moved the successor from `p.prepared` to
`p.preparedCommitting`. The cancellation helper checks only `p.prepared`, so
the visible successor survived. Its later first-frame callback still queued
the new candidate's `committed` acknowledgement, although the requested
selection had reverted to the predecessor's candidate. The server correctly
refused that changed ask in `reserve_preparation_commit`, before catalog
revalidation. This explains the immediate `replaced` projection, missing
catalog-discovery log and zero background-cancellation counters.

The prepared test harness stubbed `setSub`, hiding the viewer-command side
effect. The regression must exercise real subtitle attachment and real
selection/supersession behavior. The repair separates attaching subtitles to
a media element from selecting a subtitle: internal reattachment preserves
the quality request, while actual viewer choices still supersede it. The
server's changed-ask rejection remains intact.

The same attachment-only path also handles initial preselected text tracks,
subtitle offset refresh and reopened media. Those internal operations must
not manufacture another viewer command or fence their own startup loaders.

The saved decoder revision numbers are not evidence of this failure. Create
derives an initial decoder snapshot revision from the display target counter;
control supplies the browser's separate decoder counter. Their different
values alone do not establish a changed decoder or a rejected commit.

## Recovery defects found alongside the cause

The staged HLS error listener recognizes only a still-preparing successor.
After the DOM switch, that predicate is false, and the normal attached HLS
error observers were never installed. A fatal request on the visible
successor could therefore disappear without the ordinary recovery owner
handling or reporting it. After first-visible-frame proof, the repair adopts
the committed HLS instance into the normal established-stream observers while
preserving its attachment identity and reporter. Until that proof succeeds,
the existing bounded rollback owner retains the predecessor's recovery episode.

Established network recovery also compared the original startup deadline
after presentation. That deadline bounds startup, so an established stream's
single network retry must not expire merely because the film has played for
longer than the startup window.

Finally, a terminal HLS verdict cancels its startup episode. A queued generic
watchdog then interpreted that cancellation as a non-HLS startup and probed
the source again. Entry and continuation checks now fence the terminal
attachment, including a probe already awaiting HTTP.

The focused validation commands are:

```bash
node tests/playback/web-control.test.js --prepared-hls-recovery
node tests/playback/seek-control.test.js
node tests/playback/paused-hls-resume.test.js
node tests/web/player-dom.test.js
scripts/web-types
make paperwork
```

Asset load/order checks also pass. These are local source and mechanism
checks; they do not qualify playback of the incident title on the laptop.
The full `make web-unit-check` passes, as does the pinned Rust regression
`playback_control::tests::a_new_ask_in_the_same_packet_refuses_the_old_asks_commit`.
The new client regression is
`prepared HLS subtitle reattachment preserves the automatic candidate through commit`:
it fails before the repair with candidate 720 replacing candidate 480, then
passes with the original intent and selected subtitle attached through commit.

## Why Auto requested a downgrade

Auto requested 720p to 480p before the backward skip. The recorded throughput
estimate was about 128 Mb/s and the incumbent had about 61 seconds buffered.
Those two values do not establish the reason: policy can also react to fresh
within-fragment link pressure or measured active-encoder pressure. The original
switch's reason was logged only after successful settlement; the cancellation
bug skipped that logging path. Its exact trigger cannot be recovered from the
retained incident telemetry. Trial requests now log their reason and separate
completed-transfer, current link-pressure and active-encoder measurements
before preparation, so a failed trial retains its explanation.

The web Quality control now shows the selected mode and the presented
resolution, with exact dimensions for cropped frames. A 3.5-second notice
inside the player announces confirmed changes, including in fullscreen.
The presentation-proof and attachment fences keep requested or rolled-back
streams out of that display; initial play and ordinary same-quality seeks do
not announce a change. Existing playback faults suppress the notice.
Synthetic screenshots using the shipped CSS and player markup were checked
at 1440×900 and 390×844: the notice clears the header and the current-quality
button remains usable without horizontal overflow. This is layout evidence,
separate from live playback acceptance on the laptop.

## Remaining qualification

The original telemetry omits the commit selection and acknowledgement, so
the exact wire bytes cannot be recovered from that incident. The source
mechanism and its regression reproduce the changed ask and match the retained
server evidence; qualification still needs the repaired running browser.
The prepared loader also retains its existing limitation: it does not inspect
typed HTTP failure bodies as the ordinary startup loader does. Adoption
restores the attached event and retry owners, not complete loader equivalence.

Before closing this incident, prove the repaired browser can switch quality
with a subtitle selected and seek backward beyond its buffer. Retain the
commit selection and acknowledgement with the resulting receipt.
Also repeat the paused recovery case: the successful create and final
watchdog probe do not prove that the initial browser playlist request was
sent or completed. No live server or browser was changed during diagnosis.
