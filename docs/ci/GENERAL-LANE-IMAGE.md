# The general lane's image

**Status:** built · Added 2026-11-14 · Owner: `Dockerfile` (stage `ci`),
`.github/workflows/ci-image.yml`, every workflow job on `[self-hosted, Linux, X64, lab, general]`,
`tests/operations/test_contracts.py`

Applies to every job that selects the plain `general` label set: the main fast
lane's paperwork, scope, preflight, web and promotion-gate jobs, the Rust unit
stage's select, shard, verdict and gate jobs, and the same kind of job in
`ci.yml`, `effort-ci.yml` and the smaller workflows. For any agent (Claude,
GPT, OpenRouter) or person.

## Why

A `general` job can land on any of the fleet's general runners, and those are
not one machine: different distributions, compilers, Pythons and FFmpeg
majors. A job that ran directly on the runner inherited all of it, so the
result of a commit depended on where the job landed:

- PR #1591's preflight compiled a C fixture with `-Werror`; it failed on one
  host's gcc and passed on the others.
- The Ubuntu 24.04 / FFmpeg 6 guests were retired from the lane because host
  jobs behaved differently there.

The Rust unit shards never had this problem: they run in a pinned image. Every
other `general` job now runs in the same one.

## What a general job is now

Each job carries the block the shards carry, byte for byte:

```yaml
    runs-on: [self-hosted, Linux, X64, lab, general]
    container:
      image: ${{ vars.FLEET_REGISTRY || 'fleet-registry.unset.invalid' }}/noirr/plurx-ci@sha256:<digest>
      options: --memory 7g --memory-swap 14g --cpus 3 --pids-limit 4096
      credentials:
        username: noirr
        password: ${{ secrets.CI_REGISTRY_PULL_TOKEN }}
```

and ends with `Restore persistent runner workspace ownership`, because the
container writes the runner's bind-mounted workspace as root.

- **One digest.** The workflows are the only place it is written;
  `test_every_general_lane_job_runs_in_the_pinned_ci_image` reads it from the
  shard job and refuses a second value anywhere in `.github/workflows`.
- **One resource shape.** The options are the fleet's general-job budget
  ([RUNNER-DISK.md](RUNNER-DISK.md)), the same on every job.
- **No per-runner branches.** Nothing in a job asks which host it is on.
- **Short timeouts are 10 minutes.** The jobs that had 2 to 5 minutes (scope,
  gates, mobile version, Store shard union, effort web) now have 10: a runner
  whose janitor aged the image out pulls about 1.3 GB before the first step,
  and that pull is inside the job's timeout.

## What the pinned image holds

Read from the layers of the pinned digest on 2026-11-14 (published
2026-09-30 from the `ci` stage of `Dockerfile`):

| Tool | In the image |
|---|---|
| Distribution | Debian 12 |
| C toolchain | gcc 12, clang 14, make, cmake, ninja, nasm, pkg-config |
| Python | 3.11 (`tomllib` present), first on `PATH` through the Playwright venv |
| Node | 22.23.4, **without npm** |
| Rust | rustup with 1.97.1, rustfmt, clippy, llvm-tools |
| FFmpeg | Jellyfin FFmpeg 8 as `ffmpeg` / `ffprobe` |
| Other | git, curl, jq, openssl, tar, xz, gzip, perl, procps, mkvmerge, dovi_tool, Chromium for Playwright |

## What the pinned digest lacks, and where it is fixed

Each row is something a `general` job uses that the pinned digest does not
hold. The `ci` stage of `Dockerfile` now carries all but the last two; they
reach the lane when a digest built from it is pinned (next section). **Until
then the lane is not complete:** the tests in the second and third rows skip,
and a skipped test is a failed test.

| Missing from the pinned digest | Who needs it | In `Dockerfile` now | Until the new digest is pinned |
|---|---|---|---|
| npm | `scripts/web-types` (`fast web syntax gate`, `effort web static contracts`) | yes, from the pinned Node image | The jobs' existing `setup-node` step brings it. |
| Docker client and compose plugin | `tests/operations/test_docker_https_ports.py`, `test_docker_hardware.py`, `test_compose_inspection.py` (four tests; no daemon needed) | yes, from the pinned `docker:27.5.1-cli` image | Those tests skip in preflight on every runner. |
| autoconf, automake | `tests/operations/test_ffmpeg_support_matrix.py` (two tests) | yes | They skip on every runner. |
| libssl-dev, libgtk-4-dev, libdbus-1-dev, dbus-daemon | Rust compiles and unit shards | yes (the first three already were) | `.github/actions/rust-native-deps` and the shard's D-Bus step install them per run; `lint.yml`, `fix-evidence.yml` and the parser fuzz job call the same action. |
| cargo-audit | `rust-audit.yml` (three jobs) | no | The jobs stay on the host. |
| cargo-fuzz | `validation-nightly.yml` fuzz campaigns | no | The existing install step compiles it on every run. |

## Publishing a new digest

The `ci` stage of `Dockerfile` is the image's only definition and
`.github/workflows/ci-image.yml` its only publication path. Dispatch it on the
branch that changed the stage:

1. **`publish the ci image`** (a high-CPU general runner, on the host) builds
   `--target ci` for linux/amd64, runs a tool smoke inside the result, pushes
   it as `noirr/plurx-ci:source-<sha>-run<id>` and prints
   `ci-image digest: …@sha256:<digest>`.
2. **`tool-dependent contract tests on the published image`** runs on the
   plain `general` label, inside that image, with the lane's options. It runs
   the modules whose tests skip when Docker, compose, autoconf or a compiler
   is missing, and fails if any test skips. Its time from start to the first
   step is the cold pull of the new layers.

Pin only a digest whose second job passed: replace the digest in every job
that names `plurx-ci@sha256:` (the contract test refuses two values). A new
digest changes every Rust unit crate's content key
([RUST-UNIT-FANOUT.md](RUST-UNIT-FANOUT.md) §6), so the next lane runs the
whole unit suite once. The 2026-09-30 digest was published by hand
([SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md](SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md));
that sequence is history, not the path.

## Jobs that stay outside

The contract test holds this table and fails on a `general` job that is in
neither group.

| Job | Why |
|---|---|
| `media-runtime.yml` `index`, `media-runtime-public.yml` `copy`, `publish-release.yml` `resolve` | They drive the runner host's Docker daemon (manifest push, registry copy, buildx inspect). The image has no Docker client. |
| `rust-audit.yml` `workspace`, `fuzz`, `scheduled` | `cargo audit`; the image has no cargo-audit. |
| `ci-image.yml` probe job | It runs the image that workflow has just published, before any lane pins it. |
| `validation-nightly.yml` `ffmpeg8-pacing` | Already in a container, `ubuntu:26.04`, because it asserts that distribution's FFmpeg 8 ([VALIDATION.md](../VALIDATION.md)). |

The `[…, general, high-cpu]` jobs (image builds and release packaging) are a
different label set and are not part of this rule.

## What was not measured

- **The new image.** `ci-image.yml` has not been dispatched: no image has been
  built from the changed stage, so the stage's build, the smoke and the probe
  are unproven until its first run.
- **Cold pull.** Not measured. The one observation is the draft lane's
  `paperwork` job on this change, 55 s in the container against 26 to 66 s on
  the host for the five pull requests before it, on a runner that may already
  have held the image. The 10-minute floor stays until the probe job's first
  run gives the cold figure.
- **Reasoned from the image's contents and the job definitions, not
  observed:** the wall time of `lint.yml` and `fix-evidence.yml` under the
  3-CPU cap, and the `ci.yml` / `effort-ci.yml` Python suites on the image's
  Python 3.11 (the main fast lane's preflight keeps its own pinned 3.12).
