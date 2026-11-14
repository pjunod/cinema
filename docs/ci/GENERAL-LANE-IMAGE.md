# The general lane's image

**Status:** built · Added 2026-11-14 · Owner: `Dockerfile` (stage `ci`),
every workflow job on `[self-hosted, Linux, X64, lab, general]`,
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

## What it lacks

Each row is something a `general` job uses that the pinned digest does not
hold. The place to add all of them is the `ci` stage of `Dockerfile`; nothing
in a workflow works around a missing tool beyond the steps that were already
there.

| Missing | Who needs it | What happens until the image has it |
|---|---|---|
| npm | `scripts/web-types` (`fast web syntax gate`, `effort web static contracts`) | The jobs keep their existing `setup-node` step, which brings npm with the same Node 22.23.4. |
| Docker client and compose plugin | `tests/operations/test_docker_https_ports.py`, `test_docker_hardware.py`, `test_compose_inspection.py` (no daemon needed) | Those tests skip in preflight on every runner. They used to run only where the host had Docker. |
| autoconf, automake | `tests/operations/test_ffmpeg_support_matrix.py` | Two tests skip on every runner. |
| cargo-audit | `rust-audit.yml` (three jobs) | The jobs stay on the host. |
| cargo-fuzz | `validation-nightly.yml` fuzz campaigns | The existing install step compiles it on every run. |
| libssl-dev, libgtk-4-dev, libdbus-1-dev, dbus-daemon | Rust compiles and unit shards | Already in `Dockerfile`, absent from the published digest; `.github/actions/rust-native-deps` and the shard's D-Bus step install them per run. `lint.yml`, `fix-evidence.yml` and the parser fuzz job now call the same action. |

Publishing a new digest follows the recipe in
[SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md](SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md)
(build the `ci` target, smoke it, push a source-SHA tag, pin the registry
manifest digest in every job). A new digest changes every Rust unit crate's
content key ([RUST-UNIT-FANOUT.md](RUST-UNIT-FANOUT.md) §6), so the next lane
runs the whole unit suite once.

## Jobs that stay outside

The contract test holds this table and fails on a `general` job that is in
neither group.

| Job | Why |
|---|---|
| `media-runtime.yml` `index`, `media-runtime-public.yml` `copy`, `publish-release.yml` `resolve` | They drive the runner host's Docker daemon (manifest push, registry copy, buildx inspect). The image has no Docker client. |
| `rust-audit.yml` `workspace`, `fuzz`, `scheduled` | `cargo audit`; the image has no cargo-audit. |
| `validation-nightly.yml` `ffmpeg8-pacing` | Already in a container, `ubuntu:26.04`, because it asserts that distribution's FFmpeg 8 ([VALIDATION.md](../VALIDATION.md)). |

The `[…, general, high-cpu]` jobs (image builds and release packaging) are a
different label set and are not part of this rule.

## What was not measured

Nothing here was run on the fleet before the pull request's own lane. Reasoned
from the image's contents and the job definitions, not observed: the wall
time of `lint.yml` and `fix-evidence.yml` under the 3-CPU cap, and the
`ci.yml` / `effort-ci.yml` Python suites on the image's Python 3.11 (the main
fast lane's preflight keeps its own pinned 3.12).
