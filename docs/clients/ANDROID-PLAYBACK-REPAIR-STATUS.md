# Android playback repair — device failures and delivery progress

**Status:** open — PR #915 and PR #918 merged; server rollout and physical TCL playback acceptance pending · **Updated:** 2026-10-08

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


## Dolby Vision badge parity — 2026-10-08

**Status:** Android build 156 source compiled; [PR #959](http://forge.lan:3000/noirr/plurx/pulls/959)
is the review, qualification and merge receipt. Physical acceptance is open.

Android source badges now spell out Dolby Vision, show known profile numbers
and retain the actual delivered profile when conversion changes it. Both
conversion and downgrade arrows render. Conversion stays undimmed, and the
player badge row wraps on narrow views. Unknown profile or delivery facts do
not invent a conversion.

The badge model's 18 focused tests passed during development. Their assertions
were updated alongside the visible labels. No additional unit run is required
for this badge-only PR; current-source compilation and the PR fast lane supply
the remaining pre-merge evidence. Playback arrival and temporary-fence control
repairs are separate work and are not included in this change.


## Copied Dolby Vision playback repair — 2026-10-08

**Status:** Android build 157 and Apple build 220 source compiled; [PR #962](http://forge.lan:3000/noirr/plurx/pulls/962)
is the review, fast-lane qualification and merge receipt. Physical acceptance
is open.

The Lenovo TB322FC advertised Dolby Vision Profiles 5 and 8 on builds 153 and
155. Its manual 720p choice produced AVC/SDR; selecting Auto produced a
2160p Profile 7 to Profile 8 copy using `c2.dolby.decoder.hevc` and
`video/dolby-vision`. Pixel 10 Pro Fold advertised HDR10, HLG and HDR10+,
without Dolby Vision. The Chrome session selected the HDR10 base; the user
confirmed Safari instead plays Dolby Vision 7 to 8.1. Those HDR10 reports are
resolved without changing negotiation.

### Arrival root cause and correction

Lenovo's requested film position was 1,291,132 ms; the copy correctly began
at its preceding keyframe, 1,290,664 ms. The first frame was outside the
250 ms arrival window. The later-rendered-output check admitted only
progressive remuxes, so an HLS copy advanced past the target without settling
the pending command. One deadline reopened the copy; the next stopped it
with several seconds still buffered. The progressive-only condition was
introduced by `27ce627b8` on 2026-09-24.

The existing arrival owner now admits an attached HLS copy as well as a
progressive remux. Generation, foreground, selection readiness, playing
state, increased rendered output and the existing two-second preroll bound
remain required. Direct and transcoded attachments cannot use this path.
This removes the incorrect transport restriction; it adds no timer or
recovery owner.

### Control root cause and correction

Razr selected a Dolby Vision decoder, then recorded `control reporting
stopped (transport:503:serving_fenced)`. Android's retry whitelist, introduced
by `15dc56d90` on 2026-08-30, included `control_unavailable` but omitted the
temporary serving fence. The server's existing contract sends that refusal
with `Retry-After: 1`. Treating it as terminal permanently abandoned the
control owner after a temporary refusal; stale playhead/runway reports could
then hold the producer while client buffers depleted.

The existing reporter retry path now includes that specific response. Exact
request replay, Retry-After pacing, identity cancellation and definitive
terminal outcomes remain intact. Unknown 503 codes still stop reporting.
Apple's local reporter has the same omitted response, introduced by
`18f460e77`; the web reporter already recognizes it. Apple build 220 applies
the matching correction to its existing retry owner. Its Retry-After case now
covers both temporary 503 codes with exact request replay, and additional
cases retain definitive terminal and unknown-code stopping. These Apple
unit cases are added without running a suite before final review; iOS/tvOS
compilation and the required fast lane are the pre-merge checks.

The correction implements the temporary-refusal boundary in
[the stall recovery plan](../streaming/PLAYBACK-STALL-RECOVERY-IMPLEMENTATION.md)
§5.2; it adds no polling loop, authority bypass or recovery watchdog.

### Server trigger evidence and limits

At 22:08:10 local time, the serving node held fresh, accepted quorum evidence
(age 54 ms; term 18230; leader unchanged), but its applied index was
31,197,979 against committed index 31,197,991: a 12-entry apply gap. The
server fenced mutable media and recovered authority 208 ms later, retaining
the rolling session within its existing five-second grace. A 124 ms
state-machine apply preceded the gap and a 114 ms apply completed during
it. The expiry log does not retain the older held proof's rejection reason,
so it cannot establish whether that proof expired or was lost to a capture
race. No speculative server relaxation is included in this client repair.
The permanent-reporting failure is independently reproduced and corrected
regardless of the reason a temporary fence occurred.

The existing continuity work is narrower than uninterrupted media serving:
PR #798 (`e25824f20`) keeps rolling sessions through a brief authority loss;
PR #807 (`3423cc8d8`, `04f3e3144`) applies the same grace to progressive
playback and Live TV. New admissions still refuse immediately, and media
requests still return temporary 503 while the node is fenced. The
[October 4 incident record](../streaming/BAD-BOYS-APPLE-TV-INTERRUPTIONS-RCA.md)
explicitly leaves serving existing media during the outage as a follow-up.
Razr's retained session confirms the grace worked in this incident. Android's
permanent reporter stop violated the client side of that temporary-refusal
contract; this PR closes that gap, not the separate server-read follow-up.

### Evidence

Development validation before the final review passed 56 focused JVM tests
(26 arrival, 12 control-failure, 18 badges) and assembled the debug APK. The
control surviving-session case failed before the retry correction and passed
afterward. Cases retain terminal refusal, cancellation, unknown-code, stale
generation and unrendered-output boundaries. Per the user's current workflow,
unit suites are not repeated during implementation; current-source compilation
and the post-review fast lane supply final pre-merge evidence. Installation
and physical stutter acceptance remain separate from source qualification.

### Single implementation review and correction

The final combined native candidate received one adversarial agent review.
It found one P2: the server sends a header-only `Retry-After: 1`, but both
native local control transports read delays only from JSON `retry_after_ms`.
That discarded the real pacing instruction and used the 500 ms reporter
fallback. The transport adapters now read bounded delta-seconds headers into
the existing error field, retain body-only compatibility and use the longer
valid delay when both signals exist. Malformed, overflowing and over-budget
headers cannot change the existing retry budget or terminal classification.

After the review correction, only new/changed cases ran: three Android
transport cases and six Apple transport/reporter cases passed. Header-only
503 response objects, case-insensitive header lookup, body compatibility,
invalid/over-budget values, exact reporter replay and definitive terminal
refusals are covered. Earlier passing suites were not replayed. Android
compilation, the iOS reporter test build and tvOS compilation passed on the
corrected implementation. The isolated simulator used for those cases was
removed after the successful run. The review found no other actionable issue
and confirmed that the repairs use the existing arrival/retry owners.


### Fast-lane timeout continuation

Run 4579 passed all 389 validation and 871 operations methods, emitted a
completed 1,260-pass framed journal, then hit the ten-minute preflight ceiling
before final uploads and Node execution. No assertion failed. The receipt
adapter previously accepted failed-job artifact journals but required
whole-job success for the same completed log journal. Its framed path now
preserves terminal-job positives, retaining the success requirement for
unframed legacy logs. Node zero-execution is independently proved from the
immutable mandatory-upload barrier, both published starts and terminal log;
no Node pass or final journal is invented. The preflight budget now allows
fifteen minutes. Fresh qualification retains applicable passes and executes
only changed or unfinished checks; this does not waive the final gate.

Targeted disposition reviewed the continuation boundary and rejected both a
literal mismatch and noncanonical YAML control-key bypasses. The proof now
admits only the canonical immutable upload/Node step shapes and preserves
all-skipped histories. Its three new/changed focused regressions passed; the
original 1,260 individual Python successes will retain their own attribution.
