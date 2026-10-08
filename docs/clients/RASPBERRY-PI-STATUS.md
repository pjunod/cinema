# Raspberry Pi status — existing Plurx on a Pi 5

**Status:** built — software implementation and bounded Pi physical acceptance
complete; merged in PR #889.
**Updated:** 2026-10-08

Companion to the [implementation plan](RASPBERRY-PI-IMPLEMENTATION.md). This
page records software progress separately from physical-device acceptance.
The existing daemon and web player remain the product.

## Current acceptance — implementation merged

[PR #889](http://forge.lan:3000/noirr/plurx/pulls/889) holds the authoritative
candidate, review, retained regression journals and final merge disposition.
The Docker entry point, protected probe, exclusive idle-family admission and
software-frame Vulkan pipeline are implemented. Physical source `a72212b5b`
passed the bounded application checks below on the Pi 5.

| Application check | Physical result |
|---|---|
| Automatic direct 1080p Main10 | Chromium used `V4L2VideoDecoder`; forward seek to 10 s resumed at 10.35 s, backward seek to 3 s at 3.41 s |
| Continuous playback at default CPU pool 3 | The complete 720p/480p/shared-AAC family started with actual non-root HEVC request decoding and x264; the selected output was 1280×720; seeks resumed at 10.27 s and 3.33 s; cold start took 25.25 s |
| Daemon-selected GPU tone mapping | 4K HDR10 to 1080p used `libplacebo_software`; picture and output tags passed; the bounded comparison measured 1.57× CPU speed |

These are functional, bounded results. Sustained real-time throughput,
concurrency/soak, HDR HDMI output and Dolby Vision HDMI qualification are not
established. The GPU comparison does not qualify every source or long session.
[Physical evidence](http://forge.lan:3000/attachments/221bc723-202d-4737-bd7d-6aaf01f6e8c9)
retains the application and cleanup receipts.

All task Pi containers, browsers, compiler processes, native test paths and
test roots were removed. The user's containers were preserved. Account UID
999 is used by the user's discovery service and was retained, together with
only the image alias retaining the user's older image.

The [main integration review and compiler receipt](http://forge.lan:3000/attachments/a79aa4b9-1169-4b69-9065-cdb035d4539f)
describes earlier candidate `52eaf6566`; it is not an exact-source receipt for
a later candidate. Run 4432 (display 4411) passed preflight, Rust and web on its
candidate while Windows was pending. Main advanced, so the base guard requires
a new candidate and final CI. No final promotion gate pass is claimed.

## Historical installation blockers and their resolution

The chronology below records earlier snapshots. Its pending work and failures
are historical; current acceptance and remaining limits are recorded above.

The Docker-default installation and managed browser passed physical Main/Main10
request decoding, software-reference frame hashes, planar transfer, browser
presentation and seeking. Actual Plurx session creation exposed a separate
kernel prerequisite: the tested Pi kernel omits `CONFIG_SECURITY_LANDLOCK` and
its Landlock ABI query returns `ENOSYS`. The Pi-only namespace backend preserves
the protected identity boundary and passed its focused regressions. Paul
authorized merging PR #851 immediately and stopping the remaining unit-test
work; the merge is `6340f24ac5a75a887371e6c54b6f105730ae5e76`. Its promotion
guard rejected an advanced main base, so this was an explicit override, not a
passing promotion receipt. Another session owns unit-test failures. Landlock remains the preferred backend
wherever the kernel supports it.

Actual Docker app probing then failed at Bubblewrap's fresh procfs mount:
`Can't mount proc ... Operation not permitted`. The kernel logged `Mount too
revealing` in the same second as both the startup failure and diagnostic replay;
no matching AppArmor denial was found. [Linux 6.18 mount
source](https://github.com/torvalds/linux/blob/v6.18/fs/namespace.c#L5820)
supports locked masked proc children as the explanation, but the exact call
stack remains inferred.
Exact launcher replay succeeded
with an empty `/proc` and a read-only bind of only the namespace child's task
directory at `/proc/self`, keeping seccomp and `cap_drop: ALL` unchanged.
The narrow follow-up `a167607f4` applies this view and strengthens the existing
hostile probe regression. Adversarial review, pinned Linux compilation, Clippy,
formatting and the focused seccomp-policy assertion passed. The main-integrated
source `6724efdbe` then passed compilation and actual Docker app startup,
cataloguing and direct 1080p Main10 HEVC playback at the default settings.
Chromium reported `V4L2VideoDecoder` and platform decoding; playback advanced
to 2.33 seconds with 57 total frames, 13 dropped and none corrupt. This short check is
not a sustained throughput result. A forward seek to 10 seconds resumed at
10.63. The backward-seek assertion accepted too broad a window and is being
tightened before final qualification. Both test containers and the test
browser were stopped and removed; the user's running container was preserved.

Native app probing reached a separate continuous-session refusal:
`vod_family_capacity`. The actual frozen 720p video, 480p companion and shared
AAC recipes reserve 3 + 2 + 3 CPU units against the Pi's default pool of 3.
An admission correction is being implemented in the existing family and
permit ownership path: one oversized family may exclusively use an otherwise
idle software pool, with its full resource accounting retained until workers
are reaped. No budget default is raised. A temporary pool of 8 in the isolated
diagnostic app produced 3.24 seconds and 81 presented frames with actual HEVC
request decoding and CPU encoding. The seek step did not run because the test
helper expected an outdated request envelope. This is not default playback
acceptance. The default pool was restored and the diagnostic service stopped.

The user's subsequent `make docker-up` selected the generic image without Pi
decoder or GPU devices. That entry point is being integrated with the existing
Pi runtime provider while preserving the user's Compose overrides, data and
media mounts. That integration is committed in `9133eace3` and awaiting final
review. The same commit adds the separately probed software-frame Vulkan
pipeline and its truthful CPU transfer and GPU rendering labels.
The custom Pi FFmpeg build also lacks Vulkan/libplacebo. An isolated
Bookworm-backports Mesa 25.0.7 check reached real V3DV and produced three
distinct frames through libplacebo using packed 16-bit RGB input. Explicit
FFmpeg Vulkan frame upload failed semaphore interoperability; planar 10-bit
libplacebo input failed framebuffer setup despite exit zero. The working
route uses CPU format conversion and frame copies around GPU tone mapping;
full-size output quality and speed remain unqualified. Pinned dependency
packaging and an independently probed software-frame pipeline are being built.
Pi 5
has no fixed-function video encoder: direct playback needs no encoding, while
server video transcoding still uses a CPU encoder.

The user now has a real `plurxd` container and depends on Docker. Earlier test
cleanup assumptions that Docker was unused no longer apply. Tests must use
unique container names and isolated data, and must remove their own containers
afterward. The abandoned test container that reserved `plurxd` was removed;
the user's running container and data were preserved.

The follow-up is [PR #885](http://forge.lan:3000/noirr/plurx/pulls/885), held
as a draft for its final review and verification. One adversarial audit of
`e87f85388` found two P2 issues: ordinary presentations sharing AAC inherited
an unnecessarily fixed family ceiling, and Vulkan device validation rejected
unselected CPU drivers listed in the inventory. Both findings are addressed
with focused regression cases. Exclusive families retain their original
ceiling; ordinary sharing remains bounded by current global capacity. GPU
validation reads the selected device-properties block. Corrected source
`bad8ceca4` passed pinned Linux all-target checking, Clippy, formatting and
ARM64 linking. Fast-lane run 4391 stopped before tests because the earlier
canceled run 4389 has no final journal; its stored Forgejo task steps confirm
cancellation during the history audit before Python or Node execution.
The authenticated zero-test task evidence is retained on PR #885.
[PR #889](http://forge.lan:3000/noirr/plurx/pulls/889) continues the same batch
with a fresh ordinary fast lane; no gate is waived and no passing test is
repeated. Run 4399 retained its successful method journals and found two
contract expectations requiring correction: the newly reviewed finite test
threads/Barrier wait, and read-only budget derivation before Pi preparation.

The complete Pi FFmpeg build passed its decoder, AC-4, Dolby Vision, DRM,
Vulkan and libplacebo capability checks. The final image executed real V3DV
shaders. A physical color-conversion check found that a plain final YUV format
conversion retained RGB matrix metadata; the shared pipeline now explicitly
converts to limited-range BT.709 YUV. A red-pixel coefficient control and
ffprobe tags confirmed actual conversion, rather than only retagging.
Both isolated app modes reached `/readyz` successfully. Candidate `a72212b5b`
passed automatic 1080p Main10 direct playback using platform
`V4L2VideoDecoder`, with forward seek10 resuming at10.35s and backward seek3
at3.41s. The actual daemon selected GPU tone mapping after its 4K-to-1080p
HDR10 picture/tag comparison measured1.57x CPU speed; sustained throughput
and Dolby Vision HDMI remain unqualified. The default three-thread software
pool started the720p/480p/AAC family with actual HEVC request decoder workers
and720p browser presentation. Its fragment check selected the480p companion
instead of the advertised720p variant; the helper is corrected and only
continuous playback/seeking is being retried. Completed test containers and
browsers were removed; the user's installation remains untouched.

The initial PR creation API ignored its draft flag and scheduled run 4389.
The PR was converted to draft and that run was cancelled before Rust or
platform jobs started. It is not the post-review validation receipt.

Physical testing also exposed two deployment defects. Compose 2.26 lacks
`config --environment`; hardware configuration now uses Compose's own label
interpolation on that version. The isolated native service lacked writable
temporary storage; systemd now owns a mode-0700 runtime directory and provides
`TMPDIR` without making system files writable or hiding media under `/var/tmp`.
Each correction has a focused regression. The historical installation receipts remain on PR #851; final physical
results, cleanup and current CI are recorded on PR #889.

## Initial implementation — PR #832 (merged)

| Work | State | Evidence / next action |
|---|---|---|
| Independent clone | cleaned after merge | Original branch `codex/raspberry-pi`, base `cca4a09b997171ba91136b396f7d484097305fd0`; validation evidence retained on PR #832 |
| Compiler | baseline passed | Rust 1.97.1; `cargo check --locked -p plurxd --all-targets`, exit 0 in 1m46s before Rust edits |
| Implementation contract | implemented | Source audits of server decoder inventory and browser media predictions |
| Server decoder integration | implemented; compiler passed | Sol 6.1; separate 8/10-bit graph proof, operational capabilities, shared HLS/VOD transfer |
| Existing browser player | implemented; tests passed | Sol 6.1; delivered tuple and fenced nullable predictions; web unit inventory, syntax and types pass |
| Deployment | implemented; tests passed | Sol 6.1; `deploy/pi-player`, safe launcher ownership, bounded report; all six regressions pass |
| Adversarial review | complete; both findings addressed | Reviewed `dfa775112`; Sol repaired P1 fallback and P2 transforms, coordinator inspected the changes |
| Validation | targeted checks passed; CI promotion incomplete | 292 validation methods, 729 operations methods, web inventory and all 10 named Rust regressions have passing evidence; one optional operations test skipped |
| Physical Pi 5 | bounded acceptance complete; cleaned | 16 GB Pi 5, Raspberry Pi OS Trixie; 4K60 Wayland HDMI connected; real Main/Main10 decode-transfer probes passed |

## Standing decisions

1. **Use an independent clone.** The user's checkout is not a workspace for
   this effort. The initially requested managed worktree was unused and
   archived without edits.
2. **Batch commits into one main-bound PR.** Paul's 2026-10-06 instruction
   supersedes the repository's per-task effort PR convention for this work.
   Compile during development; defer test execution until the reviewed
   candidate's fast lane. Preserve regression fields on the landing commit.
3. **No new application or watchdog.** Fix existing decoder selection,
   media reporting and deployment seams. Do not invent device capabilities.
4. **Keep hardware observations separate.** A build, FFmpeg inventory,
   browser prediction and displayed accelerated playback are different facts.
5. **No surprise software installation.** Do not replace the user's browser,
   install third-party binaries, or modify nodes during source development.

## Evidence ledger

[Run 4222](http://forge.lan:3000/noirr/plurx/actions/runs/4222) tested
`35d90bc4db1feb061250ae918987446660271d7f`. History and regression-field checks
passed. Validation unittest ran 292 methods: 289 passed, three failed. Commit
`19764e7a5` repaired an assertion's old FFmpeg argument location and two ownership
counts; targeted invocations passed all three failed methods. Playback code did
not change. Successful siblings were retained.

Subsequent checks used that production source, with documentation-only link
scrubbing required by the operations contract:

| Command / coverage | Result |
|---|---|
| `make operations-check` plus failed-method continuation | 729/730 methods passed across retained runs; one optional upstream Hiqlite comparison skipped without its external source fixture |
| `make web-unit-check` plus unpassed-command continuation | Entire recipe passed; after a sandbox socket refusal, the existing `PLAYBACK_LAB_TEST_FILTER` retained 23 shaping passes and ran the remaining 95, followed by the unrun recipe suffix |
| `cargo test --locked -p plurx-core --features hiqlite-store --lib transcode::decoder_inventory::tests::request_` | 3 passed |
| `cargo test --locked -p plurx-core --features hiqlite-store --test decoder_selection request_` | 5 passed |
| `cargo test --locked -p plurxd --bin plurxd -- browser_predictions_do_not_claim_decoder_identity request_decode_software_encode_has_one_frozen_software_startup_successor` | 2 passed |

The operations continuation reran only 11 unsuccessful methods. Socket/process
fixtures passed with the needed permissions. The Linux janitor timeout case
needs GNU `timeout`, absent on the Mac; a source-only archive executed that one
method on Linux in 2.054 seconds, passed, and its temporary extraction was
removed. No repository credential was transferred. The infrastructure-name
scrubber corrected two new status links and eight pre-existing links in three
other documents. It changed no runtime configuration.

**Historical gate limitation before the user merged #832.** Main's fast lane bundled Python tests and
repeats overlapping web tests; it lacks the effort lane's per-test pass receipts.
The failed run was cancelled after its result to release an unused queued job.
The PR was left draft to avoid replaying successful tests; the user subsequently
merged it. No full Rust unit
suite or Windows/Apple/Android CI compilation is claimed by the targeted local
evidence, and no green CI status has been manufactured. The PR description
holds the live receipt and final merge disposition.

All 20 named PR regressions passed before merge. The physical-device evidence
collected afterward is recorded below; it is separate from those regressions.

The Pi FFmpeg source confirms its DRM SAND transfer uses planar `yuv420p`
or `yuv420p10le`; treating every 10-bit DRM frame as P010 would be wrong.
The implementation probes the actual transfer path before selecting it.
Browser support remains separate: an installed Chromium build must expose
HEVC through its own kernel/Mesa integration.

## Implementation evidence

The three implementation tasks were built by `gpt-6.1-sol` agents. The
coordinator integrated their changes without changing the user's checkout.

- Rust 1.97.1 daemon all-target compilation passed on the integrated source.
- Core decoder-selection compilation and the five request-selection regressions
  passed. The full workspace Clippy hook with denied warnings passes.
- JavaScript syntax and TypeScript ratchet checks pass; the existing type
  diagnostic count decreased from 515 to 513.
- Python deployment syntax and all six operation regressions pass.
- The static regression-field resolver exposed its missing support for the
  repository's existing `asyncTest` harness. The authoritative marker parser
  now recognizes that exact declaration; a negative-case regression is added.
- Normal pre-commit formatting, catalog lint, Clippy and embedded JavaScript
  syntax checks pass. No hook was bypassed.

## Adversarial review dispositions

The reviewer examined the complete candidate `dfa775112` before test
execution. Repairs stay in the existing media planner and startup owner.

| Finding | Consequence | Disposition |
|---|---|---|
| P2: request hardware frames bypass FFmpeg autorotation | Rotated/mirrored input can change presentation when hardware is selected | Fixed: proven identity transform required for initial and continuation paths; rotated, mirrored and unknown transforms preserve existing software handling |
| P1: startup retry excludes every software encoder | Hardware decoding paired with software encoding cannot recover when device/input initialization fails after boot | Fixed: decoder-aware retry eligibility, frozen software-only successor, retained CPU reservation, existing one-shot owner and software startup allowance |

The review found no further blocking issue in browser diagnostics, launcher
ownership, FFmpeg mapping, planar transfer, qualification boundaries or test
marker parsing. Generic DRM on non-Pi Linux can consume some of the existing
30-second inventory budget; this is a recorded bounded inventory limitation,
not a hardware-support claim. No second review or early unit run is planned.

Repair regressions are
`request_decode_requires_proven_identity_transform_even_for_continuations`
and `request_decode_software_encode_has_one_frozen_software_startup_successor`.
Both now pass; no unit execution preceded review. The existing actor still owns
one in-process retry; its separately budgeted classified-health wrapper does
not introduce another attempt or a new watchdog.

## Historical physical acceptance — 2026-10-07

Paul supplied a Pi 5 for cinema testing. Tests use an independent clone and
isolated data/browser profiles; the existing desktop and system media tools
are retained. The merge of PR #832 is complete. Its later coverage build was
killed by SIGKILL before test execution; that does not establish a Pi defect.

Initial observed runtime: Raspberry Pi OS Trixie reference 2026-10-06, kernel
`6.18.50+rpt-rpi-2712`, ARM64, 16 GB memory; FFmpeg
`7.1.5-0+deb13u1+rpt2`, Chromium `154.0.8037.92`, Mesa `26.2.2`.
The active Wayland display is 3840x2160 at 59.982 Hz. Initial thermal status
reported no throttling. These are fixture observations, not minimum versions.

Both 8-bit and 10-bit HEVC completed the exact production request-decode,
hardware download and software H.264 encode graph in under 0.2 seconds for
two 160x120 frames. Driver diagnostics identify V4L2 stateless HEVC, DRM
frames and the expected downloaded planar depth. The actual OS diagnostic
uses `swfmt rpi4_8`/`swfmt rpi4_10`, whereas the original parser required
`swfmt=`. The parser correction stays at the existing inventory authority;
the earlier equals form is retained and the remaining proof requirements
are unchanged. All four focused request-decoder regressions pass, including
the captured-log positive and negative cases.

The native daemon builds successfully with pinned Rust 1.97.1 (cold release
build: 35m57s) and returns `/readyz`. Its actual inventory now reports both
request decoder depths operational. The existing app scans the fixture
library and directly plays 2160p Main10 through the patched browser's
`V4L2VideoDecoder`. The eight-second application run drops 10/192 frames,
all already counted in the initial three-second sample, with zero corruption.

Real web caps-v2 conversion exposed a missing native runtime component:
Pi OS's dynamic FFprobe cannot supply the bound executable identity required
by the existing planner. A legacy request without a capability document did
produce H.264/AAC, but does not prove the modern web path. The correct existing
deployment seam is `PLURX_BOUND_FFPROBE`, pointing to the project's pinned
static local-file parser, alongside the native FFmpeg and scanning FFprobe.
The existing script successfully built the pinned static FFprobe 8.1.3;
identity validation is unchanged. With the bound parser configured, the real
web caps-v2 request produces 720p H.264 from the long 1080p HEVC fixture.
The worker opens `/dev/media3` and `/dev/video19`, requests DRM frames,
downloads them and uses libx264. Playback advances without a media error;
forward and backward seeks resume past 120 seconds and 15 seconds in the
same session. The settled sample records 1,031 frames, 59 dropped and zero
corrupted; this is functional conversion evidence, not frame-pacing acceptance.
The deployment recipe now records the bound-parser prerequisite.

The native runtime tested commit `ef6dd77653645fbaca9cbd62037ee7dc3a6ded1f`.
The later reviewed candidate `457efbbff` adds browser reporting, tests and
deployment documentation; its badge change was source-tested locally, not
included in that native binary.

Sustained concurrency, subtitles and HDR acceptance remain pending; small
FFmpeg probes alone do not satisfy those rows. All changes and evidence are
batched in [PR #843](http://forge.lan:3000/noirr/plurx/pulls/843).

### Browser and decoded-picture observations

Four synthetic HEVC fixtures cover 1080p/2160p, 8/10-bit, 24 fps, eight seconds,
with AAC audio. For each 2160p depth, all 192 hardware-decoded frame hashes
match the software-decoded reference. A standalone 2160p Main10 to 1080p H.264
conversion completed in 6.448 seconds for eight seconds of media (1.25x).
These are short-fixture results, not sustained throughput guarantees.

Stock Chromium rejects the HEVC video tracks with an unsupported decoder
configuration and plays only their audio. An advancing timeline is therefore
not accepted as video playback evidence. The H.264 control renders at
1920x1080 using `FFmpegVideoDecoder`, with `kIsPlatformVideoDecoder=false`.
The range-capable fixture served all 192 frames with zero dropped/corrupted
frames and completed a backward seek. An earlier fixture lacked HTTP ranges;
that harness defect was corrected before accepting seek evidence.

An isolated, checksum-verified build from the Pi Chromium HEVC patch project
(`154.0.8037.57-1-rpt1-hevc1`) selects `V4L2VideoDecoder` with
`kIsPlatformVideoDecoder=true` for all four HEVC fixtures. No system package
was replaced, and the browser runs as the normal user with sandboxing. All
four cases complete and seek. The short 2160p Main10 case drops 52/192 frames
under the concurrent release compiler; with that owned compiler briefly
paused it drops 0/192. Both runs report zero corrupted frames.

The initial browser viewport was 1905x2140 despite its fullscreen window
state. Explicitly leaving and re-entering fullscreen establishes a 3840x2160
viewport. A separate Main10 run at that full output size drops 5/192 frames,
with zero corruption and working seeking. Decoder dimensions, viewport size
and HDMI mode are therefore recorded separately; these short cases do not
establish sustained full-screen frame pacing.

### HDR and Dolby Vision scope

The cinema target now includes HDR and Dolby Vision. Track three separate
facts: source format, renderer processing, and the HDMI signal delivered to
the display. No Dolby Vision output claim follows from Main10 decoding or
from processing Dolby Vision metadata into HDR10/SDR.

The [Pi Chromium HEVC patch project](https://github.com/sslivins/chromium-rpi-hevc)
is a candidate for retaining browser playback with hardware HEVC. Its
documentation explicitly leaves HDR dependent on the display/graphics stack.
[Firefox's stateless HEVC tracker](https://bugzilla.mozilla.org/show_bug.cgi?id=1969297)
records outstanding FFmpeg and SAND import work. The Chromium fixture results
above establish decoder use, while actual HDR output remains unverified.

[LibreELEC's HDR documentation](https://wiki.libreelec.tv/configuration/4k-hdr)
supports HDR10/HLG as a native Pi playback target. A libmpv/libplacebo backend
is an architectural option if browser output cannot meet that requirement;
it would retain the existing Plurx server, UI and playback authority.
[mpv's output documentation](https://mpv.io/manual/stable/#options-target-colorspace-hint-mode)
distinguishes using dynamic metadata from transmitting Dolby Vision/HDR10+
metadata. Actual Dolby Vision HDMI output on the Pi remains unproven. The
portable desk display is for development testing. The product must discover
each user's runtime/display capabilities; it must not depend on a particular
LG model or require a model-specific allowlist. Physical HDR verification is
independent acceptance work on a suitable display chain.

The source audit also found an existing browser reporting error:
`dynamicRangeBadge` treated CSS HDR capability as observed rendering. The
repair keeps decoder and delivery admission intact, describes the delivered
format/profile, and leaves actual display output unverified. Positive and
negative CSS answers cannot turn a delivery fact into measured HDR, DV or
SDR output. Its focused regression passes after the final review.


### Follow-up review, validation and cleanup

The single final adversarial review approved candidate `457efbbff` without
blocking findings. Focused checks then passed once on that candidate:

| Check | Result |
|---|---|
| `cargo test --locked -p plurx-core --features hiqlite-store --lib transcode::decoder_inventory::tests::request_` | 4 passed |
| `node tests/playback/web-policy.test.js` | 215 checks passed |
| `node tests/playback/player-input-contract.test.js` | passed |
| `python3 -m unittest tests.operations.test_docs_index tests.operations.test_infra_names` | 18 passed |
| `make validation-lint history-check` | passed |
| `python3 -m validation.regression_field` with the candidate, base and PR description | passed; focused Rust execution recorded above |

Normal pinned Rust 1.97.1 compilation and commit hooks pass. The full Rust
unit suite and main promotion gate are not claimed. Paul authorized resolving the CI conflict and merging PR #843 on 2026-10-07.
The follow-up removes the full Rust unit step from the main fast lane while
retaining all-target compilation, Clippy, policy and regression-field checks.
Full CI and coverage remain separate. The [PR description](http://forge.lan:3000/noirr/plurx/pulls/843) holds the
current candidate, review, fast-lane result and merge disposition; earlier
#832 evidence is retained.

[Sanitized device and validation evidence](http://forge.lan:3000/attachments/348a359f-ac02-4ec5-bf00-c9e89bd52f72)
is attached to PR #843 and was downloaded again to verify its hash:
`8c21bf990859adb29c68f16eaef99863209619f6eb9e346765128d3ef2c36eac`.
It excludes credentials, browser profiles, databases and media.

All three transient Pi services (browser, daemon and fixture HTTP server)
were stopped. The SSH tunnel was closed. The exact task scratch directory,
including native builds, extracted browser, fixtures and test credentials,
was removed. Verification found no task processes, matching user services
or listeners on the test ports. System packages were not replaced.
The 30-minute concurrency run was not performed; no background acceptance
job remains running.


### Main-lane CI follow-up

The full Rust suite is separate from the blocking fast lane. All-target
compilation, workspace/vendor Clippy, static policy and named regression
evidence remain required. A failed first preflight exposed repeated glob
compilation in the history audit: bounded caching preserves matching policy
and reduced the remote audit from a ten-minute timeout to about 100 seconds.
The complete local cached audit passed in 190.93 seconds.

Main subsequently added four read-only sharing-status assertions. The broad
process-shape inventory correctly noticed four new `.status()` calls; review
confirmed they launch no process, and the count now records those reads.
The failed remote validation attempt ran 294 methods, with 293 passing and
this single inventory mismatch. Main-lane Python receipts retain attributable
passing results and execute failed, new or changed methods plus unrun checks;
they share the existing receipt machinery and preserve historical source
attribution. The PR records the final continuation result.

The new main base was merged cleanly before qualification. Its integrated
Rust compilation and Clippy passed; all four decoder regressions passed with
`hiqlite-store`. This CI work did not start another process on the Pi.

The next main update (#848, `854c206a7`) was integrated and reviewed cleanly;
Rust 1.97.1 all-target checking, Clippy, formatting and embedded JavaScript
syntax passed again on that base. Superseded run 4268 had passed Python,
web, Apple and Android. Its Python evidence totals 301 validation and 733
operations successes, with two optional skips. Rust and Windows exhausted
the 30-minute budget without reporting a compiler error: Rust provisioning
alone took 21m36s. Their ceiling is now 60 minutes, retaining all compile and
Clippy commands and Windows' serial memory bound.

Individual retries of the failed build jobs removed run 4268's artifacts
while preserving its successful attempt-1 preflight log. Main Python receipt
recovery therefore also supports authenticated, bounded log evidence, with
strict historical runner/source checks for older output. New runs preserve
framed receipt snapshots in their logs as well as artifacts. Inherited passes
still require their original authenticated provenance; skips are never passes.
The live PR records final qualification and merge. No Pi process was restarted.


## Historical Docker-default setup — 2026-10-07

[PR #851](http://forge.lan:3000/noirr/plurx/pulls/851) records the earlier
installation stage. The table below preserves that historical source-development
snapshot; its pending entries are superseded by current acceptance above.

[The installation plan](RASPBERRY-PI-INSTALLATION.md) continues the work after
PR #843 merged with every required fast-lane job green. Docker will be the
normal server choice; native/systemd remains selectable. A single setup flow
will provision the media runtime and isolated desktop browser, manage device
access, and own upgrades/uninstall without deleting user data.

| Work | State | Evidence / next action |
|---|---|---|
| Independent clone and compiler | current main integrated; executable preparation in progress | Main `9994c23d6` integrated at `4115bfada`; pinned Linux all-target check and Clippy passed; focused test executable and ARM64 build follow |
| Pi media runtime | hardware and browser acceptance passed; app playback pending | Non-root Docker HEVC Main/Main10 each matched all 48 software-reference frame hashes; managed Chromium used V4L2VideoDecoder; stock-kernel Landlock absence blocks the existing protected probe |
| Setup lifecycle | focused regressions passed | Sol 6.1 owns Docker/default, native selection, prerequisites, ownership, upgrade recovery and removal |
| Host browser | focused regressions passed | Pin and verify the previously exercised HEVC browser; preserve ordinary Chromium and sandboxing |
| Final review and fast lane | both scope reviews complete; findings repaired | Eight focused packaging/upgrade tests and all nine probe regressions passed; prior Python successes retained through run 4307; physical acceptance and affected fast lane remain |
| Pi state | isolated acceptance preparation active | Previous round restored all 1,643 baseline package/version entries. Current round added ten Docker prerequisites without baseline upgrades, staged owned native assets/account, and built runtime assets; cleanup is required again after acceptance |

The installer will build from the chosen checkout initially. This automates
prerequisites rather than assuming prebuilt native releases that the project
does not yet publish durably. Physical HDR output remains a separate claim.

Implementation is tracked in [PR #851](http://forge.lan:3000/noirr/plurx/pulls/851).
The source assembly applies all 100 Jellyfin patches and the adapted Pi
request/SAND delta; native ARM64 compilation found a missing link dependency: request decoding
uses `v4l2_fmt.o`, which upstream listed only for the stateful backend. The
adapted patch now gives the request decoder its direct dependency. The corrected ARM64 build completed and passed its runtime feature inventory
assertions; no playback success is claimed from compilation.

### Installation review disposition

The single adversarial review of `c55437408` against `7f0142d45` requested
changes. It performed no tests or device mutations. The implementation agents addressed every finding. The later authorized namespace sandbox was outside that review. The coordinator used the user's delegated decision authority to keep the batched PR and obtain one review of the new scope before its tests, recording the exception to the one-review-per-PR convention.
All six focused regression groups passed in run 4296. Main subsequently added
automatic Docker GPU discovery and receipt-continuation fixes; integration
preserves the Pi override and its explicit request devices. Current-candidate
qualification follows, retaining applicable passing Python evidence.

| Finding | Required correction | State |
|---|---|---|
| Native rollback can leave a failed replacement active | Stop the replacement before restoring files; restore and verify the prior running state | repaired; regression passed |
| Upgrade adopts operator runtime additions/changes | Preserve prior identities, refuse modified managed outputs, and record only new or legitimate provider rewrites | repaired; regression passed |
| Interruption leaves no recoverable installation ownership | Persist transaction journal and backups before mutation; reconcile interrupted work deterministically | repaired; regression passed |
| Existing Docker without Compose fails provisioning | Detect and install missing Compose independently from Docker Engine | repaired; regression passed |
| Native service cannot read some user-selected media mounts | Provision appropriate supplementary groups and validate access as the service identity | repaired; regression passed |
| Native readiness ignores configured startup budget | Derive the deadline from the existing effective configuration contract | repaired; regression passed |

### Stock Pi probe isolation

Physical app acceptance identified a concrete prerequisite: stock Pi OS kernel
`6.18.50+rpt-rpi-2712` has Landlock disabled, and the bound probe launch returns
`ENOSYS` before streaming begins. Both Docker and native services share this
kernel. The user approved a namespace backend for verified ARM64 Pi hardware
while explicitly retaining Landlock on other systems and preferring it on a Pi
when available. Unexpected permission errors do not select the alternative.

The existing probe supervisor will retain sealed-parser/source identity,
seccomp execution restrictions, output bounds, deadlines and cancellation.
Bubblewrap supplies the missing process and filesystem isolation on the stock
Pi kernel. Native Bubblewrap 0.12 completed an unprivileged namespace capability
check; this is prerequisite evidence, not completed app playback. Container
Bubblewrap 0.8 and the narrow Pi Docker policy still require direct acceptance.
See [the installation plan](RASPBERRY-PI-INSTALLATION.md#6-pi-probe-isolation--preserve-landlock-elsewhere)
for the boundary, ownership and evidence requirements.

### Namespace review disposition

The new-scope adversarial review of `d740dc6fd` requested two corrections.
Both are implemented before focused qualification; the review itself ran no
tests or device operations.

| Finding | Correction | Evidence state |
|---|---|---|
| Non-root Bubblewrap uses a second user namespace after creating private devpts; the container policy omitted that call | Permit only AArch64 `unshare(CLONE_NEWUSER)`, leaving the pinned baseline and capability drop intact | Focused policy regression passed; actual Docker probe acceptance pending |
| An older receipt could allow an upgrade to overwrite and adopt operator files at newly managed paths | Reject every existing newly managed system destination absent from the previous ownership receipt before provisioning or writes | Older-receipt regression passed for all five new profile/license/provenance destinations |

Linux Rust 1.97.1 all-target checking passed for `d740dc6fd`. Clippy found two
test-only `unwrap_err` calls; those now use descriptive `expect_err` messages.
Candidate `4115bfada`, including current main, passed all-target checking and
Clippy. Eight focused packaging/upgrade/rollback methods passed after the
review repairs; subsequent edits did not change those Python/runtime assets.
All nine focused probe regressions then passed on `4115bfada`: the Pi
namespace launcher enforces held source/parser identity, rejects secondary
execution, hides parent files/process memory, reaps descendants and completes
cancellation. Existing Landlock production regressions also passed. No failed
method was rerun. Actual Pi app playback and the final gate remain; no merge
is claimed by this source record.

The coordinator then removed a per-probe daemon copy: Bubblewrap executes the
held bootstrap descriptor directly through private procfs and the bootstrap
checks device/inode identity. A bounded capability check successfully executed
a held executable after its pathname was deleted. This avoids allocating a
roughly 350 MB daemon copy per probe; the existing namespace and Landlock
regressions still qualify the final launcher. Superseded binary preparation
was stopped before its test/ARM64 stages completed, retaining dependency caches.

The ARM64 acceptance build uses the test profile to avoid a slow optimized
daemon build. Upstream `gemm-common` FP16 helpers rely on optimization to inline
into their target-feature callers; unoptimized ARM64 code generation failed
there. The acceptance compiler enables optimization only for `gemm-common`
and `gemm-f16`, retaining the application source and other build settings.
This is an acceptance-build limitation, not a change to the shipped release
profile or a claim of release performance.
