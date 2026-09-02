# CI execution acceleration — persistent caches, native packaging, and exact sharding

**Status:** implementation in progress · **Decider:** Paul · **Reviewed by:**
Fable · **Implementation started:** 2026-09-01 · **Baseline:** `main` at
`950db83d`

This is the complete implementation and operating contract for accelerating
Plurx CI. It stands on its own: no earlier CI-overhaul document is required to
interpret, implement, operate, or roll it back. The development/promotion
mechanics remain those in [DEVELOPMENT_PIPELINE.md](DEVELOPMENT_PIPELINE.md).

The target is a materially shorter required path without weakening what a
green `Main promotion gate` means. The design keeps GitHub as the scheduler,
reuses bounded local compiler and image state, removes redundant container
compiles, separates independent replicated tests, and then shards the largest
suite with a machine-verifiable inventory receipt.

## 1. Outcome and baseline

The required path currently pays for the same work more than once:

- Rust dependency graphs are restored from or uploaded to a remote cache in
  each job even though the self-hosted workers have persistent disks.
- Docker smoke creates an ephemeral Buildx builder, compiles the application
  again inside the Dockerfile, runs amd64, and then performs a long arm64 QEMU
  build with no arm64 runtime probe.
- replicated Store and topology contracts share one serial job even though
  they exercise separable risks;
- the Store integration binary contains a large dynamic inventory and runs
  with `--test-threads=1`, making one runner the critical path;
- the initial runner inventory had only a native macOS ARM runner, which could
  not establish a Linux ARM runtime contract. Implementation added two
  isolated native Linux ARM virtual machines so required arm64 packaging no
  longer depends on x86 emulation.

The repaired baseline produced a 6 minute 8 second warm fast-Rust gate. The
first policy preflight exceeded its 3-minute job limit only after its tests had
passed; the warm rerun finished in 43 seconds. Historical replicated Store and
topology work has occupied roughly 30 minutes, and arm64 QEMU image builds
have crossed the existing 60-minute guard. These are baseline observations,
not acceptance targets: every rollout phase records fresh job timing.

Success means:

- a warm required Rust lane reuses objects without a remote cache round trip;
- amd64 and arm64 each compile once, package the exact compiled pair of
  binaries, and probe that package in the same job;
- Store and topology failures identify their own lane;
- the Store shards jointly execute exactly the discovered inventory, once
  each, with a retained receipt;
- required arm64 packaging runs on native Linux ARM rather than emulating the
  Rust compiler on x86;
- the old graph remains immediately selectable for 30 days after cutover.

## 2. Invariants — speed cannot buy a weaker verdict

These constraints bind every milestone:

1. `Main promotion gate` remains the single aggregate required-check name.
2. The accelerated graph executes the same test inventory and package/runtime
   assertions as the legacy graph. A changed schedule is not permission to
   delete evidence.
3. Missing, empty, or unrecognized `CI_EXECUTION_MODE` resolves to `legacy`.
4. The rollout switch is separate from `CI_RUNNER_MODE`; changing where jobs
   run must not silently change what jobs mean.
5. Heavy CI jobs do not run on the four production Plurx voters: `m6`, `nuc3`,
   `nuc4`, or `nynuc`. Fleet image builds performed by deployment tooling are
   a separate, explicit operator action and are not CI capacity.
6. No cluster timing assertion runs on a user laptop or on a host with an
   interactive Apple build tenant.
7. Ordinary pull requests do not upload release binaries merely to connect
   jobs. Each architecture compiles, packages, and smokes in one workspace;
   only small manifests and digests cross job boundaries.
8. Persistent state is derived from the runner, pinned Rust toolchain, and
   lane. Two runner services never write the same Cargo target directory.
9. Every cache has both a size ceiling and a host free-space floor. A cache
   miss or prune can make a run slower; it cannot make the evidence stale.
10. The BuildKit container gets its own Forgejo plain-HTTP registry setting.
    Docker daemon trust is neither assumed nor changed by CI.
11. No Docker-daemon restart, production container restart, runner install,
    or host provisioning is part of a repository PR. Those are named operator
    operations with separate authorization.
12. The full suite runs once on the frozen final effort candidate, after all
    task findings are fixed. Any candidate-tree change invalidates that run.

## 3. Control plane — three explicit execution modes

`CI_RUNNER_MODE` chooses the runner pool:

| Value | Meaning |
|---|---|
| `self-hosted` | Use the trusted lab runners; this is the default. |
| `github` | Use disposable GitHub-hosted runners where the workflow supports them. |

`CI_EXECUTION_MODE` chooses the CI graph and cache behavior:

| Value | Required graph | Additional evidence | Persistent local caches |
|---|---|---|---|
| `legacy` | Existing graph | None | Off; preserve the existing remote-cache path |
| `shadow` | Legacy graph | Accelerated lanes report separately and do not gate | On for eligible persistent builders |
| `accelerated` | Accelerated graph | Legacy remains manually runnable and scheduled | On for eligible persistent builders |

The tracked resolver is `scripts/ci-execution-mode`. Workflows run its
`resolve` command and publish the normalized value from their scope job. An
unknown value emits a warning and returns `legacy`; the composite cache
actions independently treat anything other than `shadow` or `accelerated` as
legacy. Operators inspect or change the repository variable with:

```bash
scripts/ci-execution-mode status
scripts/ci-execution-mode shadow
scripts/ci-execution-mode accelerated
scripts/ci-execution-mode legacy
```

Rollout changes affect new workflow runs only. They do not cancel an already
running qualification or change its recorded candidate tree.

## 4. Capacity audit and runner isolation

The 2026-09-01 read-only audit found:

| Host | Production voter | Observed runner capacity | Free disk | Docker build cache |
|---|---:|---|---:|---:|
| `m6` | yes | generic `general` / `high-cpu` labels | ~329 GB | ~8.8 GB |
| `nuc3` | yes | generic `general` / `high-cpu` labels | ~183 GB | ~9.2 GB |
| `nuc4` | yes | generic `general` / `high-cpu` labels | ~210 GB | ~12.2 GB |
| `nynuc` | yes | generic `general` / `high-cpu` labels | ~281 GB | ~13.8 GB |
| `nuc1` | no | one runner labeled `ci-store` (see §4.1) | not yet measured | not yet measured |
| `nuc2` | no | Android runners; ineligible for `ci-store` — no writable Cargo home (see §4.1) | not yet measured | not yet measured |
| `rogg16` | no | one runner labeled `ci-topology`, one labeled `ci-store`; other generic runners online (see §4.1) | not yet measured | not yet measured |
| MacBook Pro | no | Linux ARM64 VM `ci-arm64` primary | ~371 GB host free | new native guest |
| MacBook Air | no | macOS ARM64 `apple` / `xcode-26`; Linux ARM64 VM `ci-arm64` backup | ~70 GB host free | new native guest |

Every production voter currently satisfies the generic labels used by heavy
jobs, so the desired isolation is not yet true. Before `shadow` is enabled:

1. give intended non-production builders a dedicated label such as
   `ci-build-x86`;
2. assign that persistent-heavy label to exactly one runner service on each
   physical host, even when the host runs several GitHub runner services;
3. remove that dedicated build label from every voter and never select a
   heavy accelerated lane through `general` alone;
4. record disk totals and free space for each selected non-voter;
5. run a no-op labeled workflow to prove scheduling reaches at least two
   distinct physical x86 hosts;
6. retain `general` only for short policy/scope jobs if production load policy
   permits it; otherwise create a lightweight non-voter label too.

The supplied deployment key is not authorized for `nuc1` or `rogg16`.
`nuc2` additionally presents a changed ED25519 host key (observed fingerprint
`SHA256:LI8rEpqCBeVnsn54PLNJy2UqbmWYQaB+cuxP/A5QSoE`). The old known-hosts
entry has not been replaced and the new key has not been trusted: independent
verification is required before any host *access*. That remains a security
boundary, and it is not permission to bypass host verification.

### 4.1 The 2026-09-02 label rebalance

Withholding a shard label until that key was verified did not withhold a
shard: it created one that can never be scheduled. `store-shards.yml` shipped
a matrix row pinned to `ci-store-shard-1`, a label no runner in the fleet has
ever carried, so flipping `CI_EXECUTION_MODE` to `accelerated` would have
parked every pull request behind a job GitHub can never assign. The same shape
had already cost a day of queue: with exactly one runner carrying `ci-store`,
`replicated Store contracts (legacy)` took 45 attempts on 2026-09-02, started
15, had 27 cancelled while still queued having never run, and reached waits of
234 minutes.

Registered labels are a GitHub-side property of an already-registered runner,
not host access, so the rebalance was applied through the Actions API without
touching `nuc2`'s SSH host key. The fleet carries **two** `ci-store` slots:

| runner | host | production voter | status |
|---|---|---|---|
| `gha-nuc1-general-01` | `nuc1` | no | slot |
| `gha-rogg16-general-02` | `rogg16` | no | slot |
| `gha-nuc2-android-01` | `nuc2` | no | label revoked — see §4.2 |

with `ci-topology` on `gha-rogg16-general-01`. One slot per physical host is a
deliberate invariant, not an accident of counting: two three-voter hiqlite
tests on one loaded host is the quorum flake behind the artwork-fence
failures. No voter carries either label.

[`validation/runner-fleet.toml`](../validation/runner-fleet.toml) is the
committed roster of that fleet, and `make operations-check` now rejects any
`runs-on` in `.github/workflows/` that no rostered runner can satisfy — new
workflow files included, automatically. Adding a label to a workflow means
adding it to a runner first. See
[VALIDATION.md](VALIDATION.md#the-runner-roster--a-label-is-a-claim-about-a-machine).

### 4.2 Why there are two Store slots and not three

`gha-nuc2-android-01` was added as a third `ci-store` slot on 2026-09-02 and
failed `replicated Store contracts (legacy)` three times in a row — runs 2840,
2842 and 2845 — about twelve seconds into each job, before compiling anything:

```text
warning: failed to write cache, path: /home/runner/.cargo/registry/index/
index.crates.io-1949cf8c6b5b557f/.cache/as/yn/async-trait,
error: Permission denied (os error 13)
Permission denied (os error 13)
##[error]Process completed with exit code 2.
```

That Android guest has no runner-writable Cargo home, so `cargo test` dies on
the registry index. `gha-nuc1-general-01` (run 2838) and
`gha-rogg16-general-02` (run 2837) ran the same lane green in the same hour, so
this is the host and not the lane. The label was revoked.

The shard count therefore dropped from three to two, and this is what that
sequence is worth recording for: the count is *bounded by* the slots because
one physical host may hold only one, so a surplus shard has no runner of its
own to take and serialises behind a busy one. `make operations-check` enforces
the bound and reported the mismatch as `2 not greater than or equal to 3: the
Store shard count exceeds the number of ci-store slots` the moment the roster
lost the slot — before the workflow could reach a queue.

Two remains a real fan-out, not a degenerate one: `validation/store_shard.py`
enforces two as its floor in `assigned_tests` and again in the aggregate
validator, so exact union and disjointness are proved for two shards exactly as
for three. **Restoring a third shard is one `SHARD_COUNT` line and one matrix
index** — once `nuc2`'s Cargo home is writable and that runner passes the lane,
or once another non-voter host qualifies. Re-adding the label without fixing
the host buys three failed jobs, not a third shard.

**Open follow-up.** The rebalance was applied to the live runners through the
API. `pjunod/ansible`'s `github-runners/inventory/` does not carry the current
label sets for `gha-nuc1-general-01`, `gha-nuc2-android-01`,
`gha-rogg16-general-01` or `gha-rogg16-general-02` — it still describes them as
generic `general` builders. Re-running the runner playbook before that
inventory is corrected would revert the fleet to the one-slot state and
reintroduce the queue. `pjunod/ansible` PR #20 is open to reconcile it. Disk
facts for `nuc1`, `nuc2` and `rogg16` also remain unmeasured.

## 5. Persistent Cargo cache contract

`.github/actions/cargo-cache/action.yml` is the only workflow entry point.
On a disposable or legacy runner it preserves `Swatinem/rust-cache`. On an
eligible self-hosted runner in `shadow` or `accelerated` mode it sets:

```text
$RUNNER_TOOL_CACHE/plurx-ci/cargo/
  <runner-name>/
    rust-<rustc-release>/
      cargo-home/
      <lane>/target/
```

The runner name prevents two runner services on one host from writing one
target directory. The `rustc -Vv` release prevents a toolchain upgrade from
reusing incompatible objects. The lane separates incompatible invocations
such as coverage, release targets, browser acceptance, cluster proofs, and the
ordinary Rust gate. The action exports all three values through
`GITHUB_ENV`:

```text
CARGO_HOME=<toolchain-root>/cargo-home
CARGO_TARGET_DIR=<toolchain-root>/<lane>/target
CARGO_INCREMENTAL=0
```

`CARGO_TARGET_DIR` is one line and one absolute path; a folded YAML scalar
must never introduce whitespace into it. Release artifact paths consume the
action's explicit `target-dir` output rather than assuming `./target`.

Persistent state also requires the action's explicit `persistent-eligible`
input. It defaults to false and the cache foundation does not set it. Only a
later job route that mechanically selects the dedicated non-voter label may
pass true; changing `CI_EXECUTION_MODE` alone can therefore never activate
persistence on the generic labels currently carried by production voters.

`scripts/ci-cache-prune` canonicalizes every root and candidate before deleting
anything. It refuses traversal, symlink escapes, prefix-confusable roots, and
unrecognized path segments outside
`$RUNNER_TOOL_CACHE/plurx-ci/cargo/<runner>/rust-x.y.z`. Under pressure it can
remove stale toolchain epochs, lane targets, and recreatable Cargo registry/git
cache directories. Explicit `.last-used` markers, refreshed after each job,
provide the eviction order instead of unreliable directory mtimes.

The pruner runs before work and through an `always()` finalizer after work. It
rechecks both conditions after exhausting safe candidates and fails the job if
the Cargo root is still over budget or the filesystem remains below its
reserve. The stated limit is therefore a postcondition, not a best-effort
preflight observation.

The initial Cargo ceiling is 30 GiB per eligible runner across all toolchain
epochs. Exactly one runner service per physical builder is eligible for
persistent-heavy work, so this is also the host's active Cargo ceiling.
Toolchain epochs can coexist only while the host floor remains satisfied; the
pruner evicts stale epochs oldest-first when either the budget or floor is
violated.

## 6. Persistent BuildKit and Forgejo cache contract

`.github/actions/buildx-cache/action.yml` selects one stable builder name per
physical runner and asks `docker/setup-buildx-action` to retain BuildKit state
on cleanup. Sharing that builder across its sequential lanes makes the 50 GiB
limit host-wide instead of multiplying the allowance by the number of lanes.
Legacy and hosted jobs keep an ephemeral builder and the existing GitHub cache
backend. Persistent jobs rely on the named builder's local state, avoiding a
save/upload after every run.

The builder uses `.github/buildkitd.toml`:

```toml
[registry."192.168.4.7:3000"]
  http = true
```

This is required even when the host Docker daemon already trusts that
registry: the `docker-container` BuildKit daemon has its own registry client.
Adding this file does not restart or reconfigure Docker on any host.

The initial BuildKit ceiling is 50 GiB per eligible runner. An `always()`
finalizer computes the reserve from the filesystem containing Docker's root,
passes both limits to BuildKit, then independently verifies cache usage and
free space:

```text
cache <= 50 GiB
free space >= max(100 GiB, 20% of the Docker filesystem)
```

The Forgejo remote-cache fallback is read-mostly:

- immutable or content-addressed cache references are preferred;
- only per-architecture writer lanes receive package-write credentials;
- pull credentials are scoped to package reads;
- cache import failure is a cold-build warning, not a false green;
- cache export is best effort and never replaces test/package evidence;
- secrets are passed through GitHub secrets and never committed or printed.

The fallback is enabled only after the local-cache timing sample proves a
need and the registry credentials exist. The first cache PR deliberately
ships the BuildKit HTTP contract before it ships credentialed import/export,
so a later opt-in cannot silently fail because the builder assumed HTTPS.

## 7. Per-host disk budgets

Each intended builder is sized independently. Reserve disk before assigning
cache:

```text
reserve = max(100 GiB, 20% of filesystem size)
discretionary = max(0, free space at audit - reserve)
```

Allocate discretionary CI storage approximately 50/30/20:

- 50% BuildKit layers, capped initially at 50 GiB;
- 30% Cargo registry, git, and target state, capped initially at 30 GiB;
- 20% checkout, tools, fixtures, logs, and growth headroom.

The ratio is a sizing rule, not permission to cross the reserve. The runtime
pruners use the reserve as the stronger condition. A host with insufficient
discretionary space gets smaller action inputs or remains cache-disabled.

## 8. One build/package/smoke job per architecture

The release repair in PR #758 established the exact two-binary package
contract: `plurxd` and `plurx-cluster-check`, each with its digest. The
accelerated Docker graph reuses that contract rather than compiling the
workspace again inside an ordinary smoke build.

For each architecture, one job performs:

1. checkout the exact candidate with full version history;
2. compile both required binaries for that target;
3. record their SHA-256 digests and a manifest containing target, candidate
   commit, Git tree, toolchain, and binary names;
4. render the runtime-only Dockerfile with
   `validation.release_dockerfile`;
5. build the runtime image from those exact local binaries;
6. start, probe, stop/start, and re-probe the image;
7. retain only the small manifest/digest receipt for ordinary PRs.

There is no release-binary artifact hand-off between jobs. Pushes and final
qualifications keep the existing one-day binary retention rule. The manifest
validator rejects at least: a missing binary, an extra binary, a digest
mismatch, an architecture mismatch, and a `git_tree` from another candidate.
The tag publisher uses the same shape: a trusted helper derives a
binary-export Dockerfile from the exact historical tag, then that
architecture's job compiles, manifests, packages, pushes by digest, and smokes
without uploading binaries for a second job to download. Only the two small
smoked platform-digest receipts cross into alias publication.

The focused source-build lane remains for inputs whose risk exists only in the
builder stage. Its fail-open selector includes:

- `Dockerfile` and `.dockerignore`;
- every `Cargo.toml`, `Cargo.lock`, and `rust-toolchain.toml`;
- `vendor/**`;
- every Rust `build.rs` and tracked build helper;
- `validation/release_dockerfile.py` and its tests;
- the package job's Buildx composite, BuildKit configuration, execution-mode
  selector, and bounded-prune helper;
- unknown paths that the selector cannot classify safely.

The generated binary-export target retains only the tagged Bookworm build
stage and copies the exact runtime binary set plus `rustc -Vv` identity into a
scratch export. The runtime build consumes a different generated Dockerfile
that contains no Rust base, Cargo command, or `COPY --from=build`, proving the
workspace was compiled once per architecture job.

The strict artifact verifier reads each binary's ELF header and requires its
machine to match the declared GNU target. It does not accept the manifest's
target string as proof. Images containing `plurx-cluster-check` also execute
that binary's `build-identity` command and compare it with the exact candidate
commit, so the secondary binary receives both architecture and identity
coverage instead of merely being copied beside the daemon.

Hosted compile and runtime graphs use distinct GitHub cache scopes. The
binary-export build imports and exports a `mode=max` compile cache; the
runtime-only build round-trips a separate `mode=min` image cache. This prevents
the smaller runtime graph from overwriting the expensive compile graph. On a
persistent builder, Cargo registry access is locked and each Cargo target
mount is both locked and architecture-specific. Debug-only tagged binary
retention is best effort and runs after the required small digest receipt, so
artifact quota cannot block alias publication.

The smoke wording says stop/start where that is the actual operation. The
script may change its error text from “restart” to “stop/start”; it does not
gain authority to restart the Docker daemon.

## 9. Native ARM64 lane

Native ARM runs in isolated Lima Linux VMs, not in the macOS runner and not as
an x86 QEMU compile. The MacBook Pro hosts the primary VM; the MacBook Air
hosts a smaller backup VM alongside its existing macOS/Xcode runner. Both
guests prove `Linux/aarch64` and a `linux/aarch64` Docker engine before building,
then compile, package, start, and probe the exact candidate natively.

The first final qualification disproved the proposed required-QEMU fallback:
an x86 runner spent more than 86 minutes in the ARM compile before the job was
cancelled. That is a failed design, not a slow pass. After operator review, the
required self-hosted arm64 matrix row now selects the two-runner `ci-arm64`
pool; the GitHub-hosted mode selects native `ubuntu-24.04-arm`. Neither path
installs QEMU. The separate native shadow job was removed because it would
repeat the same compile and smoke after native ARM became the required path.

The package job and the self-hosted Apple simulator job share the repository
concurrency group `plurx-apple-silicon-heavy`, preventing the Air's macOS and
Linux tenants from doing heavy work together. No replicated cluster shard or
timing-sensitive test runs on either laptop. Both VMs start on host login and
their guest runner services start under systemd. Laptop sleep remains an
availability limitation; the two-host pool is the accepted operational
trade-off until dedicated always-on ARM hardware replaces it.

## 10. Split replicated Store from topology

The current combined job has two independently actionable responsibilities:

- Store semantics in the large `store_contract` inventory;
- multi-process topology/activation behavior in the harness and Hiqlite proof.

Phase A puts them in separate jobs on distinct non-production x86 builders.
Neither inventory changes. WAL recovery and real-daemon contracts remain
their existing independent lanes. The split is accepted only if the union of
commands equals the legacy combined command and both jobs retain their own
logs/artifacts on failure.

The topology job may use one Cargo invocation for its selected tests. It may
not run on the Apple laptop, share a host with the other replicated shard, or
weaken deliberate shutdown and election timeouts to improve its graph time.

## 11. Deterministic Store sharding

Phase B discovers test names from the compiled Store test binary, assigns
each exact name to one shard, and invokes the binary once per shard with all
assigned exact filters. It does not launch Cargo once per test.

The shard count is two, because the fleet carries two eligible `ci-store` slots
and one physical host may hold only one (§4.1, §4.2). It is written once, as
the `SHARD_COUNT` job environment variable in
`.github/workflows/store-shards.yml`, and both `validation.store_shard`
invocations read it; the matrix is the only other mention, because a matrix
cannot be built from `env`. A disagreement between the two fails closed rather
than silently dropping tests — the aggregate validator rejects a receipt set
whose size or index set does not match the count the receipts declare — and
`make operations-check` rejects the mismatch at preflight, along with a shard
count that exceeds the number of `ci-store` slots. Two is also this module's
enforced floor, so the count can rise with the fleet but cannot collapse to a
single unsharded job wearing the sharded lane's name.

Every shard selects the same label set. Giving a shard a runner class of its
own is what produced `ci-store-shard-1`; one label set for the whole matrix
means a shard cannot be pinned to a machine that does not exist.

`replicated Store shards` also accepts `workflow_dispatch`, with an
`execution-mode` input defaulting to `shadow`. That is how the sharded path is
proved on the real fleet before `CI_EXECUTION_MODE` is touched, without
spending pull-request traffic on the rehearsal.

The version-one assignment is deterministic hash partitioning. Every shard
writes a receipt containing:

- candidate commit and Git tree;
- test-binary digest;
- complete discovered inventory;
- assigned names and shard count/index;
- started/completed names and durations;
- pass/fail/ignored outcome;
- assignment algorithm version.

The aggregate validator proves:

```text
union(shard assignments) == discovered inventory
intersection(any two shard assignments) == empty
every assigned non-ignored test completed exactly once
all receipts name the same candidate tree and test binary
```

The test count is intentionally dynamic; no contract hard-codes yesterday's
inventory. Wrong-candidate-tree injection is a required regression test.

Hashing balances count, not time. Receipts feed a committed duration table;
assignment version two uses deterministic longest-processing-time placement
when the sample is stable enough. Updating that table is a reviewable tooling
operation, and missing names fall back to the median duration. The same union
validator applies to both algorithms.

A scheduled full, unsharded x86 run remains the architecture and assignment
backstop. There is no monthly partition salt: the unsharded run supplies the
independent proof without churning the duration table.

The implementation builds each shard's test binary independently in the same
`/src` path from `rust:1.97.1-bookworm`, using locked architecture-specific
BuildKit mounts. This avoids a large GitHub artifact hand-off. The aggregate
requires both SHA-256 digests to match, so host, image, or path drift fails
closed instead of combining unlike executables.

## 12. Rollout and acceptance

Each repository milestone is one reviewable PR into
`effort/ci-execution-acceleration`. Every PR receives an adversarial agent
review; findings are implemented and the changed PR is reviewed again before
merge.

| Milestone | Deliverable | Focused acceptance |
|---|---|---|
| M0 | Host/runner/label/disk audit | Two distinct non-voter x86 hosts selected; all voters excluded from heavy label |
| M1 | Cargo cache + fail-safe mode | warm lane reuses local objects; unsafe prune roots rejected |
| M2 | named BuildKit + registry contract | warm image reuses layers; size and floor visible in summary |
| M3 | native Linux ARM pool | two isolated runners online; an exact cold package/probe passes without QEMU |
| M4 | same-job package smoke | both architectures compile/package/probe exact two-binary manifest |
| M5 | Store/topology split | command/inventory union equals legacy; separate receipts retained |
| M6 | one Store shard per `ci-store` slot (two today) | exact union/disjointness validator passes; wall time improves without missing tests |
| M7 | shadow campaign | 10 consecutive accelerated shadow runs agree with legacy |
| M8 | accelerated candidate | 20 consecutive required-candidate runs pass |
| M9 | final qualification | one frozen-tree full suite passes, receipt exists, effort merges |

Any cluster-lane flake in the first 30 days after cutover immediately returns
that lane to shadow and makes legacy required again while the cause is
diagnosed. `legacy` remains runnable for the complete 30 days even if no
tripwire fires.

The final candidate process is:

```bash
git fetch origin main
git switch effort/ci-execution-acceleration
git merge --no-ff origin/main
git push
gh pr create --base main --head effort/ci-execution-acceleration
gh pr checks <pr> --watch
gh run download <run-id> -n effort-qualification-<pr>
jq . qualification-receipt.json
```

All selected jobs, unit tests, platform tests, package probes, and the `Main
promotion gate` must pass on that exact tree. Fixes restart qualification;
passing results from the previous candidate are not reused.

## 13. Rollback and incident behavior

Repository rollback is one variable change:

```bash
scripts/ci-execution-mode legacy
```

If accelerated jobs are queued behind missing labels, a cache backend is
unavailable, receipt validation disagrees, or a new flake appears, select
legacy before changing the mechanism. Cache deletion is not required for
rollback. Local state may be pruned later through the bounded scripts.

Forgejo loss produces cold local/hosted builds; it must not turn a missing
cache into missing evidence. A Docker runtime smoke failure may stop/start its
own test container. It does not authorize `systemctl restart docker`, a Docker
Desktop restart, or any production service operation.

## 14. Deferred work

These are explicitly separate efforts:

- shared three-voter fixtures across tests;
- automatic runner provisioning or access-key distribution;
- replacing the laptop-hosted required ARM pool with dedicated always-on ARM
  hardware;
- changing release publication away from its current GitHub-hosted Bookworm
  contract;
- changing production fleet deployment/build roles;
- content-fingerprint reuse of test results from another candidate tree.

## 15. Decision ledger

| Decision | Recorded choice | Reason |
|---|---|---|
| Scheduler | Keep GitHub Actions | Existing trust, UI, receipts, and gates remain useful |
| Rollout switch | `legacy` / `shadow` / `accelerated`, unknown → `legacy` | Fail-safe and reversible |
| Required check | Keep `Main promotion gate` | Stable repository contract |
| Persistent Cargo | Isolated per runner/toolchain/lane; budgeted per runner | Avoid concurrent corruption while enforcing a host-scale ceiling |
| BuildKit | One stable named local builder per runner | Avoid per-job remote save/restore and enforce one host-wide cap |
| Registry | Optional Forgejo fallback with builder-local HTTP config | Read-mostly LAN cache without daemon changes |
| Budgets | Per host; reserve first; approximate 50/30/20 | The original global arithmetic did not fit every disk |
| Package hand-off | Same job per architecture | Avoid GitHub artifact quota and wrong-workspace risk |
| ARM | Two Linux VM runners, required for self-hosted packaging | Eliminate the failed x86/QEMU compile; retain a native GitHub-hosted fallback mode |
| First shards | One shard per `ci-store` slot; non-production x86 hosts that pass the lane | Homogeneous baseline, no production/laptop load, and no shard pinned to a label nothing carries |
| Assignment v1 | Hash plus exact-union receipt | Simple, deterministic, auditable start |
| Assignment v2 | Deterministic LPT from committed durations | Balance seconds after trustworthy measurements exist |
| Scheduled backstop | Full unsharded x86, no salt | Proves inventory independent of assignment |
| Campaign | 10 shadow + 20 required-candidate runs | Deliberate cutover sample |
| Flake tripwire | Any cluster flake in 30 days returns lane to shadow | Timing regressions are rare and costly |
| Shared fixture | Separate effort | It changes test semantics, not only scheduling |
| Legacy retention | 30 days | Immediate rollback while the new lane proves itself |

## 16. Implementation record

- PR #758 repaired release packaging before this effort: the real Dockerfile
  now has an explicit two-binary contract, both binaries and digests are in
  the manifest, and the test renders the real call site.
- The effort branch began at repaired `main` commit `950db83d`.
- The initial cache foundation is default-dark behind `CI_EXECUTION_MODE` and
  adds no host or Docker lifecycle changes.
- PR #761 added the bounded Cargo and BuildKit cache foundation.
- PR #764 combined package compilation and smoke in one architecture-local job.
- PR #767 split Store and topology onto distinct non-voter hosts with separate
  logs, exact-tree receipts, caches, and failure propagation.
- Deterministic Store sharding is implemented default-dark with a stable
  required verdict, dynamic SHA-256 partitioning, exact-union validation, and a
  weekly full unsharded backstop.
- Shadow invokes the same-commit reusable shard workflow outside every required
  job's dependency graph; accelerated execution calls that workflow from the
  required Store path. An offline shadow runner cannot delay promotion.
- The first promotion qualification proved the required arm64 matrix was still
  compiling under x86 QEMU and was cancelled after more than 86 minutes. The
  package matrix now selects native Linux/ARM64 runners, proves guest and
  Docker architecture before building, shares a heavy-work concurrency lock
  with Apple CI, and has no QEMU setup. The former default-dark native shadow
  job was removed as duplicate work.
- Authorized host provisioning created native Ubuntu 26.04 ARM64 Lima VMs on
  the MacBook Pro (12 CPU, 24 GB, sparse 180 GB disk) and MacBook Air (6 CPU,
  16 GB, sparse 140 GB disk). Repository-scoped runners
  `gha-mbp-linux-arm-01` and `gha-mba-linux-arm-01` are online with the
  dedicated `ci-arm64` label; the Air is the backup. The guest Docker daemons
  are independent of Docker Desktop and production Docker.
- M0's voter audit is complete for the active Store/topology labels.
- The Store matrix fans out to one shard per `ci-store` slot after the
  2026-09-02 label rebalance (§4.1) — two today, on `nuc1` and `rogg16`. The
  unassignable `ci-store-shard-1` row is gone, and
  `validation/runner-fleet.toml` plus `make operations-check` make an
  unschedulable `runs-on` a preflight failure rather than a queue.
  `workflow_dispatch` on `store-shards.yml` proves the path on demand.
- A third slot on `gha-nuc2-android-01` was tried and revoked the same day for
  an unwritable Cargo home (§4.2). The slot bound is the check that caught the
  now-oversized shard count, at preflight, rather than in a queue. Restoring a
  third shard is a two-line change once that host is fixed.
- Disk facts for `nuc1`, `nuc2` and `rogg16`, the changed `nuc2` host key, and
  the ansible inventory reconciliation (`pjunod/ansible` PR #20) remain open;
  none of them now blocks scheduling.
- No Docker daemon was restarted or reconfigured for this implementation.
