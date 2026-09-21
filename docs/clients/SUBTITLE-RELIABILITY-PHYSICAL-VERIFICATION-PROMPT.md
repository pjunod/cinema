# Subtitle reliability — physical verification

**Status:** open · **Reconciled:** 2026-09-20

**For:** a session with the physical devices. **Against:** Apple build 171,
Android versionCode 108, web from the same deploy, server at or after the
subtitle reliability merge. **Written:** 2026-09-19.

Three viewer-visible failures were fixed with no hardware evidence at all.
Every claim below is a source-level claim until a device says otherwise, and
two of them — the routing one and the CEA-608 one — are hypotheses that this
run either confirms or refutes. A "did not reproduce" is a real result here;
say so rather than retrying until it passes.

Paste this whole document into the session that has the devices. Report back
per case: **pass**, **fail** (with what was on screen), or **not run** (with
why). Do not summarise several cases into one verdict.

## What you need

- One **HDR or Dolby Vision remux** whose English subtitle track is **PGS**
  and which also carries an **English SRT**. A Blu-ray remux usually is one.
  Note its title and file id.
- One title with an **SRT** track, any dynamic range.
- One **dual-audio anime** file: Japanese + English audio, one **ASS** track.
- One title with a subtitle track whose extraction **fails** — a track the
  server cannot read. If you have none, skip case 6 and say so; do not
  manufacture one by deleting a file mid-playback.
- The Apple TV, an iPad or iPhone, the Android device, and a browser.

## The cases

### 1. No notice, and no subtitles, on an HDR remux — all three clients

The reported symptom: *"That subtitle requires an SDR burn-in. HDR playback
was kept unchanged."* appearing with no user action, about half the time.

Open the HDR/PGS+SRT title. Do not touch the subtitle menu.

**Pass:** playback starts, **no notice appears at any point**, and the
subtitle menu shows the **English SRT** as the selected track — not the PGS
one, and not "Off". On the web client the notice used to appear about 400 ms
after the picture, so watch the first two seconds.

**Fail:** the notice appears, or the PGS track is selected, or a subtitle
menu that should have selected the SRT shows Off.

Run this on **web, Apple TV and Android** separately. The web client is the
one that showed the notice; the native ones showed silence.

### 2. The manual pick still tells the truth

Same title. Open the subtitle menu and **choose the PGS track by hand**.

**Pass:** the notice appears, *once*, and HDR playback continues unchanged.
This refusal is correct — the viewer asked, and the answer is honest.

**Fail:** the picture drops to SDR, or the track appears to enable and shows
nothing.

### 3. An SRT turned on mid-play shows cues within one segment

Any SRT title. Start playback with subtitles **Off**. Let it run past the
opening, then turn the SRT on.

**Pass:** cues appear within a few seconds — one segment, not one minute —
and keep appearing. Seek forward 10 minutes; they keep appearing.

**Fail:** the track shows as selected and no cues ever appear. That is the
defect; note **which client** and whether a later seek fixed it.

Run on **all three**. Apple's retry was gated on a field only `open()` wrote,
so mid-play selection is exactly the case that was broken there.

### 4. A quality change keeps the subtitle showing — Apple

Apple TV or iPad, SRT title, subtitles **on and visibly showing**. Force a
server-driven quality change: constrain the network, or pick a different
quality from the menu, and wait for the switch to actually happen.

**Pass:** cues keep appearing across the switch. A gap of a second or two is
the switch; a permanent stop is the defect.

**Fail:** cues stop at the switch and never return, while the menu still
shows the track selected.

Also run it with subtitles **Off**: **pass** is that they stay off. A master
carrying a DEFAULT rendition used to be able to turn them on by itself.

### 5. The anime file gets its subtitles — all three

The dual-audio ASS file. Do not touch the menus.

**Pass:** Japanese audio, and the ASS subtitles are **showing**.

**Fail:** Japanese audio and no subtitles. This case exists because the first
draft of the server fix would have produced exactly that, silently, on every
such file; it is worth confirming on hardware rather than trusting a unit
test.

If the file is HDR, subtitles being **off** is the correct answer instead —
an implicit burn may not spend the grade. Say which case applies.

### 6. A failed extraction — the three engine observations

This one is not a pass/fail on the fix; it is the **measurement three
advisory rows in Settings → Developer are waiting for**, and until it is
taken `playback.subtitle_not_ready_503` stays off.

With the switch **off** (the default), play the title whose subtitle
extraction fails and select that track. Expected: the track selects, no cues
ever appear, video plays normally. Note it.

Then turn **Settings → Developer → Refuse a subtitle segment that failed**
**on**, and do it again on each platform. What is being measured is one
thing: **does the video keep playing?**

- **AVPlayer (Apple TV, iPad):** AVPlayer gives a subtitle segment about two
  seconds and blocks the muxed video while it waits, so this is the refusal
  with a picture riding on it. Report: video continues / video stalls / app
  shows an error.
- **Media3 (Android):** report whether the refusal surfaces as a subtitle
  problem or stops playback outright.
- **hls.js (browser):** report whether playback continues or the player
  reports a fatal network error.

Turn the switch back **off** afterwards whatever the result. Report the three
observations verbatim; they go on the Developer card, and a tick nobody
earned is worse than the grey "not observable" that is there now.

### 7. Direct play keeps an embedded ASS track out of a re-encode — Android

Android, a **direct-play** title (no transcode, no remux) carrying an
embedded ASS or `mov_text` track. Select it.

**Pass:** cues appear and **no new server session is created** — the stream
does not restart, the picture does not blink, and quality does not change.

**Fail:** the picture restarts, which means it took a burn. The routing order
was asking "can this be a rendition" before "is there a container to read it
from", so this is the case that changed.

### 8. The CEA-608 phantom — confirm or refute

Every master now carries `CLOSED-CAPTIONS=NONE` unconditionally. The
hypothesis it closes is that without it, AVFoundation and ExoPlayer invent a
caption option in the text group, shifting every rendition ordinal beneath it
— so a client asking for the first subtitle rendition gets the second.

On a title with **two or more** subtitle renditions, on **Apple TV and
Android**: select each track in turn and confirm the cues that appear are the
**language you selected**.

**Pass:** each selection shows its own language. **Fail:** selecting one
language shows another, or the last track in the list shows nothing.

If you never saw this before the change, say so — "could not reproduce the
original symptom" is the honest result and it retires the hypothesis.

## What to report

For each case: the platform, the title, pass/fail/not-run, and what was on
screen. For case 6, the three engine observations in full. For anything that
failed, the client build number and whether it reproduced twice.

No deploys are part of this. The server changes are merged; the owner
deploys.
