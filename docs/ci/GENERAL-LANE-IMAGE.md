# The general lane's image

**Status:** built · Added 2026-11-14 · Owner: `Dockerfile` (stage `ci`),
`.github/workflows/ci-image.yml`, `scripts/ci-image-probe`,
`tests/ci-image-skips.toml`, every workflow job on
`[self-hosted, Linux, X64, lab, general]`, `tests/operations/test_contracts.py`

Applies to every job that selects the plain `general` label set: the main fast
lane's paperwork, scope, preflight, web and promotion-gate jobs, the Rust unit
stage's select, shard, verdict and gate jobs, and the same kind of job in
`ci.yml`, `effort-ci.yml` and the smaller workflows. For any agent (Claude,
GPT, OpenRouter) or person.

> **Not yet in force.** The workflows on this change are written for an image
> built from the current `ci` stage. That image has not been published: the
> pinned digest is still the one from 2026-09-30, which has Python 3.11 and no
> npm. On it the preflight and web jobs fail (they do not skip), so the lane
> cannot go green until `ci-image.yml` has run and its digest is pinned. See
> [What the first dispatch has to prove](#what-the-first-dispatch-has-to-prove).

## Why

A `general` job can land on any of the fleet's general runners, and those are
not one machine: different distributions, compilers, Pythons and FFmpeg
majors. A job that ran directly on the runner inherited all of it, so the
result of a commit depended on where the job landed:

- PR #1591's preflight compiled a C fixture with `-Werror`; it failed on one
  host's gcc and passed on the others.
- The Ubuntu 24.04 / FFmpeg 6 guests were retired from the lane because host
  jobs behaved differently there.
- Tests gated on a tool (`docker`, `autoconf`) ran on the hosts that had the
  tool and skipped on the others, and the lane was green either way.

The Rust unit shards never had the first two problems: they run in a pinned
image. Every other `general` job now runs in the same one, and the image is
required to run every preflight test.

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
    defaults:
      run:
        shell: bash
```

and ends with `Restore persistent runner workspace ownership`, because the
container writes the runner's bind-mounted workspace as root.

- **One digest.** The workflows are the only place it is written;
  `test_every_general_lane_job_runs_in_the_pinned_ci_image` reads it from the
  shard job and refuses a second value anywhere in `.github/workflows`.
- **One resource shape.** The options are the fleet's general-job budget
  ([RUNNER-DISK.md](RUNNER-DISK.md)), the same on every job.
- **bash, stated.** A job container's default shell is `sh`; the steps were
  written for the host's bash (`pipefail`, `mapfile`, `[[ ]]`).
- **Nothing downloaded per run.** No `setup-node`, no job-local Python: the
  image is the environment. The contract test refuses either in an image job.
- **No per-runner branches.** Nothing in a job asks which host it is on.
- **The registry is now in front of every general job,** including the gate
  jobs that only compare results. A registry outage fails them closed.

The contract test also fails on a `runs-on` it cannot read as label sets, and
on a `general` selector written any other way than the literal above, so a
job cannot leave the rule by being spelled differently.

## What the image holds

The `ci` stage of `Dockerfile` is the definition. Its build asserts each row.

| Tool | In the stage |
|---|---|
| Distribution | Debian 12 |
| Python | CPython 3.12.15 (the checksummed build the main fast lane used to download per job), as `python3` through the Playwright venv built from it |
| Node | 22.23.4 with npm |
| C toolchain | gcc 12, clang 14, make, cmake, ninja, nasm, pkg-config, autoconf, automake |
| Docker | client 27.5.1 and the compose plugin, from a digest-pinned image; no daemon and no socket |
| Rust | rustup with 1.97.1, rustfmt, clippy, llvm-tools |
| Native headers | OpenSSL, GTK4, D-Bus; `dbus-daemon` |
| FFmpeg | Jellyfin FFmpeg 8 as `ffmpeg` / `ffprobe` |
| Other | git, curl, jq, openssl, tar, xz, gzip, perl, procps, mkvmerge, dovi_tool, Chromium for Playwright |

Not in it: cargo-audit (so `rust-audit.yml` stays on the host) and cargo-fuzz
(the nightly fuzz jobs' existing step compiles it per run).

The digest pinned today, published 2026-09-30, differs from the stage (read
from its layers): Python 3.11, no npm, no Docker client, no autoconf or
automake, no OpenSSL/GTK4/D-Bus headers. `tests/operations/test_contracts.py`
does not even import on its Python.

## Publishing a new digest

The `ci` stage is the image's only definition and
`.github/workflows/ci-image.yml` its only publication path. Dispatch it on the
branch that changed the stage:

1. **`publish the ci image`** (a high-CPU general runner, on the host) logs in
   to the fleet registry, refuses a tag that already exists, builds
   `--target ci` for linux/amd64 with the media runtime base taken from the
   fleet registry, runs a tool smoke inside the result, pushes it as
   `noirr/plurx-ci:source-<sha>-run<id>-attempt<n>` and prints
   `ci-image digest: …@sha256:<digest>`.
2. **`preflight suites on the published image, no unreviewed skip`** runs on
   the plain `general` label, inside that image, with the lane's options. It
   checks out full history, provisions the K08 oracle sources as preflight
   does, and runs `scripts/ci-image-probe`: all of `tests/validation` and
   `tests/operations` on the image's `python3`. It fails on a test failure,
   on an error, and on any skip that `tests/ci-image-skips.toml` does not
   carry with a reason. That file is empty: nothing in either suite is meant
   to skip in the image. An entry whose test did not skip also fails, so the
   list cannot outlive its reasons.

Pin only a digest whose second job passed: replace the digest in every job
that names `plurx-ci@sha256:` (the contract test refuses two values). That
replacement is the whole change. A new digest changes every Rust unit crate's
content key ([RUST-UNIT-FANOUT.md](RUST-UNIT-FANOUT.md) §6), so the next lane
runs the whole unit suite once. The 2026-09-30 digest was published by hand
([SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md](SERVICE-LIMITS-CHILD-PRIORITIES-AND-BUILD-HYGIENE.md));
that sequence is history, not the path.

A skip is fixed where it comes from. A missing tool goes into the stage. A
test that skipped because the suite runs as root now drops privileges itself
(`test_ci_janitor.py` runs the janitor as `nobody` over a fixture it owns).

## Jobs that stay outside

The contract test holds this table and fails on a `general` job that is in
neither group.

| Job | Why |
|---|---|
| `media-runtime.yml` `index`, `media-runtime-public.yml` `copy`, `publish-release.yml` `resolve` | Their subject is the runner host's Docker daemon (manifest push, registry copy, buildx inspect). A job container is given no daemon socket. |
| `rust-audit.yml` `workspace`, `fuzz`, `scheduled` | `cargo audit`; the image has no cargo-audit. |
| `ci-image.yml` probe job | It runs the image that workflow has just published, before any lane pins it. |
| `validation-nightly.yml` `ffmpeg8-pacing` | Already in a container, `ubuntu:26.04`, because it asserts that distribution's FFmpeg 8 ([VALIDATION.md](../VALIDATION.md)). |

The `[…, general, high-cpu]` jobs (image builds and release packaging) are a
different label set and are not part of this rule.

## Timeouts

None of these is measured in the image yet; each is a bound with its basis,
to be replaced by the figure from the first run named beside it.

| Job | Timeout | Basis | Measured by |
|---|---|---|---|
| Jobs that had 2 to 5 minutes (scope, gates, mobile version, Store shard union, effort web) | 10 min | A runner whose janitor aged the image out pulls it inside the job's timeout; the old digest is about 1.3 GB compressed. | the probe job of the first `ci-image.yml` run (start to first step is a cold pull of the new layers) |
| `main-fast-lane.yml` / `ci.yml` preflight | 30 min (unchanged) | On a host: 1872 tests in about 193 s, history audit up to 7 min. Now under 3 CPUs / 7 GiB. | the first full lane on the pinned digest |
| `effort-ci.yml` preflight | 30 min (was 15) | The same work as above plus a possible cold pull; 15 left no room for both. | the first `Effort development gate` dispatch |
| `lint.yml` | 40 min (was 10) | Workspace Clippy took 6.7 min warm at 4 CPUs and 10.5 to 12.1 min at `-j1` ([RUST-GATE-IMAGE.md](RUST-GATE-IMAGE.md)); this job has 3 CPUs, a cache that starts cold in the new environment, and an apt step until the new digest is pinned. | its first dispatch |

## What the first dispatch has to prove

Nothing below has run. One `paperwork` job ran in the old image on the draft
lane (36 to 55 s, against 26 to 66 s on the host for the five pull requests
before it); that is the only observation.

1. The changed `ci` stage builds on the pinned base: the apt set, the Python
   download and checksum, the venv, the npm links, the Docker client copy.
2. The smoke passes in the built image and the push as `fleet` succeeds under
   a new tag.
3. `container.image: ${{ needs.publish.outputs.image }}` starts the probe job
   on a `general` runner; its start-to-first-step time is the cold pull.
4. Both suites pass in the image as root on Python 3.12.15 with **zero
   skips**: the four Docker/compose tests, the two autoconf tests, the janitor
   test under dropped privileges, `test_root_preserves_nonroot_data_owner_for_daemon_replay`
   (which needs root and never ran on the hosts), and the OpenSSL Ed25519
   fixtures in `test_desktop_release.py`.
5. After the digest is pinned, one full lane: scope, preflight, web, the
   Rust unit select / verdict / gate jobs and the promotion gate in the
   image, with the timings for the table above.

Still unexercised after that, because a pull-request lane does not start
them: `ci.yml`, `effort-ci.yml`, `lint.yml`, `fix-evidence.yml`, the nightly
fuzz jobs, `release-readiness.yml` `tag`, `mobile-channels.yml` `scope` and
`store-shards.yml` `aggregate`. Each needs one dispatch (or its next
scheduled or tag run) on the pinned digest before its conversion counts as
observed.
