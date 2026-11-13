# Rust unit fan-out — the relevant tests, compiled once, run across the fleet

**Status:** open — Rust built; Android JVM and Apple unit stages planned (§8) ·
**Decision:** Paul, 2026-11-12 ("all I care about is the code getting the
relevant unit tests run on it") · **Workflow:**
[`rust-units.yml`](../../.github/workflows/rust-units.yml) ·
**Code:** [`validation/rust_units.py`](../../validation/rust_units.py) ·
**Inputs:** [`validation/rust-unit-inputs.toml`](../../validation/rust-unit-inputs.toml)

Until this stage the ready-PR lane compiled Rust and ran Clippy, and the Rust
unit suites ran only in the manual full `ci.yml`
([RUST-TEST-EXECUTION-POLICY.md](RUST-TEST-EXECUTION-POLICY.md) §2.1). This
stage runs, on every ready pull request, exactly the Rust unit tests whose
inputs the change touches, each at most once per set of inputs, spread over
idle runners. It follows Paul's standing rules: a test only has to pass once on
the code being merged; rebasing must not repeat tests that already passed on
the same inputs; only what failed runs again; history that cannot be
authenticated is simply not adopted.

## 1. What runs when

| Trigger | What happens |
|---|---|
| Ready main-bound PR whose scope includes Rust | `main-fast-lane.yml` calls `rust-units.yml` after preflight; `Main promotion gate` requires its result. Drafts run none of it. |
| `workflow_dispatch` of `rust-units.yml` on any branch | The same stage against `base` (default `origin/main`). Its receipts count for later PR runs. |
| Docs-only or client-only PR | Nothing: `validation.ci_scope` says `rust=false`, and even if it ran, no crate's key would change. |

The stage is six jobs (names are part of receipt authentication):

1. **`Rust units: select`** (any `general` runner, a few seconds). Computes
   every unit crate's content key at the head and at the merge-base with the
   base (§3). A crate is *due* when they differ. For each due crate it then
   looks up authenticated receipts under the head key (§4) and drops the tests
   that already passed. If nothing is left, no other job does any work.
2. **`Rust units: build archive`** (`ci-nonvoter-compile-amd64`, the bounded
   20 GiB/4 CPU container in the private `plurx-ci` image ci.yml's full Rust lane
   uses). Builds `cargo nextest archive --workspace --exclude plurx-cluster-check`
   — the whole unit workspace, so feature unification is exactly `make unit`'s
   (plurx-core gets `hiqlite-store` from plurxd) — but archives only the due
   crates' test binaries. Unchanged crates are fresh in the persistent
   `rust-units` Cargo lane, so a warm build costs what changed. It also
   checks the selector's manifest graph against `cargo metadata` and fails if
   they disagree. It then lists the archive and writes the plan: remaining
   tests per crate and the shard count, `ceil(tests / 700)` capped at 6 (one
   shard for a small change).
3. **`Rust units shard i`** (matrix from the plan, any idle x64 `general`
   runner, the same `plurx-ci` image with the general-job cap of 7 GiB +
   7 GiB swap and 3 CPUs, so a shard on a production voter host is bounded
   like every general job). Runs `cargo-nextest nextest run --archive-file …
   --partition hash:i/N --profile ci --test-threads 2` with the plan's
   filtersets. Shard 1 also runs `make vodencode-restart-check`'s two ignored
   real-FFmpeg restart tests serially. A shard fails only when it could not
   run; test failures go into its receipt.
4. **`Rust units: verdict`** proves the shards ran every planned test exactly
   once (a missing shard or test fails it; a test in two shards or one outside
   the plan is an integrity failure), publishes one receipt per crate, and
   lists failures.
5. **`Rust units: merge-base check of failures`** runs only when something
   failed. It checks the merge-base out in place (head's tools kept aside),
   runs exactly the failing tests there, and classifies each: **fails at the
   merge-base too → pre-existing on main**, reported in the summary, not
   blocking; passes there, is skipped there, or does not exist there →
   **new**, blocking.
6. **`Rust unit verdict`** is the stage's single result.

What `make unit` runs and this stage does not: doctests (nextest cannot run
them; `make unit` runs them through `cargo test`), and `spike-lock-check` /
`test-socket-permission-check` (preconditions, not tests; the Rust gate runs
`spike-lock-check`). Each test runs in its own process under nextest, where
`make unit` runs a binary's tests as threads of one process. Two harness
adjustments follow from that, both in files that are key inputs:

- `RUST_MIN_STACK=8388608` (the workflow): state `cargo test` builds once per
  binary is built again on every test's own 2 MiB thread, and the first full
  run overflowed it in 59 plurxd tests (mostly `transcode::source_actor`).
  8 MiB is what the replicated Store shards already use.
- The ci profile's `default-filter` leaves out
  `scan::probe::tests::fixture_child_*`: child bodies that decide they are the
  child from `--exact <name>` in their arguments — exactly how nextest starts
  every test — and then sleep for minutes or exit 13. Their parent tests still
  spawn them; a test keeps the filter equal to that set.

A test that only passed because a process-local lock serialized it against
another would also surface here; the merge-base check reports it as new or
pre-existing like any other failure.

## 2. Run the same selection locally — one command

```bash
python3 -m validation.rust_units local --base origin/main --run
```

It prints each due crate with the inputs that changed, then runs the same
`cargo nextest run --workspace --exclude plurx-cluster-check -E 'package(=…)'`
and the serial ignored tests. Without `--run` it only prints the commands.
Local runs do not read or write receipts. Install nextest with
`cargo install --locked cargo-nextest@0.9.151` (or its release binary).

## 3. How a crate's content key is computed

`Keys.key(crate)` is SHA-256 over `rust-units-key-v1`, the crate name, and the
sorted `mode blob path` lines of its input set, all read from git objects of
one commit (no checkout). The input set
([`rust-unit-inputs.toml`](../../validation/rust-unit-inputs.toml) explains it
in place) is:

1. every tracked file outside `crates/` that no `exempt` glob matches — so
   `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `clippy.toml`,
   `vendor/**` and anything new at the top level re-key **every** crate;
2. the `harness`: this workflow (which pins the CI image digest and the
   nextest release), `.config/nextest.toml`, the FFmpeg and native-deps
   actions, `validation/rust_units.py` and the inputs file itself;
3. the directories of the crate and of every workspace crate in its closure:
   its normal, build and dev dependencies, then their normal and build
   dependencies (a dependency's dev-dependencies are not part of your test
   binary), plus external path dependencies;
4. every tracked path a closure crate names through `../` in a string literal
   (`include_str!`, `include!`, `concat!(env!("CARGO_MANIFEST_DIR"), "/../..")`,
   `Path::join("../../scripts/mkpgs")`), resolved against the file's and the
   manifest's directory — this is how `plurxd` picks up `scripts/mkpgs`,
   `docs/CLIENTS.md` and the `tests/playback` fixtures although those trees are
   exempt;
5. the declared `reads` of the crate's own `[[dynamic]]` sites: a test that
   finds the workspace at run time (`CARGO_MANIFEST_DIR` then `.parent()`/
   `.ancestors()`/`.pop()`) can read anything, so each such site is declared
   with what it reads and which tests read it. These key only the declaring
   crate (no other crate links its tests), and when they are the only inputs
   that changed, only those named tests are due — the rest of the crate's
   inputs are unchanged. `tests/validation/test_rust_units.py` fails when a
   site appears, moves function, or disappears without the declaration
   changing; the archive job fails when a named test does not exist.

Consequences worth knowing: a change under `crates/plurxd/src` also makes
plurx-core's three census tests due (they read plurxd's sources), and any
crate's `src` change makes plurxd's seam census test due, but not the rest of
either crate; a `plurxd/build.rs` change re-keys plurxd and nothing else; `git describe`
(embedded by plurxd's build script) is deliberately not an input — it is the
commit's identity, not its content. Exempting a tree is a claim that no Rust
test reads it except as items 4 and 5 find; keep that list to documentation,
clients, CI configuration and non-Rust test suites.

## 4. Receipts — how a pass is reused, and when it is not

The verdict publishes, for each crate it ran, an artifact named
`rust-units-<crate>-<key>` (90-day retention) holding `receipt.json`: crate,
key, commit, run, publishing job, the shard jobs whose logs hold the
evidence, the crate's full test listing digest, and the passed and failed
test ids (`<binary-id> <test name>`). The name is content-addressed, so
`select` finds every earlier result for exactly these inputs with one
`GET /actions/artifacts?name=…` per due crate, whatever branch, PR or session
produced it.

A receipt is adopted only when all of these hold (`Authenticator`):
the run exists in this repository and its commit is the receipt's commit; the
publishing job is terminal and its own log shows that exact artifact being
uploaded; **the key recomputed by the current code from the receipt's commit
equals the key being looked up** — the key covers the harness, so a run with a
different workflow, CI image or producer can never match; and each adopted
pass appears as a nextest `PASS` line in a named shard job's log. A pass the
logs do not show is dropped, a logged failure can never be adopted as a pass,
and any lookup, download, fetch or parse problem means "not adopted; its tests
run". History never fails the stage; only the current run's own integrity
does.

Passes union across runs: a crate whose union of passes covers its whole
listing under the key is skipped outright; otherwise only the tests that have
not passed run. That is what makes a re-push, a rebase onto an unrelated
`main`, or a second PR with the same crate inputs near-free, and what makes a
re-run after one failure run only that failure. `tests/validation/test_rust_units.py`
proves the invalidation rules on real git histories: a dependency's source
change re-keys its dependents, an unrelated crate or a docs change does not,
`Cargo.lock` and the harness re-key everything, an exempt fixture named by a
literal re-keys only its reader.

## 5. Re-running

- **One shard, from the Forgejo UI:** "Re-run" on the `Rust units shard i`
  job. Its receipt artifact is overwritten; re-run the verdict and the gate
  after it if Forgejo does not do so itself.
- **Agents (Forgejo 16 has no re-run API):** start a new lane run — close and
  reopen the PR (`PATCH /pulls/<n>` `{"state":"closed"}` then `"open"`), or
  push. Receipts make the new run execute only tests that have not passed
  under their current keys, so this is the "re-run only what failed" path.
- **Any branch, by hand:** `POST /actions/workflows/rust-units.yml/dispatches`
  with `{"ref": "<branch>", "inputs": {"base": "<sha or origin/main>"}}`.

## 6. Known constraints carried over

- Environment: the `plurx-ci@sha256:3d84b711…` image (Jellyfin FFmpeg 8, Rust
  1.97.1), the same as ci.yml's full Rust lane, not the fast Rust gate's Ubuntu
  24.04 / FFmpeg 6. A change of image is a change of every key.
- `hiqlite-store`: kept by building the whole unit workspace (§1).
  `plurx-cluster-check`, `cluster-integration-tests` and `hiqlite-contract-tests`
  stay in their ci.yml lanes.
- Memory: two test threads per shard inside the 7 GiB general cap; the
  `unit-memory-*` labels remain reserved for coverage.
- Pre-existing failures: never a list — always the merge-base check, so a
  failure is "pre-existing" only if it fails on main's code too. A flaky test
  that passes at the merge-base blocks; re-run as in §5.
- Archived binaries run from the extraction directory: a test that spawns a
  sibling binary must read `CARGO_BIN_EXE_<name>` at run time (nextest sets
  it) with the compile-time value as the fallback, as
  `plurx-desktop-control/tests/trust_roots.rs` does.

## 7. cargo-nextest — the trade-off

nextest 0.9.151 (`Apache-2.0 OR MIT`), a single static binary fetched from
its GitHub release and checked against its SHA-256; nothing enters the Cargo
graph. It buys the archive (compile once on one host, execute on others, with
the workspace remapped), deterministic `hash:i/N` partitions, filtersets that
name exact tests, and per-test process isolation; each outcome is read from
nextest's own status lines in the job log — the same lines a later run
authenticates a receipt against. The costs: one
more pinned tool to bump (version in the workflow, `.config/nextest.toml` and
`NEXTEST_VERSION` in the module — a test keeps them equal), no doctests, and
per-process isolation that can surface tests which relied on sharing a
process. The alternative, partitioning libtest binaries by hand as
`validation/store_shard.py` does for one Store binary, would have to
reimplement the archive and remapping for thirty test binaries.

## 8. Measured cost

Filled from dispatched runs on the branch; see the Execution log.

## 9. Other languages — follow-up plan

| Surface | PR-time unit execution today | Plan |
|---|---|---|
| Python (`tests/validation`, `tests/operations`) | Runs in preflight with per-test receipts ([PYTHON-UNIT-PR-RECEIPTS.md](PYTHON-UNIT-PR-RECEIPTS.md)). | No change. |
| Web / Node | Every Node web and playback test runs in `fast web syntax gate`, with receipts (`validation.web_unit_receipts`). | No change. |
| Android JVM | `fast Android compile` builds the debug app; `make android-test` runs only in ci.yml. | Add `make android-test` to the Android job, gated by `ci_scope.android_jvm`, with a content key over `clients/android/**` plus the Gradle wrapper, stored in this receipt format with Gradle's per-test JUnit as evidence. |
| Apple unit (XCTest) | The Apple job compiles and runs the 32 TV navigation journeys with native receipts ([NATIVE-CI-RECEIPTS.md](NATIVE-CI-RECEIPTS.md)); `make apple-test` runs only in ci.yml. | Add `make apple-test` beside the journeys on the same runner, keyed by the native receipts' existing source closure. |
| Rust doctests | Not run at PR time. | One `cargo test --doc` job per due crate on the archive host, keyed by the same crate key. |
| Merge-base outcomes as receipts | Each lane run with failures rebuilds the merge-base and re-runs them, even when the same merge-base was checked before. | Publish the merge-base check's outcomes under the merge-base keys (`selection.json` has them) and consult them before compiling. |

## Execution log

| Date | Model | Session | Milestone | Commit / PR | Outcome / evidence |
|---|---|---|---|---|---|
| 2026-11-12 | claude-opus (subagent) | https://claude.ai/code/session_01BCWLA2XZeAA1hHD47D9JB2 | Rust selection, receipts, nextest fan-out, docs | branch `claude/ci-unit-fanout` | Built; measurements in §8. |
