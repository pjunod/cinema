# The Rust gate's image, pool and parallelism

**Status:** built · Added 2026-11-12 (PR #1550) · Owner: `Dockerfile.rust-gate`,
`.github/workflows/rust-gate-image.yml`, `scripts/rust-gate-image`

Applies to the main fast lane's `fast Rust gate` and the effort lane's
`effort Rust compile`. For any agent (Claude, GPT, OpenRouter) or person.

## What the job does now

1. **Verifies its container limits** before any tool runs (20 GiB memory, no
   swap, 4 CPUs on `0-3`, 4096 PIDs), exactly as before.
2. **Runs in a pinned image**, `noirr/plurx-rust-gate@sha256:…` from the fleet
   registry. The image already holds the Ubuntu 24.04 build packages,
   FFmpeg 6, Node 22.23.4 and Rust 1.97.1 with rustfmt and clippy, so the job
   no longer runs `apt-get` or downloads a toolchain.
3. **Refuses a stale image.** The image records the sha256 of the
   `Dockerfile.rust-gate` it was built from; the job compares it with the file
   in the checkout and fails with the bump instructions when they differ.
4. **Sizes Cargo from the container.** `scripts/ci-cargo-jobs` reads the
   job's own `cpu.max` and `memory.max` and sets
   `CARGO_BUILD_JOBS = min(CPUs, memory / 5 GiB)` — 4 in the current
   container, where it used to be a fixed 1.
5. **Type-checks once.** The fast gate runs `make fast-rust-gate`: format and
   spike-lock checks, one all-target Clippy pass with `-D warnings`, and the
   vendored hiqlite Clippy. It used to run `cargo check --all-targets` first
   and then Clippy over the same targets; Clippy's driver is rustc plus lints,
   so the check pass compiled the workspace a second time for nothing. The
   effort lane keeps its compile-only `make effort-rust-check`.

## Timings

Fast Rust gate, from the job logs of 2026-11-12 (before) and this PR's
dispatched probe, rust-gate-image run 7483 (after). "Provision" is job start
to the first cargo command; the "before" check column is the separate
`cargo check` pass the gate no longer runs. A cold-cache timing on the new
image was not measured (it needs a wiped runner cache); the image removes the
per-run apt and toolchain provisioning whether the cache is warm or cold.

| | Runner | Provision | Check | Clippy | Total |
|---|---|---|---|---|---|
| before | lab1 guests (`-j1`) | 2.1–2.8 min | 9.2–10.7 min | 10.5–12.1 min | 22.4–25.0 min |
| before | lab5 compile slot (`-j1`) | 2.2–2.6 min | 5.8–6.8 min | 6.6–7.9 min | 14.6–17.4 min |
| after | lab1 guest `gha-lab1-plurx-heavy-02` (`-j4`, warm cache, first pull of the image) | 1.0 min | — | 6.7 min (+0.2 vendored) | 8.2 min |

## Bumping the image (one command per step)

Edit `Dockerfile.rust-gate`, commit and push the branch, then:

```sh
scripts/rust-gate-image publish          # dispatches rust-gate-image.yml on this branch, waits, prints the digest
scripts/rust-gate-image pin sha256:<it>  # rewrites the pin in every lane
git commit -am 'ci(rust-gate): pin the image built from <change>'
```

`publish` needs the git-ignored `scripts/ci-failures.fleet.json` (forge URL,
repository and token file; copy `scripts/ci-failures.fleet.example.json`).
The workflow builds the image on a high-CPU general runner, smoke-tests it,
pushes it under a tag unique to the run (so a pinned digest never loses its
tag), prints `rust-gate-image digest: …`, and then runs the gate itself inside
the new image on the lane's pool. Pin only a digest whose probe job passed.
`scripts/rust-gate-image check` (also run by `tests/operations`) refuses lanes
that pin different digests.

To roll back, `pin` the previous digest from git history.

## Why the gate stays on the nonvoter compile pool

The idle `[self-hosted, Linux, X64, lab, general, high-cpu]` runners (three
on media1, two on lab5) were the intended pool. A probe on 2026-11-12 ran the
gate's container there (media1, `gha-media1-general-02`) with
`--memory --memory-swap --cpus --pids-limit` and read back:

```
memory.max=20401094656        # honoured
memory.swap.max=20401094656   # --memory-swap ignored: may swap up to 19 GiB
cpu.max=max 100000            # --cpus ignored
pids.max=74781                # --pids-limit ignored
SwapTotal: 8388604 kB         # and the host has 8 GiB of swap
```

media1 is a production voter (`validation/runner-fleet.toml`), and a
swapping, CPU-unbounded build on a voter is the shape behind the 2026-11-01
leader losses ([PLAYBACK-QUORUM-GAPS-RCA.md](../cluster/PLAYBACK-QUORUM-GAPS-RCA.md)).
The verification step refuses that container, as it should, so routing the
gate there would only turn every run that lands on media1 red. No existing
label selects lab5's high-CPU runners without also selecting media1.

The gate therefore stays on `ci-nonvoter-compile-amd64` and gains from the
image, the single type-check and `-j4`. Using the high-CPU runners needs a
host-side change first: their runner or Docker has to apply the job's
`--cpus`, `--pids-limit` and `--memory-swap`. Once a probe there prints
those limits back, moving the gate is a `runs-on` change in the two lanes and
the probe job.

## Cache

Unchanged: `.github/actions/cargo-cache` keeps a bounded runner-local cache
(lane `main-fast-rust` or `effort-rust`) after its free-disk pre-check, and
`cargo-cache-finalize` enforces the budget. The image's toolchain lives in
`/usr/local/rustup`, outside `CARGO_HOME`, so pointing `CARGO_HOME` at the
cache does not hide it. The probe job uses the lane's cache name, so a publish
also warms the cache on the runner it lands on.
