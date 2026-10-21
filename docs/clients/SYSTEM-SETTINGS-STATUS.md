# System settings — approved layout, measured playback and acceptance

**Status:** built; macOS locally qualified; Linux hardware acceptance open · **Updated:** 2026-10-20 · **Owner:** Sol 6.1 implementation,
Astra coordination/review · **Branch:** `codex/system-settings` · **Base:**
`2c0e8b14d` · Not deployed.

Companion to [API](../API.md#52-get-apiv1system) (response contracts) and
[operations](../OPERATIONS.md#hardware-transcode--recent-intel-gpus) (how to
read the controls). This ledger executes the approved Cinema System layouts.
It distinguishes implementation from measured platform acceptance.

## Implemented layout and facts

| Surface | Contract |
|---|---|
| Versions | One Cinema entry, one Jellyfin FFmpeg entry; independent collapsed package/build/location details; exact required package from backend metadata. |
| Transcoding | Encoding preference first; six collapsed configuration/capability rows; green Enabled or neutral Disabled; short backend/device details. |
| Additional support | AC-4, Dolby Vision, Vulkan/libplacebo and OpenCL say Available when present, without claiming alternatives are selected. |
| Performance | Identical representative decode → process → encode workloads, CPU and hardware pipeline columns; null until measured; explicit completion/fallback evidence. |
| Storage and logs | Collapsed initially; warnings remain visible; no hidden System logs or playback-history polling. |
| Devices | Observed inventory and configured/default/unresolved binding; no inferred cross-card routing. |

Runtime directory means the containing directory of the actual Cinema
executable. It does not imply a managed installation. Required source revision
is the package contract pin, rather than a claim that an arbitrary executable
reported that revision. Linux package identity remains separately unverified.

## Performance policy and workload boundaries

The user approved hardware-first Auto on 2026-10-20: prefer working hardware,
use representative playback speed to rank hardware options, and fall back to
CPU when needed. Explicit preferences remain explicit. FFmpeg process CPU is
reported separately: 100% means one CPU core; it excludes operating-system
and driver services.

The three cases are 1080p H.264 SDR → 720p SDR, 4K HEVC SDR → 1080p SDR and
4K HEVC HDR10 → 1080p SDR. The same fixed frames and output target are used for
software and each relevant hardware path, with warm-up and repeated medians.
Fixture acquisition/preparation is not timed. The former raw synthetic test
is not overall playback health or Auto ranking evidence. Aborted, partial,
fallback and stale reports cannot qualify Auto.

The user clarified that performance compares **pipelines**, not physical
cards. Linux columns are CPU, NVIDIA NVENC, Intel Quick Sync and VA-API;
macOS columns are CPU and VideoToolbox. Each measures the actual production
default for that backend. Physical inventory/binding is separate diagnostic
information; there is no per-card comparison, grouping or selection UI.

## Source-bound evidence and remaining acceptance

`tests/web/system-settings.test.js` drives the shipping renderer for package
pins, configured states, incomplete comparisons, missing CPU data, collapsed
logs and preference-safe polling. Existing settings and request-budget suites
remain part of the focused check.

`scripts/system-settings-preview` serves the actual shell and assets with
labelled macOS/Linux/unavailable API fixtures. These illustrate responses;
they are not a scan of the review machine. `scripts/system-settings-browser-check`
checks desktop/mobile, dark/light and alternate theme rendering, keyboard
expansion, idle network reads and preference preservation. Captures and a
receipt live in `target/system-settings-browser/`; generated files are not
repository fixtures.

| Evidence | State |
|---|---|
| Shipping JS/CSS source-bound browser rendering | Six earlier automated Chrome fixture renders passed; no script errors or page overflow; macOS/Linux/mobile/light/noirr/unavailable fixtures. |
| Focused frontend regressions | 14/14 targeted checks; existing settings 71/71 and request-budget 44/44; 95 served scripts parse. |
| Rust build, Clippy and focused regressions | Pinned Rust 1.97.1 all-target core/daemon check, Clippy with warnings denied and workspace formatting passed; focused backend regressions passed (19 passed, one ignored hardware test run separately). |
| Actual macOS representative comparison | Passed against local Jellyfin FFmpeg 8.1.3: six warm-ups and 18 measured runs, exact 240 frames each, no fallback. Native VideoToolbox decode/scaling and Metal HDR tone mapping selected. |
| Actual Linux multi-GPU comparison | Pending real hardware/driver acceptance; illustrative API fixtures prove presentation only. |
| Parent independent CUA review | Final shipping macOS measured and Linux fixture desktop captures accepted; Linux mobile has no page overflow. Separate from earlier automated Chrome fixture captures. |

No daemon is restarted or deployed by this work. A browser fixture does not
qualify picture fidelity, Linux hardware, energy use or physical playback.

## Local macOS measurement receipt

The ignored `real_media_production_tools_comparison` test passed in 90.22 s
with native macOS processing enabled and the shared background-process
launcher. All six warm-ups and 18 measured attempts completed exactly 240
frames without fallback. The generated receipt is
`target/system-settings-macos-comparison.json`; the preview's
`?fixture=macos-measured` view reads that receipt. Its FFmpeg SHA-256 is
`37d84341799d4d640ed87eb82a9f113e2e7564498eac2c26acac904730880251`.
The production planner selected software decode/processing for the CPU
baseline, VideoToolbox decode/scaling for both SDR cases, and VideoToolbox
decode plus Metal tone mapping for HDR10.

| Fixed 10-second source | CPU median | VideoToolbox median | CPU-relative speed |
|---|---:|---:|---:|
| 1080p H.264 SDR → 720p SDR | 1.344 s | 0.927 s | 1.45× |
| 4K HEVC SDR → 1080p SDR | 3.015 s | 1.418 s | 2.13× |
| 4K HEVC HDR10 → 1080p SDR | 3.531 s | 1.434 s | 2.46× |

Each case uses H.264 output at 4 Mb/s, one warm-up and the median of three
complete runs. These bounded local results do not establish whole-system
energy use, picture fidelity or Linux driver acceptance.

Reproduce the native test with the node's exact runtime pair (no daemon
contact or restart):

```bash
PLURX_FFMPEG=/path/to/runtime/ffmpeg \
PLURX_FFPROBE=/path/to/runtime/ffprobe \
PLURX_COMPARISON_MACOS_PROCESSING=1 \
PLURX_COMPARISON_RECEIPT="$PWD/target/system-settings-macos-comparison.json" \
scripts/quiet /opt/homebrew/bin/rustup run 1.97.1 cargo test -p plurxd --bin plurxd \
  transcoder_optimization::comparison::tests::real_media_production_tools_comparison \
  -- --ignored --nocapture --test-threads=1
```

The accepted run's log is
`target/quiet/target-debug-deps-plurxd-52eb889ac18c7d28-transcoder_optimiz-20261020-190102.log`.

## Final focused verification

Pinned Rust 1.97.1 passed on the frozen source before the final native run:

```bash
/opt/homebrew/bin/rustup run 1.97.1 cargo check -p plurx-core -p plurxd --all-targets --features plurx-core/hiqlite-store
/opt/homebrew/bin/rustup run 1.97.1 cargo clippy -p plurx-core -p plurxd --all-targets --features plurx-core/hiqlite-store -- -D warnings
/opt/homebrew/bin/rustup run 1.97.1 cargo fmt --all -- --check
/opt/homebrew/bin/rustup run 1.97.1 cargo test -p plurxd --bin plurxd -- ffmpeg_support::tests transcoder_optimization:: \
  --nocapture --skip complete_macos_package_runs_metal_and_vulkan
```

The focused backend suite passed 19 tests, with the native hardware test
run separately. Final check/Clippy/format/test logs are under `target/quiet/`
with timestamps `185617`, `185648`, `185620` and `185859` on 2026-10-20.
The final frontend command
`node --test tests/web/system-settings.test.js tests/web/transcoder-backend-requests.test.js`
passed 14 tests after the CPU-unit copy and storage refresh regression.

## Main-bound integration

The reviewed implementation commit `322c0c928` was preserved on
`codex/system-settings-reviewed` and ported alone onto current main
`2c0e8b14d`. The original branch also contained unrelated formal-verification
documentation; those commits are excluded from this pull request. The port
retained both sides of changelog/backlog insertions. The replay itself changed
only those insertion contexts; this ledger records the new base and validation.
Production comparison and renderer JavaScript are unchanged from the
independently reviewed implementation; the mobile CSS containment correction
below preserves the approved layout.

Astra's completed adversarial review and final shipping-render acceptance
apply to that implementation. Sol reviewed the integration delta and reran
the pinned compiler/lint/focused-unit loop and web syntax/contracts against
the main-based candidate. The earlier native measurement receipt still
applies to the unchanged production pipeline; Linux hardware acceptance
remains open. The ready pull request must pass the affected main fast lane
before merge. No deployment is part of this integration.

The main-based loop passed on 2026-10-20: all-target core/daemon check and
Clippy with warnings denied, workspace formatting, 19 daemon regressions
(one native hardware test intentionally separate), two core inventory/binding
regressions with `hiqlite-store`, 14 targeted System/request tests, existing
settings/read-budget/asset suites, 95 served-script syntax checks, unchanged
frontend type baseline, 12 package-matrix checks and 11 license-notice checks.
Paperwork passes all 15 rules with the PR's exact regression-anchor body.
The macOS test linker reports its existing compact-unwind size warning;
the focused suites pass and Clippy emits no warnings.

CI registration preserves the reviewed ownership boundaries: the structural
census records one additional owned child/wait/kill-on-drop and a net one
timer (two comparison bounds replace the startup benchmark bound). Patterns
and scan roots remain unchanged. The native comparison is registered as an
exact opt-in hardware fixture, separate from known-red product debt; the
unknown-ignore count is unchanged. Production and renderer code are untouched.

The complete `make web-unit-check` target passes after replacing two System
mobile grid tracks with `minmax(0,1fr)`, retaining the approved single-column
layout while preventing intrinsic-width overflow. CUA verified the shipping
Linux fixture at a 390 px viewport: page client/scroll widths are both 375 px.
The temporary viewport override was reset. This changes neither the native
comparison pipeline nor its measurement receipt.
