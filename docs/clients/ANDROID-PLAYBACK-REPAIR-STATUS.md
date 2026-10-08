# Android playback repair — device failures and delivery progress

**Status:** investigating and implementing · **Updated:** 2026-10-08

Companion to [Android parity](ANDROID-CLIENT-PARITY.md): repairs the native
TCL and Lenovo playback failures and Razr video geometry and playback menus.
Work uses an independent agent clone and one combined main-bound PR.

## Confirmed observations

| Issue | Evidence | Current work |
|---|---|---|
| Razr picture squeezed to roughly one-third width | Avatar reproduced on physical hardware; decoded buffer is 3840×2160; application-owned child surface applies a second geometry scale | Correct the buffer-to-parent mapping and verify it on hardware |
| Razr playback menu appears blank/deformed | Light theme paints the panel white while unselected rows, sync value and switch labels explicitly use white | Use the theme foreground for panel content |
| Lenovo never starts Avatar | Repeated init.mp4 503 then producer_failed 502; encoded rendition waits for a permit while background admission owns capacity | Repair retirement/publication lock deadlock; compile and verify regression after review |
| TCL playback failure | User reproduced across Avatar, Bad Boys and other titles; device not currently connected | Locate device and verify after root fixes |

## Delivery sequence

1. Identify causes and implement repairs; compile while building.
2. Commit related work normally and combine it in one draft PR.
3. Obtain one adversarial agent review when the PR is ready to merge.
4. Address findings, then run the fast lane once. Retry only failed checks.
5. Merge the passing candidate and clean up temporary work.

## Root causes and implementation

- The Android child surface already maps decoder buffers into its initial
  surface dimensions. Applying decoder-to-view scaling again squeezes 4K
  output. Geometry now transforms that surface coordinate space into the
  current view, including rotation and pixel aspect ratio.
- Player menu content used hardcoded white over a theme-controlled panel.
  Unselected rows, switches and audio offset values now use the theme foreground.
- VOD retirement joins the actual output writer before releasing admission.
  Background cancellation and dormant cleanup can hold the rendition build
  gate during that join; the writer queues on the same gate. The child is
  reaped but admission never releases. A generation-owned retirement signal
  now cancels only queued publication-lock acquisition. In-progress writes
  and accounting still finish before the writer settles and permits release.

The native release builds and the pinned Rust 1.97.1 compiler loop works.
The repaired release is installed on the Razr; visual verification awaits its
unlock. The server regression queues a real writer behind each publication
lock, retires a real child, and checks that background admission releases
before either lock is dropped.

The server trace shows a background hardware permit with no FFmpeg process
running and no running background job. Its exact suspended Rust task cannot
be inspected in the deployed binary; the lock cycle is identified in source
and awaits the deterministic regression and runtime acceptance.

No watchdogs, quality downgrades, feature gates or admission bypasses are added.
Tests remain deferred until the reviewed candidate, following the explicit
task instruction over the repository's default local-test sequence. No test
or repaired-device acceptance is claimed yet.
