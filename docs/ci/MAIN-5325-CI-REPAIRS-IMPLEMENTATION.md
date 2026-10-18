# Main 5325 CI repairs — implementation plan

— four repo-side fixes for jobs that cannot pass in the full `ci.yml` sweep, each from a measured root cause

**Status:** open · **Recorded:** 2026-10-17

Full run 5325 on main `89d790d2` failed six jobs. PR #1150 (`claude/main-5325-repairs`) already fixes two of them: the shared-source fixture contention and the web golden drift. It also fixes the Cinema browser harness that the drift was hiding. This plan covers the remaining four. Claim board: issue #1148. The maintainer approved all four on 2026-10-17, including moving `check` to the topology runner.

**Rules for the builder.** Make one commit per item on `claude/main-5325-repairs`, and keep PR #1150 a draft (`WIP:`). Do not dispatch CI, and do not merge. Change the repository only: no runner services, quotas, or labels, except the one provisioning prerequisite in item 2, which is called out there. Committed text names runners by role or label, never by host name or address. A commit whose subject starts with `fix(` or `perf(` is corrective. `make history-check` requires such a commit to change a surviving test or assertion in the same commit, or to add a `validation/regressions.d/<first-commit-prefix>-<slug>.toml` fragment (see [VALIDATION.md](../VALIDATION.md), "One mapping, one file"). Before pushing, run `python3 -m unittest tests.operations.test_contracts tests.operations.test_docs_index`, `make validation-lint` and `make history-check`. A skipped check counts as a failed one.

## Shared context: the topology runner is the only big runner

The `[baremetal, ci-topology]` label has exactly one runner. Its cgroup allows 18 CPUs and has a 24 GiB MemoryHigh. The "high-cpu" general runners have 4–8 GiB. The topology runner's host is also oversubscribed by its other runner instances: their combined quotas exceed its RAM, and swap ran 100 % full during run 5325. That is infrastructure for the maintainer, outside this plan. It makes every fix below more important, because each one removes repeated or serial work.

Measured on a 16-core build host at `b7a9d6f55`, with a cold cache:

| Build | Wall time | Target size |
|---|---|---|
| `cargo check --workspace --all-targets` at `-j8` | 220 s; peak combined rustc RSS ≈ 15 GB | 1.0 GB |
| `plurxd` crate, test codegen, single rustc | peak RSS ≈ 13.8 GB | — |
| Hiqlite WAL lib tests `--no-run` | 1 m 36 s | 1.4 GB |
| `plurx-cluster-check` `transport-recovery` profile | 10 m 25 s | 2.4 GB |
| `plurx-cluster-check --lib --no-run` | 2 m 14 s | 1.7 GB |
| topology lane (Hiqlite clippy + check tests + spike) | 57 s | 3.5 GB |
| rust-gate (`clippy --workspace` + `test --workspace --no-run`) | 6 m 39 s | 5.3 GB |

Persistent runner targets grow past these clean sizes, because cargo never garbage-collects stale artifacts. On the runner, the contracts lane alone measured 12.5 GB.

## 1. Topology runner: parallel builds and a cache that fits

**Root cause.** Commits 526d790d and d1cd7235f moved `rust_compile` and `cluster_wal` onto the topology runner (and `rust_compile` in effort-ci.yml). They also kept `CARGO_BUILD_JOBS: "1"`, a setting meant for the 4 GiB runners where parallel rustc thrashes. On the topology runner it serialises every compile, including rustc codegen through the jobserver. Run 5325's WAL job spent 12 m 21 s compiling dependencies alone (06:45:36 → 06:57:57), then ran out of its 30-minute timeout while still compiling `plurxd`. A cold parallel WAL job on 2026-09-26 compiled the same dependency set in 1 m 17 s and `plurxd` in 3 m 41 s.

Separately, `.github/actions/cargo-cache/action.yml` defaults `budget-gb` to `30`. That budget applies to the whole runner instance, across every lane. Nine lanes share the topology runner: `rust-compile`, `effort-rust`, `cluster-topology`, `cluster-transport-recovery-contracts`, `-voter`, `-learner`, `-aggregate`, `cluster-wal`, and a dead `cluster-transport-recovery` directory left on disk. They do not fit. `scripts/ci-cache-prune` evicts the least recently used lane, and a job killed at its deadline never runs `cargo-cache-finalize`. So the WAL lane never refreshes its `.last-used` marker and is evicted every run. Run 5325's WAL lane directory was created at the run's own start, after the target from run 5228 was gone.

**Edits.**
- In `.github/workflows/ci.yml` (lines 266 and 933) and `.github/workflows/effort-ci.yml` (line 128), set `CARGO_BUILD_JOBS: "6"`. Add a comment: the topology cgroup is 24 GiB; the `plurxd` test codegen peaks at about 13.8 GB on its own; the workspace check at `-j8` peaked at about 15 GB combined. Six leaves headroom for the `plurxd` crate plus five smaller crates. Do not touch the Windows or coverage `CARGO_BUILD_JOBS: "1"` lines; they run elsewhere.
- Merge the voter, learner and aggregate lanes into one `lane: cluster-transport-recovery` (ci.yml lines 696, 764 and 834). All three build the same `plurx-cluster-check` `transport-recovery` profile. Cargo's own target-dir lock makes the shared lane safe. Keep `cluster-transport-recovery-contracts` separate, because it is a different profile.
- On every `cargo-cache` step of a job whose `runs-on` contains `ci-topology`, pass `budget-gb: "60"`. That covers `rust_compile`, `cluster_topology`, the four transport-recovery jobs, `cluster_wal`, and `check` after item 2. Every job on one runner must pass the same budget, because whichever job passes the smallest one prunes everyone else's lanes. The runner workspace showed more than 400 GB free.
- In `tests/operations/test_contracts.py`, change the assertions that pin `CARGO_BUILD_JOBS: "1"` for these jobs (about lines 1749 and 2186–2189). They should now assert the new value and the shared budget, and that no job on the topology label passes a different `budget-gb`. Add an assertion that the three transport-recovery role jobs share one lane.

**Subject:** `fix(ci): build in parallel on the topology runner and fit its cache lanes`. The test change satisfies the history check.

**Proof in one full run:** `replicated WAL recovery contracts` finishes inside 30 minutes, and its compile step's `Compiling plurxd` line appears within minutes of the job's start.

## 2. `check` ("Rust unit and SQLite contracts") on the topology runner

**Root cause.** `check` has not succeeded since 2026-10-08. It runs in the `plurx-ci` container on `[general, high-cpu]`. Inside a container, `cargo-cache` falls back to a cache root inside the ephemeral workspace (see the "Container jobs inherit…" comment in action.yml). So every run compiles cold and downloads cmake again. The 4–8 GiB "high-cpu" runners cannot hold the `plurxd` test build: one stalled for 70 minutes compiling `plurxd` (job 52415). Run 5325 spent 4 m on clippy and 6.5 m building tests. The core library tests then took 1320 s and integration tests 825 s, all under host swap pressure, and the job was killed while the `plurxd` binary tests were running.

**Edits.**
- At ci.yml line 301, replace `runs-on: [self-hosted, Linux, X64, lab, general, high-cpu]` and the `container:` block with `runs-on: [self-hosted, Linux, X64, lab, baremetal, ci-topology]`, running directly on the host.
- Remove the two workspace-owner steps, "Capture mounted runner workspace owner" and "Restore persistent runner workspace ownership". They exist only for the container bind mount.
- Keep `rust-toolchain`, `rust-native-deps`, `ffmpeg` (major 8, binary `/usr/lib/jellyfin-ffmpeg/ffmpeg`), `cargo-cache` (`lane: rust-gate`, `budget-gb: "60"`), `make ci-rust-gate`, and `cargo-cache-finalize`. Add `CARGO_BUILD_JOBS: "6"` as in item 1.
- **Provisioning prerequisite (one-time, on the topology runner host).** The `ffmpeg` action refuses to apt-install as a non-root, self-hosted runner (action.yml, the `id -u` branch at about line 42). It expects the binary to already be on the runner. Before the full run, verify `/usr/lib/jellyfin-ffmpeg/ffmpeg -version` reports major 8 and `pkg-config --exists openssl` succeeds on that host. If not, install `jellyfin-ffmpeg8` and `libssl-dev` there. This is the only host change, and the maintainer approved it with option (a).
- Keep `timeout-minutes: 60` until the full run measures a warm, uncontended duration on this runner. Record that duration in the PR.
- Update any contract test that pins `check` to the container image or the `high-cpu` label.

**Subject:** `fix(ci): run the unit and SQLite gate on the topology runner with a persistent cache`.

**Proof:** the job shows `prune decision` with an existing `rust-gate` lane, compiles without downloading cmake, and finishes green.

## 3. Android instrumented tests: our own emulator boot

**Root cause.** On the slow Android VM runner, the upstream `reactivecircus/android-emulator-runner` sees `sys.boot_completed`. Then, while ADB is still reconnecting, it immediately sends `adb shell input keyevent 82`. The emulator reports `device offline`, then `device 'emulator-5554' not found` about 0.5 s before boot completes, and the unlock exits 255. The action does not retry that unlock. The existing "Retry a pre-test emulator boot failure once" step (ci.yml line 1517) re-runs the same race, and in run 5325 both attempts failed. Record: 1 pass and 6 failures on that VM; 3 passes and 1 failure on the faster runner. Boot takes 79–126 s against 34–42 s on the faster one. Nothing else ran on that VM during the boot, so this is not contention.

**Edits.**
- Add `scripts/ci-android-emulator-run`, a bash script. It must:
  - create the AVD with the same parameters the action uses now: API 36, `android-tv`, `x86`, `tv_1080p`, forced creation;
  - start `emulator` with the same `emulator-options` in the background and record its PID;
  - `adb -s emulator-<port> wait-for-device`;
  - poll every 2 s, up to a bounded deadline of 600 s, until `getprop sys.boot_completed` is `1` **and** `adb get-state` reports `device` on three consecutive polls;
  - only then send the unlock (`input keyevent 82`), retrying it until ADB is stable;
  - run `PLURX_ANDROID_SERIAL=emulator-<port> make android-instrumentation-run`;
  - kill the emulator and delete the AVD on exit (`trap`).

  Reuse the SDK, `avdmanager`, `sdkmanager` and the system image the job already provisions or caches. The "Provision or verify emulator acceleration" step and the cache step stay.
- In ci.yml, replace the action step at line 1490, the "Freeze whether the first instrumentation script started" step, the "Retry…" step, and the "Propagate…" step with one step that runs the script. A test failure fails the step directly; nothing is retried.
- Update `tests/operations/test_contracts.py` assertions on these steps, if any exist. Add a test that the job has no retry step and calls the script.

**Subject:** `fix(ci): boot the Android emulator only once ADB is stable`.

**Proof:** the job's log shows the stable-state wait, then instrumentation results, on whichever Android runner takes it.

## 4. amd64 package: keep the media package image-side

**Root cause.** `package and smoke (amd64)` builds the Dockerfile target `linux-dolby-package-export` (Dockerfile line 415: `FROM scratch`, `COPY … /package`) with `outputs: type=local,dest=linux-dolby-export` (ci.yml at about line 1628). Then `Dockerfile.release` consumes it as `build-contexts: linux-dolby-package=./linux-dolby-export/package`. In both runs that reached the export, buildx hung in `exporting to client directory / copying files`, with the host idle, until the 90-minute deadline: 5228 (job 51662) and 5325 (job 52751). Every other amd64 run since c4beb5b5f (2026-10-08) failed earlier. The amd64 package has not passed since then. The exporter's hang itself could not be reproduced. The fix removes the step that hangs, rather than tuning it.

**Edits.**
- Build `linux-dolby-package-export` as an image instead of a local directory: `load: true`, `tags: plurx/linux-dolby-package:ci-amd64`, no `outputs`.
- Have the release build consume it as a named context, `build-contexts: linux-dolby-package=docker-image://plurx/linux-dolby-package:ci-amd64`. The image's root holds `/package`, so adjust the `COPY --from=linux-dolby-package` source path in `Dockerfile.release` (or wherever the job generates it) to `/package/...`.
- If the job's buildx builder uses the `docker-container` driver, it cannot see images loaded into the local daemon. In that case, either use the `docker` driver for these two steps or use an `oci-layout://` context written by a `type=oci,tar=false` export. Pick one and say why in the commit.
- If any later step needs the package files on disk, extract them with `docker create` plus `docker cp` from the loaded image, with a timeout, rather than a buildx local export.
- Keep every existing verification step: GLIBC ceiling, architecture and identity, stop/start smoke, and artifacts.
- Update contract tests that pin `outputs: type=local` or the `./linux-dolby-export/package` path.

**Subject:** `fix(ci): feed the audited media package to the runtime image without a local export`.

**Proof:** the amd64 job reaches and passes "Verify architecture, identity, and stop/start runtime".

## After the four commits

Report the commit hashes and local check results on PR #1150. The PR's owner then runs the adversarial review and **one** full `ci.yml` dispatch on the PR head, reruns only the jobs that fail, and merges once green. Run 5325's other failures are already fixed on the branch: the shared-source fixtures, the web golden, and the Cinema harness.
