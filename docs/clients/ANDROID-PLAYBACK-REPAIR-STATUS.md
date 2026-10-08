# Android playback repair — device failures and delivery progress

**Status:** PR #915 merged; TCL follow-up validated in PR #918 · **Updated:** 2026-10-08

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

The combined draft is PR #915. The single adversarial review confirmed the
lock cycle and found a completion-proof race under retirement contention.
The existing generation owner now records the genuine trailer immediately and
applies completion metadata after confirmed writer/process settlement. A second
regression holds the manifest through reap and checks that proof survives.

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
Validation follows the requested review-first sequence. The four geometry
JVM tests and both real Android rendering tests passed on the reviewed code.
The rendering checks exercise the platform child surface and light panel pixels.
The two Rust lifecycle regressions and the current-candidate fast lane are
tracked in PR #915; its description and checks are the live delivery status.
No full unit sweep is part of this repair. Physical-device acceptance remains
separate from emulator evidence; no TCL or repaired Razr acceptance is claimed.
The server change requires deployment after merge to repair existing runtime
state; merging alone does not replace the deployed binary.


## Final gate adjustment

All eight focused regressions passed once. The first fast-lane preflight found
two missing client-catalog mappings for this PR and two existing main commits
without audited landing attribution. Their explicit mappings and historical
errata are recorded; validator behavior and boundaries are unchanged.

The user subsequently authorized merging when lint, syntax and web checks
pass, without waiting on the remaining unit-test work. The PR records those
final checks and the merge result; other sessions own unrelated unit failures.

## TCL Dolby Vision follow-up — 2026-10-08

**Status:** implemented and validated; PR #918 records merge and rollout.

The physical TCL 9445X runs build 153 and reports an SDR display with no
Dolby Vision decoder. Avatar: Fire and Ash (file 6751) correctly selects an
SDR transcode, but its continuous family bootstrap fails before Media3 opens.
The serving node now runs the merged PR #915 revision; this is a separate
failure from the retired-writer lock cycle.

At 15:36:09 UTC, `quality-family` returned 502: “rendition does not match
the initial continuous H.264 SDR family.” Both generated video init records
contain AVC profile bytes `64 0c 05`; the family requires `64 00 32`.
VAAPI interprets the decimal `5.0` argument as integer level_idc 5 instead
of level_idc 50, and emits constrained-High flags. The shared encode builder
must supply each encoder's level representation and normalize the two newer
constraint bits before muxing. Existing family validation remains strict.
The byte-producing recipe receives a new cache identity so existing invalid
inits are not reused. No Android capability override or retry is involved.

Compiler: pinned Rust 1.97.1 local loop. Review and focused emitted-byte
regressions will run at the merge boundary, followed by lint, syntax and web
checks under the user's current merge convention. Physical acceptance and
server deployment are not yet claimed.

History: commit `91f9d5d184` introduced the decimal level argument on September
30; `03a4824367` reused it for the strict continuous family on October 1;
`530c55c11` added Android enrollment on October 2. The ordinary path does not
require this exact family record. The last successful physical TCL session
has not been correlated, so its precise route is not claimed.

The single merge-boundary review found no production blocker. Its test finding
was addressed: emitted-byte tests use the repository's configured FFmpeg helper.
A one-second VAAPI encode with the serving node's configured Jellyfin FFmpeg
now emits `64 00 32` in both avcC and SPS. The initial hardware check used the
container's unrelated default FFmpeg and is not production evidence.

All four tests selected by `cargo +1.97.1 test -p plurx-core --features
hiqlite-store --lib continuous_avc_` passed, including actual software-encoded
init acceptance by the unchanged family verifier. The initial no-feature test
compile hit unrelated storage test helpers; the production-feature retry passed.
The complete Node web suite passed across the initial run and continuation:
the localhost-socket test was retried with permission, followed only by checks
that had not yet run. Passing checks were not repeated. Commit lint/syntax and
the PR description carry the final merge receipt; full unit suites are deferred.

The normal commit hook passed catalog lint, Rust formatting, workspace
Clippy with warnings denied, and syntax for all 77 served scripts. Generated
web configuration, Player typedefs and the existing web type baseline passed.
The branch was rebased onto the concurrent Apple-only main update and the
pinned all-target server compiler check passed again; behavior tests were not
repeated for that unrelated change. PR #918 is the live merge/deployment
receipt. A server rollout and an actual TCL playback remain outstanding.
