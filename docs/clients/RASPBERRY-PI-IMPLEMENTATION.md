# Raspberry Pi implementation — extend the existing server and web player

**Status:** ready to build, physical acceptance pending · **Written:**
2026-10-06 · **Executes:** Paul's request for server and HDMI playback on one
Pi 5, using existing Plurx and as much real hardware acceleration as possible.

Read [the status page](RASPBERRY-PI-STATUS.md) for progress and evidence,
[the web map](WEB-SHELL-LAYOUT.md) before touching the embedded application,
and [the development pipeline](../DEVELOPMENT_PIPELINE.md) for the ordinary
workflow. Work within existing media selection and playback ownership. A
step that appears to require a new client, a second playback controller, a
watchdog, a browser fork or replacement of the global media runtime must be
recorded as a separate decision, not slipped into this implementation.

## 1. Objective — one Pi, the same product

Run the existing ARM64 `plurxd` service and existing fullscreen web interface
on Raspberry Pi OS 64-bit. Preserve direct play and remux. Add server HEVC
request decoding where the installed FFmpeg actually implements it, and
report browser capabilities without claiming they prove hardware activity.

```text
 SSD / NAS ──▶ existing plurxd ──▶ original / remux ──▶ existing web player
                    │                                      │
                    │ video conversion when required       │ browser owns
                    ▼                                      │ client decoder
          HEVC V4L2 Request decoder                         ▼
                    │                               Pi graphics / HDMI
          transfer to CPU frames
                    │
          existing renderer + software encoder
```

The server's decoder does not decode the browser's video element. Serving an
HEVC stream without converting it can leave server decoder utilization at
zero while the browser uses the same board's hardware decoder. That is the
preferred successful outcome, not missing acceleration.

### 1.1 Supported target and physical limits

| Surface | Target | Limit / evidence needed |
|---|---|---|
| Host | Raspberry Pi 5, ARM64 Raspberry Pi OS | Record board, OS, kernel, Mesa and package versions |
| Server | Existing Linux binary and systemd service | Existing ARM64 packages; on-device startup still needs proof |
| Display | Existing web UI in installed Chromium, normal desktop user | Browser build must expose the appropriate media path |
| HEVC decode | Main/Main10 4:2:0 through V4L2 Request | Exact runtime and frame transfer must work; advertisements are insufficient |
| Other codecs | Existing software or independently discovered backend | Do not infer acceleration from Pi model or GPU presence |
| Video encode | Existing software encode on Pi 5 | The board has no hardware video encoder |
| HDR | Preserve original HDR where browser and display attest support | Decoder support alone does not prove HDR output |
| Subtitles | Existing delivery and renderer policy | Do not force new burn-in or pretend current web overlays cover all formats |
| Concurrent use | Local display plus serving another client | Measure sustained performance and thermal throttling |

Initial suggested fixture hardware is an 8 GB board with active cooling,
SSD storage and wired Ethernet. These are validation conditions, not code
requirements. No model-name check may disable the app on other hardware.

### 1.2 Non-goals

- No new native client, Kodi integration or replacement web application.
  Existing product behavior is the scope the user selected.
- No fabricated Pi hardware encoder, VA-API mapping, or stateful
  `hevc_v4l2m2m` substitute for the stateless request interface.
- No driver installation, unsafe browser flags, privileged container, or
  downloading an unreviewed browser binary as part of the installer.
- No global replacement of Jellyfin FFmpeg. Its AC-4, Dolby Vision bitstream
  filter and tone-map contracts must survive this work.
- No new recovery loop, polling watchdog, retry policy or transcoding farm.
  Existing recovery and cluster routing keep their current ownership.
- No claims of 4K60, HDR, zero-copy, passthrough or multiple-stream capacity
  until the exact physical path has been exercised.

## 2. Root causes — integration gaps, not a platform rewrite

Source baseline: `cca4a09b997171ba91136b396f7d484097305fd0`.
Re-verify symbols at implementation time; these are source anchors, not
immutable line numbers.

| Gap | Source | Architectural correction |
|---|---|---|
| Server backend vocabulary has no request decoder | [decode.rs](../../crates/plurx-core/src/transcode/decode.rs), `DecodeBackend` | Represent the backend independently of encoder family and FFmpeg CLI spelling |
| Software encoding always prefers software decoding | Same file, `preferred_backend` / `resolve_transcode` | Permit measured compatible HEVC hardware decode with CPU encode |
| DRM advertisement cannot establish request support | [decoder_inventory.rs](../../crates/plurx-core/src/transcode/decoder_inventory.rs), `advertised_backends` / `probe_decode` | Exercise request-specific initialization and transfer; preserve unavailable/unknown |
| Manager currently supplies no hardware capability rows | [manager/plan.rs](../../crates/plurxd/src/transcode/manager/plan.rs), `DecodeCapabilities::new` | Carry the node's measured result through the existing snapshot, without inventing qualification |
| Browser prediction can describe source rather than delivered output | [measurements.js](../../crates/plurxd/src/web/player/measurements.js), `probeDecode` | Probe an evidenced delivered codec, dimensions, cadence and bitrate |
| Browser efficiency prediction is labelled hardware identity | [stats.js](../../crates/plurxd/src/web/player/stats.js), decoder row | Report prediction provenance; hardware identity remains unknown |
| Service and browser have separate device access | [plurxd.service](../../deploy/plurxd.service) and [install](../../deploy/install) | Document minimum device-group access per process; run browser as desktop user |

The web capability selector already probes codec/container support at runtime.
There is no demonstrated Linux HEVC blacklist to remove. Do not add a
Pi-specific allowance that bypasses negative browser evidence.

## 3. Decoder contract — backend, implementation and frames are separate

### 3.1 Backend identity and FFmpeg arguments

Extend the existing `DecodeBackend` vocabulary with a V4L2 Request variant
and stable `v4l2_request` spelling. Extend exhaustive parse/name/ALL matches
and tests. The semantic backend name is not necessarily an FFmpeg method.
For the inspected Pi FFmpeg implementation:

| Concept | Value |
|---|---|
| Semantic backend | `v4l2_request` |
| FFmpeg hardware method | `drm` |
| FFmpeg video decoder | `hevc` |
| Hardware implementation within that decoder | `hevc_v4l2request` |
| Hardware frame format | `drm_prime` |

Do not emit `-c:v hevc_v4l2request` or `-hwaccel v4l2_request` for this ABI.
Keep backend-to-CLI mapping in one shared function, used by inventory and
production argument construction. Existing backend names and cache identities
must remain unchanged for unaffected plans.

### 3.2 Discover operational support, with bounded work

Extend the existing startup inventory rather than create a new supervisor.
Use its current per-process and overall deadlines, process teardown and
output bounds. Only probe HEVC request decoding on relevant Linux builds.
A generic `drm` entry is a candidate, never a success.

A successful result must establish all of:

1. The requested HEVC decoder opened and decoded real input.
2. Request-specific initialization identified the hardware implementation;
   generic DRM frame allocation or a software decode cannot satisfy it.
3. The output frame format and required CPU transfer/conversion worked.
4. The invocation exited successfully and supplied expected output frames.

Probe 8-bit and 10-bit support separately if both are to be selected. Record
which class succeeded; an 8-bit probe must not admit Main10. A tiny startup
clip proves operability for its measured class, not a 4K60 performance
qualification. Keep failed, absent and unmeasured results distinguishable.

### 3.3 Select hardware decode independently of software encode

Use the existing `DecodeCapabilities`, `DecodePolicy`, restrictions and
resolved-plan machinery. Prefer this backend only for the HEVC input class
actually supported by the installed runtime. Preserve explicit software
selection, existing decoder exclusions, failure fallback, delivered dynamic
range and all software-only renderer requirements.

A Pi backend is ordinary discovered infrastructure; no Pi feature gate or
extra switch is needed. Actual incompatible media may still select another
working decoder: that is media planning, not a restriction on a user's saved
choice. Existing Developer switches keep their current advisory lifecycle.

Do not label a startup success as a health-qualified diagnostic contract.
Existing contract identities bind exact FFmpeg build, backend, decoder and
media class. Never copy a software health receipt to the Pi backend or move
unqualified output into the qualified artifact namespace.

### 3.4 Transfer frames through the existing renderer

Represent DRM PRIME frames in the existing surface contract. Server software
encoding requires CPU-addressable frames. Verify the Pi runtime's actual
SAND/tiled-to-linear transfer behavior and preserve bit depth before scaling,
subtitle rendering or tone mapping. Do not assume every DRM frame is linear
NV12 or that every 10-bit transfer returns P010.

Shared argument construction must cover both HLS and VOD. Keep input hardware
arguments before the input; introduce only one required transfer before CPU
filters. Hardware decode plus a CPU transfer is acceleration, but is not an
end-to-end zero-copy claim. If the runtime cannot transfer the measured
class, leave it unsupported and use the existing fallback.

## 4. Browser contract — keep the existing player and truthful evidence

### 4.1 Delivered media determines the probe

`probeDecode()` must use the attached output stream's codec/profile, encoded
size, bitrate and cadence. Prefer parsed active representation data or the
server's explicit output facts. For unchanged direct/remux video, source
facts may describe that same encoded track. Do not use original HEVC facts
for H.264 output, the first codec in a speculative ladder, or invented 24 fps.
If required facts are missing, leave the prediction unavailable.

Keep the existing media-attachment ownership checks around asynchronous
results. Clear stale prediction data at attachment/representation changes;
results for an earlier probe must not overwrite a later stream's facts.
Use existing media events, not a new polling loop.

### 4.2 Prediction does not identify the active decoder

Preserve `supported`, `smooth` and `powerEfficient` as separate nullable
observations. Label them as MediaCapabilities predictions. Neither
`powerEfficient: false` nor missing API support proves software decoding or
absence of a GPU path. Do not report `supported: false` as an active playback
failure from this diagnostic.

Keep existing telemetry consumers compatible. A legacy hardware field must
remain unknown unless backed by actual hardware identity; add explicit
prediction fields where the receiving contract supports them. Update shared
contract descriptions through their source generator, not just generated JS.
The ordinary browser cannot name its active platform decoder through this
API; operator evidence comes from Chromium media diagnostics on the Pi.

## 5. Deployment — existing installation, explicit runtime prerequisites

### 5.1 Native server is the first hardware-enabled deployment

Use the existing `deploy/install linux --binary ...` path. Keep the current
`PLURX_FFMPEG` and `PLURX_FFPROBE` configuration as the media-runtime seam.
Native Pi OS FFmpeg is eligible only when its operational probe succeeds.
Document package versions and both selected executable paths in acceptance.

Add an opt-in fullscreen launcher for the existing web URL, using the
installed Chromium executable and a normal desktop user's session. It must
preserve sandboxing, validate URL/arguments, have deterministic install and
uninstall behavior, avoid tokens in command lines, and never overwrite the
user's existing browser profile. Use the existing auth flow.

Document service `video`/`render` group access, actual `/dev/video*`,
`/dev/media*` and render-node permissions. Do not hard-code `video19`, make
nodes world-writable, grant blanket privileges or infer backend support from
a device filename. Browser permissions belong to the desktop session, not
the `plurx` service account. Avoid changes to existing units by default.

### 5.2 Container and browser runtime limits

The standard ARM64 container is an execution target, not a promise of Pi
request decoding. Its pinned Jellyfin runtime is not established to provide
the inspected request backend. This change does not replace it with an older
Pi fork that loses existing media contracts. Native compatible FFmpeg is the
initial accelerated server route; container runtime packaging is a separate
follow-up only if source/runtime validation demonstrates it is needed.

The browser's HEVC path likewise depends on Chromium, kernel, Mesa and
Wayland support. The external Chromium Pi HEVC patch project demonstrates
that compatible builds exist; it is not an endorsed automatic dependency.
Start by measuring the installed browser. If it cannot expose HEVC hardware
decode, record that exact limitation and the required runtime update. Do not
route around it with a hidden external player or a new app.

### 5.3 Read-only readiness report

Provide a small operator diagnostic which reports OS/architecture, chosen
FFmpeg/ffprobe and Chromium versions, advertised hardware methods, and visible
media-device permissions. Bound subprocess execution, tolerate missing tools,
and never print environment secrets. Separate candidates from proof. A JSON
mode should support storing a sanitized receipt and a text mode should say
what needs attention. The report must not install packages or write config.

## 6. Build ownership — Sol 6.1 processes, one integration branch

Paul's latest instruction on 2026-10-06 supersedes the normal effort/task-PR
workflow for this change. Work only in the independent agent clone, retain
normal commits, batch into one main-bound PR, and perform one adversarial
review when its implementation is ready. Then run the fast lane and rerun
failed checks only. Do not weaken CI or hide failing checks to obtain green.

| Milestone | Owner | Files / boundary | Acceptance |
|---|---|---|---|
| P0 contract and status | Coordinator | This plan, status, docs index, PR and integration | Traceable decisions, source anchors, progress page |
| P1 server request decode | Sol 6.1 decoder agent | Core decode/inventory/arguments, manager plan, affected exhaustive matches and decoder tests | Pinned compiler and regressions below; device qualification separate |
| P2 web output evidence | Sol 6.1 web agent | Measurements/stats, authoritative info contract and generated descriptions, existing playback tests | Wrong-source prediction removed; stale/unknown evidence handled |
| P3 native deployment | Sol 6.1 deployment agent | Installer/launcher/report, deploy documentation and operations tests | Safe dry-run/install/uninstall contracts; no hardware claims |
| P4 integration and review | Coordinator + adversarial agent | Combined diff, review disposition, regression fields | Findings fixed, final candidate recorded |
| P5 final validation | Coordinator | Existing fast-lane jobs and targeted physical evidence | Each required check passes on merge candidate; unavailable physical rows explicit |

Agents share the independent clone with disjoint ownership; the coordinator
serializes Git staging and commits. No agent may reset, clean or switch the
shared branch, touch the user's checkout, run tests early or publish a partial
ready PR. Compile checks may run during development; use the repository-pinned
Rust 1.97.1 and one coordinated target directory to avoid duplicate builds.

## 7. Validation — exact candidate, bounded checks, honest hardware limits

### 7.1 Development compiler evidence

The host has the pinned toolchain; its default Homebrew Rust is different.
Use the explicit pinned executable path or `rustup run 1.97.1`. Compile the
unchanged baseline before Rust edits, then changed production and test targets
before push. Normal commits run the tracked lint/syntax hook.

```bash
rustup run 1.97.1 rustc --version
rustup run 1.97.1 cargo check --locked -p plurxd --all-targets
rustup run 1.97.1 cargo check --locked -p plurx-core --test decoder_selection
rustup run 1.97.1 cargo fmt --all --check
rustup run 1.97.1 cargo clippy --workspace --all-targets -- -D warnings
```

These are compilation/lint commands, not early unit-suite execution. If the
base moves, refresh compilation against the actual candidate. Do not use CI
to discover ordinary compiler errors.

### 7.2 Focused regressions retained for the final validation window

| Area | Required negative and positive cases |
|---|---|
| Backend identity | Semantic/CLI names differ correctly; existing names unchanged |
| Probe | DRM-only, software fallback, missing initialization, timeout, transfer failure rejected; real request diagnostics accepted |
| Media class | HEVC 8/10-bit handled separately; unsupported chroma/codec/unknown class does not gain fabricated evidence |
| Selection | Software encoder with measured request decode; explicit software, exclusions, incompatible renderers and fallback retain meaning |
| Arguments | HLS/VOD same input method and transfer; depth preserved; no invented decoder name |
| Artifacts | Distinct backend identity; unqualified probe cannot mint health qualification |
| Web | HEVC source/H.264 output; unknown dimensions/cadence/bitrate; missing and false predictions; stale attachment/representation |
| Deployment | Shell argument safety, non-root browser, invalid/token URLs, missing tools, bounded diagnostics, isolated profile, uninstall ownership |
| Documentation | Docs index, playback contract generation and references stay consistent |

Record actual runnable test names as they are implemented and include one
`Regression-Test: path::test_name` per retained regression in the PR body and
landing message. Use existing test targets and the fast-lane scope; no new
periodic test schedule. Passing unrelated checks are retained if one test
fails; rerun the failure and checks affected by its correction.

### 7.3 Physical acceptance matrix

No Pi was identified at plan creation. Keep these rows pending until executed;
software completion does not imply hardware acceptance.

| Scenario | Run | What proves success |
|---|---|---|
| Install/start | Native daemon, reboot, fullscreen browser | `/readyz`, normal login, existing library/player works |
| HEVC Main/Main10 | 1080p and 2160p fixtures with declared cadence/bitrate | Actual platform decoder evidence, correct visible output, no persistent drops |
| Server HEVC conversion | Known input to software output encode | Request initialization, correct transfer, output picture and audio, measured speed |
| Software codecs | H.264 plus representative VP9/AV1 | Correct playback or truthful capability-based selection; no hardware claim |
| Seeking | Repeated forward/backward seeks in direct, remux and converted playback | Existing session ownership; no stale capability facts or restart loop |
| Subtitles/audio | Text, ASS/PGS cases supported by existing paths, track changes | Correct timing and output; burn-in cost visible where required |
| HDR/SDR | Main10 on each connected display mode | Actual output color/HDR verified, not inferred from decoder support |
| Concurrency | 30-minute local playback plus a remote direct-play client and scan | CPU/memory/temperature/drop/underrun measurements; no throttling-driven failure |
| Missing device/runtime | Remove access or select software-only runtime in a disposable setup | Existing software fallback; accurate readiness; user choices preserved |

Keep fixture identity/hash, candidate SHA, exact commands, runtime versions,
selected backend, elapsed time and results. Do not store credentials, bearer
URLs, browser profiles or media files in the repository. System warnings are
advisory; no pending row disables a saved Developer choice.

## 8. Review, merge and cleanup

Freeze the code candidate before the single adversarial review. Review the
root-cause claims, planner invariants, process/output bounds, frame transfer,
health/cache identities, diagnostic truthfulness and installer ownership.
Record findings and their concrete disposition on the status page. Fix
findings before marking the PR ready and starting its fast lane.

After required checks are green, merge through Forgejo with the regression
lines in `MergeMessageField`. If main changes incompatibly, reconcile the
candidate and record which evidence needs refreshing. No deploy or hardware
acceptance is implied by merge. Remove agent-only branches, logs, compiler
artifacts and scratch tools after durable evidence and documents are retained.
Never clean the user's clone or delete operator-installed assets.

## 9. Sources and assumptions

- [Pi 5 specifications](https://www.raspberrypi.com/products/raspberry-pi-5/):
  HEVC decode block and display capability; not a browser support matrix.
- [Pi multimedia architecture](https://www.raspberrypi.com/news/optimising-raspberry-pi-5s-software-environment/):
  V4L2 stateless HEVC and absence of video encode hardware.
- [Pi FFmpeg request implementation](https://github.com/jc-kynesim/rpi-ffmpeg/blob/dev/7.1.1/rpi_import_1/libavcodec/v4l2_request_hevc.c):
  implementation and frame ABI; this branch is evidence, not a package pin.
- [Chromium Pi HEVC patches](https://github.com/sslivins/chromium-rpi-hevc):
  external browser integration and prerequisites, not an installation promise.
- [Existing playback architecture](../PLAYBACK.md) and
  [client contract](../CLIENTS.md): preserve shared product behavior.

Inspected 2026-10-06. External runtime versions must be captured on the device
rather than inferred from these pages or from installed package names.
